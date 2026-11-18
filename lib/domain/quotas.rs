use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
};
use axum::http::StatusCode;
use serde::Serialize;
use sqlx::Row;
use std::sync::{Arc, RwLock};

pub const DEFAULT_MAX_FILE_BYTES: i64 = 50_000_000;
pub const DEFAULT_MAX_STUDY_BYTES: i64 = 150_000_000;
pub const DEFAULT_MAX_ACCOUNT_BYTES: i64 = 500_000_000;

#[derive(Clone, Debug, Serialize)]
pub struct QuotaConfig {
    pub max_file_bytes: i64,
    pub max_study_bytes: i64,
    pub max_account_bytes: i64,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_study_bytes: DEFAULT_MAX_STUDY_BYTES,
            max_account_bytes: DEFAULT_MAX_ACCOUNT_BYTES,
        }
    }
}

#[derive(Clone, Default)]
pub struct QuotaCache {
    snapshot: Arc<RwLock<QuotaConfig>>,
}

impl QuotaCache {
    pub fn new(config: QuotaConfig) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(config)),
        }
    }

    pub fn get(&self) -> QuotaConfig {
        self.snapshot.read().unwrap().clone()
    }

    pub fn set(&self, config: QuotaConfig) {
        *self.snapshot.write().unwrap() = config;
    }

    pub async fn refresh_from_db(&self, store: &Store) -> Result<QuotaConfig> {
        let rows = sqlx::query(
            "SELECT key, value_int FROM runtime_settings WHERE key IN ('max_file_bytes', 'max_study_bytes', 'max_account_bytes')",
        )
        .fetch_all(&store.db)
        .await?;
        let mut cfg = QuotaConfig::default();
        for row in rows {
            let key: String = row.get("key");
            let val: Option<i64> = row.get("value_int");
            if let Some(v) = val.filter(|v| *v > 0) {
                match key.as_str() {
                    "max_file_bytes" => cfg.max_file_bytes = v,
                    "max_study_bytes" => cfg.max_study_bytes = v,
                    "max_account_bytes" => cfg.max_account_bytes = v,
                    _ => {}
                }
            }
        }
        self.set(cfg.clone());
        Ok(cfg)
    }

    pub async fn update_in_db(
        &self,
        store: &Store,
        cfg: QuotaConfig,
        updated_by: &str,
    ) -> Result<QuotaConfig> {
        if cfg.max_file_bytes <= 0
            || cfg.max_study_bytes < cfg.max_file_bytes
            || cfg.max_account_bytes < cfg.max_study_bytes
        {
            return Err(Error::unprocessable(
                "invalid_quota: Require 0 < max_file_bytes <= max_study_bytes <= max_account_bytes",
            ));
        }
        let ts = now();
        let mut tx = store.db.begin().await?;
        for (k, v) in [
            ("max_file_bytes", cfg.max_file_bytes),
            ("max_study_bytes", cfg.max_study_bytes),
            ("max_account_bytes", cfg.max_account_bytes),
        ] {
            sqlx::query(
                "INSERT INTO runtime_settings(key, value_int, updated_at, updated_by) VALUES($1, $2, $3, $4)
                 ON CONFLICT(key) DO UPDATE SET value_int = excluded.value_int, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            )
            .bind(k)
            .bind(v)
            .bind(ts)
            .bind(updated_by)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.set(cfg.clone());
        Ok(cfg)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct QuotaMeter {
    pub used_bytes: i64,
    pub reserved_bytes: i64,
    pub max_file_bytes: i64,
    pub max_study_bytes: i64,
    pub max_account_bytes: i64,
    pub remaining_account_bytes: i64,
}

pub async fn meter_for_owner(
    store: &Store,
    owner_id: &str,
    cfg: &QuotaConfig,
) -> Result<QuotaMeter> {
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0) FROM source_ledger WHERE owner_id = $1 AND state = 'committed'",
    )
    .bind(owner_id)
    .fetch_one(&store.db)
    .await?;
    let reserved: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0) FROM source_ledger WHERE owner_id = $1 AND state = 'reserved'",
    )
    .bind(owner_id)
    .fetch_one(&store.db)
    .await?;
    let total = used.saturating_add(reserved);
    Ok(QuotaMeter {
        used_bytes: used,
        reserved_bytes: reserved,
        max_file_bytes: cfg.max_file_bytes,
        max_study_bytes: cfg.max_study_bytes,
        max_account_bytes: cfg.max_account_bytes,
        remaining_account_bytes: (cfg.max_account_bytes - total).max(0),
    })
}

pub async fn reserve_bytes(
    store: &Store,
    owner_id: &str,
    study_id: &str,
    source_id: &str,
    bytes: i64,
    cfg: &QuotaConfig,
) -> Result<String> {
    if bytes <= 0 || bytes > cfg.max_file_bytes {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "quota_exceeded: File size exceeds max_file_bytes".into(),
        ));
    }
    let mut tx = store.db.begin().await?;
    let account_total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0) FROM source_ledger WHERE owner_id = $1 AND state IN ('reserved', 'committed')",
    )
    .bind(owner_id)
    .fetch_one(&mut *tx)
    .await?;
    if account_total.saturating_add(bytes) > cfg.max_account_bytes {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "quota_exceeded: Account storage quota exceeded".into(),
        ));
    }
    let study_total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes), 0) FROM source_ledger WHERE study_id = $1 AND state IN ('reserved', 'committed')",
    )
    .bind(study_id)
    .fetch_one(&mut *tx)
    .await?;
    if study_total.saturating_add(bytes) > cfg.max_study_bytes {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "quota_exceeded: Study storage quota exceeded".into(),
        ));
    }
    let ledger_id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO source_ledger(id, owner_id, study_id, source_id, state, bytes, created_at, updated_at) VALUES($1, $2, $3, $4, 'reserved', $5, $6, $6)",
    )
    .bind(&ledger_id)
    .bind(owner_id)
    .bind(study_id)
    .bind(source_id)
    .bind(bytes)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ledger_id)
}

pub async fn commit_ledger(store: &Store, ledger_id: &str) -> Result<()> {
    sqlx::query("UPDATE source_ledger SET state = 'committed', updated_at = $1 WHERE id = $2")
        .bind(now())
        .bind(ledger_id)
        .execute(&store.db)
        .await?;
    Ok(())
}

pub async fn release_ledger(store: &Store, ledger_id: &str) -> Result<()> {
    sqlx::query("UPDATE source_ledger SET state = 'released', updated_at = $1 WHERE id = $2")
        .bind(now())
        .bind(ledger_id)
        .execute(&store.db)
        .await?;
    Ok(())
}
