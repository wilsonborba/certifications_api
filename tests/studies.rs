use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::{Store, fsm::MemoryFsm},
    domain::{
        App,
        quotas::{QuotaCache, QuotaConfig},
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
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
        gateway_url: String::new(),
        certifications_service_key: String::new(),
    }
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
async fn quota_enforces_limits_and_releases_failed_uploads() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();

    let quotas = QuotaCache::default();
    quotas
        .update_in_db(
            &store,
            QuotaConfig {
                max_file_bytes: 70,
                max_study_bytes: 80,
                max_account_bytes: 80,
            },
            "admin-1",
        )
        .await
        .unwrap();
    let refreshed = QuotaCache::default();
    let loaded = refreshed.refresh_from_db(&store).await.unwrap();
    assert_eq!(loaded.max_file_bytes, 70);

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: refreshed,
    });

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Quota Study"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap();

    // Failed upload (EICAR) is rejected (422) and releases its byte reservation.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "bad.txt",
            "mime_type": "text/plain",
            "content_text": "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Valid upload of 45 bytes succeeds.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "notes.txt",
            "mime_type": "text/plain",
            "content_text": "123456789012345678901234567890123456789012345"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // Second upload of 40 bytes exceeds 80-byte account/study quota (413).
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "notes2.txt",
            "mime_type": "text/plain",
            "content_text": "1234567890123456789012345678901234567890"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);

    let (st, detail) = call(&app, "GET", &format!("/studies/{sid}"), Some("u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["quota"]["used_bytes"], 45);
    assert_eq!(detail["quota"]["reserved_bytes"], 0);
    assert_eq!(detail["quota"]["remaining_account_bytes"], 35);
}

#[tokio::test]
async fn selections_survive_refresh_and_prepared_studies_reuse_fsm_without_upload() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    fsm.seed("annotations/u1/file-1", b"Cell respiration uses ATP.");
    store.fsm = fsm.clone() as Arc<dyn certifications_api::dal::fsm::FsmClient>;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
    });

    // Prepared study from Annotations arrives filled and reuses stored FSM refs without putting bytes.
    let (st, prepared) = call(
        &app,
        "POST",
        "/studies/prepared",
        Some("u1"),
        Some(json!({
            "title": "Biology Prep",
            "sources": [{
                "filename": "bio.txt",
                "mime_type": "text/plain",
                "size_bytes": 26,
                "fsm_ref": "annotations/u1/file-1",
                "selection_kind": "passages",
                "selection_spec": "p1:0-25"
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(fsm.put_count(), 0);
    let sid = prepared["study"]["id"].as_str().unwrap();
    let src_id = prepared["sources"][0]["id"].as_str().unwrap();

    // Cross-user access fails with 403.
    let (st, _) = call(&app, "GET", &format!("/studies/{sid}"), Some("u2"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Update selection and verify it survives refresh.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/sources/{src_id}/selection"),
        Some("u1"),
        Some(json!({"kind": "pages", "spec": "1-3,5"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, refreshed) = call(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{src_id}"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(refreshed["selection_kind"], "pages");
    assert_eq!(refreshed["selection_spec"], "1-3,5");

    // Analysis summary reflects sources.
    let (st, analysis) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/analysis"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(analysis["total_sources"], 1);
    assert_eq!(analysis["ready_sources"], 1);

    // Expired abandoned draft returns 410 Gone.
    sqlx::query("UPDATE studies SET expires_at = 1 WHERE id = $1")
        .bind(sid)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, _) = call(&app, "GET", &format!("/studies/{sid}"), Some("u1"), None).await;
    assert_eq!(st, StatusCode::GONE);
}
