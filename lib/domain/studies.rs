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

const MAX_SELECTION_SPEC_BYTES: usize = 2000;
const MAX_SELECTION_RANGES: usize = 100;

fn parse_ascii_u64(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

pub fn canonicalize_selection(
    kind: &str,
    spec: &str,
    mime_type: Option<&str>,
    max_pages: Option<u32>,
    max_duration_ms: Option<u64>,
) -> Result<String> {
    if !matches!(kind, "all" | "pages" | "passages" | "intervals") {
        return Err(Error::unprocessable(
            "invalid_selection: Selection kind must be all, pages, passages, or intervals",
        ));
    }
    let trimmed = spec.trim();
    if trimmed.len() > MAX_SELECTION_SPEC_BYTES {
        return Err(Error::unprocessable(
            "invalid_selection: Selection spec exceeds 2000 bytes",
        ));
    }
    if kind == "all" {
        if !trimmed.is_empty() {
            return Err(Error::unprocessable(
                "invalid_selection: Selection spec must be empty when kind is all",
            ));
        }
        return Ok(String::new());
    }
    if trimmed.is_empty() {
        return Err(Error::unprocessable(
            "invalid_selection: Selection spec is required when kind is not all",
        ));
    }

    match kind {
        "pages" => {
            if let Some(mt) = mime_type
                && !mt.trim().eq_ignore_ascii_case("application/pdf")
            {
                return Err(Error::unprocessable(
                    "invalid_selection: pages selection requires application/pdf source",
                ));
            }
            let parts: Vec<&str> = trimmed.split(',').collect();
            if parts.len() > MAX_SELECTION_RANGES {
                return Err(Error::unprocessable(
                    "invalid_selection: Too many page ranges",
                ));
            }
            let mut ranges: Vec<(u32, u32)> = Vec::with_capacity(parts.len());
            for raw_part in parts {
                let part = raw_part.trim();
                if part.is_empty() {
                    return Err(Error::unprocessable(
                        "invalid_selection: Empty page range segment",
                    ));
                }
                let (s, e) = if let Some((a, b)) = part.split_once('-') {
                    let sv = parse_ascii_u64(a)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| {
                            Error::unprocessable("invalid_selection: Invalid page start")
                        })?;
                    let ev = parse_ascii_u64(b)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| {
                            Error::unprocessable("invalid_selection: Invalid page end")
                        })?;
                    (sv, ev)
                } else {
                    let v = parse_ascii_u64(part)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| {
                            Error::unprocessable("invalid_selection: Invalid page number")
                        })?;
                    (v, v)
                };
                if s == 0 || e < s || e > 100_000 {
                    return Err(Error::unprocessable(
                        "invalid_selection: Page range must be 1-based with start <= end",
                    ));
                }
                if let Some(limit) = max_pages
                    && e > limit
                {
                    return Err(Error::unprocessable(
                        "invalid_selection: Page range exceeds PDF page_count",
                    ));
                }
                if let Some(&(_, prev_end)) = ranges.last()
                    && s <= prev_end
                {
                    return Err(Error::unprocessable(
                        "invalid_selection: Page ranges must be strictly increasing and disjoint",
                    ));
                }
                ranges.push((s, e));
            }
            Ok(ranges
                .into_iter()
                .map(|(s, e)| {
                    if s == e {
                        s.to_string()
                    } else {
                        format!("{s}-{e}")
                    }
                })
                .collect::<Vec<_>>()
                .join(","))
        }
        "passages" => {
            if let Some(mt) = mime_type {
                let m = mt.trim().to_ascii_lowercase();
                if !m.starts_with("text/")
                    && m != "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                {
                    return Err(Error::unprocessable(
                        "invalid_selection: passages selection requires text or DOCX source",
                    ));
                }
            }
            let parts: Vec<&str> = trimmed.split(',').collect();
            if parts.len() > MAX_SELECTION_RANGES {
                return Err(Error::unprocessable(
                    "invalid_selection: Too many passage ranges",
                ));
            }
            let mut ranges: Vec<(u64, u64)> = Vec::with_capacity(parts.len());
            for raw_part in parts {
                let part = raw_part.trim();
                let rest = part.strip_prefix("p1:").ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Passage range must use p1:start-end")
                })?;
                let (a, b) = rest.split_once('-').ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Passage range must use p1:start-end")
                })?;
                let s = parse_ascii_u64(a).ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Invalid passage start offset")
                })?;
                let e = parse_ascii_u64(b).ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Invalid passage end offset")
                })?;
                if e <= s {
                    return Err(Error::unprocessable(
                        "invalid_selection: Passage range must be half-open with end > start",
                    ));
                }
                if let Some(&(_, prev_end)) = ranges.last()
                    && s < prev_end
                {
                    return Err(Error::unprocessable(
                        "invalid_selection: Passage ranges must be sorted and non-overlapping",
                    ));
                }
                ranges.push((s, e));
            }
            Ok(ranges
                .into_iter()
                .map(|(s, e)| format!("p1:{s}-{e}"))
                .collect::<Vec<_>>()
                .join(","))
        }
        "intervals" => {
            if let Some(mt) = mime_type
                && !mt.trim().to_ascii_lowercase().starts_with("audio/")
            {
                return Err(Error::unprocessable(
                    "invalid_selection: intervals selection requires an audio source",
                ));
            }
            let body = trimmed.strip_prefix("ms:").ok_or_else(|| {
                Error::unprocessable("invalid_selection: Intervals spec must start with ms:")
            })?;
            if body.is_empty() {
                return Err(Error::unprocessable(
                    "invalid_selection: Intervals spec requires at least one start-end range",
                ));
            }
            let parts: Vec<&str> = body.split(',').collect();
            if parts.len() > MAX_SELECTION_RANGES {
                return Err(Error::unprocessable(
                    "invalid_selection: Too many audio intervals",
                ));
            }
            let mut ranges: Vec<(u64, u64)> = Vec::with_capacity(parts.len());
            for raw_part in parts {
                let part = raw_part.trim();
                let (a, b) = part.split_once('-').ok_or_else(|| {
                    Error::unprocessable(
                        "invalid_selection: Each interval must be integer milliseconds start-end",
                    )
                })?;
                let s = parse_ascii_u64(a).ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Invalid interval start milliseconds")
                })?;
                let e = parse_ascii_u64(b).ok_or_else(|| {
                    Error::unprocessable("invalid_selection: Invalid interval end milliseconds")
                })?;
                if e <= s {
                    return Err(Error::unprocessable(
                        "invalid_selection: Interval must be half-open with end > start",
                    ));
                }
                if let Some(dur) = max_duration_ms
                    && e > dur
                {
                    return Err(Error::unprocessable(
                        "invalid_selection: Interval exceeds known audio duration",
                    ));
                }
                if let Some(last) = ranges.last_mut() {
                    if s < last.1 {
                        return Err(Error::unprocessable(
                            "invalid_selection: Intervals must be sorted and disjoint",
                        ));
                    }
                    if s == last.1 {
                        last.1 = e;
                        continue;
                    }
                }
                ranges.push((s, e));
            }
            let joined = ranges
                .into_iter()
                .map(|(s, e)| format!("{s}-{e}"))
                .collect::<Vec<_>>()
                .join(",");
            Ok(format!("ms:{joined}"))
        }
        _ => unreachable!(),
    }
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

