use crate::{
    core::{Error, Result, now},
    dal::{AnyQuery, Store, fsm::sha256_hex, new_id},
    domain::{
        auth::{CallerIdentity, require_owner},
        cleanup,
        quotas::{self, QuotaCache, QuotaMeter},
        security,
    },
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::{Row, any::AnyRow};

pub const STUDY_DRAFT_TTL_SECONDS: i64 = 1800;
pub const MAX_PAGE_LIMIT: i64 = 50;
pub const MAX_SELECTION_RANGES: usize = 100;

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

#[derive(Clone, Debug, Default)]
pub struct StudyHistoryFilter {
    pub search: Option<String>,
    pub created_from: Option<i64>,
    pub created_to: Option<i64>,
    pub status: Option<String>,
    pub quiz_id: Option<String>,
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AvailableStudyTag {
    pub slug: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StudyHistoryPage {
    pub items: Vec<StudySummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub available_tags: Vec<AvailableStudyTag>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<PreparedStudyConfig>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AnalysisSummary {
    pub study_id: String,
    pub total_sources: i64,
    pub ready_sources: i64,
    pub total_bytes: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct AnalysisStreamEvent {
    pub event: &'static str,
    pub data: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct AnalysisInspection {
    pub study_id: String,
    pub total_sources: usize,
    pub total_ready_sources: usize,
    pub total_bytes: i64,
    pub sources: Vec<SourceSummary>,
}

const MAX_ANALYSIS_SOURCE_BYTES: usize = 16 * 1024 * 1024;

pub async fn prepare_analysis_inspection(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<AnalysisInspection> {
    let _ = load_owned_study(store, caller, study_id).await?;
    let all = load_sources(store, study_id).await?;
    let ready: Vec<_> = all
        .iter()
        .filter(|source| source.status == "ready")
        .cloned()
        .collect();
    let total_bytes = all.iter().try_fold(0i64, |sum, source| {
        sum.checked_add(source.size_bytes.max(0))
            .ok_or_else(Error::unavailable)
    })?;
    Ok(AnalysisInspection {
        study_id: study_id.to_string(),
        total_sources: all.len(),
        total_ready_sources: ready.len(),
        total_bytes,
        sources: ready,
    })
}

pub async fn inspect_analysis_source(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source: &SourceSummary,
) -> AnalysisStreamEvent {
    let started = std::time::Instant::now();
    let inspected = inspect_analysis_source_bytes(store, caller, study_id, source).await;
    match inspected {
        Ok((text_char_count, page_count, duration_seconds)) => AnalysisStreamEvent {
            event: "source_inspected",
            data: serde_json::json!({
                "stage": "inspecting_source",
                "source_id": source.id,
                "source_kind": source.source_kind,
                "mime_type": source.mime_type,
                "selection_kind": source.selection_kind,
                "selection_spec": source.selection_spec,
                "size_bytes": source.size_bytes,
                "page_count": page_count,
                "duration_seconds": duration_seconds,
                "text_char_count": text_char_count,
                "elapsed_ms": started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            }),
        },
        Err(code) => AnalysisStreamEvent {
            event: "error",
            data: serde_json::json!({
                "stage": "source_error",
                "source_id": source.id,
                "code": code,
            }),
        },
    }
}

async fn inspect_analysis_source_bytes(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source: &SourceSummary,
) -> std::result::Result<(Option<usize>, Option<u32>, Option<f64>), &'static str> {
    let current = current_analysis_source_row(store, caller, study_id, source).await?;
    if source.size_bytes < 0 || source.size_bytes as usize > MAX_ANALYSIS_SOURCE_BYTES {
        return Err("source_size_unsupported");
    }
    let bytes = if source.source_kind == "report" {
        let (project, report, dataset) =
            parse_report_external_ref(&source.external_ref).map_err(|_| "source_access_denied")?;
        let checked = store
            .reports
            .check_report_access(&caller.user_id, project, report)
            .await
            .map_err(|_| "source_access_denied")?;
        let material = store
            .reports
            .fetch_report_dataset(&caller.user_id, project, report, dataset)
            .await
            .map_err(|_| "source_unavailable")?;
        let expected_restricted = current.get::<i64, _>("is_restricted") != 0;
        let expected_calculation: String = current.get("report_calculation_id");
        let expected_version: i64 = current.get("report_calculation_version");
        let expected_rows: i64 = current.get("report_row_count");
        let expected_lineage: String = current.get("report_lineage_sha256");
        if (checked.is_restricted || material.is_restricted) != expected_restricted
            || material.lineage.calculation_id != expected_calculation
            || material.lineage.calculation_version != expected_version
            || material.row_count != expected_rows
            || material.lineage_sha256 != expected_lineage
        {
            return Err("source_integrity_failed");
        }
        material.bytes
    } else {
        if ensure_source_blob_readable(store, caller, study_id, source)
            .await
            .is_err()
        {
            return Err("source_access_denied");
        }
        store
            .fsm
            .get_bytes_bounded(&source.fsm_ref, MAX_ANALYSIS_SOURCE_BYTES)
            .await
            .map_err(|_| "source_unavailable")?
    };
    if bytes.len() != source.size_bytes as usize
        || source.sha256.trim().is_empty()
        || sha256_hex(&bytes) != source.sha256
    {
        return Err("source_integrity_failed");
    }
    let latest = current_analysis_source_row(store, caller, study_id, source).await?;
    if source.source_kind == "report"
        && (latest.get::<String, _>("report_calculation_id")
            != current.get::<String, _>("report_calculation_id")
            || latest.get::<i64, _>("report_calculation_version")
                != current.get::<i64, _>("report_calculation_version")
            || latest.get::<i64, _>("report_row_count")
                != current.get::<i64, _>("report_row_count")
            || latest.get::<String, _>("report_lineage_sha256")
                != current.get::<String, _>("report_lineage_sha256"))
    {
        return Err("source_integrity_failed");
    }
    let text_char_count = if source.mime_type.to_ascii_lowercase().starts_with("text/")
        || source.source_kind == "report"
    {
        let (selected, _) = crate::domain::generations::select_text_content(
            &bytes,
            &source.mime_type,
            &source.selection_kind,
            &source.selection_spec,
        )
        .map_err(|_| "selection_invalid")?;
        Some(selected.chars().count())
    } else {
        None
    };
    let page_count = source
        .mime_type
        .eq_ignore_ascii_case("application/pdf")
        .then(|| extract_pdf_page_count(&bytes))
        .flatten();
    let duration_seconds = matches!(
        source.mime_type.to_ascii_lowercase().as_str(),
        "audio/wav" | "audio/x-wav" | "audio/wave"
    )
    .then(|| cheap_wav_duration_seconds(&bytes))
    .flatten();
    Ok((text_char_count, page_count, duration_seconds))
}

async fn current_analysis_source_row(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    source: &SourceSummary,
) -> std::result::Result<AnyRow, &'static str> {
    // A stream can outlive its preflight. Reuse the normal draft-expiry and
    // ownership checks before each read and before returning inspected data.
    let _ = load_owned_study(store, caller, study_id)
        .await
        .map_err(|_| "source_unavailable")?;
    let current = sqlx::query(
        "SELECT s.owner_id, st.owner_id AS study_owner_id, s.status, s.source_kind, s.mime_type, s.size_bytes, s.sha256, s.fsm_ref, s.selection_kind, s.selection_spec, s.is_restricted, s.external_ref, s.report_calculation_id, s.report_calculation_version, s.report_row_count, s.report_lineage_sha256 FROM sources s JOIN studies st ON st.id = s.study_id WHERE s.id = $1 AND s.study_id = $2",
    )
    .bind(&source.id)
    .bind(study_id)
    .fetch_optional(&store.db)
    .await
    .map_err(|_| "source_unavailable")?
    .ok_or("source_unavailable")?;
    if current.get::<String, _>("owner_id") != caller.user_id
        || current.get::<String, _>("study_owner_id") != caller.user_id
    {
        return Err("source_access_denied");
    }
    if current.get::<String, _>("status") != "ready" {
        return Err("source_unavailable");
    }
    if current.get::<String, _>("source_kind") != source.source_kind
        || current.get::<String, _>("mime_type") != source.mime_type
        || current.get::<i64, _>("size_bytes") != source.size_bytes
        || current.get::<String, _>("sha256") != source.sha256
        || current.get::<String, _>("fsm_ref") != source.fsm_ref
        || current.get::<String, _>("selection_kind") != source.selection_kind
        || current.get::<String, _>("selection_spec") != source.selection_spec
        || (current.get::<i64, _>("is_restricted") != 0) != source.is_restricted
        || current.get::<String, _>("external_ref") != source.external_ref
    {
        return Err("source_integrity_failed");
    }
    Ok(current)
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
    pub config: Option<PreparedStudyConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparedStudyConfig {
    #[serde(default = "default_config_language")]
    pub language: String,
    #[serde(default = "default_config_count")]
    pub count: i64,
}

fn default_config_language() -> String {
    "en".to_string()
}

fn default_config_count() -> i64 {
    10
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
const MAX_SELECTED_PDF_PAGES: u64 = 100;

fn normalize_mime_base(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

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
                && normalize_mime_base(mt) != "application/pdf"
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
            let mut total_pages: u64 = 0;
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
                total_pages = total_pages.saturating_add(u64::from(e - s + 1));
                if total_pages > MAX_SELECTED_PDF_PAGES {
                    return Err(Error::unprocessable(
                        "invalid_selection: Total selected PDF pages must not exceed 100",
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
            if let Some(mt) = mime_type
                && !normalize_mime_base(mt).starts_with("text/")
            {
                return Err(Error::unprocessable(
                    "invalid_selection: passages selection requires text/* source",
                ));
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
                && !normalize_mime_base(mt).starts_with("audio/")
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

pub(crate) async fn ensure_source_blob_readable(
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
    if let Some(ref cfg) = input.config {
        if !matches!(cfg.language.as_str(), "en" | "pt-BR" | "th") {
            return Err(Error::unprocessable(
                "invalid_config: Supported languages are en, pt-BR, and th",
            ));
        }
        if cfg.count < 1 || cfg.count > 20 {
            return Err(Error::unprocessable(
                "invalid_config: Question count must be between 1 and 20",
            ));
        }
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

    if let Some(ref cfg) = input.config {
        sqlx::query(
            "INSERT INTO study_configs(study_id, language, count, created_at, updated_at)
             VALUES($1, $2, $3, $4, $4)",
        )
        .bind(&study_id)
        .bind(&cfg.language)
        .bind(cfg.count)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
    }

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

fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    format!("%{escaped}%")
}

fn push_study_history_filters(
    query: &mut AnyQuery,
    caller: &CallerIdentity,
    ts: i64,
    filter: &StudyHistoryFilter,
    search_pattern: Option<&str>,
) {
    query
        .push(" WHERE s.owner_id = ")
        .push_bind_text(caller.user_id.clone())
        .push(" AND s.status <> 'expired' AND (s.status <> 'draft' OR s.expires_at IS NULL OR s.expires_at >= ")
        .push_bind_i64(ts)
        .push(")");
    if let Some(search_pattern) = search_pattern {
        query
            .push(" AND (LOWER(s.title) LIKE ")
            .push_bind_text(search_pattern.to_string())
            .push(r#" ESCAPE '\' OR LOWER(s.description) LIKE "#)
            .push_bind_text(search_pattern.to_string())
            .push(r#" ESCAPE '\' OR LOWER(qv.title) LIKE "#)
            .push_bind_text(search_pattern.to_string())
            .push(r#" ESCAPE '\')"#);
    }
    if let Some(created_from) = filter.created_from {
        query
            .push(" AND s.created_at >= ")
            .push_bind_i64(created_from);
    }
    if let Some(created_to) = filter.created_to {
        query
            .push(" AND s.created_at <= ")
            .push_bind_i64(created_to);
    }
    if let Some(status) = filter
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query
            .push(" AND s.status = ")
            .push_bind_text(status.to_string());
    }
    if let Some(quiz_id) = filter
        .quiz_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query
            .push(" AND s.quiz_id = ")
            .push_bind_text(quiz_id.to_string());
    }
    if let Some(tag) = filter
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        query
            .push(
                " AND EXISTS (
                    SELECT 1
                    FROM quizzes tag_quiz
                    JOIN quiz_tags qt ON qt.quiz_id = tag_quiz.id
                    JOIN tags t ON t.id = qt.tag_id
                    WHERE tag_quiz.id = s.quiz_id AND tag_quiz.owner_id = s.owner_id
                      AND t.slug = ",
            )
            .push_bind_text(tag.to_string())
            .push(")");
    }
}

pub async fn list_studies(
    store: &Store,
    caller: &CallerIdentity,
    filter: &StudyHistoryFilter,
    limit: i64,
    offset: i64,
) -> Result<StudyHistoryPage> {
    if filter
        .created_from
        .zip(filter.created_to)
        .is_some_and(|(from, to)| from > to)
        || filter.created_from.is_some_and(|value| value < 0)
        || filter.created_to.is_some_and(|value| value < 0)
    {
        return Err(Error::unprocessable(
            "invalid_date_range: created_from and created_to must be non-negative and ordered",
        ));
    }

    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if search.is_some_and(|value| value.chars().count() > 200) {
        return Err(Error::unprocessable(
            "invalid_search: q must contain no more than 200 characters",
        ));
    }
    let search_pattern = search.map(|value| escape_like_literal(&value.to_lowercase()));
    let status = filter
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if status.is_some_and(|value| !matches!(value, "draft" | "active" | "expired")) {
        return Err(Error::unprocessable(
            "invalid_status: status must be draft, active, or expired",
        ));
    }
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let offset = offset.max(0);
    let ts = now();

    let mut count = AnyQuery::new(
        "SELECT COUNT(*) FROM studies s
         LEFT JOIN quizzes q ON q.id = s.quiz_id AND q.owner_id = s.owner_id
         LEFT JOIN quiz_versions qv ON qv.id = q.active_version_id",
    );
    push_study_history_filters(&mut count, caller, ts, filter, search_pattern.as_deref());
    let total: i64 = count.query().fetch_one(&store.db).await?.try_get(0)?;

    let tag_rows = sqlx::query(
        "SELECT DISTINCT t.slug, t.label, LOWER(t.label) AS sort_label
         FROM studies s
         JOIN quizzes q ON q.id = s.quiz_id AND q.owner_id = s.owner_id
         JOIN quiz_tags qt ON qt.quiz_id = q.id
         JOIN tags t ON t.id = qt.tag_id
         WHERE s.owner_id = $1 AND s.quiz_id <> '' AND s.status <> 'expired'
           AND (s.status <> 'draft' OR s.expires_at IS NULL OR s.expires_at >= $2)
         ORDER BY sort_label, t.slug",
    )
    .bind(&caller.user_id)
    .bind(ts)
    .fetch_all(&store.db)
    .await?;
    let available_tags = tag_rows
        .into_iter()
        .map(|row| {
            Ok(AvailableStudyTag {
                slug: row.try_get("slug")?,
                label: row.try_get("label")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut query = AnyQuery::new(
        "SELECT s.id, s.title, s.description, s.source_origin, s.status, s.quiz_id,
                s.expires_at, s.created_at, s.updated_at
         FROM studies s
         LEFT JOIN quizzes q ON q.id = s.quiz_id AND q.owner_id = s.owner_id
         LEFT JOIN quiz_versions qv ON qv.id = q.active_version_id",
    );
    push_study_history_filters(&mut query, caller, ts, filter, search_pattern.as_deref());
    query
        .push(" ORDER BY s.updated_at DESC, s.id DESC LIMIT ")
        .push_bind_i64(limit)
        .push(" OFFSET ")
        .push_bind_i64(offset);
    let rows = query.query().fetch_all(&store.db).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(StudySummary {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                description: row.try_get("description")?,
                source_origin: row.try_get("source_origin")?,
                status: row.try_get("status")?,
                quiz_id: row.try_get("quiz_id")?,
                expires_at: row.try_get("expires_at")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(StudyHistoryPage {
        items,
        total,
        limit,
        offset,
        available_tags,
    })
}

async fn load_study_config(store: &Store, study_id: &str) -> Result<Option<PreparedStudyConfig>> {
    let row = sqlx::query("SELECT language, count FROM study_configs WHERE study_id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?;
    Ok(row.map(|r| PreparedStudyConfig {
        language: r.get("language"),
        count: r.get("count"),
    }))
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
    let config = load_study_config(store, study_id).await?;
    let quota = quotas::meter_for_owner(store, &caller.user_id, &quotas.get()).await?;
    Ok(StudyDetail {
        study,
        sources,
        quota,
        config,
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
    sqlx::query("DELETE FROM study_configs WHERE study_id = $1")
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
    if input.kind == "report" && !input.fsm_ref.trim().is_empty() {
        return Err(Error::unprocessable(
            "invalid_report_ref: Report sources must not include an FSM reference",
        ));
    }
    let mut is_restricted = input.is_restricted;
    let mut effective_size = input.size_bytes.max(0);
    let mut sha256 = String::new();
    let mut report_calculation_id = String::new();
    let mut report_calculation_version = 0i64;
    let mut report_row_count = 0i64;
    let mut report_lineage_sha256 = String::new();
    let effective_mime_type = if input.kind == "report" {
        "text/plain"
    } else {
        input.mime_type.trim()
    };
    if input.kind == "fsm_ref" {
        if let Some(verified_size) = authorize_fsm_ref(store, caller, &input.fsm_ref, false).await?
        {
            effective_size = verified_size;
        }
    } else if input.kind == "report" {
        let (project, report, dataset) = parse_report_external_ref(&input.external_ref)?;
        let checked = store
            .reports
            .check_report_access(&caller.user_id, project, report)
            .await?;
        let material = store
            .reports
            .fetch_report_dataset(&caller.user_id, project, report, dataset)
            .await?;
        effective_size = i64::try_from(material.bytes.len()).map_err(|_| {
            Error::unprocessable(
                "report_source_too_large: Dataset exceeds the supported source size",
            )
        })?;
        sha256 = sha256_hex(&material.bytes);
        is_restricted = checked.is_restricted || material.is_restricted;
        report_calculation_id = material.lineage.calculation_id.clone();
        report_calculation_version = material.lineage.calculation_version;
        report_row_count = material.row_count;
        report_lineage_sha256 = material.lineage_sha256.clone();
    }
    let source_id = new_id();
    let ts = now();
    let restricted = i32::from(is_restricted);
    let provenance = if input.kind == "fsm_ref" {
        "owned_ref"
    } else {
        "unknown"
    };
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, source_provenance, report_calculation_id, report_calculation_version, report_row_count, report_lineage_sha256, created_at, updated_at)
         VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ready', 'all', '', $10, $11, $12, $13, $14, $15, $16, $17, $17)",
    )
    .bind(&source_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(input.filename.trim())
    .bind(effective_mime_type)
    .bind(effective_size)
    .bind(&sha256)
    .bind(input.fsm_ref.trim())
    .bind(&input.kind)
    .bind(restricted)
    .bind(input.external_ref.trim())
    .bind(provenance)
    .bind(&report_calculation_id)
    .bind(report_calculation_version)
    .bind(report_row_count)
    .bind(&report_lineage_sha256)
    .bind(ts)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(SourceSummary {
        id: source_id,
        filename: input.filename.trim().to_string(),
        mime_type: effective_mime_type.to_string(),
        size_bytes: effective_size,
        sha256,
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

pub(crate) fn parse_report_external_ref(external_ref: &str) -> Result<(&str, &str, &str)> {
    let mut parts = external_ref.trim().split('/');
    let (Some(project), Some(report), Some(dataset), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::unprocessable(
            "invalid_report_ref: Report external_ref must be <project>/<report>/<dataset>",
        ));
    };
    let valid_slug = |part: &str| {
        !part.is_empty()
            && part.len() <= 128
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if [project, report, dataset]
        .iter()
        .any(|part| *part != part.trim() || !valid_slug(part))
    {
        return Err(Error::unprocessable(
            "invalid_report_ref: Report external_ref must contain valid project, report, and dataset identifiers",
        ));
    }
    Ok((project, report, dataset))
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

#[cfg(test)]
mod selection_tests {
    use super::{MAX_SELECTION_RANGES, canonicalize_selection};

    #[test]
    fn selection_specs_enforce_the_shared_size_and_range_caps() {
        let at_limit = (0..MAX_SELECTION_RANGES)
            .map(|range| format!("p1:{}-{}", range * 2, range * 2 + 1))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            canonicalize_selection("passages", &at_limit, Some("text/plain"), None, None).is_ok()
        );
        let start = MAX_SELECTION_RANGES * 2;
        let over_limit = format!("{at_limit},p1:{start}-{}", start + 1);
        assert!(
            canonicalize_selection("passages", &over_limit, Some("text/plain"), None, None)
                .is_err()
        );
        assert!(canonicalize_selection("all", "not-empty", None, None, None).is_err());
        assert!(
            canonicalize_selection(
                "passages",
                &"x".repeat(2001),
                Some("text/plain"),
                None,
                None
            )
            .is_err()
        );
    }
}
