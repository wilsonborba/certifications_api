use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::auth::{CallerIdentity, require_owner_or_admin},
};
use serde::Serialize;
use sqlx::{Any, Row, pool::PoolConnection};
use std::future::Future;

pub const REMOVED_MESSAGE: &str = "Removed from your workspace.";
pub const DAILY_BATCH_LIMIT: i64 = 100;
const QUESTION_VISUAL_LOCK_CLASS: i32 = 0x5156_4953;

enum QuestionVisualOperationLock {
    Postgres(PoolConnection<Any>, i32, tokio::sync::OwnedSemaphorePermit),
    Sqlite(tokio::sync::OwnedMutexGuard<()>),
}

async fn acquire_question_visual_operation_lock(
    store: &Store,
    render_job_id: &str,
) -> Result<QuestionVisualOperationLock> {
    if store.postgres() {
        // Keep one of the five pool connections available for the operation's
        // ordinary queries/transaction while this session lock is held.
        let permit = store
            .question_visual_operation_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::unavailable())?;
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(render_job_id.as_bytes());
        let lock_id = i32::from_be_bytes(digest[..4].try_into().unwrap());
        let mut connection = store.db.acquire().await?;
        // A cancelled request must close this physical session so its advisory
        // lock cannot be returned to the pool and strand later operations.
        connection.close_on_drop();
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
            .bind(QUESTION_VISUAL_LOCK_CLASS)
            .bind(lock_id)
            .fetch_one(&mut *connection)
            .await?;
        if !acquired {
            return Err(Error::unavailable());
        }
        Ok(QuestionVisualOperationLock::Postgres(
            connection, lock_id, permit,
        ))
    } else {
        Ok(QuestionVisualOperationLock::Sqlite(
            store.sqlite_worker_lock.clone().lock_owned().await,
        ))
    }
}

async fn release_question_visual_operation_lock(lock: QuestionVisualOperationLock) -> Result<()> {
    match lock {
        QuestionVisualOperationLock::Postgres(mut connection, lock_id, _permit) => {
            let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1, $2)")
                .bind(QUESTION_VISUAL_LOCK_CLASS)
                .bind(lock_id)
                .fetch_one(&mut *connection)
                .await?;
            if unlocked {
                Ok(())
            } else {
                Err(Error::unavailable())
            }
        }
        QuestionVisualOperationLock::Sqlite(_guard) => Ok(()),
    }
}

pub(crate) async fn with_question_visual_operation_lock<T, F, Fut>(
    store: &Store,
    render_job_id: &str,
    operation: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let lock = acquire_question_visual_operation_lock(store, render_job_id).await?;
    let result = operation().await;
    release_question_visual_operation_lock(lock).await?;
    result
}

async fn reserved_visual_job_for_resource(
    store: &Store,
    resource_column: &str,
    resource_ref: &str,
) -> Result<Option<String>> {
    // The column name is selected only from the two internal call sites below.
    let sql = format!(
        "SELECT render_job_id FROM question_visual_jobs WHERE {resource_column} = $1 AND operation_fingerprint <> '' ORDER BY created_at DESC LIMIT 1"
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(resource_ref)
        .fetch_optional(&store.db)
        .await?)
}

