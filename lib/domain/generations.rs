use crate::{
    core::{Error, Result, now},
    dal::{
        DatabaseBackend, Store,
        cortex::{GeneratedQuestion, GenerationRequest, GenerationSource},
        fsm::sha256_hex,
        new_id,
    },
    domain::{
        auth::CallerIdentity,
        auth::require_owner,
        catalog,
        estimates::{self, StepMeasurement},
        guardrail,
        studies::{self, SourceSummary},
    },
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Any, Connection, Row, pool::PoolConnection};
use std::collections::HashSet;

const GENERATION_WORKER_LOCK_CLASS: i32 = 0x43455254;
const GENERATION_WORKER_LOCK_ID: i32 = 0x47454E31;
type WorkerConnection = std::sync::Arc<tokio::sync::Mutex<PoolConnection<Any>>>;

async fn execute_worker_state_query<'q>(
    store: &Store,
    worker_connection: Option<&WorkerConnection>,
    query: sqlx::query::Query<'q, Any, sqlx::any::AnyArguments<'q>>,
) -> Result<sqlx::any::AnyQueryResult> {
    if let Some(connection) = worker_connection {
        let mut connection = connection.lock().await;
        Ok(query.execute(&mut **connection).await?)
    } else {
        Ok(query.execute(&store.db).await?)
    }
}

async fn record_worker_step(
    store: &Store,
    job_id: &str,
    step: &StepMeasurement,
    worker_connection: Option<&WorkerConnection>,
) -> Result<()> {
    if let Some(worker_connection) = worker_connection {
        let mut connection = worker_connection.lock().await;
        estimates::record_step_on_connection(&mut connection, job_id, step).await
    } else {
        estimates::record_step(store, job_id, step).await
    }
}

pub const QUESTION_SCHEMA_NAME: &str = "question-v2";
pub const LEGACY_QUESTION_SCHEMA_VERSION: i64 = 1;
pub const QUESTION_SCHEMA_VERSION: i64 = 2;

#[derive(Clone, Debug, Serialize)]
pub struct GenerationJobSummary {
    pub id: String,
    pub study_id: String,
    pub quiz_id: Option<String>,
    pub status: String,
    pub language: String,
    pub requested_count: i64,
    pub queue_position: i64,
    pub saved_count: i64,
    pub stop_requested: bool,
    pub error_code: String,
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GenerationEvent {
    pub seq: i64,
    pub event_type: String,
    pub message: String,
    pub created_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartGenerationInput {
    pub language: String,
    pub count: i64,
    #[serde(default)]
    pub idempotency_key: String,
    #[serde(default)]
    pub quiz_id: Option<String>,
}

pub fn has_cjk_or_kana(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3040}'..='\u{30FF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
    })
}

