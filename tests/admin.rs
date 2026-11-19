use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::Store,
    domain::{App, quotas::QuotaCache},
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

async fn setup_app() -> Router {
    let store = Store::fixture().await;
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
    level: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-uuid", user)
        .header("x-access-level", level);
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
async fn normal_sessions_are_refused_and_admin_settings_schemas_stats_work() {
    let app = setup_app().await;

    // Normal sessions are refused on every admin route.
    for (method, uri, body) in [
        ("GET", "/admin/settings", None),
        (
            "PATCH",
            "/admin/settings",
            Some(json!({ "items": [{ "key": "k", "value_int": 1 }] })),
        ),
        ("GET", "/admin/schemas/custom-v1/versions", None),
        (
            "POST",
            "/admin/schemas/custom-v1/versions",
            Some(json!({
                "version": 1,
                "fields": [{ "field_path": "a", "field_type": "string", "is_required": true }]
            })),
        ),
        ("GET", "/admin/cleanup", None),
        ("GET", "/admin/statistics", None),
        (
            "PATCH",
            "/admin/quizzes/q1/origin",
            Some(json!({ "origin": "generated_with_cortex" })),
        ),
        (
            "PATCH",
            "/admin/holders/h1",
            Some(json!({ "display_name": "Test" })),
        ),
        (
            "POST",
            "/admin/certificates/c1/revocation",
            Some(json!({ "reason": "test" })),
        ),
    ] {
        let (st, _) = call(&app, method, uri, "normal-user", "0", body).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "route {method} {uri}");
    }

    // Admin settings get & patch.
    let (st, patched) = call(
        &app,
        "PATCH",
        "/admin/settings",
        "admin-1",
        "1",
        Some(json!({
            "items": [{
                "key": "max_study_bytes",
                "value_int": 1048576,
                "description": "Max bytes per study"
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(patched["items"][0]["key"], "max_study_bytes");

    let (st, got) = call(&app, "GET", "/admin/settings", "admin-1", "1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(got["items"][0]["value_int"], 1048576);

    // Admin schema draft lifecycle: create, edit, delete draft; create v2, publish, retire.
    let (st, draft1) = call(
        &app,
        "POST",
        "/admin/schemas/custom-v1/versions",
        "admin-1",
        "1",
        Some(json!({
            "version": 1,
            "description": "Draft 1",
            "fields": [{ "field_path": "title", "field_type": "string", "is_required": true }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(draft1["status"], "draft");

    let (st, _) = call(
        &app,
        "DELETE",
        "/admin/schemas/custom-v1/versions/1",
        "admin-1",
        "1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let (st, _) = call(
        &app,
        "POST",
        "/admin/schemas/custom-v1/versions",
        "admin-1",
        "1",
        Some(json!({
            "version": 2,
            "description": "Published v2",
            "fields": [{ "field_path": "title", "field_type": "string", "is_required": true }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, pub2) = call(
        &app,
        "PATCH",
        "/admin/schemas/custom-v1/versions/2/status",
        "admin-1",
        "1",
        Some(json!({ "status": "published" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(pub2["status"], "published");

    // Published schema cannot be deleted or edited.
    let (st, _) = call(
        &app,
        "DELETE",
        "/admin/schemas/custom-v1/versions/2",
        "admin-1",
        "1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, ret2) = call(
        &app,
        "PATCH",
        "/admin/schemas/custom-v1/versions/2/status",
        "admin-1",
        "1",
        Some(json!({ "status": "retired" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ret2["status"], "retired");

    // Admin statistics.
    let (st, stats) = call(&app, "GET", "/admin/statistics", "admin-1", "1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(stats["studies_total"], 0);
}
