use crate::{
    core::{Error, Result, now},
    dal::{
        Store,
        cortex::{CortexJobOutcome, GeneratedQuestion, GenerationRequest},
        new_id,
    },
    domain::{
        auth::CallerIdentity,
        auth::require_owner,
        catalog,
        estimates::{self, StepMeasurement},
        guardrail,
    },
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::collections::HashSet;

pub const QUESTION_SCHEMA_NAME: &str = "question-v2";
pub const QUESTION_SCHEMA_VERSION: i64 = 1;

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
        "SELECT id, owner_id, study_id, quiz_id, status, language, requested_count, queue_position, saved_count, error_code, queued_at, started_at, finished_at
         FROM generation_jobs WHERE id = $1",
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
        error_code: row.get("error_code"),
        queued_at: row.get("queued_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    })
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

    // Verify owned study and active sources.
    let study_row = sqlx::query("SELECT owner_id FROM studies WHERE id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let study_owner: String = study_row.get("owner_id");
    require_owner(caller, &study_owner)?;

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
        return Err(Error(
            StatusCode::CONFLICT,
            "generation_already_active: Only one queued or running generation per user is allowed"
                .into(),
        ));
    }

    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM sources WHERE study_id = $1 AND status = 'ready' ORDER BY created_at ASC",
    )
    .bind(study_id)
    .fetch_all(&store.db)
    .await?;
    if source_ids.is_empty() {
        return Err(Error::unprocessable(
            "no_ready_sources: Study has no ready sources for generation",
        ));
    }

    let ahead: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE status IN ('queued', 'running')",
    )
    .fetch_one(&store.db)
    .await?;
    let queue_position = ahead + 1;

    let job_id = new_id();
    let ts = now();
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "INSERT INTO generation_jobs(id, owner_id, study_id, quiz_id, status, language, requested_count, idempotency_key, queue_position, saved_count, queued_at)
         VALUES($1, $2, $3, $4, 'queued', $5, $6, $7, $8, 0, $9)",
    )
    .bind(&job_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(&input.quiz_id)
    .bind(&input.language)
    .bind(input.count)
    .bind(idem)
    .bind(queue_position)
    .bind(ts)
    .execute(&mut *tx)
    .await?;

    for sid in &source_ids {
        sqlx::query("INSERT INTO generation_job_sources(job_id, source_id) VALUES($1, $2)")
            .bind(&job_id)
            .bind(sid)
            .execute(&mut *tx)
            .await?;
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

pub async fn process_job(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    let job = get_job(store, caller, job_id).await?;
    if job.status == "cancelled" || job.status == "completed" {
        return Ok(job);
    }

    // Check existing Cortex outcome before issuing a duplicate external request.
    let existing_cortex_id: String =
        sqlx::query_scalar("SELECT cortex_job_id FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;
    if !existing_cortex_id.is_empty()
        && let Ok(CortexJobOutcome::Pending) = store
            .cortex
            .check_request_outcome(&existing_cortex_id)
            .await
    {
        return Ok(job);
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
    sqlx::query("UPDATE generation_jobs SET status = 'running', started_at = $1 WHERE id = $2")
        .bind(started)
        .bind(job_id)
        .execute(&store.db)
        .await?;
    emit_event(store, job_id, "running", "Calling Cortex T0 Easy").await?;

    let prep_inst = std::time::Instant::now();
    let prep_started_at = now();
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_id FROM generation_job_sources WHERE job_id = $1 ORDER BY source_id",
    )
    .bind(job_id)
    .fetch_all(&store.db)
    .await?;
    let allowed_sources: HashSet<String> = source_ids.iter().cloned().collect();

    let mut existing_prompts = Vec::new();
    if let Some(qid) = &job.quiz_id {
        let rows = sqlx::query(
            "SELECT qv.couch_doc_id FROM quiz_version_questions qvq
             JOIN quiz_versions qver ON qver.id = qvq.quiz_version_id
             JOIN question_versions qv ON qv.id = qvq.question_version_id
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
        job_id: job_id.to_string(),
        language: job.language.clone(),
        count: job.requested_count,
        source_ids,
        existing_prompts: existing_prompts.clone(),
    };
    let prep_ms = prep_inst.elapsed().as_millis() as i64;
    estimates::record_step(
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
    let mut generated = match store.cortex.generate_questions(&req).await {
        Ok(q) => q,
        Err(err) => {
            let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
            let _ = estimates::record_step(
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
            sqlx::query(
                "UPDATE generation_jobs SET status = 'failed', error_code = 'cortex_failed', finished_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(job_id)
            .execute(&store.db)
            .await?;
            emit_event(store, job_id, "failed", "Cortex generation failed").await?;
            return Err(err);
        }
    };

    if has_wrong_script(&job.language, &generated) {
        emit_event(
            store,
            job_id,
            "language_correction",
            "Detected wrong script; requesting Cortex correction",
        )
        .await?;
        generated = store
            .cortex
            .correct_language(&caller.user_id, &job.language, &generated)
            .await?;
        if has_wrong_script(&job.language, &generated) {
            let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
            let _ = estimates::record_step(
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
            sqlx::query(
                "UPDATE generation_jobs SET status = 'failed', error_code = 'wrong_language_script', finished_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(job_id)
            .execute(&store.db)
            .await?;
            return Err(Error::unprocessable(
                "wrong_language_script: Generated questions contain unsupported script",
            ));
        }
    }
    let cortex_ms = cortex_inst.elapsed().as_millis() as i64;
    estimates::record_step(
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

    // Re-check cancellation before committing results (safe boundary).
    let current_status: String =
        sqlx::query_scalar("SELECT status FROM generation_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&store.db)
            .await?;
    if current_status == "cancelled" {
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
            sqlx::query(
                "UPDATE generation_jobs SET status = 'failed', error_code = 'unverified_citation', finished_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(job_id)
            .execute(&store.db)
            .await?;
            return Err(Error::unprocessable(
                "unverified_citation: Citation references a source not in this job",
            ));
        }
        let key = q.prompt.trim().to_ascii_lowercase();
        if seen_prompts.insert(key) {
            accepted.push(q);
        }
    }

    let ts = now();
    let quiz_id = match job.quiz_id {
        Some(ref qid) if !qid.is_empty() => qid.clone(),
        _ => {
            let qid = new_id();
            sqlx::query(
                "INSERT INTO quizzes(id, owner_id, study_id, visibility, origin, active_version_id, created_at, updated_at)
                 VALUES($1, $2, $3, 'private', 'generated_with_cortex', '', $4, $4)",
            )
            .bind(&qid)
            .bind(&caller.user_id)
            .bind(&job.study_id)
            .bind(ts)
            .execute(&store.db)
            .await?;
            qid
        }
    };

    let next_ver: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_num), 0) + 1 FROM quiz_versions WHERE quiz_id = $1",
    )
    .bind(&quiz_id)
    .fetch_one(&store.db)
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
    .execute(&store.db)
    .await?;

    let mut saved_count = 0i64;
    for (pos, q) in accepted.iter().enumerate() {
        let doc_id = new_id();
        let payload = json!({
            "prompt": q.prompt,
            "alternatives": q.alternatives,
            "correct_id": q.correct_id,
            "explanation": q.explanation,
            "citation_ref": q.citation_ref,
        });
        guardrail::store_validated_document(
            store,
            QUESTION_SCHEMA_NAME,
            QUESTION_SCHEMA_VERSION,
            &doc_id,
            &payload,
        )
        .await?;

        let question_id = new_id();
        let qv_id = new_id();
        if let Err(err) = async {
            let mut tx = store.db.begin().await?;
            sqlx::query("INSERT INTO questions(id, owner_id, origin, created_at) VALUES($1, $2, 'generated_with_cortex', $3)")
                .bind(&question_id)
                .bind(&caller.user_id)
                .bind(ts)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
                 VALUES($1, $2, 1, $3, $4, $5, $6, $7)",
            )
            .bind(&qv_id)
            .bind(&question_id)
            .bind(&job.language)
            .bind(QUESTION_SCHEMA_NAME)
            .bind(QUESTION_SCHEMA_VERSION)
            .bind(&doc_id)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at)
                 VALUES($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(&qv_id)
            .bind(&q.source_id)
            .bind(&q.citation_ref)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id, source_scope)
                 VALUES($1, $2, $3, 'generated_from_current_selected_sources')",
            )
            .bind(&quiz_version_id)
            .bind(pos as i64)
            .bind(&qv_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok::<_, Error>(())
        }
        .await
        {
            guardrail::cleanup_orphan_document(store, &doc_id).await;
            return Err(err);
        }
        saved_count += 1;
    }

    let done = now();
    let mut publication_tx = store.db.begin().await?;
    sqlx::query("UPDATE quizzes SET active_version_id = $1, updated_at = $2 WHERE id = $3")
        .bind(&quiz_version_id)
        .bind(done)
        .bind(&quiz_id)
        .execute(&mut *publication_tx)
        .await?;
    catalog::refresh_public_question_search_in_tx(store, &mut publication_tx, &quiz_id).await?;
    publication_tx.commit().await?;
    sqlx::query(
        "UPDATE studies SET status = 'active', quiz_id = $1, updated_at = $2 WHERE id = $3",
    )
    .bind(&quiz_id)
    .bind(done)
    .bind(&job.study_id)
    .execute(&store.db)
    .await?;
    sqlx::query(
        "UPDATE generation_jobs SET status = 'completed', quiz_id = $1, saved_count = $2, finished_at = $3 WHERE id = $4",
    )
    .bind(&quiz_id)
    .bind(saved_count)
    .bind(done)
    .bind(job_id)
    .execute(&store.db)
    .await?;
    emit_event(
        store,
        job_id,
        "completed",
        &format!("Saved {saved_count} questions to private quiz {quiz_id}"),
    )
    .await?;

    let save_ms = save_inst.elapsed().as_millis() as i64;
    estimates::record_step(
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

pub async fn stop_generation(
    store: &Store,
    caller: &CallerIdentity,
    job_id: &str,
) -> Result<GenerationJobSummary> {
    let job = get_job(store, caller, job_id).await?;
    if matches!(job.status.as_str(), "queued" | "running") {
        sqlx::query(
            "UPDATE generation_jobs SET status = 'cancelled', stopped_reason = 'user_stopped', finished_at = $1 WHERE id = $2",
        )
        .bind(now())
        .bind(job_id)
        .execute(&store.db)
        .await?;
        emit_event(store, job_id, "cancelled", "Stopped at safe boundary").await?;
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
    sqlx::query(
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
    .await?;
    sqlx::query(
        "INSERT INTO generation_job_sources(job_id, source_id) SELECT $1, source_id FROM generation_job_sources WHERE job_id = $2",
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
