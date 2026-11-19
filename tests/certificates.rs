use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::{
        Store,
        fsm::MemoryFsm,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        App,
        generations::{QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION},
        quotas::QuotaCache,
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

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

async fn setup_app() -> Router {
    let mut store = Store::fixture().await;
    store.fsm = MemoryFsm::new();
    publish_question_schema(&store).await;

    presentation::router(App {
        store,
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    })
}

async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(l) = level {
        req = req.header("x-access-level", l);
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
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, bytes) = call_raw(app, method, uri, user, level, body).await;
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn certificate_identity_idempotent_issuance_snapshot_files_and_revocation() {
    let app = setup_app().await;

    // Publish a manual quiz by author-1.
    let (_, study) = call(
        &app,
        "POST",
        "/studies",
        Some("author-1"),
        None,
        Some(json!({ "title": "Cert Quiz" })),
    )
    .await;
    let sid = study["id"].as_str().unwrap().to_string();

    call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        Some("author-1"),
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q1?",
            "alternatives": [
                { "id": "a", "text": "Yes" },
                { "id": "b", "text": "No" }
            ],
            "correct_id": "a",
            "explanation": "Yes is right"
        })),
    )
    .await;

    let (_, pub_q) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        Some("author-1"),
        None,
        Some(json!({ "tags": ["cert"] })),
    )
    .await;
    let quiz_id = pub_q["id"].as_str().unwrap().to_string();

    // Normal creator taking own manual quiz cannot issue a certificate (practice only).
    let (_, a_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{quiz_id}/attempts"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    let a_att_id = a_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{a_att_id}/submit"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/attempts/{a_att_id}/certificate"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Learner-1 completes attempt with 0 score (no pass/fail threshold: still eligible!).
    let (_, l_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{quiz_id}/attempts"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    let l_att_id = l_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/events"),
        Some("learner-1"),
        None,
        Some(json!({
            "events": [{ "dedup_key": "e1", "event_kind": "page_hidden", "occurred_at": 100 }]
        })),
    )
    .await;
    call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/submit"),
        Some("learner-1"),
        None,
        None,
    )
    .await;

    // Must confirm holder identity before issuing.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Confirm holder identity once; ordinary rename is blocked.
    let (st, holder) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-1"),
        None,
        Some(json!({ "display_name": "Ana Silva" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let holder_id = holder["holder_id"].as_str().unwrap().to_string();
    assert_ne!(holder_id, "learner-1");

    let (st, _) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-1"),
        None,
        Some(json!({ "display_name": "Other Name" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Idempotent issuance returns the exact same certificate.
    let (st, cert1) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, cert2) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cert1["verification_code"], cert2["verification_code"]);
    let code = cert1["verification_code"].as_str().unwrap().to_string();

    // Admin updates holder name, but historic certificate snapshot stays "Ana Silva".
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/admin/holders/{holder_id}"),
        Some("admin-1"),
        Some("1"),
        Some(json!({ "display_name": "Ana M. Silva" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Public verification shows snapshot, activity summary, and no private answers.
    let (st, ver) = call(
        &app,
        "GET",
        &format!("/certificates/{code}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver["holder_name"], "Ana Silva");
    assert_eq!(ver["score_correct"], 0);
    assert_eq!(ver["score_total"], 1);
    assert_eq!(ver["activity_summary"]["page_hidden_count"], 1);
    assert!(!ver.to_string().contains("Yes is right"));

    // PDF and image files are served from FSM.
    let (st, pdf_bytes) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{code}/files/pdf"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(pdf_bytes.starts_with(b"%PDF-1.4"));

    let (st, img_bytes) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{code}/files/image"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(String::from_utf8_lossy(&img_bytes).contains("<svg"));

    // Admin revocation is reflected on public verification.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/admin/certificates/{code}/revocation"),
        Some("admin-1"),
        Some("1"),
        Some(json!({ "reason": "Issued in error" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, ver_after) = call(
        &app,
        "GET",
        &format!("/certificates/{code}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver_after["status"], "revoked");
    assert_eq!(ver_after["revocation_reason"], "Issued in error");
}
