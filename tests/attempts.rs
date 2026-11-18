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

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-uuid", user);
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
async fn taking_never_leaks_keys_grading_and_activity_are_idempotent_with_three_stats_groups() {
    let app = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-1",
        Some(json!({ "title": "Attempts Quiz" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let (st, q) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-1",
        Some(json!({
            "language": "en",
            "prompt": "Boiling point of water at 1 atm?",
            "alternatives": [
                { "id": "a", "text": "100 C" },
                { "id": "b", "text": "50 C" }
            ],
            "correct_id": "a",
            "explanation": "Water boils at 100 degrees Celsius at sea level."
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid = q["id"].as_str().unwrap().to_string();

    let (st, pub_q) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        "author-1",
        Some(json!({ "tags": ["physics"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let quiz_id = pub_q["id"].as_str().unwrap().to_string();

    // Start attempt as learner-1.
    let (st, att) = call(
        &app,
        "POST",
        &format!("/quizzes/{quiz_id}/attempts"),
        "learner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let att_id = att["id"].as_str().unwrap().to_string();

    // Whole-response check: keys and explanations NEVER appear before submission.
    let raw_att = att.to_string();
    assert!(!raw_att.contains("correct_id"));
    assert!(!raw_att.contains("100 degrees Celsius"));

    // Results endpoint is blocked before submission.
    let (st, _) = call(
        &app,
        "GET",
        &format!("/attempts/{att_id}/results"),
        "learner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Other user cannot view or answer this private attempt.
    let (st, _) = call(
        &app,
        "GET",
        &format!("/attempts/{att_id}"),
        "other-user",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Idempotent answer save (first wrong, then updated to right).
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/attempts/{att_id}/answers/{qid}"),
        "learner-1",
        Some(json!({ "selected_option_id": "b" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/attempts/{att_id}/answers/{qid}"),
        "learner-1",
        Some(json!({ "selected_option_id": "a" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Deduplicated activity events.
    let (st, ev_res) = call(
        &app,
        "POST",
        &format!("/attempts/{att_id}/events"),
        "learner-1",
        Some(json!({
            "events": [
                { "dedup_key": "k1", "event_kind": "page_hidden", "occurred_at": 1000 },
                { "dedup_key": "k1", "event_kind": "page_hidden", "occurred_at": 1000 },
                { "dedup_key": "k2", "event_kind": "away_duration", "duration_ms": 2500, "occurred_at": 1003 },
                { "dedup_key": "k3", "event_kind": "page_return", "occurred_at": 1003 }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ev_res["inserted"], 3);

    // Submit idempotently twice.
    let (st, res1) = call(
        &app,
        "POST",
        &format!("/attempts/{att_id}/submit"),
        "learner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, res2) = call(
        &app,
        "POST",
        &format!("/attempts/{att_id}/submit"),
        "learner-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(res1["score_correct"], 1);
    assert_eq!(res2["score_correct"], 1);

    // Three stats groups with real denominators (empty group has null percentage, not 0).
    assert_eq!(res1["stats"]["overall"]["percentage"], 100.0);
    assert_eq!(res1["stats"]["supplied_by_creator"]["total"], 1);
    assert_eq!(res1["stats"]["generated_with_cortex"]["total"], 0);
    assert!(res1["stats"]["generated_with_cortex"]["percentage"].is_null());

    // Activity summary is factual and never emits a cheating verdict.
    assert_eq!(res1["activity_summary"]["page_hidden_count"], 1);
    assert_eq!(res1["activity_summary"]["known_away_ms"], 2500);
    assert_eq!(
        res1["activity_summary"]["completeness"],
        "complete_observed"
    );

    // History lists completed attempt.
    let (st, hist) = call(&app, "GET", "/attempts", "learner-1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(hist["items"].as_array().unwrap().len(), 1);
}
