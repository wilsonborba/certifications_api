use crate::{
    core::{Error, Result, now},
    dal::{Store, fsm::sha256_hex, new_id},
    domain::{
        auth::{CallerIdentity, require_owner},
        cleanup,
        quotas::{self, QuotaCache, QuotaMeter},
        security,
    },
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row;

pub const STUDY_DRAFT_TTL_SECONDS: i64 = 1800;
pub const MAX_PAGE_LIMIT: i64 = 50;

#[derive(Clone, Debug, Serialize)]
pub struct StudySummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub source_origin: String,
    pub status: String,
    pub quiz_id: String,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceSummary {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub fsm_ref: String,
    pub source_kind: String,
    pub status: String,
    pub selection_kind: String,
    pub selection_spec: String,
    pub is_restricted: bool,
    pub external_ref: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StudyDetail {
    pub study: StudySummary,
    pub sources: Vec<SourceSummary>,
    pub quota: QuotaMeter,
}

#[derive(Clone, Debug, Serialize)]
pub struct AnalysisSummary {
    pub study_id: String,
    pub total_sources: i64,
    pub ready_sources: i64,
    pub total_bytes: i64,
    pub updated_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateStudyInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateStudyInput {
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSourceRef {
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub fsm_ref: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default = "default_selection_kind")]
    pub selection_kind: String,
    #[serde(default)]
    pub selection_spec: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedStudyInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub sources: Vec<PreparedSourceRef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionInput {
    pub kind: String,
    #[serde(default)]
    pub spec: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSourceInput {
    pub kind: String,
    pub filename: String,
    #[serde(default = "default_text_mime")]
    pub mime_type: String,
    #[serde(default)]
    pub external_ref: String,
    #[serde(default)]
    pub fsm_ref: String,
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default)]
    pub is_restricted: bool,
}

fn default_selection_kind() -> String {
    "all".to_string()
}
fn default_text_mime() -> String {
    "text/plain".to_string()
}

fn validate_selection(kind: &str, spec: &str) -> Result<()> {
    if !matches!(kind, "all" | "pages" | "passages" | "intervals") {
        return Err(Error::unprocessable(
            "invalid_selection: Selection kind must be all, pages, passages, or intervals",
        ));
    }
    if kind != "all" && spec.trim().is_empty() {
        return Err(Error::unprocessable(
            "invalid_selection: Selection spec is required when kind is not all",
        ));
    }
    if spec.len() > 2000 {
        return Err(Error::unprocessable(
            "invalid_selection: Selection spec is too long",
        ));
    }
    Ok(())
}

async fn load_owned_study(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<StudySummary> {
    let row = sqlx::query(
        "SELECT id, owner_id, title, description, source_origin, status, quiz_id, expires_at, created_at, updated_at FROM studies WHERE id = $1",
    )
    .bind(study_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;

    let status: String = row.get("status");
    let expires_at: Option<i64> = row.get("expires_at");
    if status == "draft"
        && let Some(exp) = expires_at
        && now() > exp
    {
        sqlx::query("UPDATE studies SET status = 'expired' WHERE id = $1")
            .bind(study_id)
            .execute(&store.db)
            .await?;
        return Err(Error(
            StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if status == "expired" {
        return Err(Error(
            StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }

    Ok(StudySummary {
        id: row.get("id"),
        title: row.get("title"),
        description: row.get("description"),
        source_origin: row.get("source_origin"),
        status,
        quiz_id: row.get("quiz_id"),
        expires_at,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

async fn load_sources(store: &Store, study_id: &str) -> Result<Vec<SourceSummary>> {
    let rows = sqlx::query(
        "SELECT id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at FROM sources WHERE study_id = $1 AND status <> 'deleted' ORDER BY created_at ASC",
    )
    .bind(study_id)
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SourceSummary {
            id: r.get("id"),
            filename: r.get("filename"),
            mime_type: r.get("mime_type"),
            size_bytes: r.get("size_bytes"),
            sha256: r.get("sha256"),
            fsm_ref: r.get("fsm_ref"),
            source_kind: r.get("source_kind"),
            status: r.get("status"),
            selection_kind: r.get("selection_kind"),
            selection_spec: r.get("selection_spec"),
            is_restricted: r.get::<i64, _>("is_restricted") != 0,
            external_ref: r.get("external_ref"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })
        .collect())
}

pub async fn create_study(
    store: &Store,
    caller: &CallerIdentity,
    input: CreateStudyInput,
) -> Result<StudySummary> {
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(Error::unprocessable(
            "invalid_title: Title must be 1 to 200 characters",
        ));
    }
    let id = new_id();
    let ts = now();
    let exp = ts + STUDY_DRAFT_TTL_SECONDS;
    sqlx::query(
        "INSERT INTO studies(id, owner_id, title, description, source_origin, status, quiz_id, expires_at, created_at, updated_at) VALUES($1, $2, $3, $4, 'direct', 'draft', '', $5, $6, $6)",
    )
    .bind(&id)
    .bind(&caller.user_id)
    .bind(title)
    .bind(input.description.trim())
    .bind(exp)
    .bind(ts)
    .execute(&store.db)
    .await?;

    Ok(StudySummary {
        id,
        title: title.to_string(),
        description: input.description.trim().to_string(),
        source_origin: "direct".into(),
        status: "draft".into(),
        quiz_id: String::new(),
        expires_at: Some(exp),
        created_at: ts,
        updated_at: ts,
    })
}

pub async fn create_prepared_study(
    store: &Store,
    caller: &CallerIdentity,
    input: PreparedStudyInput,
    quotas: &QuotaCache,
) -> Result<StudyDetail> {
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(Error::unprocessable(
            "invalid_title: Title must be 1 to 200 characters",
        ));
    }
    if input.sources.is_empty() || input.sources.len() > 20 {
        return Err(Error::unprocessable(
            "invalid_sources: Prepared study requires 1 to 20 sources",
        ));
    }
    for s in &input.sources {
        if s.fsm_ref.trim().is_empty() || s.filename.trim().is_empty() || s.size_bytes < 0 {
            return Err(Error::unprocessable(
                "invalid_source: Prepared source requires filename, fsm_ref, and non-negative size",
            ));
        }
        validate_selection(&s.selection_kind, &s.selection_spec)?;
    }

    let study_id = new_id();
    let ts = now();
    let exp = ts + STUDY_DRAFT_TTL_SECONDS;
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "INSERT INTO studies(id, owner_id, title, description, source_origin, status, quiz_id, expires_at, created_at, updated_at) VALUES($1, $2, $3, $4, 'annotations', 'draft', '', $5, $6, $6)",
    )
    .bind(&study_id)
    .bind(&caller.user_id)
    .bind(title)
    .bind(input.description.trim())
    .bind(exp)
    .bind(ts)
    .execute(&mut *tx)
    .await?;

    for s in input.sources {
        let source_id = new_id();
        sqlx::query(
            "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at)
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, 'fsm_ref', 'ready', $9, $10, 0, '', $11, $11)",
        )
        .bind(&source_id)
        .bind(&caller.user_id)
        .bind(&study_id)
        .bind(s.filename.trim())
        .bind(s.mime_type.trim())
        .bind(s.size_bytes)
        .bind(s.sha256.trim())
        .bind(s.fsm_ref.trim())
        .bind(s.selection_kind)
        .bind(s.selection_spec.trim())
        .bind(ts)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    get_study(store, caller, &study_id, quotas).await
}

pub async fn list_studies(
    store: &Store,
    caller: &CallerIdentity,
    limit: i64,
) -> Result<Vec<StudySummary>> {
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let ts = now();
    let rows = sqlx::query(
        "SELECT id, title, description, source_origin, status, quiz_id, expires_at, created_at, updated_at
         FROM studies WHERE owner_id = $1 AND status <> 'expired' AND (status <> 'draft' OR expires_at IS NULL OR expires_at >= $2)
         ORDER BY updated_at DESC LIMIT $3",
    )
    .bind(&caller.user_id)
    .bind(ts)
    .bind(limit)
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StudySummary {
            id: r.get("id"),
            title: r.get("title"),
            description: r.get("description"),
            source_origin: r.get("source_origin"),
            status: r.get("status"),
            quiz_id: r.get("quiz_id"),
            expires_at: r.get("expires_at"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })
        .collect())
}

pub async fn get_study(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    quotas: &QuotaCache,
) -> Result<StudyDetail> {
    let mut study = load_owned_study(store, caller, study_id).await?;
    if study.status == "draft" {
        let new_exp = now() + STUDY_DRAFT_TTL_SECONDS;
        sqlx::query("UPDATE studies SET expires_at = $1 WHERE id = $2")
            .bind(new_exp)
            .bind(study_id)
            .execute(&store.db)
            .await?;
        study.expires_at = Some(new_exp);
    }
    let sources = load_sources(store, study_id).await?;
    let quota = quotas::meter_for_owner(store, &caller.user_id, &quotas.get()).await?;
    Ok(StudyDetail {
        study,
        sources,
        quota,
    })
}

pub async fn update_study(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    input: UpdateStudyInput,
) -> Result<StudySummary> {
    let mut study = load_owned_study(store, caller, study_id).await?;
    if let Some(t) = input.title {
        let trimmed = t.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 200 {
            return Err(Error::unprocessable(
                "invalid_title: Title must be 1 to 200 characters",
            ));
        }
        study.title = trimmed.to_string();
    }
    if let Some(d) = input.description {
        study.description = d.trim().to_string();
    }
    study.updated_at = now();
    sqlx::query("UPDATE studies SET title = $1, description = $2, updated_at = $3 WHERE id = $4")
        .bind(&study.title)
        .bind(&study.description)
        .bind(study.updated_at)
        .bind(study_id)
        .execute(&store.db)
        .await?;
    Ok(study)
}

pub async fn delete_study(store: &Store, caller: &CallerIdentity, study_id: &str) -> Result<()> {
    let _ = load_owned_study(store, caller, study_id).await?;
    let sources = load_sources(store, study_id).await?;
    let ts = now();
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "UPDATE generation_jobs SET status = 'cancelled', error_code = 'study_deleted', finished_at = $1 WHERE study_id = $2 AND status IN ('queued', 'running')",
    )
    .bind(ts)
    .bind(study_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE source_ledger SET state = 'released', updated_at = $1 WHERE study_id = $2 AND state IN ('reserved', 'committed')",
    )
    .bind(ts)
    .bind(study_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM generation_job_sources WHERE source_id IN (SELECT id FROM sources WHERE study_id = $1)",
    )
    .bind(study_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM question_citations WHERE source_id IN (SELECT id FROM sources WHERE study_id = $1)",
    )
    .bind(study_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM studies WHERE id = $1")
        .bind(study_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    for src in sources {
        if src.source_kind == "upload" && !src.fsm_ref.is_empty() {
            cleanup::delete_fsm_or_queue(store, &src.fsm_ref).await?;
        }
    }
    Ok(())
}

pub async fn add_source_upload(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
    quotas: &QuotaCache,
) -> Result<SourceSummary> {
    let _ = load_owned_study(store, caller, study_id).await?;
    if filename.trim().is_empty() {
        return Err(Error::unprocessable("invalid_source: Filename is required"));
    }
    let source_id = new_id();
    let size_bytes = bytes.len() as i64;
    let cfg = quotas.get();
    let ts = now();
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at)
         VALUES($1, $2, $3, $4, $5, $6, '', '', 'upload', 'uploading', 'all', '', 0, '', $7, $7)",
    )
    .bind(&source_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(filename.trim())
    .bind(mime_type.trim())
    .bind(size_bytes)
    .bind(ts)
    .execute(&store.db)
    .await?;

    let ledger_id = match quotas::reserve_bytes(
        store,
        &caller.user_id,
        study_id,
        &source_id,
        size_bytes,
        &cfg,
    )
    .await
    {
        Ok(id) => id,
        Err(err) => {
            let _ = sqlx::query(
                "UPDATE sources SET status = 'failed', error_code = 'quota_exceeded', updated_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(&source_id)
            .execute(&store.db)
            .await;
            return Err(err);
        }
    };

    let fail_cleanup = |code: &str| {
        let code = code.to_string();
        let store = store.clone();
        let source_id = source_id.clone();
        let ledger_id = ledger_id.clone();
        async move {
            let _ = quotas::release_ledger(&store, &ledger_id).await;
            let _ = sqlx::query(
                "UPDATE sources SET status = 'failed', error_code = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(code)
            .bind(now())
            .bind(source_id)
            .execute(&store.db)
            .await;
        }
    };

    if let Err(err) = security::scan_and_validate(store.scanner.as_ref(), mime_type, bytes).await {
        fail_cleanup("security_rejected").await;
        return Err(err);
    }

    let hash = sha256_hex(bytes);
    let fsm_key = format!("certifications/{}/{}", caller.user_id, source_id);
    if let Err(err) = store.fsm.put_bytes(&fsm_key, bytes).await {
        fail_cleanup("fsm_upload_failed").await;
        return Err(err);
    }

    quotas::commit_ledger(store, &ledger_id).await?;
    let done_ts = now();
    sqlx::query(
        "UPDATE sources SET sha256 = $1, fsm_ref = $2, status = 'ready', updated_at = $3 WHERE id = $4",
    )
    .bind(&hash)
    .bind(&fsm_key)
    .bind(done_ts)
    .bind(&source_id)
    .execute(&store.db)
    .await?;

    Ok(SourceSummary {
        id: source_id,
        filename: filename.trim().to_string(),
        mime_type: mime_type.trim().to_string(),
        size_bytes,
        sha256: hash,
        fsm_ref: fsm_key,
        source_kind: "upload".into(),
        status: "ready".into(),
        selection_kind: "all".into(),
        selection_spec: String::new(),
        is_restricted: false,
        external_ref: String::new(),
        created_at: ts,
        updated_at: done_ts,
    })
}

pub async fn add_source_reference(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    input: ReferenceSourceInput,
) -> Result<SourceSummary> {
    let _ = load_owned_study(store, caller, study_id).await?;
    if !matches!(input.kind.as_str(), "fsm_ref" | "link" | "report") {
        return Err(Error::unprocessable(
            "invalid_source_kind: Kind must be fsm_ref, link, or report",
        ));
    }
    let mut is_restricted = input.is_restricted;
    if input.kind == "report" {
        let (project, report) = input
            .external_ref
            .trim()
            .split_once('/')
            .filter(|(p, r)| !p.is_empty() && !r.is_empty())
            .ok_or_else(|| {
                Error::unprocessable(
                    "invalid_report_ref: Report external_ref must be <project>/<report>",
                )
            })?;
        let checked = store
            .reports
            .check_report_access(&caller.user_id, project, report)
            .await?;
        is_restricted = checked.is_restricted;
    }
    let source_id = new_id();
    let ts = now();
    let restricted = i32::from(is_restricted);
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at)
         VALUES($1, $2, $3, $4, $5, $6, '', $7, $8, 'ready', 'all', '', $9, $10, $11, $11)",
    )
    .bind(&source_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(input.filename.trim())
    .bind(input.mime_type.trim())
    .bind(input.size_bytes.max(0))
    .bind(input.fsm_ref.trim())
    .bind(&input.kind)
    .bind(restricted)
    .bind(input.external_ref.trim())
    .bind(ts)
    .execute(&store.db)
    .await?;

    Ok(SourceSummary {
        id: source_id,
        filename: input.filename.trim().to_string(),
        mime_type: input.mime_type.trim().to_string(),
        size_bytes: input.size_bytes.max(0),
        sha256: String::new(),
        fsm_ref: input.fsm_ref.trim().to_string(),
        source_kind: input.kind,
        status: "ready".into(),
        selection_kind: "all".into(),
        selection_spec: String::new(),
        is_restricted,
        external_ref: input.external_ref.trim().to_string(),
        created_at: ts,
        updated_at: ts,
    })
}

pub async fn get_source(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
) -> Result<SourceSummary> {
    let _ = load_owned_study(store, caller, study_id).await?;
    let sources = load_sources(store, study_id).await?;
    sources
        .into_iter()
        .find(|s| s.id == source_id)
        .ok_or_else(Error::missing)
}

pub async fn preview_source(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
) -> Result<serde_json::Value> {
    let src = get_source(store, caller, study_id, source_id).await?;
    if src.status != "ready" {
        return Err(Error::unprocessable(
            "source_not_ready: Source is not ready for preview",
        ));
    }
    let preview_text = if !src.fsm_ref.is_empty() && src.mime_type.starts_with("text/") {
        let bytes = store.fsm.get_bytes(&src.fsm_ref).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        text.chars().take(1000).collect::<String>()
    } else {
        String::new()
    };
    Ok(serde_json::json!({
        "source_id": src.id,
        "filename": src.filename,
        "mime_type": src.mime_type,
        "size_bytes": src.size_bytes,
        "selection_kind": src.selection_kind,
        "selection_spec": src.selection_spec,
        "preview": preview_text,
    }))
}

pub async fn update_selection(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
    input: SelectionInput,
) -> Result<SourceSummary> {
    validate_selection(&input.kind, &input.spec)?;
    let _ = get_source(store, caller, study_id, source_id).await?;
    let ts = now();
    sqlx::query(
        "UPDATE sources SET selection_kind = $1, selection_spec = $2, updated_at = $3 WHERE id = $4 AND study_id = $5",
    )
    .bind(&input.kind)
    .bind(input.spec.trim())
    .bind(ts)
    .bind(source_id)
    .bind(study_id)
    .execute(&store.db)
    .await?;
    get_source(store, caller, study_id, source_id).await
}

pub async fn delete_source(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
) -> Result<()> {
    let src = get_source(store, caller, study_id, source_id).await?;
    let ts = now();
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "UPDATE generation_jobs SET status = 'cancelled', error_code = 'source_deleted', finished_at = $1
         WHERE id IN (SELECT job_id FROM generation_job_sources WHERE source_id = $2) AND status IN ('queued', 'running')",
    )
    .bind(ts)
    .bind(source_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM generation_job_sources WHERE source_id = $1")
        .bind(source_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE source_ledger SET state = 'released', updated_at = $1 WHERE source_id = $2 AND state IN ('reserved', 'committed')",
    )
    .bind(ts)
    .bind(source_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM sources WHERE id = $1")
        .bind(source_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    if src.source_kind == "upload" && !src.fsm_ref.is_empty() {
        cleanup::delete_fsm_or_queue(store, &src.fsm_ref).await?;
    }
    Ok(())
}

pub async fn analyze_study(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<AnalysisSummary> {
    let _ = load_owned_study(store, caller, study_id).await?;
    let sources = load_sources(store, study_id).await?;
    let total_sources = sources.len() as i64;
    let ready_sources = sources.iter().filter(|s| s.status == "ready").count() as i64;
    let total_bytes: i64 = sources.iter().map(|s| s.size_bytes).sum();
    let ts = now();
    sqlx::query(
        "INSERT INTO study_analysis(study_id, total_sources, total_bytes, ready_sources, updated_at)
         VALUES($1, $2, $3, $4, $5)
         ON CONFLICT(study_id) DO UPDATE SET total_sources = excluded.total_sources, total_bytes = excluded.total_bytes, ready_sources = excluded.ready_sources, updated_at = excluded.updated_at",
    )
    .bind(study_id)
    .bind(total_sources)
    .bind(total_bytes)
    .bind(ready_sources)
    .bind(ts)
    .execute(&store.db)
    .await?;
    Ok(AnalysisSummary {
        study_id: study_id.to_string(),
        total_sources,
        ready_sources,
        total_bytes,
        updated_at: ts,
    })
}

pub async fn get_analysis(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<AnalysisSummary> {
    let _ = load_owned_study(store, caller, study_id).await?;
    if let Some(row) = sqlx::query(
        "SELECT study_id, total_sources, total_bytes, ready_sources, updated_at FROM study_analysis WHERE study_id = $1",
    )
    .bind(study_id)
    .fetch_optional(&store.db)
    .await?
    {
        return Ok(AnalysisSummary {
            study_id: row.get("study_id"),
            total_sources: row.get("total_sources"),
            total_bytes: row.get("total_bytes"),
            ready_sources: row.get("ready_sources"),
            updated_at: row.get("updated_at"),
        });
    }
    analyze_study(store, caller, study_id).await
}