async fn authorize_fsm_ref(
    store: &Store,
    caller: &CallerIdentity,
    fsm_ref: &str,
    trusted_service: bool,
) -> Result<Option<i64>> {
    let trimmed = fsm_ref.trim();
    if trimmed.is_empty() {
        return Err(Error::unprocessable("invalid_source: fsm_ref is required"));
    }
    let existing = sqlx::query(
        "SELECT owner_id, size_bytes, status FROM sources WHERE fsm_ref = $1 AND status <> 'deleted' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(trimmed)
    .fetch_optional(&store.db)
    .await?;

    if let Some(row) = existing {
        let owner_id: String = row.get("owner_id");
        let size_bytes: i64 = row.get("size_bytes");
        let status: String = row.get("status");
        if owner_id != caller.user_id || status != "ready" {
            return Err(Error::forbidden(
                "fsm_ref_forbidden: Caller is not authorized to reference this file",
            ));
        }
        return Ok(Some(size_bytes));
    }

    if trusted_service {
        return Ok(None);
    }
    Err(Error::forbidden(
        "fsm_ref_forbidden: Reference is not an authorized owned file",
    ))
}

async fn ensure_source_blob_readable(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    src: &SourceSummary,
) -> Result<()> {
    let trimmed = src.fsm_ref.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.contains("..")
        || trimmed.contains("://")
    {
        return Err(Error::forbidden(
            "fsm_ref_forbidden: Source blob reference is not an authorized path",
        ));
    }
    let conflict = sqlx::query(
        "SELECT owner_id FROM sources WHERE fsm_ref = $1 AND status <> 'deleted' AND owner_id <> $2 LIMIT 1",
    )
    .bind(trimmed)
    .bind(&caller.user_id)
    .fetch_optional(&store.db)
    .await?;
    if conflict.is_some() {
        return Err(Error::forbidden(
            "fsm_ref_forbidden: Caller does not own referenced blob",
        ));
    }
    let row_provenance: Option<String> =
        sqlx::query("SELECT source_provenance FROM sources WHERE id = $1 AND owner_id = $2")
            .bind(&src.id)
            .bind(&caller.user_id)
            .fetch_optional(&store.db)
            .await?
            .map(|r| r.get("source_provenance"));
    if !matches!(
        row_provenance.as_deref(),
        Some("upload" | "prepared_service" | "owned_ref")
    ) {
        return Err(Error::forbidden(
            "fsm_ref_forbidden: Source row lacks verified server-only authorization provenance",
        ));
    }
    let proven = match src.source_kind.as_str() {
        "upload" => {
            let expected_prefix = format!("certifications/{}/", caller.user_id);
            trimmed.starts_with(&expected_prefix) && !src.sha256.trim().is_empty()
        }
        "fsm_ref" => {
            let owned_upload = sqlx::query(
                "SELECT 1 FROM sources WHERE fsm_ref = $1 AND owner_id = $2 AND source_kind = 'upload' AND status = 'ready' AND sha256 <> '' LIMIT 1",
            )
            .bind(trimmed)
            .bind(&caller.user_id)
            .fetch_optional(&store.db)
            .await?
            .is_some();
            if owned_upload {
                true
            } else {
                let prepared_origin: Option<String> = sqlx::query(
                    "SELECT source_origin FROM studies WHERE id = $1 AND owner_id = $2",
                )
                .bind(study_id)
                .bind(&caller.user_id)
                .fetch_optional(&store.db)
                .await?
                .map(|r| r.get("source_origin"));
                prepared_origin.as_deref() == Some("annotations")
            }
        }
        _ => false,
    };
    if !proven {
        return Err(Error::forbidden(
            "fsm_ref_forbidden: Historical or unverified blob reference lacks authoritative ownership provenance",
        ));
    }
    Ok(())
}

pub async fn create_prepared_study(
    store: &Store,
    caller: &CallerIdentity,
    input: PreparedStudyInput,
    quotas: &QuotaCache,
    trusted_service: bool,
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
    let mut verified_sizes = Vec::with_capacity(input.sources.len());
    let mut canonical_specs = Vec::with_capacity(input.sources.len());
    for s in &input.sources {
        if s.fsm_ref.trim().is_empty() || s.filename.trim().is_empty() || s.size_bytes < 0 {
            return Err(Error::unprocessable(
                "invalid_source: Prepared source requires filename, fsm_ref, and non-negative size",
            ));
        }
        let canon = canonicalize_selection(
            &s.selection_kind,
            &s.selection_spec,
            Some(&s.mime_type),
            None,
            None,
        )?;
        let verified_size = authorize_fsm_ref(store, caller, &s.fsm_ref, trusted_service).await?;
        verified_sizes.push(verified_size.unwrap_or(s.size_bytes));
        canonical_specs.push(canon);
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

    let provenance = if trusted_service {
        "prepared_service"
    } else {
        "owned_ref"
    };
    for ((s, effective_size), canon_spec) in input
        .sources
        .into_iter()
        .zip(verified_sizes)
        .zip(canonical_specs)
    {
        let source_id = new_id();
        sqlx::query(
            "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, source_provenance, created_at, updated_at)
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, 'fsm_ref', 'ready', $9, $10, 0, '', $11, $12, $12)",
        )
        .bind(&source_id)
        .bind(&caller.user_id)
        .bind(&study_id)
        .bind(s.filename.trim())
        .bind(s.mime_type.trim())
        .bind(effective_size)
        .bind(s.sha256.trim())
        .bind(s.fsm_ref.trim())
        .bind(s.selection_kind)
        .bind(canon_spec)
        .bind(provenance)
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
        "UPDATE sources SET sha256 = $1, fsm_ref = $2, status = 'ready', source_provenance = 'upload', updated_at = $3 WHERE id = $4",
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
    let mut effective_size = input.size_bytes.max(0);
    if input.kind == "fsm_ref" {
        if let Some(verified_size) = authorize_fsm_ref(store, caller, &input.fsm_ref, false).await?
        {
            effective_size = verified_size;
        }
    } else if input.kind == "report" {
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
    let provenance = if input.kind == "fsm_ref" {
        "owned_ref"
    } else {
        "unknown"
    };
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, source_provenance, created_at, updated_at)
         VALUES($1, $2, $3, $4, $5, $6, '', $7, $8, 'ready', 'all', '', $9, $10, $11, $12, $12)",
    )
    .bind(&source_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(input.filename.trim())
    .bind(input.mime_type.trim())
    .bind(effective_size)
    .bind(input.fsm_ref.trim())
    .bind(&input.kind)
    .bind(restricted)
    .bind(input.external_ref.trim())
    .bind(provenance)
    .bind(ts)
    .execute(&store.db)
    .await?;

    Ok(SourceSummary {
        id: source_id,
        filename: input.filename.trim().to_string(),
        mime_type: input.mime_type.trim().to_string(),
        size_bytes: effective_size,
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

fn strip_pdf_comments_and_streams(bytes: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(bytes.len().min(512 * 1024));
    let mut i = 0usize;
    let mut paren_depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if paren_depth > 0 {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b' ');
                i += 2;
                continue;
            }
            if b == b'(' {
                paren_depth += 1;
            } else if b == b')' {
                paren_depth -= 1;
            }
            out.push(b' ');
            i += 1;
            continue;
        }
        if b == b'(' {
            paren_depth = 1;
            out.push(b' ');
            i += 1;
            continue;
        }
        if b == b'%' {
            while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            out.push(b' ');
            continue;
        }
        if bytes[i..].starts_with(b"stream")
            && (i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b'>')
            && bytes.get(i + 6).is_some_and(|c| *c == b'\r' || *c == b'\n')
        {
            if let Some(rel) = bytes[i + 6..].windows(9).position(|w| w == b"endstream") {
                out.extend_from_slice(b" ");
                i = i + 6 + rel + 9;
                continue;
            }
            return None;
        }
        out.push(b);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn parse_pdf_ref(dict: &str, key: &str) -> Option<(u32, u16)> {
    let mut pos = 0usize;
    while let Some(idx) = dict[pos..].find(key) {
        let after = &dict[pos + idx + key.len()..];
        let next = after.chars().next();
        if next.is_none_or(|c| c.is_ascii_whitespace() || c == '<' || c == '/') {
            let mut parts = after
                .split(|c: char| {
                    c.is_ascii_whitespace() || matches!(c, '<' | '>' | '[' | ']' | '/')
                })
                .filter(|s| !s.is_empty());
            if let (Some(o), Some(g), Some("R")) = (parts.next(), parts.next(), parts.next())
                && let (Ok(obj), Ok(gen_num)) = (o.parse::<u32>(), g.parse::<u16>())
            {
                return Some((obj, gen_num));
            }
        }
        pos += idx + key.len();
    }
    None
}

fn parse_pdf_int(dict: &str, key: &str) -> Option<u32> {
    let mut pos = 0usize;
    while let Some(idx) = dict[pos..].find(key) {
        let after = &dict[pos + idx + key.len()..];
        let next = after.chars().next();
        if next.is_none_or(|c| c.is_ascii_whitespace()) {
            let tok = after
                .split(|c: char| {
                    c.is_ascii_whitespace() || matches!(c, '<' | '>' | '[' | ']' | '/')
                })
                .find(|s| !s.is_empty())?;
            if let Ok(val) = tok.parse::<u32>() {
                return Some(val);
            }
        }
        pos += idx + key.len();
    }
    None
}

fn find_pdf_object_body(cleaned: &str, obj: u32, gen_num: u16) -> Option<&str> {
    let needle = format!("{obj} {gen_num} obj");
    let mut pos = 0usize;
    while let Some(idx) = cleaned[pos..].find(&needle) {
        let start = pos + idx;
        let boundary_ok = start == 0 || cleaned.as_bytes()[start - 1].is_ascii_whitespace();
        let after_idx = start + needle.len();
        let after_ok = cleaned
            .as_bytes()
            .get(after_idx)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'<' || *b == b'[');
        if boundary_ok && after_ok {
            let rest = &cleaned[after_idx..];
            let end = rest.find("endobj")?;
            return Some(&rest[..end]);
        }
        pos = after_idx;
    }
    None
}

pub fn extract_pdf_page_count(bytes: &[u8]) -> Option<u32> {
    if !bytes.starts_with(b"%PDF-") || bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    let cleaned = strip_pdf_comments_and_streams(bytes)?;
    if cleaned.contains("/ObjStm") || cleaned.contains("/Encrypt") {
        return None;
    }
    let trailer_idx = cleaned.rfind("trailer")?;
    let trailer = &cleaned[trailer_idx..];
    let (root_obj, root_gen) = parse_pdf_ref(trailer, "/Root")?;
    let catalog = find_pdf_object_body(&cleaned, root_obj, root_gen)?;
    let (pages_obj, pages_gen) = parse_pdf_ref(catalog, "/Pages")?;
    let pages_dict = find_pdf_object_body(&cleaned, pages_obj, pages_gen)?;
    let count = parse_pdf_int(pages_dict, "/Count")?;
    (1..=100_000).contains(&count).then_some(count)
}

pub fn cheap_wav_duration_seconds(bytes: &[u8]) -> Option<f64> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12usize;
    let mut byte_rate: Option<u32> = None;
    let mut data_size: Option<u32> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]);
        let payload_start = offset + 8;
        let payload_end = payload_start.saturating_add(size as usize);
        if payload_end > bytes.len() {
            break;
        }
        if id == b"fmt " && size >= 16 {
            byte_rate = Some(u32::from_le_bytes([
                bytes[payload_start + 8],
                bytes[payload_start + 9],
                bytes[payload_start + 10],
                bytes[payload_start + 11],
            ]));
        } else if id == b"data" {
            data_size = Some(size);
        }
        if byte_rate.is_some() && data_size.is_some() {
            break;
        }
        let padded = (size as usize).saturating_add((size as usize) & 1);
        offset = payload_start.saturating_add(padded);
    }
    match (byte_rate, data_size) {
        (Some(br), Some(ds)) if br > 0 => Some((ds as f64) / (br as f64)),
        _ => None,
    }
}