async fn visual_document_reservation_is_retryable(store: &Store, doc_id: &str) -> Result<bool> {
    let expires_at: Option<i64> = sqlx::query_scalar(
        "SELECT expires_at FROM question_visual_jobs WHERE result_document_id = $1 AND operation_fingerprint <> '' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(doc_id)
    .fetch_optional(&store.db)
    .await?;
    // Uploads remain valid through the expiry second (`now() <= expires_at`).
    Ok(expires_at.is_some_and(|expires_at| now() <= expires_at))
}

pub(crate) async fn queue_question_document_cleanup_with_visual_operation_lock_held(
    store: &Store,
    doc_id: &str,
) -> Result<()> {
    let ts = now();
    sqlx::query(
        "INSERT INTO cleanup_tasks(id, target_kind, target_ref, status, attempts, last_error, created_at, updated_at)
         SELECT $1, 'couch_doc', $2, 'pending', 0, 'reserved visual document cleanup deferred until reservation expiry', $3, $3
         WHERE NOT EXISTS (SELECT 1 FROM cleanup_tasks WHERE target_kind = 'couch_doc' AND target_ref = $2 AND status IN ('pending', 'failed'))",
    )
    .bind(new_id())
    .bind(doc_id)
    .bind(ts)
    .execute(&store.db)
    .await?;
    Ok(())
}

async fn delete_fsm_or_queue_locked(store: &Store, fsm_ref: &str) -> Result<()> {
    if is_fsm_ref_in_use(store, fsm_ref).await? {
        return Ok(());
    }
    if let Err(err) = store.fsm.delete_ref(fsm_ref).await {
        let ts = now();
        sqlx::query(
            "INSERT INTO cleanup_tasks(id, target_kind, target_ref, status, attempts, last_error, created_at, updated_at)
             VALUES($1, 'fsm_blob', $2, 'pending', 1, $3, $4, $4)",
        )
        .bind(new_id())
        .bind(fsm_ref)
        .bind(err.to_string())
        .bind(ts)
        .execute(&store.db)
        .await?;
        tracing::warn!(module = "cleanup", target_ref = %fsm_ref, "Deferred FSM cleanup queued");
    }
    Ok(())
}

pub(crate) async fn delete_fsm_or_queue_with_visual_operation_lock_held(
    store: &Store,
    fsm_ref: &str,
) -> Result<()> {
    delete_fsm_or_queue_locked(store, fsm_ref).await
}

async fn delete_question_document_locked(
    store: &Store,
    doc_id: &str,
    queue_on_failure: bool,
) -> Result<()> {
    if is_question_document_in_use(store, doc_id).await? {
        return Ok(());
    }
    if visual_document_reservation_is_retryable(store, doc_id).await? {
        return queue_question_document_cleanup_with_visual_operation_lock_held(store, doc_id)
            .await;
    }
    if let Err(err) = store.docs.delete(doc_id).await {
        if !queue_on_failure {
            return Err(err);
        }
        let ts = now();
        sqlx::query(
            "INSERT INTO cleanup_tasks(id, target_kind, target_ref, status, attempts, last_error, created_at, updated_at) VALUES($1, 'couch_doc', $2, 'pending', 1, $3, $4, $4)",
        )
        .bind(new_id())
        .bind(doc_id)
        .bind(err.to_string())
        .bind(ts)
        .execute(&store.db)
        .await?;
    }
    Ok(())
}

pub(crate) async fn cleanup_question_document(store: &Store, doc_id: &str) -> Result<()> {
    if let Some(render_job_id) =
        reserved_visual_job_for_resource(store, "result_document_id", doc_id).await?
    {
        with_question_visual_operation_lock(store, &render_job_id, || async {
            delete_question_document_locked(store, doc_id, true).await
        })
        .await
    } else {
        delete_question_document_locked(store, doc_id, true).await
    }
}

pub(crate) async fn cleanup_question_document_with_visual_operation_lock_held(
    store: &Store,
    doc_id: &str,
) -> Result<()> {
    delete_question_document_locked(store, doc_id, true).await
}

#[derive(Clone, Debug, Serialize)]
pub struct CleanupItem {
    pub id: String,
    pub target_kind: String,
    pub target_ref: String,
    pub status: String,
    pub attempts: i64,
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CleanupSummary {
    pub pending_count: i64,
    pub failed_count: i64,
    pub items: Vec<CleanupItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BatchResult {
    pub attempted: usize,
    pub completed: usize,
    pub failed: usize,
}

pub async fn is_fsm_ref_in_use(store: &Store, fsm_ref: &str) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sources WHERE fsm_ref = $1 AND status <> 'deleted'",
    )
    .bind(fsm_ref)
    .fetch_one(&store.db)
    .await?;
    if count > 0 {
        return Ok(true);
    }
    let retained_visual: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM question_visuals v JOIN question_version_visual_refs r ON r.visual_id = v.id WHERE v.fsm_ref = $1",
    )
    .bind(fsm_ref)
    .fetch_one(&store.db)
    .await?;
    Ok(retained_visual > 0)
}

