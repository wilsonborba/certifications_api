use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    routing::put,
};
use certifications_api::{
    core::{Result as CoreResult, Settings},
    dal::{
        Store,
        cortex::{
            BoxFuture, CortexClient, GeneratedAlternative, GeneratedQuestion, GenerationRequest,
            GenerationResult,
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
use sqlx::Row;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use tower::ServiceExt;

struct CortexGate {
    entered: Notify,
    release: Notify,
}

#[derive(Clone, Default)]
struct CouchGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    completed: Arc<AtomicBool>,
    documents: Arc<Mutex<std::collections::HashMap<String, Value>>>,
}

async fn couch_put(
    State(gate): State<CouchGate>,
    Path((_database, doc_id)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> StatusCode {
    gate.entered.notify_one();
    gate.release.notified().await;
    body["_rev"] = json!("test-revision");
    gate.documents.lock().unwrap().insert(doc_id, body);
    gate.completed.store(true, Ordering::SeqCst);
    StatusCode::CREATED
}

async fn couch_get(
    State(gate): State<CouchGate>,
    Path((_database, doc_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    let body = gate.documents.lock().unwrap().get(&doc_id).cloned();
    match body {
        Some(body) => (StatusCode::OK, Json(body)),
        None => (StatusCode::NOT_FOUND, Json(Value::Null)),
    }
}

async fn couch_delete(
    State(gate): State<CouchGate>,
    Path((_database, doc_id)): Path<(String, String)>,
) -> StatusCode {
    gate.documents.lock().unwrap().remove(&doc_id);
    StatusCode::OK
}

struct MockCortex {
    calls: AtomicUsize,
    emit_cjk_first: AtomicBool,
    emit_bad_citation: AtomicBool,
    source_text: Mutex<String>,
    prompt_suffix: Mutex<String>,
    gate: Mutex<Option<Arc<CortexGate>>>,
}

impl MockCortex {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            emit_cjk_first: AtomicBool::new(false),
            emit_bad_citation: AtomicBool::new(false),
            source_text: Mutex::new(String::new()),
            prompt_suffix: Mutex::new(String::new()),
            gate: Mutex::new(None),
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            *self.source_text.lock().unwrap() = req
                .sources
                .first()
                .map(|source| String::from_utf8_lossy(&source.bytes).into_owned())
                .unwrap_or_default();
            let src = if self.emit_bad_citation.load(Ordering::SeqCst) {
                "fabricated-source".to_string()
            } else {
                req.source_ids[0].clone()
            };
            let base_prompt = if self.emit_cjk_first.load(Ordering::SeqCst) {
                "อะไรคือ ATP 细胞".to_string()
            } else if req.language == "th" {
                "อะไรคือ ATP".to_string()
            } else {
                "What is ATP?".to_string()
            };
            let prompt = format!("{base_prompt}{}", self.prompt_suffix.lock().unwrap());
            Ok(GenerationResult {
                questions: vec![GeneratedQuestion {
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
                }],
                session_id: "temporary-session".into(),
                output_id: "temporary-output".into(),
                status: "done".into(),
            })
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

async fn wait_for_job_status(app: &Router, job_id: &str, expected: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let (_, job) = call(
                app,
                "GET",
                &format!("/generations/{job_id}"),
                Some("u1"),
                None,
            )
            .await;
            if job["status"] == expected {
                return job;
            }
            if matches!(
                job["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                panic!("job reached unexpected terminal status: {job}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker did not reach the expected job status")
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
    let (_, source) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({"filename": "bio.txt", "mime_type": "text/plain", "content_text": "ATP is energy."})),
    )
    .await;
    let source_id = source["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/sources/{source_id}/selection"),
        Some("u1"),
        Some(json!({"kind":"passages","spec":"p1:0-3"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

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

    // Selection edits after queueing do not change the frozen source snapshot.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/sources/{source_id}/selection"),
        Some("u1"),
        Some(json!({"kind":"passages","spec":"p1:7-13"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

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
    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    let _ = wait_for_job_status(&app, &jid2, "completed").await;
    let done = generations::get_job(&store, &caller, &jid2).await.unwrap();
    assert_eq!(done.status, "completed");
    assert_eq!(done.saved_count, 1);
    assert_eq!(*cortex.source_text.lock().unwrap(), "ATP");
    let qid = done.quiz_id.unwrap();
    let vis: String = sqlx::query_scalar("SELECT visibility FROM quizzes WHERE id = $1")
        .bind(&qid)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(vis, "private");
    let generated_membership = sqlx::query(
        "SELECT qvq.source_scope, qvq.origin_quiz_id, qvq.origin_quiz_version_id,
                COUNT(qc.id) AS citation_count
         FROM quizzes q
         JOIN quiz_version_questions qvq ON qvq.quiz_version_id = q.active_version_id
         LEFT JOIN question_citations qc ON qc.question_version_id = qvq.question_version_id
         WHERE q.id = $1
         GROUP BY qvq.source_scope, qvq.origin_quiz_id, qvq.origin_quiz_version_id",
    )
    .bind(&qid)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(
        generated_membership.get::<String, _>("source_scope"),
        "generated_from_current_selected_sources"
    );
    assert_eq!(
        generated_membership.get::<Option<String>, _>("origin_quiz_id"),
        None
    );
    assert_eq!(
        generated_membership.get::<Option<String>, _>("origin_quiz_version_id"),
        None
    );
    assert_eq!(generated_membership.get::<i64, _>("citation_count"), 1);

    // A corrupted queued integrity snapshot must fail before Cortex is called.
    let (st, sabotaged) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/generations"),
        Some("u1"),
        Some(json!({"language": "en", "count": 1, "idempotency_key": "req-sabotaged"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sabotaged_id = sabotaged["id"].as_str().unwrap();
    sqlx::query("UPDATE generation_job_sources SET sha256 = 'wrong' WHERE job_id = $1")
        .bind(sabotaged_id)
        .execute(&store.db)
        .await
        .unwrap();
    let _ = wait_for_job_status(&app, sabotaged_id, "failed").await;
    let sabotaged = generations::get_job(&store, &caller, sabotaged_id)
        .await
        .unwrap();
    assert_eq!(sabotaged.status, "failed");
    assert_eq!(sabotaged.error_code, "source_integrity_failed");
    assert_eq!(cortex.calls.load(Ordering::SeqCst), 1);

    worker.abort();
    let _ = worker.await;

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

#[tokio::test]
async fn start_rejects_foreign_other_study_and_public_quiz_targets_and_expired_drafts() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    let fsm = certifications_api::dal::fsm::MemoryFsm::new();
    store.fsm = fsm.clone();
    store.cortex = cortex.clone();
    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    let (_, first) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "First"})),
    )
    .await;
    let first_id = first["id"].as_str().unwrap();
    let (_, second) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Second"})),
    )
    .await;
    let second_id = second["id"].as_str().unwrap();
    let (_, foreign_study) = call(
        &app,
        "POST",
        "/studies",
        Some("u2"),
        Some(json!({"title": "Foreign"})),
    )
    .await;
    let foreign_study_id = foreign_study["id"].as_str().unwrap();

    for (quiz_id, owner_id, study_id, visibility) in [
        ("other-study", "u1", second_id, "private"),
        ("foreign-owner", "u2", first_id, "private"),
        ("foreign-study", "u2", foreign_study_id, "private"),
        ("public-target", "u1", first_id, "public"),
    ] {
        sqlx::query(
            "INSERT INTO quizzes(id, owner_id, study_id, visibility, origin, active_version_id, created_at, updated_at)
             VALUES($1, $2, $3, $4, 'generated_with_cortex', '', 1, 1)",
        )
        .bind(quiz_id)
        .bind(owner_id)
        .bind(study_id)
        .bind(visibility)
        .execute(&store.db)
        .await
        .unwrap();
        let (status, _) = call(
            &app,
            "POST",
            &format!("/studies/{first_id}/generations"),
            Some("u1"),
            Some(json!({"language":"en", "count":1, "quiz_id":quiz_id})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "target {quiz_id}");
    }

    sqlx::query("UPDATE studies SET expires_at = 0 WHERE id = $1")
        .bind(first_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/studies/{first_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    let study_status: String = sqlx::query_scalar("SELECT status FROM studies WHERE id = $1")
        .bind(first_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(study_status, "expired");
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM generation_jobs")
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    assert_eq!(cortex.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fsm.get_count(), 0);
}

#[tokio::test]
async fn worker_checks_server_provenance_before_any_fsm_read() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    let fsm = certifications_api::dal::fsm::MemoryFsm::new();
    store.fsm = fsm.clone();
    store.cortex = cortex.clone();
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
        Some(json!({"title":"Provenance"})),
    )
    .await;
    let study_id = study["id"].as_str().unwrap();
    let (_, source) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/sources"),
        Some("u1"),
        Some(json!({"filename":"source.txt", "mime_type":"text/plain", "content_text":"source text"})),
    )
    .await;
    let source_id = source["id"].as_str().unwrap();
    sqlx::query("UPDATE sources SET source_provenance = 'unknown' WHERE id = $1")
        .bind(source_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (status, job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = job["id"].as_str().unwrap();
    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    let failed = wait_for_job_status(&app, job_id, "failed").await;
    assert_eq!(failed["error_code"], "source_unavailable");
    assert_eq!(fsm.get_count(), 0);
    assert_eq!(cortex.calls.load(Ordering::SeqCst), 0);
    worker.abort();
    let _ = worker.await;
}

#[tokio::test]
async fn stop_during_cortex_call_saves_the_current_valid_step_then_stops() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    let gate = Arc::new(CortexGate {
        entered: Notify::new(),
        release: Notify::new(),
    });
    *cortex.gate.lock().unwrap() = Some(gate.clone());
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
        Some(json!({"title":"Stop boundary"})),
    )
    .await;
    let study_id = study["id"].as_str().unwrap();
    call(
        &app,
        "POST",
        &format!("/studies/{study_id}/sources"),
        Some("u1"),
        Some(json!({"filename":"source.txt", "mime_type":"text/plain", "content_text":"source text"})),
    )
    .await;
    let (status, job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = job["id"].as_str().unwrap();
    let docs_before = match &store.docs {
        certifications_api::dal::documents::Documents::Memory(docs) => docs.len(),
        certifications_api::dal::documents::Documents::Remote(_) => unreachable!(),
    };
    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("worker did not enter Cortex call");
    let (status, stopped) = call(
        &app,
        "POST",
        &format!("/generations/{job_id}/stop"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["status"], "running");
    assert_eq!(stopped["stop_requested"], true);
    gate.release.notify_one();
    let cancelled = wait_for_job_status(&app, job_id, "cancelled").await;
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["saved_count"], 1);
    let quizzes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
        .bind(study_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(quizzes, 1);
    let doc_id: String = sqlx::query_scalar(
        "SELECT qv.couch_doc_id FROM quizzes q JOIN quiz_version_questions qvq ON qvq.quiz_version_id = q.active_version_id
         JOIN question_versions qv ON qv.id = qvq.question_version_id WHERE q.study_id = $1",
    )
    .bind(study_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    match &store.docs {
        certifications_api::dal::documents::Documents::Memory(docs) => {
            assert!(docs.len() > docs_before);
            assert!(docs.contains(&doc_id));
        }
        certifications_api::dal::documents::Documents::Remote(_) => unreachable!(),
    }
    worker.abort();
    let _ = worker.await;
}

#[tokio::test]
async fn stop_racing_with_document_save_commits_the_valid_step_as_cancelled() {
    let mut store = Store::fixture().await;
    let cortex = MockCortex::new();
    store.cortex = cortex;
    publish_question_schema(&store).await;

    let couch_gate = CouchGate::default();
    let couch = Router::new()
        .route(
            "/{database}/{doc_id}",
            put(couch_put).get(couch_get).delete(couch_delete),
        )
        .with_state(couch_gate.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let couch_server = tokio::spawn(async move {
        axum::serve(listener, couch).await.unwrap();
    });
    let mut settings = test_settings();
    settings.couch_url = format!("http://{address}");
    store.docs =
        certifications_api::dal::documents::Documents::remote(&settings, reqwest::Client::new());
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    let (_, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title":"Stop during save"})),
    )
    .await;
    let study_id = study["id"].as_str().unwrap();
    call(
        &app,
        "POST",
        &format!("/studies/{study_id}/sources"),
        Some("u1"),
        Some(json!({"filename":"source.txt", "mime_type":"text/plain", "content_text":"source text"})),
    )
    .await;
    let (status, job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = job["id"].as_str().unwrap();
    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        couch_gate.entered.notified(),
    )
    .await
    .expect("worker did not reach the document save boundary");

    let (status, stopped) = call(
        &app,
        "POST",
        &format!("/generations/{job_id}/stop"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["status"], "running");
    assert_eq!(stopped["stop_requested"], true);
    couch_gate.release.notify_one();
    let _ = wait_for_job_status(&app, job_id, "cancelled").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !couch_gate.completed.load(Ordering::SeqCst)
            || couch_gate.documents.lock().unwrap().is_empty()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker did not commit the in-flight CouchDB question");
    let quizzes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
        .bind(study_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(quizzes, 1);
    worker.abort();
    let _ = worker.await;
    couch_server.abort();
    let _ = couch_server.await;
}

#[tokio::test]
async fn extra_round_keeps_existing_quiz_membership() {
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
        Some(json!({"title":"Extra round"})),
    )
    .await;
    let study_id = study["id"].as_str().unwrap();
    call(
        &app,
        "POST",
        &format!("/studies/{study_id}/sources"),
        Some("u1"),
        Some(json!({"filename":"source.txt", "mime_type":"text/plain", "content_text":"source text"})),
    )
    .await;
    let (_, first_job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    let first_id = first_job["id"].as_str().unwrap();
    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    let first_done = wait_for_job_status(&app, first_id, "completed").await;
    let quiz_id = first_done["quiz_id"].as_str().unwrap();
    let first_version: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let first_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
    )
    .bind(&first_version)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(first_count, 1);

    *cortex.prompt_suffix.lock().unwrap() = " Next".into();
    let (status, second_job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1, "quiz_id":quiz_id})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let second_id = second_job["id"].as_str().unwrap();
    let second_done = wait_for_job_status(&app, second_id, "completed").await;
    assert_eq!(second_done["saved_count"], 1);
    let active_version: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
    )
    .bind(&active_version)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(active_count, 2);
    let preserved_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
    )
    .bind(&first_version)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(preserved_count, 1);
    worker.abort();
    let _ = worker.await;
}

#[tokio::test]
async fn worker_marks_guardrail_failure_terminal_instead_of_leaving_running_job() {
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
        Some(json!({"title":"Worker failure"})),
    )
    .await;
    let study_id = study["id"].as_str().unwrap();
    call(
        &app,
        "POST",
        &format!("/studies/{study_id}/sources"),
        Some("u1"),
        Some(json!({"filename":"source.txt", "mime_type":"text/plain", "content_text":"source text"})),
    )
    .await;
    let (_, job) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/generations"),
        Some("u1"),
        Some(json!({"language":"en", "count":1})),
    )
    .await;
    let job_id = job["id"].as_str().unwrap();
    sqlx::query("DELETE FROM schema_versions WHERE schema_name = $1 AND version = $2")
        .bind(QUESTION_SCHEMA_NAME)
        .bind(QUESTION_SCHEMA_VERSION)
        .execute(&store.db)
        .await
        .unwrap();

    let worker = tokio::spawn(generations::run_queue_worker(store.clone()));
    let failed = wait_for_job_status(&app, job_id, "failed").await;
    assert_eq!(failed["error_code"], "cortex_outcome_unknown");
    assert_eq!(cortex.calls.load(Ordering::SeqCst), 1);
    let caller = CallerIdentity {
        user_id: "u1".into(),
        access_level: 3,
        display_name: None,
    };
    let error = generations::resume_generation(&store, &caller, job_id)
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    let quiz_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
        .bind(study_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(quiz_count, 0);
    worker.abort();
    let _ = worker.await;
}
