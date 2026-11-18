use crate::{
    core::{Error, Result, now},
    dal::Store,
    domain::auth::CallerIdentity,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSettingItem {
    pub key: String,
    pub value_int: Option<i64>,
    pub value_text: Option<String>,
    pub description: String,
    pub updated_at: i64,
    pub updated_by: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateSettingInput {
    pub key: String,
    pub value_int: Option<i64>,
    pub value_text: Option<String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminStatistics {
    pub studies_total: i64,
    pub quizzes_public: i64,
    pub quizzes_private: i64,
    pub questions_cortex: i64,
    pub questions_creator: i64,
    pub attempts_total: i64,
    pub attempts_completed: i64,
    pub certificates_active: i64,
    pub certificates_revoked: i64,
    pub cleanup_pending_or_failed: i64,
}

fn ensure_admin(caller: &CallerIdentity) -> Result<()> {
    if !caller.is_admin() {
        return Err(Error::forbidden("admin_required: Admin access required"));
    }
    Ok(())
}

pub async fn get_settings(
    store: &Store,
    caller: &CallerIdentity,
) -> Result<Vec<RuntimeSettingItem>> {
    ensure_admin(caller)?;
    let rows = sqlx::query(
        "SELECT key, value_int, value_text, description, updated_at, updated_by
         FROM runtime_settings ORDER BY key",
    )
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RuntimeSettingItem {
            key: r.get("key"),
            value_int: r.try_get("value_int").ok(),
            value_text: r.try_get("value_text").ok(),
            description: r.get("description"),
            updated_at: r.get("updated_at"),
            updated_by: r.get("updated_by"),
        })
        .collect())
}

pub async fn patch_settings(
    store: &Store,
    caller: &CallerIdentity,
    updates: Vec<UpdateSettingInput>,
) -> Result<Vec<RuntimeSettingItem>> {
    ensure_admin(caller)?;
    if updates.is_empty() {
        return Err(Error::unprocessable(
            "empty_settings: At least one setting update is required",
        ));
    }
    let ts = now();
    let mut tx = store.db.begin().await?;
    for item in updates {
        let key = item.key.trim();
        if key.is_empty() {
            return Err(Error::unprocessable(
                "invalid_setting_key: Setting key must be non-empty",
            ));
        }
        sqlx::query(
            "INSERT INTO runtime_settings(key, value_int, value_text, description, updated_at, updated_by)
             VALUES($1, $2, $3, $4, $5, $6)
             ON CONFLICT(key) DO UPDATE SET
               value_int = $2,
               value_text = $3,
               description = CASE WHEN $4 <> '' THEN $4 ELSE runtime_settings.description END,
               updated_at = $5,
               updated_by = $6",
        )
        .bind(key)
        .bind(item.value_int)
        .bind(item.value_text)
        .bind(item.description.trim())
        .bind(ts)
        .bind(&caller.user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    get_settings(store, caller).await
}

pub async fn get_statistics(store: &Store, caller: &CallerIdentity) -> Result<AdminStatistics> {
    ensure_admin(caller)?;
    let studies_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM studies")
        .fetch_one(&store.db)
        .await?;
    let quizzes_public: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM quizzes WHERE visibility = 'public'")
            .fetch_one(&store.db)
            .await?;
    let quizzes_private: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM quizzes WHERE visibility = 'private'")
            .fetch_one(&store.db)
            .await?;
    let questions_cortex: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM questions WHERE origin = 'generated_with_cortex'")
            .fetch_one(&store.db)
            .await?;
    let questions_creator: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM questions WHERE origin = 'supplied_by_creator'")
            .fetch_one(&store.db)
            .await?;
    let attempts_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attempts")
        .fetch_one(&store.db)
        .await?;
    let attempts_completed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM attempts WHERE status = 'completed'")
            .fetch_one(&store.db)
            .await?;
    let certificates_active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM certificates WHERE status = 'active'")
            .fetch_one(&store.db)
            .await?;
    let certificates_revoked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM certificates WHERE status = 'revoked'")
            .fetch_one(&store.db)
            .await?;
    let cleanup_pending_or_failed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cleanup_tasks WHERE status IN ('pending', 'failed')",
    )
    .fetch_one(&store.db)
    .await?;

    Ok(AdminStatistics {
        studies_total,
        quizzes_public,
        quizzes_private,
        questions_cortex,
        questions_creator,
        attempts_total,
        attempts_completed,
        certificates_active,
        certificates_revoked,
        cleanup_pending_or_failed,
    })
}