pub fn parse_byte_range(header: &str, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start_str, end_str) = spec.split_once('-')?;
    if start_str.is_empty() {
        let suffix: usize = end_str.trim().parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = total.saturating_sub(suffix);
        return Some((start, total - 1));
    }
    let start: usize = start_str.trim().parse().ok()?;
    if start >= total {
        return None;
    }
    let end = if end_str.trim().is_empty() {
        total - 1
    } else {
        let parsed: usize = end_str.trim().parse().ok()?;
        if parsed < start {
            return None;
        }
        parsed.min(total - 1)
    };
    Some((start, end))
}

pub async fn get_source_content(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
    max_bytes: usize,
) -> Result<(SourceSummary, Vec<u8>)> {
    let src = get_source(store, caller, study_id, source_id).await?;
    if src.status != "ready" {
        return Err(Error::unprocessable(
            "source_not_ready: Source is not ready for content streaming",
        ));
    }
    if src.fsm_ref.trim().is_empty() {
        return Err(Error::missing());
    }
    ensure_source_blob_readable(store, caller, study_id, &src).await?;
    let bytes = store.fsm.get_bytes_bounded(&src.fsm_ref, max_bytes).await?;
    Ok((src, bytes))
}

pub async fn preview_source(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
    max_bytes: usize,
) -> Result<serde_json::Value> {
    let src = get_source(store, caller, study_id, source_id).await?;
    if src.status != "ready" {
        return Err(Error::unprocessable(
            "source_not_ready: Source is not ready for preview",
        ));
    }
    let has_fsm = !src.fsm_ref.trim().is_empty();
    let mut preview_text = String::new();
    let mut page_count: Option<u32> = None;
    let mut duration_seconds: Option<f64> = None;

    if has_fsm {
        ensure_source_blob_readable(store, caller, study_id, &src).await?;
        let needs_bytes = src.mime_type.starts_with("text/")
            || src.mime_type == "application/pdf"
            || matches!(
                src.mime_type.as_str(),
                "audio/wav" | "audio/x-wav" | "audio/wave"
            );
        if needs_bytes && let Ok(bytes) = store.fsm.get_bytes_bounded(&src.fsm_ref, max_bytes).await
        {
            if src.mime_type.starts_with("text/") {
                let text = String::from_utf8_lossy(&bytes);
                preview_text = text.chars().take(1000).collect::<String>();
            } else if src.mime_type == "application/pdf" {
                page_count = extract_pdf_page_count(&bytes);
            } else {
                duration_seconds = cheap_wav_duration_seconds(&bytes);
            }
        }
    }

    let content_url = has_fsm.then(|| format!("/studies/{study_id}/sources/{source_id}/content"));
    Ok(serde_json::json!({
        "source_id": src.id,
        "filename": src.filename,
        "mime_type": src.mime_type,
        "size_bytes": src.size_bytes,
        "selection_kind": src.selection_kind,
        "selection_spec": src.selection_spec,
        "preview": preview_text,
        "content_url": content_url,
        "supports_range": has_fsm,
        "page_count": page_count,
        "duration_seconds": duration_seconds,
    }))
}