async fn is_question_document_in_use(store: &Store, doc_id: &str) -> Result<bool> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM question_versions WHERE couch_doc_id = $1")
            .bind(doc_id)
            .fetch_one(&store.db)
            .await?;
    Ok(count > 0)
}

pub async fn delete_fsm_or_queue(store: &Store, fsm_ref: &str) -> Result<()> {
    if fsm_ref.trim().is_empty() {
        return Ok(());
    }
    if let Some(render_job_id) =
        reserved_visual_job_for_resource(store, "result_fsm_ref_key", fsm_ref).await?
    {
        with_question_visual_operation_lock(store, &render_job_id, || async {
            delete_fsm_or_queue_locked(store, fsm_ref).await
        })
        .await
    } else {
        delete_fsm_or_queue_locked(store, fsm_ref).await
    }
}

pub async fn list_cleanup(store: &Store, limit: i64) -> Result<CleanupSummary> {
    let limit = limit.clamp(1, DAILY_BATCH_LIMIT);
    let pending_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cleanup_tasks WHERE status = 'pending'")
            .fetch_one(&store.db)
            .await?;
    let failed_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cleanup_tasks WHERE status = 'failed'")
            .fetch_one(&store.db)
            .await?;
    let rows = sqlx::query(
        "SELECT id, target_kind, target_ref, status, attempts, last_error, created_at, updated_at
         FROM cleanup_tasks ORDER BY updated_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&store.db)
    .await?;

    let items = rows
        .into_iter()
        .map(|r| CleanupItem {
            id: r.get("id"),
            target_kind: r.get("target_kind"),
            target_ref: r.get("target_ref"),
            status: r.get("status"),
            attempts: r.get("attempts"),
            last_error: r.get("last_error"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })
        .collect();

    Ok(CleanupSummary {
        pending_count,
        failed_count,
        items,
    })
}

async fn execute_cleanup_task(
    store: &Store,
    id: &str,
    target_kind: &str,
    target_ref: &str,
) -> Result<bool> {
    let outcome = match target_kind {
        "fsm_blob" => {
            if let Some(render_job_id) =
                reserved_visual_job_for_resource(store, "result_fsm_ref_key", target_ref).await?
            {
                with_question_visual_operation_lock(store, &render_job_id, || async {
                    if is_fsm_ref_in_use(store, target_ref).await? {
                        Ok(true)
                    } else {
                        store.fsm.delete_ref(target_ref).await.map(|()| true)
                    }
                })
                .await
            } else if is_fsm_ref_in_use(store, target_ref).await? {
                Ok(true)
            } else {
                store.fsm.delete_ref(target_ref).await.map(|()| true)
            }
        }
        "couch_doc" => {
            if let Some(render_job_id) =
                reserved_visual_job_for_resource(store, "result_document_id", target_ref).await?
            {
                with_question_visual_operation_lock(store, &render_job_id, || async {
                    if is_question_document_in_use(store, target_ref).await? {
                        return Ok(true);
                    }
                    if visual_document_reservation_is_retryable(store, target_ref).await? {
                        return Ok(false);
                    }
                    delete_question_document_locked(store, target_ref, false)
                        .await
                        .map(|()| true)
                })
                .await
            } else {
                delete_question_document_locked(store, target_ref, false)
                    .await
                    .map(|()| true)
            }
        }
        _ => Ok(true),
    };

    match outcome {
        Ok(false) => Ok(false),
        Ok(true) => {
            sqlx::query("DELETE FROM cleanup_tasks WHERE id = $1")
                .bind(id)
                .execute(&store.db)
                .await?;
            Ok(true)
        }
        Err(err) => {
            sqlx::query(
                "UPDATE cleanup_tasks SET status = 'failed', attempts = attempts + 1, last_error = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(err.to_string())
            .bind(now())
            .bind(id)
            .execute(&store.db)
            .await?;
            Ok(false)
        }
    }
}

pub async fn retry_one(store: &Store, cleanup_id: &str) -> Result<bool> {
    let row = sqlx::query("SELECT id, target_kind, target_ref FROM cleanup_tasks WHERE id = $1")
        .bind(cleanup_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let kind: String = row.get("target_kind");
    let target_ref: String = row.get("target_ref");
    execute_cleanup_task(store, cleanup_id, &kind, &target_ref).await
}

/// Bounded daily cleanup task retrying outstanding deferred deletions.
pub async fn run_daily_batch(store: &Store, batch_limit: i64) -> Result<BatchResult> {
    let limit = batch_limit.clamp(1, DAILY_BATCH_LIMIT);
    let rows = sqlx::query(
        "SELECT id, target_kind, target_ref FROM cleanup_tasks WHERE status IN ('pending', 'failed') ORDER BY updated_at ASC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&store.db)
    .await?;

    let mut res = BatchResult {
        attempted: rows.len(),
        completed: 0,
        failed: 0,
    };
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("target_kind");
        let target_ref: String = row.get("target_ref");
        if execute_cleanup_task(store, &id, &kind, &target_ref).await? {
            res.completed += 1;
        } else {
            res.failed += 1;
        }
    }
    tracing::info!(
        module = "cleanup",
        attempted = res.attempted,
        completed = res.completed,
        failed = res.failed,
        "Daily cleanup batch finished"
    );
    Ok(res)
}

pub async fn delete_quiz(
    store: &Store,
    caller: Option<&CallerIdentity>,
    quiz_id: &str,
    as_admin: bool,
) -> Result<()> {
    let row = sqlx::query("SELECT owner_id, visibility FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    let visibility: String = row.get("visibility");

    if !as_admin {
        let c = caller.ok_or_else(|| Error::unauthorized("identity_required"))?;
        require_owner_or_admin(c, &owner_id)?;
        if visibility == "public" {
            return Err(Error::forbidden(
                "published_quiz_locked: Published quizzes cannot be unpublished or deleted by the owner",
            ));
        }
    }

    sqlx::query("DELETE FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .execute(&store.db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod question_visual_lock_tests {
    use super::with_question_visual_operation_lock;
    use crate::dal::Store;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::Notify;

    #[tokio::test]
    async fn sqlite_visual_failure_cleanup_and_same_job_retry_are_serialized() {
        let store = Arc::new(Store::fixture().await);
        let entered_cleanup = Arc::new(Notify::new());
        let finish_cleanup = Arc::new(Notify::new());
        let retry_entered = Arc::new(AtomicBool::new(false));

        let first = {
            let store = store.clone();
            let entered = entered_cleanup.clone();
            let finish = finish_cleanup.clone();
            tokio::spawn(async move {
                with_question_visual_operation_lock(&store, "same-render-job", || async {
                    entered.notify_one();
                    finish.notified().await;
                    Ok(())
                })
                .await
            })
        };
        entered_cleanup.notified().await;

        let retry = {
            let store = store.clone();
            let entered = retry_entered.clone();
            tokio::spawn(async move {
                with_question_visual_operation_lock(&store, "same-render-job", || async {
                    entered.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!retry_entered.load(Ordering::SeqCst));

        finish_cleanup.notify_one();
        first.await.unwrap().unwrap();
        retry.await.unwrap().unwrap();
        assert!(retry_entered.load(Ordering::SeqCst));
    }
}
