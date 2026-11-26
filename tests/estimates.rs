use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::{Result as CoreResult, Settings},
    dal::{
        Store,
        cortex::{
            BoxFuture, CortexClient, GeneratedAlternative, GeneratedQuestion, GenerationRequest,
            GenerationResult, TimingAverage,
        },
        fsm::MemoryFsm,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        App,
        auth::CallerIdentity,
        generations::{self, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION},
        quotas::QuotaCache,
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt;

struct MockCortex {
    return_timing: AtomicBool,
}

impl MockCortex {
    fn new(return_timing: bool) -> Arc<Self> {
        Arc::new(Self {
            return_timing: AtomicBool::new(return_timing),
        })
    }
}

impl CortexClient for MockCortex {
    fn is_configured(&self) -> bool {
        true
    }

    fn generate_questions<'a>(
        &'a self,
        req: &'a GenerationRequest,
    ) -> BoxFuture<'a, CoreResult<GenerationResult>> {
        Box::pin(async move {
            let sid = req
                .source_ids
                .first()
                .cloned()
                .unwrap_or_else(|| "missing".into());
            Ok(GenerationResult {
                questions: (0..req.count)
                    .map(|idx| GeneratedQuestion {
                        prompt: format!("Estimate test question {idx}?"),
                        alternatives: vec![
                            GeneratedAlternative {
                                id: "a".into(),
                                text: "Answer A".into(),
                            },
                            GeneratedAlternative {
                                id: "b".into(),
                                text: "Answer B".into(),
                            },
                        ],
                        correct_id: "a".into(),
                        explanation: "Verified explanation".into(),
                        source_id: sid.clone(),
                        citation_ref: format!("p. {}", idx + 1),
                    })
                    .collect(),
                session_id: "temporary-session".into(),
                output_id: "temporary-output".into(),
                status: "done".into(),
            })
        })
    }

    fn correct_language<'a>(
        &'a self,
        _owner_id: &'a str,
        _target_language: &'a str,
        questions: &'a [GeneratedQuestion],
    ) -> BoxFuture<'a, CoreResult<Vec<GeneratedQuestion>>> {
        Box::pin(async move { Ok(questions.to_vec()) })
    }

    fn fetch_timing_averages<'a>(&'a self) -> BoxFuture<'a, CoreResult<Vec<TimingAverage>>> {
        Box::pin(async move {
            if !self.return_timing.load(Ordering::SeqCst) {
                return Ok(Vec::new());
            }
            Ok(vec![TimingAverage {
                kind: "generation".to_string(),
                expected_ms: 1500,
                sample_count: 8,
            }])
        })
    }
}

fn test_settings() -> Settings {
    Settings {
        host: "127.0.0.1".into(),
        port: 0,
        development: false,
        database_url: String::new(),
        redis_url: String::new(),
        redis_namespace: "certifications_api:".into(),
        couch_url: String::new(),
        couch_database: "certifications".into(),
        couch_user: String::new(),
        couch_password: String::new(),
        fsm_url: String::new(),
        fsm_key: String::new(),
        cortex_url: String::new(),
        reports_url: String::new(),
        gateway_url: String::new(),
        reports_service_key: String::new(),
        certifications_service_key: String::new(),
        clamav: Default::default(),
    }
}

async fn publish_question_schema(store: &Store) {
    let draft = DraftSchema {
        schema_name: QUESTION_SCHEMA_NAME.into(),
        version: QUESTION_SCHEMA_VERSION,
        description: "Question v2".into(),
        fields: vec![
            SchemaField {
                field_path: "prompt".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "alternatives".into(),
                field_type: "array".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "correct_id".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "explanation".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "citation_ref".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
        ],
    };
    schemas::save_draft(&store.db, &draft).await.unwrap();
    schemas::publish(&store.db, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION)
        .await
        .unwrap();
}

async fn setup_app(cortex: Arc<dyn CortexClient>) -> (Router, Store) {
    let mut store = Store::fixture().await;
    store.cortex = cortex;
    store.fsm = MemoryFsm::new();
    publish_question_schema(&store).await;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    (app, store)
}

async fn req_json(
    router: &Router,
    method: &str,
    uri: &str,
    user: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-uuid", user);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    };
    let res = router
        .clone()
        .oneshot(builder.body(body).expect("req"))
        .await
        .expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("collect").to_bytes();
    let val = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, val)
}