pub async fn update_selection(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source_id: &str,
    input: SelectionInput,
) -> Result<SourceSummary> {
    let src = get_source(store, caller, study_id, source_id).await?;
    let mut max_pages = None;
    let mut max_duration_ms = None;
    if !src.fsm_ref.trim().is_empty()
        && src.status == "ready"
        && matches!(input.kind.as_str(), "pages" | "intervals")
        && ensure_source_blob_readable(store, caller, study_id, &src)
            .await
            .is_ok()
        && let Ok(bytes) = store
            .fsm
            .get_bytes_bounded(&src.fsm_ref, 16 * 1024 * 1024)
            .await
    {
        if input.kind == "pages" && src.mime_type.eq_ignore_ascii_case("application/pdf") {
            max_pages = extract_pdf_page_count(&bytes);
        } else if input.kind == "intervals" {
            max_duration_ms =
                cheap_wav_duration_seconds(&bytes).map(|secs| (secs * 1000.0).round() as u64);
        }
    }
    let canon = canonicalize_selection(
        &input.kind,
        &input.spec,
        Some(&src.mime_type),
        max_pages,
        max_duration_ms,
    )?;
    let ts = now();
    sqlx::query(
        "UPDATE sources SET selection_kind = $1, selection_spec = $2, updated_at = $3 WHERE id = $4 AND study_id = $5",
    )
    .bind(&input.kind)
    .bind(&canon)
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
