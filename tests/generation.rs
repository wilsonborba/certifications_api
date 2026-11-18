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
            BoxFuture, CortexClient, CortexJobOutcome, GeneratedAlternative, GeneratedQuestion,
            GenerationRequest,
        },
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
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;

struct MockCortex {
    calls: AtomicUsize,
    emit_cjk_first: AtomicBool,
    emit_bad_citation: AtomicBool,
}

impl MockCortex {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            emit_cjk_first: AtomicBool::new(false),
            emit_bad_citation: AtomicBool::new(false),
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
    ) -> BoxFuture<'a, CoreResult<Vec<GeneratedQuestion>>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let src = if self.emit_bad_citation.load(Ordering::SeqCst) {
                "fabricated-source".to_string()
            } else {
                req.source_ids[0].clone()
            };
            let prompt = if self.emit_cjk_first.load(Ordering::SeqCst) {
                "อะไรคือ ATP 细胞".to_string()
            } else if req.language == "th" {
                "อะไรคือ ATP".to_string()
            } else {
                "What is ATP?".to_string()
            };
            Ok(vec![GeneratedQuestion {
                prompt,
                alternatives: vec![
                    GeneratedAlternative {
                        id: "a".into(),
                        text: if req.language == "th" {
                            "พลังงาน".into()
                        } else {
                            "Energy currency".into()
                        },
                    },
                    GeneratedAlternative {
                        id: "b".into(),
                        text: if req.language == "th" {
                            "น้ำ".into()
                        } else {
                            "Water".into()
                        },
                    },
                ],
                correct_id: "a".into(),
                explanation: if req.language == "th" {
                    "ใช้ในเซลล์".into()
                } else {
                    "Used in cells".into()
                },
                source_id: src,
                citation_ref: "p.1".into(),
            }])
        })
    }
    fn correct_language<'a>(
        &'a self,
        _owner_id: &'a str,
        _language: &'a str,
        questions: &'a [GeneratedQuestion],
    ) -> BoxFuture<'a, CoreResult<Vec<GeneratedQuestion>>> {
        Box::pin(async move {
            Ok(questions
                .iter()
                .map(|q| GeneratedQuestion {
                    prompt: q.prompt.replace("细胞", "").trim().to_string(),
                    alternatives: q.alternatives.clone(),
                    correct_id: q.correct_id.clone(),
                    explanation: q.explanation.clone(),
                    source_id: q.source_id.clone(),
                    citation_ref: q.citation_ref.clone(),
                })
                .collect())
        })
    }
    fn check_request_outcome<'a>(
        &'a self,
        _cortex_job_id: &'a str,
    ) -> BoxFuture<'a, CoreResult<CortexJobOutcome>> {
        Box::pin(async { Ok(CortexJobOutcome::NotFound) })
    }
    fn fetch_timing_averages<'a>(
        &'a self,
    ) -> BoxFuture<'a, CoreResult<Vec<certifications_api::dal::cortex::TimingAverage>>> {
        Box::pin(async { Ok(Vec::new()) })
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
        certifications_service_key: String::new(),
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

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn refresh_is_idempotent_second_active_job_blocked_and_stop_resume_requeues() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    store.cortex = cortex.clone();
    publish_question_schema(&store).await;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    let (_, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Bio"})),
    )
    .await;
    let sid = study["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({"filename": "bio.txt", "mime_type": "text/plain", "content_text": "ATP is energy."})),
    )
    .await;

    // Start generation with idempotency_key.
    let (st, job1) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "en", "count": 1, "idempotency_key": "req-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let jid1 = job1["id"].as_str().unwrap().to_string();
    assert_eq!(job1["queue_position"], 1);

    // Refresh with same idempotency_key returns the same job.
    let (st, job_dup) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "en", "count": 1, "idempotency_key": "req-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(job_dup["id"], jid1);

    // Starting a different job while one is queued/running returns 409.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "en", "count": 2, "idempotency_key": "req-2"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Stop and resume requeues cleanly.
    let (st, stopped) = call(
        &app,
        "POST",
        &format!("/generations/{jid1}/stop"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(stopped["status"], "cancelled");

    let (st, resumed) = call(
        &app,
        "POST",
        &format!("/generations/{jid1}/resume"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let jid2 = resumed["id"].as_str().unwrap().to_string();
    assert_ne!(jid2, jid1);
    assert_eq!(resumed["status"], "queued");

    // Process the resumed job and verify SSE events + private quiz saved.
    let caller = CallerIdentity {
        user_id: "u1".into(),
        access_level: 3,
        display_name: None,
    };
    let done = generations::process_job(&store, &caller, &jid2)
        .await
        .unwrap();
    assert_eq!(done.status, "completed");
    assert_eq!(done.saved_count, 1);
    let qid = done.quiz_id.unwrap();
    let vis: String = sqlx::query_scalar("SELECT visibility FROM quizzes WHERE id = $1")
        .bind(&qid)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(vis, "private");

    let sse_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/generations/{jid2}/events?after=0"))
                .header("x-uuid", "u1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    assert_eq!(sse_resp.headers()["content-type"], "text/event-stream");
}

#[tokio::test]
async fn wrong_script_is_corrected_and_unverified_citation_is_rejected() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    cortex.emit_cjk_first.store(true, Ordering::SeqCst);
    store.cortex = cortex.clone();
    publish_question_schema(&store).await;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    let (_, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Thai Bio"})),
    )
    .await;
    let sid = study["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({"filename": "bio.txt", "mime_type": "text/plain", "content_text": "เซลล์ใช้พลังงาน"})),
    )
    .await;

    let (_, job) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "th", "count": 1})),
    )
    .await;
    let jid = job["id"].as_str().unwrap().to_string();
    let caller = CallerIdentity {
        user_id: "u1".into(),
        access_level: 3,
        display_name: None,
    };
    let done = generations::process_job(&store, &caller, &jid)
        .await
        .unwrap();
    assert_eq!(done.status, "completed");

    // Unverified citation is rejected.
    cortex.emit_cjk_first.store(false, Ordering::SeqCst);
    cortex.emit_bad_citation.store(true, Ordering::SeqCst);
    let (_, job2) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "th", "count": 1})),
    )
    .await;
    let jid2 = job2["id"].as_str().unwrap().to_string();
    let err = generations::process_job(&store, &caller, &jid2)
        .await
        .unwrap_err();
    assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
}