async fn create_study_with_source(router: &Router, user: &str, filename: &str) -> (String, String) {
    let (st, s) = req_json(
        router,
        "POST",
        "/studies",
        user,
        Some(json!({ "title": "Timing Study", "description": "desc" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let study_id = s["id"].as_str().expect("study id").to_string();

    let (st, src) = req_json(
        router,
        "POST",
        &format!("/studies/{study_id}/sources"),
        user,
        Some(json!({
            "filename": filename,
            "mime_type": "text/plain",
            "content_text": "Secret textbook paragraph that must never appear in telemetry."
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let source_id = src["id"].as_str().expect("source id").to_string();
    (study_id, source_id)
}

#[tokio::test]
async fn estimate_is_unavailable_without_history_or_cortex_timing() {
    let cortex = MockCortex::new(false);
    let (router, _store) = setup_app(cortex).await;
    let (study_id, _source_id) =
        create_study_with_source(&router, "owner-1", "secret-file.txt").await;

    let (st, est) = req_json(
        &router,
        "GET",
        &format!("/studies/{study_id}/estimate?language=en&question_count=5"),
        "owner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(est["status"], "unavailable");
    assert!(est["estimated_processing_ms"].is_null());
    assert_eq!(est["queue_ahead_count"], 0);
    assert_eq!(est["source"], "none");
}

#[tokio::test]
async fn estimate_uses_cortex_timing_and_reports_queue_ahead() {
    let cortex = MockCortex::new(true);
    let (router, _store) = setup_app(cortex).await;
    let (study_a, _source_a) = create_study_with_source(&router, "owner-1", "a.txt").await;
    let (study_b, _source_b) = create_study_with_source(&router, "owner-1", "b.txt").await;

    let (st, _job) = req_json(
        &router,
        "POST",
        &format!("/studies/{study_a}/generations"),
        "owner-1",
        Some(json!({
            "language": "en",
            "count": 3
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, est) = req_json(
        &router,
        "GET",
        &format!("/studies/{study_b}/estimate?language=en&question_count=5"),
        "owner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(est["status"], "available");
    assert_eq!(est["source"], "cortex_timing");
    assert_eq!(est["estimated_processing_ms"], 1500);
    assert_eq!(est["queue_ahead_count"], 1);
}

#[tokio::test]
async fn steps_and_calibration_persist_and_survive_study_deletion_without_content() {
    let cortex = MockCortex::new(false);
    let (router, store) = setup_app(cortex).await;
    let (study_id, _source_id) =
        create_study_with_source(&router, "owner-1", "confidential-notes.pdf").await;

    let (st, job) = req_json(
        &router,
        "POST",
        &format!("/studies/{study_id}/generations"),
        "owner-1",
        Some(json!({
            "language": "en",
            "count": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id = job["id"].as_str().expect("job id").to_string();

    let caller = CallerIdentity {
        user_id: "owner-1".into(),
        access_level: 1,
        display_name: Some("Owner".into()),
    };
    let done = generations::process_job(&store, &caller, &job_id)
        .await
        .expect("process");
    assert_eq!(done.status, "completed");

    let step_names: Vec<String> = sqlx::query_scalar(
        "SELECT step_name FROM generation_steps WHERE job_id = $1 ORDER BY step_name",
    )
    .bind(&job_id)
    .fetch_all(&store.db)
    .await
    .expect("steps");
    assert_eq!(
        step_names,
        vec!["cortex_generate", "prepare_sources", "save_quiz"]
    );

    // Local estimation_statistics now feeds GET /studies/{id}/estimate.
    let (st, est) = req_json(
        &router,
        "GET",
        &format!("/studies/{study_id}/estimate?language=en&question_count=2"),
        "owner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(est["status"], "available");
    assert_eq!(est["source"], "comparable_runs");
    assert_eq!(est["sample_count"], 1);

    // Run a second job so estimated_ms was non-null and error_ms calibration is populated.
    let (st, job2) = req_json(
        &router,
        "POST",
        &format!("/studies/{study_id}/generations"),
        "owner-1",
        Some(json!({
            "language": "en",
            "count": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job2_id = job2["id"].as_str().expect("job2 id").to_string();
    generations::process_job(&store, &caller, &job2_id)
        .await
        .expect("process 2");

    // Delete study and verify estimation_statistics + non-content CouchDB docs survive.
    let (st, _del) = req_json(
        &router,
        "DELETE",
        &format!("/studies/{study_id}"),
        "owner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let remaining_stats =
        sqlx::query("SELECT couch_doc_id, operation_kind FROM estimation_statistics")
            .fetch_all(&store.db)
            .await
            .expect("surviving stats");
    assert_eq!(remaining_stats.len(), 2);

    let mut calibrated_count = 0usize;
    for row in remaining_stats {
        let doc_id: String = row.get("couch_doc_id");
        let op: String = row.get("operation_kind");
        assert_eq!(op, "generation");
        let doc = store.docs.get(&doc_id).await.expect("doc survives");
        let serialized = doc.to_string();
        assert!(!serialized.contains("confidential-notes.pdf"));
        assert!(!serialized.contains("Secret textbook paragraph"));
        assert!(doc["process_ms"].is_i64());
        assert_eq!(doc["language"], "en");
        if doc["estimated_ms"].is_i64() && doc["error_ms"].is_i64() {
            calibrated_count += 1;
        }
    }
    assert_eq!(calibrated_count, 1);
}