pub fn has_wrong_script(language: &str, questions: &[GeneratedQuestion]) -> bool {
    for q in questions {
        let mut combined = String::from(&q.prompt);
        combined.push(' ');
        combined.push_str(&q.explanation);
        for alt in &q.alternatives {
            combined.push(' ');
            combined.push_str(&alt.text);
        }
        match language {
            "th" => {
                if has_cjk_or_kana(&combined) {
                    return true;
                }
            }
            "en" | "pt-BR" => {
                if has_cjk_or_kana(&combined)
                    || combined
                        .chars()
                        .any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c))
                {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

async fn emit_event(store: &Store, job_id: &str, event_type: &str, message: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO generation_events(job_id, event_type, message, created_at) VALUES($1, $2, $3, $4)",
    )
    .bind(job_id)
    .bind(event_type)
    .bind(message)
    .bind(now())
    .execute(&store.db)
    .await?;
    Ok(())
}

async fn emit_event_with_worker_connection(
    store: &Store,
    job_id: &str,
    event_type: &str,
    message: &str,
    worker_connection: Option<&WorkerConnection>,
) -> Result<()> {
    execute_worker_state_query(
        store,
        worker_connection,
        sqlx::query(
            "INSERT INTO generation_events(job_id, event_type, message, created_at) VALUES($1, $2, $3, $4)",
        )
        .bind(job_id)
        .bind(event_type)
        .bind(message)
        .bind(now()),
    )
    .await?;
    Ok(())
}

pub async fn list_events(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
    after_seq: i64,
) -> Result<Vec<GenerationEvent>> {
    let _ = get_job(store, caller, job_id).await?;
    let rows = sqlx::query(
        "SELECT seq, event_type, message, created_at FROM generation_events WHERE job_id = $1 AND seq > $2 ORDER BY seq ASC LIMIT 100",
    )
    .bind(job_id)
    .bind(after_seq)
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GenerationEvent {
            seq: r.get("seq"),
            event_type: r.get("event_type"),
            message: r.get("message"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn get_job(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    let row = sqlx::query(
        "SELECT job.id, job.owner_id, job.study_id, job.quiz_id, job.status, job.language,
                job.requested_count,
                CASE WHEN job.status IN ('queued', 'running') THEN (
                    SELECT COUNT(*) FROM generation_jobs AS active
                    WHERE active.status IN ('queued', 'running')
                      AND (active.queued_at < job.queued_at
                           OR (active.queued_at = job.queued_at AND active.id <= job.id))
                ) ELSE 0 END AS queue_position,
                job.saved_count, job.stopped_reason, job.error_code, job.queued_at,
                job.started_at, job.finished_at
         FROM generation_jobs AS job WHERE job.id = $1",
    )
    .bind(job_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    Ok(GenerationJobSummary {
        id: row.get("id"),
        study_id: row.get("study_id"),
        quiz_id: row.get("quiz_id"),
        status: row.get("status"),
        language: row.get("language"),
        requested_count: row.get("requested_count"),
        queue_position: row.get("queue_position"),
        saved_count: row.get("saved_count"),
        stop_requested: row.get::<String, _>("stopped_reason") == "user_stop_requested",
        error_code: row.get("error_code"),
        queued_at: row.get("queued_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    })
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.is_unique_violation())
}

fn active_generation_conflict() -> Error {
    Error(
        StatusCode::CONFLICT,
        "generation_already_active: Only one queued or running generation per user is allowed"
            .into(),
    )
}

pub async fn start_generation(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    input: StartGenerationInput,
) -> Result<GenerationJobSummary> {
    if !matches!(input.language.as_str(), "en" | "pt-BR" | "th") {
        return Err(Error::unprocessable(
            "invalid_language: Supported languages are en, pt-BR, and th",
        ));
    }
    if !(1..=20).contains(&input.count) {
        return Err(Error::unprocessable(
            "invalid_count: Question count must be between 1 and 20",
        ));
    }

    // Verify that the caller can start work from this study and expire stale drafts.
    let study_row = sqlx::query("SELECT owner_id, status, expires_at FROM studies WHERE id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let study_owner: String = study_row.get("owner_id");
    require_owner(caller, &study_owner)?;
    let study_status: String = study_row.get("status");
    let expires_at: Option<i64> = study_row.get("expires_at");
    if study_status == "expired"
        || (study_status == "draft" && expires_at.is_some_and(|expiry| now() > expiry))
    {
        if study_status == "draft" {
            sqlx::query("UPDATE studies SET status = 'expired', updated_at = $1 WHERE id = $2 AND status = 'draft'")
                .bind(now())
                .bind(study_id)
                .execute(&store.db)
                .await?;
        }
        return Err(Error(
            StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if !matches!(study_status.as_str(), "draft" | "active") {
        return Err(Error(
            StatusCode::CONFLICT,
            "study_unavailable: Study cannot start a generation in its current state".into(),
        ));
    }

    let target_quiz_id = input
        .quiz_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    if let Some(quiz_id) = target_quiz_id.as_deref() {
        let target_ok = sqlx::query(
            "SELECT id FROM quizzes WHERE id = $1 AND owner_id = $2 AND study_id = $3 AND visibility = 'private'",
        )
        .bind(quiz_id)
        .bind(&caller.user_id)
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?;
        if target_ok.is_none() {
            return Err(Error(
                StatusCode::NOT_FOUND,
                "quiz_not_available: Target quiz is not an owned private quiz in this study".into(),
            ));
        }
    }

    // Idempotent retry on same idempotency_key.
    let idem = input.idempotency_key.trim();
    if !idem.is_empty()
        && let Some(existing_id) = sqlx::query_scalar::<_, String>(
            "SELECT id FROM generation_jobs WHERE owner_id = $1 AND study_id = $2 AND idempotency_key = $3",
        )
        .bind(&caller.user_id)
        .bind(study_id)
        .bind(idem)
        .fetch_optional(&store.db)
        .await?
    {
        return get_job(store, caller, &existing_id).await;
    }

    // Enforce one queued or running generation per user.
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE owner_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(&caller.user_id)
    .fetch_one(&store.db)
    .await?;
    if active_count > 0 {
        if !idem.is_empty()
            && let Some(existing_id) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM generation_jobs WHERE owner_id = $1 AND study_id = $2 AND idempotency_key = $3",
            )
            .bind(&caller.user_id)
            .bind(study_id)
            .bind(idem)
            .fetch_optional(&store.db)
            .await?
        {
            return get_job(store, caller, &existing_id).await;
        }
        return Err(active_generation_conflict());
    }

    let mut tx = store.db.begin().await?;
    let source_rows = sqlx::query(
        "SELECT id, filename, mime_type, size_bytes, sha256, source_kind, selection_kind, selection_spec, external_ref,
                report_calculation_id, report_calculation_version, report_row_count, report_lineage_sha256
         FROM sources WHERE study_id = $1 AND status = 'ready' ORDER BY created_at ASC, id ASC",
    )
    .bind(study_id)
    .fetch_all(&mut *tx)
    .await?;
    if source_rows.is_empty() {
        return Err(Error::unprocessable(
            "no_ready_sources: Study has no ready sources for generation",
        ));
    }
    let ahead: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE status IN ('queued', 'running')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let queue_position = ahead + 1;

    let job_id = new_id();
    let ts = now();
    let insert = sqlx::query(
        "INSERT INTO generation_jobs(id, owner_id, study_id, quiz_id, status, language, requested_count, idempotency_key, queue_position, saved_count, queued_at)
         VALUES($1, $2, $3, $4, 'queued', $5, $6, $7, $8, 0, $9)",
    )
    .bind(&job_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(&target_quiz_id)
    .bind(&input.language)
    .bind(input.count)
    .bind(idem)
    .bind(queue_position)
    .bind(ts)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        if is_unique_violation(&error) {
            tx.rollback().await?;
            if !idem.is_empty()
                && let Some(existing_id) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM generation_jobs WHERE owner_id = $1 AND study_id = $2 AND idempotency_key = $3",
                )
                .bind(&caller.user_id)
                .bind(study_id)
                .bind(idem)
                .fetch_optional(&store.db)
                .await?
            {
                return get_job(store, caller, &existing_id).await;
            }
            return Err(active_generation_conflict());
        }
        return Err(error.into());
    }

    for source in source_rows {
        sqlx::query(
            "INSERT INTO generation_job_sources
             (job_id, source_id, filename, mime_type, size_bytes, sha256, source_kind, selection_kind, selection_spec, external_ref,
              report_calculation_id, report_calculation_version, report_row_count, report_lineage_sha256)
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
            .bind(&job_id)
            .bind(source.get::<String, _>("id"))
            .bind(source.get::<String, _>("filename"))
            .bind(source.get::<String, _>("mime_type"))
            .bind(source.get::<i64, _>("size_bytes"))
            .bind(source.get::<String, _>("sha256"))
            .bind(source.get::<String, _>("source_kind"))
            .bind(source.get::<String, _>("selection_kind"))
            .bind(source.get::<String, _>("selection_spec"))
            .bind(source.get::<String, _>("external_ref"))
            .bind(source.get::<String, _>("report_calculation_id"))
            .bind(source.get::<i64, _>("report_calculation_version"))
            .bind(source.get::<i64, _>("report_row_count"))
            .bind(source.get::<String, _>("report_lineage_sha256"))
            .execute(&mut *tx)
            .await?;
        if source.get::<String, _>("source_kind") == "report"
            && (source
                .get::<String, _>("report_calculation_id")
                .trim()
                .is_empty()
                || source
                    .get::<String, _>("report_lineage_sha256")
                    .trim()
                    .is_empty())
        {
            return Err(Error::unprocessable(
                "source_unavailable: A Reports dataset has no frozen source snapshot",
            ));
        }
    }
    tx.commit().await?;

    emit_event(
        store,
        &job_id,
        "queued",
        &format!("Queued at position {queue_position}"),
    )
    .await?;
    get_job(store, caller, &job_id).await
}

fn utf16_offset_to_byte(text: &str, target: usize) -> Option<usize> {
    let mut units = 0usize;
    for (byte, ch) in text.char_indices() {
        if units == target {
            return Some(byte);
        }
        let next = units.checked_add(ch.len_utf16())?;
        if target > units && target < next {
            return None;
        }
        units = next;
    }
    (units == target).then_some(text.len())
}

pub(crate) fn select_text_content(
    bytes: &[u8],
    mime_type: &str,
    kind: &str,
    spec: &str,
) -> Result<(String, String)> {
    if spec.len() > 2000
        || (kind != "all" && spec.split(',').count() > studies::MAX_SELECTION_RANGES)
    {
        return Err(Error::unprocessable(
            "invalid_source_selection: Selection exceeds the shared size or range limit",
        ));
    }
    if !mime_type.starts_with("text/") {
        return Err(Error::unprocessable(
            "unsupported_source: Non-text sources require Cortex file selection support",
        ));
    }
    let decoded = std::str::from_utf8(bytes).map_err(|_| {
        Error::unprocessable("invalid_source_encoding: Text source is not valid UTF-8")
    })?;
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let text = decoded.replace("\r\n", "\n").replace('\r', "\n");
    match kind {
        "all" => Ok((text, "all".into())),
        "passages" => {
            let mut selected = Vec::new();
            let mut normalized = Vec::new();
            for range in spec.split(',') {
                let (page, offsets) = range.trim().split_once(':').ok_or_else(|| {
                    Error::unprocessable("invalid_source_selection: Expected p1:start-end ranges")
                })?;
                if page != "p1" {
                    return Err(Error::unprocessable(
                        "invalid_source_selection: Text selections use page p1",
                    ));
                }
                let (start, end) = offsets.split_once('-').ok_or_else(|| {
                    Error::unprocessable("invalid_source_selection: Expected start-end offsets")
                })?;
                let start = start.parse::<usize>().map_err(|_| {
                    Error::unprocessable("invalid_source_selection: Offsets must be integers")
                })?;
                let end = end.parse::<usize>().map_err(|_| {
                    Error::unprocessable("invalid_source_selection: Offsets must be integers")
                })?;
                if end <= start {
                    return Err(Error::unprocessable(
                        "invalid_source_selection: Text ranges must be non-empty and forward",
                    ));
                }
                let start_byte = utf16_offset_to_byte(&text, start).ok_or_else(|| {
                    Error::unprocessable(
                        "invalid_source_selection: Text offsets cannot split a UTF-16 surrogate pair",
                    )
                })?;
                let end_byte = utf16_offset_to_byte(&text, end).ok_or_else(|| {
                    Error::unprocessable(
                        "invalid_source_selection: Text offsets cannot split a UTF-16 surrogate pair",
                    )
                })?;
                if end_byte <= start_byte {
                    return Err(Error::unprocessable(
                        "invalid_source_selection: Text range is empty or outside the source",
                    ));
                }
                selected.push(text[start_byte..end_byte].to_string());
                normalized.push(format!("p1:{start}-{end}"));
            }
            if selected.is_empty() {
                return Err(Error::unprocessable(
                    "invalid_source_selection: At least one text range is required",
                ));
            }
            Ok((selected.join("\n"), normalized.join(",")))
        }
        _ => Err(Error::unprocessable(
            "unsupported_source: This text source requires all or passages selection",
        )),
    }
}

async fn fail_job_state(
    store: &Store,
    job_id: &str,
    code: &str,
    message: &str,
    worker_connection: Option<&WorkerConnection>,
) -> Result<GenerationJobSummary> {
    let failed = execute_worker_state_query(
        store,
        worker_connection,
        sqlx::query(
        "UPDATE generation_jobs SET status = 'failed', error_code = $1, finished_at = $2 WHERE id = $3 AND status = 'running'",
        )
        .bind(code)
        .bind(now())
        .bind(job_id),
    )
    .await?;
    if failed.rows_affected() > 0 {
        emit_event_with_worker_connection(store, job_id, "failed", message, worker_connection)
            .await?;
    }
    Err(Error::unprocessable(format!("{code}: {message}")))
}

async fn stop_was_requested(store: &Store, job_id: &str) -> Result<bool> {
    let reason: String =
        sqlx::query_scalar("SELECT stopped_reason FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;
    Ok(reason == "user_stop_requested")
}

async fn finish_requested_stop_state(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
    worker_connection: Option<&WorkerConnection>,
) -> Result<GenerationJobSummary> {
    let stopped = execute_worker_state_query(
        store,
        worker_connection,
        sqlx::query(
            "UPDATE generation_jobs SET status = 'cancelled', error_code = '', finished_at = $1
         WHERE id = $2 AND status = 'running' AND stopped_reason = 'user_stop_requested'",
        )
        .bind(now())
        .bind(job_id),
    )
    .await?;
    if stopped.rows_affected() > 0 {
        emit_event_with_worker_connection(
            store,
            job_id,
            "cancelled",
            "Stopped at a safe boundary after the current step",
            worker_connection,
        )
        .await?;
    }
    get_job(store, caller, job_id).await
}

pub async fn process_job(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    process_job_with_worker_connection(store, caller, job_id, None).await
}

async fn process_job_with_worker_connection(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
    worker_connection: Option<&WorkerConnection>,
) -> Result<GenerationJobSummary> {
    macro_rules! fail_job {
        ($store:expr, $job_id:expr, $code:expr, $message:expr $(,)?) => {
            fail_job_state($store, $job_id, $code, $message, worker_connection)
        };
    }
    macro_rules! finish_requested_stop {
        ($store:expr, $caller:expr, $job_id:expr $(,)?) => {
            finish_requested_stop_state($store, $caller, $job_id, worker_connection)
        };
    }
    macro_rules! emit_event {
        ($store:expr, $job_id:expr, $event_type:expr, $message:expr $(,)?) => {
            emit_event_with_worker_connection(
                $store,
                $job_id,
                $event_type,
                $message,
                worker_connection,
            )
        };
    }
    macro_rules! record_step {
        ($store:expr, $job_id:expr, $step:expr $(,)?) => {
            record_worker_step($store, $job_id, $step, worker_connection)
        };
    }
    let job = get_job(store, caller, job_id).await?;
    if job.status == "cancelled" || job.status == "completed" {
        return Ok(job);
    }

    if job.status == "running" {
        let code: String =
            sqlx::query_scalar("SELECT error_code FROM generation_jobs WHERE id = $1")
                .bind(job_id)
                .fetch_one(&store.db)
                .await?;
        if code == "cortex_outcome_unknown" {
            return Ok(job);
        }
        return Err(Error::conflict(
            "generation_already_running: This generation is already being processed",
        ));
    }

    let estimated_before = estimates::estimate_for_study(
        store,
        caller,
        &job.study_id,
        &job.language,
        job.requested_count,
    )
    .await
    .ok()
    .and_then(|e| e.estimated_processing_ms);

    let job_queued_at: i64 =
        sqlx::query_scalar("SELECT queued_at FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;

    let started = now();
    let wait_ms = (started - job_queued_at).max(0) * 1000;
    let claimed = execute_worker_state_query(store, worker_connection, sqlx::query("UPDATE generation_jobs SET status = 'running', started_at = $1 WHERE id = $2 AND status = 'queued'")
        .bind(started)
        .bind(job_id)).await?;
    if claimed.rows_affected() == 0 {
        return get_job(store, caller, job_id).await;
    }
    emit_event!(store, job_id, "running", "Calling Cortex T0 Easy").await?;
    if stop_was_requested(store, job_id).await? {
        return finish_requested_stop!(store, caller, job_id).await;
    }

    let study = sqlx::query("SELECT owner_id, status, expires_at FROM studies WHERE id = $1")
        .bind(&job.study_id)
        .fetch_optional(&store.db)
        .await?;
    let Some(study) = study else {
        return fail_job!(
            store,
            job_id,
            "study_unavailable",
            "The selected study is no longer available",
        )
        .await;
    };
    let study_owner: String = study.get("owner_id");
    let study_status: String = study.get("status");
    let study_expiry: Option<i64> = study.get("expires_at");
    if study_owner != caller.user_id {
        return fail_job!(
            store,
            job_id,
            "study_unavailable",
            "The selected study is no longer available",
        )
        .await;
    }
    if study_status == "expired"
        || (study_status == "draft" && study_expiry.is_some_and(|expiry| now() > expiry))
    {
        if study_status == "draft" {
            sqlx::query("UPDATE studies SET status = 'expired', updated_at = $1 WHERE id = $2 AND status = 'draft'")
                .bind(now())
                .bind(&job.study_id)
                .execute(&store.db)
                .await?;
        }
        return fail_job!(
            store,
            job_id,
            "study_expired",
            "The selected study draft expired before generation started",
        )
        .await;
    }
    if !matches!(study_status.as_str(), "draft" | "active") {
        return fail_job!(
            store,
            job_id,
            "study_unavailable",
            "The selected study is no longer available",
        )
        .await;
    }

    if let Some(quiz_id) = job.quiz_id.as_deref().filter(|id| !id.trim().is_empty()) {
        let target = sqlx::query(
            "SELECT id FROM quizzes WHERE id = $1 AND owner_id = $2 AND study_id = $3 AND visibility = 'private'",
        )
        .bind(quiz_id)
        .bind(&caller.user_id)
        .bind(&job.study_id)
        .fetch_optional(&store.db)
        .await?;
        if target.is_none() {
            return fail_job!(
                store,
                job_id,
                "quiz_not_available",
                "The target quiz is no longer an owned private quiz in this study",
            )
            .await;
        }
    }

    let prep_inst = std::time::Instant::now();
    let prep_started_at = now();
    let source_rows = sqlx::query(
        "SELECT gjs.source_id, gjs.filename, gjs.mime_type, gjs.size_bytes, gjs.sha256,
                gjs.source_kind, gjs.selection_kind, gjs.selection_spec, gjs.external_ref,
                gjs.report_calculation_id, gjs.report_calculation_version, gjs.report_row_count, gjs.report_lineage_sha256,
                s.owner_id, s.status, s.fsm_ref, s.source_kind AS current_source_kind,
                s.mime_type AS current_mime_type, s.size_bytes AS current_size_bytes,
                s.sha256 AS current_sha256, s.is_restricted, s.external_ref AS current_external_ref,
                s.report_calculation_id AS current_report_calculation_id,
                s.report_calculation_version AS current_report_calculation_version,
                s.report_row_count AS current_report_row_count,
                s.report_lineage_sha256 AS current_report_lineage_sha256,
                s.created_at, s.updated_at
         FROM generation_job_sources gjs JOIN sources s ON s.id = gjs.source_id
         WHERE gjs.job_id = $1 ORDER BY gjs.source_id",
    )
    .bind(job_id)
    .fetch_all(&store.db)
    .await?;
    let mut sources = Vec::with_capacity(source_rows.len());
    let mut source_ids = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let source_id: String = row.get("source_id");
        let source_owner: String = row.get("owner_id");
        let status: String = row.get("status");
        let fsm_ref: String = row.get("fsm_ref");
        let size_bytes: i64 = row.get("size_bytes");
        let sha256: String = row.get("sha256");
        let mime_type: String = row.get("mime_type");
        let selection_kind: String = row.get("selection_kind");
        let selection_spec: String = row.get("selection_spec");
        let current_source_kind: String = row.get("current_source_kind");
        let current_mime_type: String = row.get("current_mime_type");
        let current_size_bytes: i64 = row.get("current_size_bytes");
        let current_sha256: String = row.get("current_sha256");
        let source = SourceSummary {
            id: source_id.clone(),
            filename: row.get("filename"),
            mime_type: current_mime_type.clone(),
            size_bytes: current_size_bytes,
            sha256: current_sha256.clone(),
            fsm_ref: fsm_ref.clone(),
            source_kind: current_source_kind.clone(),
            status: status.clone(),
            selection_kind: selection_kind.clone(),
            selection_spec: selection_spec.clone(),
            is_restricted: row.get::<i64, _>("is_restricted") != 0,
            external_ref: row.get("external_ref"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        };
        let external_ref: String = row.get("external_ref");
        let current_external_ref: String = row.get("current_external_ref");
        let is_report = current_source_kind == "report";
        if source_owner != caller.user_id
            || status != "ready"
            || (!is_report && fsm_ref.trim().is_empty())
            || external_ref != current_external_ref
        {
            return fail_job!(
                store,
                job_id,
                "source_unavailable",
                "A selected source is no longer available for this generation",
            )
            .await;
        }
        if current_source_kind != row.get::<String, _>("source_kind")
            || current_mime_type != mime_type
            || current_size_bytes != size_bytes
            || current_sha256 != sha256
            || (is_report
                && (row.get::<String, _>("current_report_calculation_id")
                    != row.get::<String, _>("report_calculation_id")
                    || row.get::<i64, _>("current_report_calculation_version")
                        != row.get::<i64, _>("report_calculation_version")
                    || row.get::<i64, _>("current_report_row_count")
                        != row.get::<i64, _>("report_row_count")
                    || row.get::<String, _>("current_report_lineage_sha256")
                        != row.get::<String, _>("report_lineage_sha256")))
        {
            return fail_job!(
                store,
                job_id,
                "source_integrity_failed",
                "A selected source no longer matches its saved metadata",
            )
            .await;
        }
        let bytes = if is_report {
            let (project, report, dataset) = match studies::parse_report_external_ref(&external_ref)
            {
                Ok(parts) => parts,
                Err(_) => {
                    return fail_job!(
                        store,
                        job_id,
                        "source_unavailable",
                        "A selected Reports dataset is no longer available",
                    )
                    .await;
                }
            };
            let checked = if let Ok(checked) = store
                .reports
                .check_report_access(&caller.user_id, project, report)
                .await
            {
                checked
            } else {
                return fail_job!(
                    store,
                    job_id,
                    "source_unavailable",
                    "A selected Reports dataset is no longer authorized",
                )
                .await;
            };
            let material = match store
                .reports
                .fetch_report_dataset(&caller.user_id, project, report, dataset)
                .await
            {
                Ok(material) => material,
                Err(_) => {
                    return fail_job!(
                        store,
                        job_id,
                        "source_unavailable",
                        "A selected Reports dataset is no longer authorized",
                    )
                    .await;
                }
            };
            if (checked.is_restricted || material.is_restricted) != source.is_restricted {
                return fail_job!(
                    store,
                    job_id,
                    "source_unavailable",
                    "A selected Reports dataset is no longer authorized",
                )
                .await;
            }
            if material.lineage.calculation_id != row.get::<String, _>("report_calculation_id")
                || material.lineage.calculation_version
                    != row.get::<i64, _>("report_calculation_version")
                || material.row_count != row.get::<i64, _>("report_row_count")
                || material.lineage_sha256 != row.get::<String, _>("report_lineage_sha256")
            {
                return fail_job!(
                    store,
                    job_id,
                    "source_integrity_failed",
                    "A selected Reports dataset no longer matches its saved calculation lineage",
                )
                .await;
            }
            material.bytes
        } else {
            if studies::ensure_source_blob_readable(store, caller, &job.study_id, &source)
                .await
                .is_err()
            {
                return fail_job!(
                    store,
                    job_id,
                    "source_unavailable",
                    "A selected source is not authorized for this generation",
                )
                .await;
            }
            match store.fsm.get_bytes(&fsm_ref).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return fail_job!(
                        store,
                        job_id,
                        "source_unavailable",
                        "A selected source is no longer available for this generation",
                    )
                    .await;
                }
            }
        };
        if size_bytes < 0
            || bytes.len() as i64 != size_bytes
            || sha256.trim().is_empty()
            || sha256_hex(&bytes) != sha256
        {
            return fail_job!(
                store,
                job_id,
                "source_integrity_failed",
                "A selected source no longer matches its saved integrity check",
            )
            .await;
        }
        let local_text = current_source_kind == "report" || mime_type.starts_with("text/");
        if local_text {
            let (selected, selection) =
                match select_text_content(&bytes, &mime_type, &selection_kind, &selection_spec) {
                    Ok(selected) => selected,
                    Err(error) => {
                        let message = if error.1.starts_with("unsupported_source") {
                            "This source type requires the supported Cortex file-selection bridge"
                        } else {
                            "A saved source selection is invalid"
                        };
                        let code = if error.1.starts_with("unsupported_source") {
                            "unsupported_source_selection"
                        } else {
                            "invalid_source_selection"
                        };
                        return fail_job!(store, job_id, code, message).await;
                    }
                };
            let suffix = match mime_type.as_str() {
                "text/markdown" => "md",
                "text/csv" => "csv",
                _ => "txt",
            };
            sources.push(GenerationSource {
                source_id: source_id.clone(),
                filename: format!("source-{source_id}.{suffix}"),
                mime_type: "text/plain".into(),
                selection,
                selection_kind: "all".into(),
                selection_spec: String::new(),
                bytes: selected.into_bytes(),
            });
        } else {
            let normalized_mime = mime_type.trim().to_ascii_lowercase();
            let selection_supported = match selection_kind.as_str() {
                "all" => true,
                "pages" => normalized_mime == "application/pdf",
                "passages" => {
                    normalized_mime
                        == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                }
                "intervals" => normalized_mime.starts_with("audio/"),
                _ => false,
            };
            if !selection_supported {
                return fail_job!(
                    store,
                    job_id,
                    "invalid_source_selection",
                    "The saved selection does not match the source type",
                )
                .await;
            }
            let suffix = match normalized_mime.as_str() {
                "application/pdf" => "pdf",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
                "audio/wav" | "audio/x-wav" => "wav",
                "audio/mpeg" => "mp3",
                _ if normalized_mime.starts_with("image/") => "img",
                _ if normalized_mime.starts_with("audio/") => "audio",
                _ => "bin",
            };
            let selection = if selection_kind == "all" {
                "all".to_string()
            } else {
                format!("{selection_kind}:{selection_spec}")
            };
            sources.push(GenerationSource {
                source_id: source_id.clone(),
                filename: format!("source-{source_id}.{suffix}"),
                mime_type: mime_type.clone(),
                selection,
                selection_kind,
                selection_spec,
                bytes,
            });
        }
        source_ids.push(source_id);
    }
    let allowed_sources: HashSet<String> = source_ids.iter().cloned().collect();

    let mut existing_prompts = Vec::new();
    if let Some(qid) = &job.quiz_id {
        let rows = sqlx::query(
            "SELECT qv.couch_doc_id FROM quiz_version_questions qvq
             JOIN quiz_versions qver ON qver.id = qvq.quiz_version_id
             JOIN question_versions qv ON qv.id = qvq.question_version_id
             JOIN quizzes q ON q.id = qver.quiz_id AND q.active_version_id = qver.id
             WHERE qver.quiz_id = $1",
        )
        .bind(qid)
        .fetch_all(&store.db)
        .await?;
        for r in rows {
            let doc_id: String = r.get("couch_doc_id");
            if let Ok(doc) = store.docs.get(&doc_id).await
                && let Some(p) = doc["prompt"].as_str()
            {
                existing_prompts.push(p.to_string());
            }
        }
    }

    let req = GenerationRequest {
        owner_id: caller.user_id.clone(),
        language: job.language.clone(),
        count: job.requested_count,
        source_ids,
        sources,
        existing_prompts: existing_prompts.clone(),
    };
    let prep_ms = prep_inst.elapsed().as_millis() as i64;
    record_step!(
        store,
        job_id,
        &StepMeasurement {
            step_name: "prepare_sources".into(),
            status: "completed".into(),
            wait_ms,
            process_ms: prep_ms,
            started_at: prep_started_at,
            finished_at: now(),
        },
    )
    .await?;

    let cortex_inst = std::time::Instant::now();
    let cortex_started_at = now();
    let cortex_claim = execute_worker_state_query(
        store,
        worker_connection,
        sqlx::query(
            "UPDATE generation_jobs SET error_code = 'cortex_request_in_flight'
         WHERE id = $1 AND status = 'running' AND stopped_reason <> 'user_stop_requested'",
        )
        .bind(job_id),
    )
    .await?;
    if cortex_claim.rows_affected() == 0 {
        if stop_was_requested(store, job_id).await? {
            return finish_requested_stop!(store, caller, job_id).await;
        }
        return get_job(store, caller, job_id).await;
    }
    let result = match store.cortex.generate_questions(&req).await {
        Ok(result) => result,
        Err(err) => {
            let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
            let _ = record_step!(
                store,
                job_id,
                &StepMeasurement {
                    step_name: "cortex_generate".into(),
                    status: "failed".into(),
                    wait_ms: 0,
                    process_ms: cortex_ms,
                    started_at: cortex_started_at,
                    finished_at: now(),
                },
            )
            .await;
            let _ = err;
            return fail_job!(
                store,
                job_id,
                "cortex_outcome_unknown",
                "Cortex may have accepted this generation; automatic retry is disabled",
            )
            .await;
        }
    };
    execute_worker_state_query(store, worker_connection, sqlx::query(
        "UPDATE generation_jobs SET cortex_session_id = $1, cortex_output_id = $2, error_code = 'cortex_outcome_unknown' WHERE id = $3",
    )
    .bind(&result.session_id)
    .bind(&result.output_id)
    .bind(job_id)).await?;
    let current_status: String =
        sqlx::query_scalar("SELECT status FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;
    if current_status != "running" {
        return get_job(store, caller, job_id).await;
    }
    if result.status != "done" {
        let (code, message) = if result.status == "failed" {
            (
                "cortex_failed",
                "Cortex reported that the generation failed",
            )
        } else {
            (
                "cortex_outcome_unknown",
                "Cortex returned without a final result; automatic retry is disabled",
            )
        };
        return fail_job!(store, job_id, code, message).await;
    }
    let mut generated = result.questions;

    if has_wrong_script(&job.language, &generated) {
        if stop_was_requested(store, job_id).await? {
            return finish_requested_stop!(store, caller, job_id).await;
        }
        emit_event!(
            store,
            job_id,
            "language_correction",
            "Detected wrong script; requesting Cortex correction",
        )
        .await?;
        let correction_claim = execute_worker_state_query(
            store,
            worker_connection,
            sqlx::query(
                "UPDATE generation_jobs SET error_code = 'cortex_correction_in_flight'
             WHERE id = $1 AND status = 'running' AND stopped_reason <> 'user_stop_requested'",
            )
            .bind(job_id),
        )
        .await?;
        if correction_claim.rows_affected() == 0 {
            if stop_was_requested(store, job_id).await? {
                return finish_requested_stop!(store, caller, job_id).await;
            }
            return get_job(store, caller, job_id).await;
        }
        generated = match store
            .cortex
            .correct_language(&caller.user_id, &job.language, &generated)
            .await
        {
            Ok(questions) => questions,
            Err(_) => {
                return fail_job!(
                    store,
                    job_id,
                    "cortex_outcome_unknown",
                    "Cortex may have accepted language correction; automatic retry is disabled",
                )
                .await;
            }
        };
        let current_status: String =
            sqlx::query_scalar("SELECT status FROM generation_jobs WHERE id = $1")
                .bind(job_id)
                .fetch_one(&store.db)
                .await?;
        if current_status != "running" {
            return get_job(store, caller, job_id).await;
        }
        execute_worker_state_query(store, worker_connection, sqlx::query(
            "UPDATE generation_jobs SET error_code = 'cortex_outcome_unknown' WHERE id = $1 AND status = 'running'",
        )
        .bind(job_id)).await?;
        if has_wrong_script(&job.language, &generated) {
            let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
            let _ = record_step!(
                store,
                job_id,
                &StepMeasurement {
                    step_name: "cortex_generate".into(),
                    status: "failed".into(),
                    wait_ms: 0,
                    process_ms: cortex_ms,
                    started_at: cortex_started_at,
                    finished_at: now(),
                },
            )
            .await;
            return fail_job!(
                store,
                job_id,
                "wrong_language_script",
                "Generated questions contain unsupported script",
            )
            .await;
        }
    }
    let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
    record_step!(
        store,
        job_id,
        &StepMeasurement {
            step_name: "cortex_generate".into(),
            status: "completed".into(),
            wait_ms: 0,
            process_ms: cortex_ms,
            started_at: cortex_started_at,
            finished_at: now(),
        },
    )
    .await?;

    let save_inst = std::time::Instant::now();
    let save_started_at = now();

    let current_status: String =
        sqlx::query_scalar("SELECT status FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;
    if current_status != "running" {
        return get_job(store, caller, job_id).await;
    }

    // Verify citations and deduplicate against existing prompts.
    let mut seen_prompts: HashSet<String> = existing_prompts
        .into_iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .collect();
    let mut accepted = Vec::new();
    for q in generated {
        if !allowed_sources.contains(&q.source_id) {
            return fail_job!(
                store,
                job_id,
                "unverified_citation",
                "Citation references a source not in this job",
            )
            .await;
        }
        let key = q.prompt.trim().to_ascii_lowercase();
        if seen_prompts.insert(key) {
            accepted.push(q);
        }
    }

    let quiz_id = job
        .quiz_id
        .clone()
        .filter(|qid| !qid.trim().is_empty())
        .unwrap_or_else(new_id);
    let ts = now();
    let mut doc_ids: Vec<String> = Vec::with_capacity(accepted.len());
    let mut saved_questions = Vec::with_capacity(accepted.len());
    for q in &accepted {
        if worker_connection.is_some()
            && let Err(error) =
                execute_worker_state_query(store, worker_connection, sqlx::query("SELECT 1")).await
        {
            for created_doc_id in &doc_ids {
                guardrail::cleanup_orphan_document(store, created_doc_id).await;
            }
            return Err(error);
        }
        let doc_id = new_id();
        doc_ids.push(doc_id.clone());
        let payload = json!({
            "prompt": q.prompt,
            "alternatives": q.alternatives,
            "correct_id": q.correct_id,
            "explanation": q.explanation,
            "citation_ref": q.citation_ref,
            "visual_ids": [],
        });
        if let Err(error) = guardrail::store_validated_document(
            store,
            QUESTION_SCHEMA_NAME,
            QUESTION_SCHEMA_VERSION,
            &doc_id,
            &payload,
        )
        .await
        {
            for created_doc_id in &doc_ids {
                guardrail::cleanup_orphan_document(store, created_doc_id).await;
            }
            return Err(error);
        }
        saved_questions.push((
            new_id(),
            new_id(),
            doc_id,
            q.source_id.clone(),
            q.citation_ref.clone(),
        ));
    }
    let saved_count = saved_questions.len() as i64;
    let done = now();
    let finalize_result: Result<bool> = async {
        let mut locked_connection = if let Some(worker_connection) = worker_connection {
            Some(worker_connection.lock().await)
        } else {
            None
        };
        let mut tx = if let Some(connection) = locked_connection.as_deref_mut() {
            connection.begin().await?
        } else {
            store.db.begin().await?
        };
        // This compare-and-set serializes final publication with stop/cancel.
        let claim = sqlx::query(
            "UPDATE generation_jobs SET error_code = 'publishing' WHERE id = $1 AND status = 'running'",
        )
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        if claim.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }

        // Follow the same study-then-quiz lock order as visual and author edits.
        let study_lock_query = if store.postgres() {
            "SELECT id FROM studies WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT id FROM studies WHERE id = $1"
        };
        if sqlx::query(study_lock_query)
            .bind(&job.study_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_none()
        {
            return Err(Error::missing());
        }

        if job.quiz_id.as_deref().is_some_and(|qid| !qid.trim().is_empty()) {
            let target_query = if store.postgres() {
                "SELECT id FROM quizzes WHERE id = $1 AND owner_id = $2 AND study_id = $3 AND visibility = 'private' FOR UPDATE"
            } else {
                "SELECT id FROM quizzes WHERE id = $1 AND owner_id = $2 AND study_id = $3 AND visibility = 'private'"
            };
            let target_exists = sqlx::query(target_query)
            .bind(&quiz_id)
            .bind(&caller.user_id)
            .bind(&job.study_id)
            .fetch_optional(&mut *tx)
            .await?;
            if target_exists.is_none() {
                return Err(Error(
                    StatusCode::NOT_FOUND,
                    "quiz_not_available: Target quiz is not an owned private quiz in this study".into(),
                ));
            }
        } else {
            sqlx::query(
                "INSERT INTO quizzes(id, owner_id, study_id, visibility, origin, active_version_id, created_at, updated_at)
                 VALUES($1, $2, $3, 'private', 'generated_with_cortex', '', $4, $4)",
            )
            .bind(&quiz_id)
            .bind(&caller.user_id)
            .bind(&job.study_id)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
        }

        let next_ver: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version_num), 0) + 1 FROM quiz_versions WHERE quiz_id = $1",
        )
        .bind(&quiz_id)
        .fetch_one(&mut *tx)
        .await?;
        let quiz_version_id = new_id();
        sqlx::query(
            "INSERT INTO quiz_versions(id, quiz_id, version_num, title, created_at) VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&quiz_version_id)
        .bind(&quiz_id)
        .bind(next_ver)
        .bind(format!("Generated Quiz v{next_ver}"))
        .bind(ts)
        .execute(&mut *tx)
        .await?;

        // Extra rounds copy the exact current membership before adding new questions.
        sqlx::query(
            "INSERT INTO quiz_version_questions
                 (quiz_version_id, position, question_version_id, source_scope, origin_quiz_id, origin_quiz_version_id)
             SELECT $1, qvq.position, qvq.question_version_id, qvq.source_scope, qvq.origin_quiz_id, qvq.origin_quiz_version_id
             FROM quizzes q JOIN quiz_version_questions qvq ON qvq.quiz_version_id = q.active_version_id
             WHERE q.id = $2 AND q.active_version_id <> ''",
        )
        .bind(&quiz_version_id)
        .bind(&quiz_id)
        .execute(&mut *tx)
        .await?;
        let mut next_position: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM quiz_version_questions WHERE quiz_version_id = $1",
        )
        .bind(&quiz_version_id)
        .fetch_one(&mut *tx)
        .await?;

        for (question_id, qv_id, doc_id, source_id, citation_ref) in &saved_questions {
            sqlx::query("INSERT INTO questions(id, owner_id, origin, created_at) VALUES($1, $2, 'generated_with_cortex', $3)")
                .bind(question_id)
                .bind(&caller.user_id)
                .bind(ts)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
                 VALUES($1, $2, 1, $3, $4, $5, $6, $7)",
            )
            .bind(qv_id)
            .bind(question_id)
            .bind(&job.language)
            .bind(QUESTION_SCHEMA_NAME)
            .bind(QUESTION_SCHEMA_VERSION)
            .bind(doc_id)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at)
                 VALUES($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(qv_id)
            .bind(source_id)
            .bind(citation_ref)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id, source_scope)
                 VALUES($1, $2, $3, 'generated_from_current_selected_sources')",
            )
            .bind(&quiz_version_id)
            .bind(next_position)
            .bind(qv_id)
            .execute(&mut *tx)
            .await?;
            next_position += 1;
        }

        let activated = sqlx::query(
            "UPDATE quizzes SET active_version_id = $1, updated_at = $2 WHERE id = $3 AND owner_id = $4 AND study_id = $5 AND visibility = 'private'",
        )
        .bind(&quiz_version_id)
        .bind(done)
        .bind(&quiz_id)
        .bind(&caller.user_id)
        .bind(&job.study_id)
        .execute(&mut *tx)
        .await?;
        if activated.rows_affected() != 1 {
            return Err(Error(
                StatusCode::NOT_FOUND,
                "quiz_not_available: Target quiz is not an owned private quiz in this study".into(),
            ));
        }
        catalog::refresh_public_question_search_in_tx(store, &mut tx, &quiz_id).await?;

        let study_updated = sqlx::query(
            "UPDATE studies SET status = 'active', quiz_id = $1, updated_at = $2
             WHERE id = $3 AND owner_id = $4 AND status IN ('draft', 'active')
               AND (status <> 'draft' OR expires_at IS NULL OR expires_at >= $2)",
        )
        .bind(&quiz_id)
        .bind(done)
        .bind(&job.study_id)
        .bind(&caller.user_id)
        .execute(&mut *tx)
        .await?;
        if study_updated.rows_affected() != 1 {
            return Err(Error(
                StatusCode::GONE,
                "study_expired: Study is no longer available for generation".into(),
            ));
        }

        let stop_requested: i64 = sqlx::query_scalar(
            "SELECT CASE WHEN stopped_reason = 'user_stop_requested' THEN 1 ELSE 0 END FROM generation_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_one(&mut *tx)
        .await?;
        let final_status = if stop_requested == 1 {
            "cancelled"
        } else {
            "completed"
        };
        let completed = sqlx::query(
            "UPDATE generation_jobs SET status = $1, quiz_id = $2, saved_count = $3, error_code = '', finished_at = $4
             WHERE id = $5 AND status = 'running'",
        )
        .bind(final_status)
        .bind(&quiz_id)
        .bind(saved_count)
        .bind(done)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        if completed.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO generation_events(job_id, event_type, message, created_at) VALUES($1, $2, $3, $4)",
        )
        .bind(job_id)
        .bind(final_status)
        .bind(format!("Saved {saved_count} new questions to private quiz {quiz_id}"))
        .bind(done)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
    .await;

    match finalize_result {
        Ok(true) => {}
        Ok(false) => {
            for doc_id in &doc_ids {
                guardrail::cleanup_orphan_document(store, doc_id).await;
            }
            return get_job(store, caller, job_id).await;
        }
        Err(error) => {
            for doc_id in &doc_ids {
                guardrail::cleanup_orphan_document(store, doc_id).await;
            }
            return Err(error);
        }
    }

    let save_ms = save_inst.elapsed().as_millis() as i64;
    record_step!(
        store,
        job_id,
        &StepMeasurement {
            step_name: "save_quiz".into(),
            status: "completed".into(),
            wait_ms: 0,
            process_ms: save_ms,
            started_at: save_started_at,
            finished_at: done,
        },
    )
    .await?;

    let counts_row = sqlx::query(
        "SELECT COUNT(*) AS cnt, COALESCE(SUM(s.size_bytes), 0) AS bytes
         FROM sources s
         JOIN generation_job_sources gjs ON gjs.source_id = s.id
         WHERE gjs.job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&store.db)
    .await?;
    let source_count: i64 = counts_row.get("cnt");
    let total_bytes: i64 = counts_row.get("bytes");
    let total_process_ms = prep_ms + cortex_ms + save_ms;
    if worker_connection.is_some() {
        execute_worker_state_query(store, worker_connection, sqlx::query("SELECT 1")).await?;
    }
    estimates::record_estimation_sample(
        store,
        &caller.user_id,
        "generation",
        &job.language,
        source_count,
        total_bytes,
        saved_count.max(job.requested_count),
        wait_ms,
        total_process_ms,
        estimated_before,
    )
    .await?;

    get_job(store, caller, job_id).await
}

/// Drain the oldest persisted generation at a time. The conditional status
/// transition in `process_job` is the durable claim, so two pollers cannot
/// submit the same queued job to Cortex.
async fn process_next_queued(
    store: &Store,
    worker_connection: Option<WorkerConnection>,
) -> Result<bool> {
    let row = sqlx::query(
        "SELECT id, owner_id FROM generation_jobs WHERE status = 'queued' ORDER BY queued_at ASC, id ASC LIMIT 1",
    )
    .fetch_optional(&store.db)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let caller = CallerIdentity {
        user_id: row.get("owner_id"),
        access_level: 0,
        display_name: None,
    };
    let job_id: String = row.get("id");
    if let Err(error) =
        process_job_with_worker_connection(store, &caller, &job_id, worker_connection.as_ref())
            .await
    {
        tracing::warn!(job_id = %job_id, error = %error, "Generation queue item stopped");
        let terminal = execute_worker_state_query(
            store,
            worker_connection.as_ref(),
            sqlx::query(
            "UPDATE generation_jobs SET status = 'failed',
                 error_code = CASE WHEN error_code IN ('cortex_request_in_flight', 'cortex_correction_in_flight', 'cortex_outcome_unknown')
                                   THEN 'cortex_outcome_unknown' ELSE 'generation_processing_failed' END,
                 finished_at = $1
             WHERE id = $2 AND status = 'running'",
            )
            .bind(now())
            .bind(&job_id),
        )
        .await?;
        if terminal.rows_affected() > 0 {
            let _ = emit_event_with_worker_connection(
                store,
                &job_id,
                "failed",
                "Generation stopped after an internal processing error",
                worker_connection.as_ref(),
            )
            .await;
        }
    }
    Ok(true)
}

async fn acquire_postgres_worker_lock(store: &Store) -> Result<Option<WorkerConnection>> {
    let mut connection = store.db.acquire().await?;
    // A session advisory lock must not be returned to the shared pool when this
    // future is cancelled. Closing the physical connection releases the lock.
    connection.close_on_drop();
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
        .bind(GENERATION_WORKER_LOCK_CLASS)
        .bind(GENERATION_WORKER_LOCK_ID)
        .fetch_one(&mut *connection)
        .await?;
    Ok(acquired.then(|| std::sync::Arc::new(tokio::sync::Mutex::new(connection))))
}

async fn reconcile_interrupted_jobs_on_connection(
    connection: &mut sqlx::AnyConnection,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT id, error_code, stopped_reason, started_at FROM generation_jobs
         WHERE status = 'running' ORDER BY started_at ASC, id ASC",
    )
    .fetch_all(&mut *connection)
    .await?;

    for row in rows {
        let job_id: String = row.get("id");
        let old_error: String = row.get("error_code");
        let old_stop: String = row.get("stopped_reason");
        let (new_status, new_error, event_type, message, started_at, finished_at) =
            if old_error.is_empty() && old_stop == "user_stop_requested" {
                (
                    "cancelled",
                    "",
                    "cancelled",
                    "Generation stopped before contacting Cortex after worker recovery",
                    None,
                    Some(now()),
                )
            } else if old_error.is_empty() {
                (
                    "queued",
                    "",
                    "queued",
                    "Generation requeued after recovery before contacting Cortex",
                    None,
                    None,
                )
            } else {
                (
                    "failed",
                    "cortex_outcome_unknown",
                    "failed",
                    "Worker stopped during or after a Cortex request; automatic retry is disabled",
                    row.try_get::<Option<i64>, _>("started_at")?,
                    Some(now()),
                )
            };

        let mut tx = connection.begin().await?;
        let updated = sqlx::query(
            "UPDATE generation_jobs
             SET status = $1, error_code = $2, started_at = $3, finished_at = $4
             WHERE id = $5 AND status = 'running' AND error_code = $6 AND stopped_reason = $7",
        )
        .bind(new_status)
        .bind(new_error)
        .bind(started_at)
        .bind(finished_at)
        .bind(&job_id)
        .bind(&old_error)
        .bind(&old_stop)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() > 0 {
            sqlx::query(
                "INSERT INTO generation_events(job_id, event_type, message, created_at)
                 VALUES($1, $2, $3, $4)",
            )
            .bind(&job_id)
            .bind(event_type)
            .bind(message)
            .bind(now())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

async fn reconcile_interrupted_jobs(
    store: &Store,
    worker_connection: Option<&WorkerConnection>,
) -> Result<()> {
    if let Some(worker_connection) = worker_connection {
        let mut connection = worker_connection.lock().await;
        reconcile_interrupted_jobs_on_connection(&mut connection).await
    } else {
        let mut connection = store.db.acquire().await?;
        reconcile_interrupted_jobs_on_connection(&mut connection).await
    }
}

async fn run_queue_loop(store: &Store, worker_connection: Option<WorkerConnection>) {
    loop {
        if worker_connection.is_some()
            && let Err(error) = execute_worker_state_query(
                store,
                worker_connection.as_ref(),
                sqlx::query("SELECT 1"),
            )
            .await
        {
            tracing::error!(error = %error, "Generation worker lock connection was lost");
            return;
        }
        match process_next_queued(store, worker_connection.clone()).await {
            Ok(true) => continue,
            Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Err(error) => {
                tracing::error!(error = %error, "Generation queue poll failed");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn run_queue_worker(store: Store) {
    if store.database_backend == DatabaseBackend::Sqlite {
        // SQLite is used by single-process fixtures; this lock makes no
        // cross-process coordination claim.
        let _local_guard = store.sqlite_worker_lock.lock().await;
        if let Err(error) = reconcile_interrupted_jobs(&store, None).await {
            tracing::error!(error = %error, "Generation recovery failed; worker is stopping");
            return;
        }
        run_queue_loop(&store, None).await;
        return;
    }

    loop {
        match acquire_postgres_worker_lock(&store).await {
            Ok(Some(connection)) => {
                if let Err(error) = reconcile_interrupted_jobs(&store, Some(&connection)).await {
                    tracing::error!(error = %error, "Generation recovery failed; worker will retry after reacquiring its lock");
                    drop(connection);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                run_queue_loop(&store, Some(connection.clone())).await;
                drop(connection);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Err(error) => {
                tracing::error!(error = %error, "Generation worker lock acquisition failed");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn stop_generation(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    let job = get_job(store, caller, job_id).await?;
    if job.status == "queued" {
        let stopped = sqlx::query(
            "UPDATE generation_jobs SET status = 'cancelled', stopped_reason = 'user_stopped', finished_at = $1 WHERE id = $2 AND status = 'queued'",
        )
        .bind(now())
        .bind(job_id)
        .execute(&store.db)
        .await?;
        if stopped.rows_affected() > 0 {
            emit_event(
                store,
                job_id,
                "cancelled",
                "Stopped before generation started",
            )
            .await?;
        }
    } else if job.status == "running" {
        let requested = sqlx::query(
            "UPDATE generation_jobs SET stopped_reason = 'user_stop_requested'
             WHERE id = $1 AND status = 'running' AND stopped_reason <> 'user_stop_requested'",
        )
        .bind(job_id)
        .execute(&store.db)
        .await?;
        if requested.rows_affected() > 0 {
            emit_event(
                store,
                job_id,
                "stop_requested",
                "Stop requested; the current step will finish safely",
            )
            .await?;
        }
    }
    get_job(store, caller, job_id).await
}

pub async fn resume_generation(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    let prev = get_job(store, caller, job_id).await?;
    if !matches!(prev.status.as_str(), "cancelled" | "failed") {
        return Err(Error::conflict(
            "cannot_resume: Only stopped or failed generation jobs can be resumed",
        ));
    }
    if prev.error_code == "cortex_outcome_unknown" {
        return Err(Error::conflict(
            "cannot_resume_unknown_cortex_outcome: Cortex may already have accepted this request",
        ));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE owner_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(&caller.user_id)
    .fetch_one(&store.db)
    .await?;
    if active > 0 {
        return Err(Error(
            StatusCode::CONFLICT,
            "generation_already_active: Only one queued or running generation per user is allowed"
                .into(),
        ));
    }
    let ahead: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE status IN ('queued', 'running')",
    )
    .fetch_one(&store.db)
    .await?;
    let queue_position = ahead + 1;
    let new_job_id = new_id();
    let ts = now();
    let mut tx = store.db.begin().await?;
    let insert = sqlx::query(
        "INSERT INTO generation_jobs(id, owner_id, study_id, quiz_id, status, language, requested_count, idempotency_key, queue_position, saved_count, queued_at)
         VALUES($1, $2, $3, $4, 'queued', $5, $6, '', $7, $8, $9)",
    )
    .bind(&new_job_id)
    .bind(&caller.user_id)
    .bind(&prev.study_id)
    .bind(&prev.quiz_id)
    .bind(&prev.language)
    .bind(prev.requested_count)
    .bind(queue_position)
    .bind(prev.saved_count)
    .bind(ts)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        if is_unique_violation(&error) {
            tx.rollback().await?;
            return Err(active_generation_conflict());
        }
        return Err(error.into());
    }
    sqlx::query(
        "INSERT INTO generation_job_sources
         (job_id, source_id, filename, mime_type, size_bytes, sha256, source_kind, selection_kind, selection_spec,
          external_ref, report_calculation_id, report_calculation_version, report_row_count, report_lineage_sha256)
         SELECT $1, source_id, filename, mime_type, size_bytes, sha256, source_kind, selection_kind, selection_spec,
                external_ref, report_calculation_id, report_calculation_version, report_row_count, report_lineage_sha256
         FROM generation_job_sources WHERE job_id = $2",
    )
    .bind(&new_job_id)
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    emit_event(
        store,
        &new_job_id,
        "queued",
        &format!("Requeued at position {queue_position}"),
    )
    .await?;
    get_job(store, caller, &new_job_id).await
}
