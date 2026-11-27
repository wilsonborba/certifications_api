use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::auth::{CallerIdentity, require_owner_or_admin},
};
use serde::Serialize;
use sqlx::Row;

pub const REMOVED_MESSAGE: &str = "Removed from your workspace.";
pub const DAILY_BATCH_LIMIT: i64 = 100;

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
            if is_fsm_ref_in_use(store, target_ref).await? {
                Ok(())
            } else {
                store.fsm.delete_ref(target_ref).await
            }
        }
        "couch_doc" => {
            if is_question_document_in_use(store, target_ref).await? {
                Ok(())
            } else {
                store.docs.delete(target_ref).await
            }
        }
        _ => Ok(()),
    };

    match outcome {
        Ok(()) => {
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
