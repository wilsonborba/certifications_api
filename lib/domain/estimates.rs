use crate::{
    core::{Result, now},
    dal::{
        Store, new_id,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        auth::{CallerIdentity, require_owner},
        guardrail,
    },
};
use serde::Serialize;
use serde_json::json;
use sqlx::Row;

pub const ESTIMATION_SCHEMA_NAME: &str = "estimation-stats-v1";
pub const ESTIMATION_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct EstimateResponse {
    pub status: String,
    pub estimated_processing_ms: Option<i64>,
    pub sample_count: i64,
    pub queue_ahead_count: i64,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct StepMeasurement {
    pub step_name: String,
    pub status: String,
    pub wait_ms: i64,
    pub process_ms: i64,
    pub started_at: i64,
    pub finished_at: i64,
}

pub async fn ensure_estimation_schema(store: &Store) -> Result<()> {
    if schemas::get_published(&store.db, ESTIMATION_SCHEMA_NAME, ESTIMATION_SCHEMA_VERSION)
        .await
        .is_ok()
    {
        return Ok(());
    }
    let draft = DraftSchema {
        schema_name: ESTIMATION_SCHEMA_NAME.into(),
        version: ESTIMATION_SCHEMA_VERSION,
        description: "Permanent non-content generation estimation metrics".into(),
        fields: vec![
            SchemaField {
                field_path: "operation_kind".into(),
                field_type: "string".into(),
                is_required: true,
                description: "Operation kind".into(),
            },
            SchemaField {
                field_path: "language".into(),
                field_type: "string".into(),
                is_required: true,
                description: "Target language".into(),
            },
            SchemaField {
                field_path: "source_count".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Source count".into(),
            },
            SchemaField {
                field_path: "total_bytes".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Total source bytes".into(),
            },
            SchemaField {
                field_path: "question_count".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Question count".into(),
            },
            SchemaField {
                field_path: "wait_ms".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Queue/wait milliseconds".into(),
            },
            SchemaField {
                field_path: "process_ms".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Processing milliseconds".into(),
            },
            SchemaField {
                field_path: "estimated_ms".into(),
                field_type: "integer".into(),
                is_required: false,
                description: "Estimated processing ms".into(),
            },
            SchemaField {
                field_path: "error_ms".into(),
                field_type: "integer".into(),
                is_required: false,
                description: "Absolute calibration error ms".into(),
            },
        ],
    };
    schemas::save_draft(&store.db, &draft).await?;
    schemas::publish(&store.db, ESTIMATION_SCHEMA_NAME, ESTIMATION_SCHEMA_VERSION).await?;
    Ok(())
}

pub async fn record_step(store: &Store, job_id: &str, step: &StepMeasurement) -> Result<()> {
    sqlx::query(
        "INSERT INTO generation_steps(id, job_id, step_name, status, wait_ms, process_ms, started_at, finished_at)
         VALUES($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(new_id())
    .bind(job_id)
    .bind(&step.step_name)
    .bind(&step.status)
    .bind(step.wait_ms.max(0))
    .bind(step.process_ms.max(0))
    .bind(step.started_at)
    .bind(step.finished_at)
    .execute(&store.db)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn record_estimation_sample(
    store: &Store,
    owner_id: &str,
    operation_kind: &str,
    language: &str,
    source_count: i64,
    total_bytes: i64,
    question_count: i64,
    wait_ms: i64,
    process_ms: i64,
    estimated_ms: Option<i64>,
) -> Result<()> {
    ensure_estimation_schema(store).await?;
    let doc_id = new_id();
    let mut payload = json!({
        "operation_kind": operation_kind,
        "language": language,
        "source_count": source_count,
        "total_bytes": total_bytes,
        "question_count": question_count,
        "wait_ms": wait_ms.max(0),
        "process_ms": process_ms.max(0),
    });
    if let Some(est) = estimated_ms {
        let obj = payload.as_object_mut().unwrap();
        obj.insert("estimated_ms".into(), json!(est));
        obj.insert("error_ms".into(), json!((est - process_ms).abs()));
    }
    guardrail::store_validated_document(
        store,
        ESTIMATION_SCHEMA_NAME,
        ESTIMATION_SCHEMA_VERSION,
        &doc_id,
        &payload,
    )
    .await?;

    if let Err(err) = sqlx::query(
        "INSERT INTO estimation_statistics(id, owner_id, operation_kind, schema_name, schema_version, couch_doc_id, created_at)
         VALUES($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new_id())
    .bind(owner_id)
    .bind(operation_kind)
    .bind(ESTIMATION_SCHEMA_NAME)
    .bind(ESTIMATION_SCHEMA_VERSION)
    .bind(&doc_id)
    .bind(now())
    .execute(&store.db)
    .await
    {
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(err.into());
    }
    Ok(())
}

pub async fn estimate_for_study(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    language: &str,
    question_count: i64,
) -> Result<EstimateResponse> {
    let row = sqlx::query("SELECT owner_id FROM studies WHERE id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(crate::core::Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;

    let queue_ahead_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE status IN ('queued', 'running')",
    )
    .fetch_one(&store.db)
    .await?;

    let rows = sqlx::query(
        "SELECT couch_doc_id FROM estimation_statistics WHERE operation_kind = 'generation' ORDER BY created_at DESC LIMIT 20",
    )
    .fetch_all(&store.db)
    .await?;

    let mut samples = Vec::new();
    for r in rows {
        let doc_id: String = r.get("couch_doc_id");
        if let Ok(doc) = store.docs.get(&doc_id).await
            && doc["language"].as_str() == Some(language)
            && let Some(proc_ms) = doc["process_ms"].as_i64()
        {
            let q_cnt = doc["question_count"]
                .as_i64()
                .unwrap_or(question_count)
                .max(1);
            let per_q = proc_ms / q_cnt;
            samples.push(per_q * question_count.max(1));
        }
    }

    if !samples.is_empty() {
        let sum: i64 = samples.iter().sum();
        let avg = (sum / samples.len() as i64).max(1);
        return Ok(EstimateResponse {
            status: "available".into(),
            estimated_processing_ms: Some(avg),
            sample_count: samples.len() as i64,
            queue_ahead_count,
            source: "comparable_runs".into(),
        });
    }

    if let Ok(timings) = store.cortex.fetch_timing_averages().await
        && let Some(t) = timings
            .iter()
            .find(|t| t.kind == "generation" || t.kind == "chat")
        && t.expected_ms > 0
    {
        return Ok(EstimateResponse {
            status: "available".into(),
            estimated_processing_ms: Some(t.expected_ms),
            sample_count: t.sample_count.max(1),
            queue_ahead_count,
            source: "cortex_timing".into(),
        });
    }

    Ok(EstimateResponse {
        status: "unavailable".into(),
        estimated_processing_ms: None,
        sample_count: 0,
        queue_ahead_count,
        source: "none".into(),
    })
}
