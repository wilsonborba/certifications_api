use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use certifications_api::dal::reports::HttpReports;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;

type SeenRequest = (String, String, String, String, String);

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Option<SeenRequest>>>);

async fn dataset(
    State(seen): State<Seen>,
    Path((project, report, dataset)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let user = headers
        .get("x-uuid")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let paging = format!(
        "{}:{}:{}",
        query.get("lang").map(String::as_str).unwrap_or(""),
        query.get("offset").map(String::as_str).unwrap_or(""),
        query.get("limit").map(String::as_str).unwrap_or(""),
    );
    *seen.0.lock().unwrap() = Some((user, project, report, dataset, paging));
    Json(json!({
        "dataset_id": "d1",
        "title": "Q4 metrics",
        "description": "descriptor text is not generation material",
        "equation": null,
        "data_tier": "public",
        "fields": [],
        "filters": [],
        "can_view": true,
        "visibility": "public",
        "meta": null,
        "language": "en",
        "rows": [{"z": {"b": 1, "a": "x"}, "a": 2}],
        "offset": 0,
        "limit": 5000,
        "total_rows": 1,
        "applied_filters": {},
        "lineage": {"calculation_id": "c1", "calculation_version": 1, "inputs": [], "source_watermark": {}, "filters": {}, "calculated_at": "2026-11-24T00:00:00Z"},
        "visual": {"kind": "ignored"}
    }))
}

#[derive(Clone, Default)]
struct PageBehavior {
    change_lineage: bool,
    too_many_rows: bool,
    offsets: Arc<Mutex<Vec<usize>>>,
}

async fn paged_dataset(
    State(state): State<PageBehavior>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let offset = query
        .get("offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    state.offsets.lock().unwrap().push(offset);
    let rows = if offset == 0 && state.too_many_rows {
        (0..5000)
            .map(|index| json!({"index": index}))
            .collect::<Vec<_>>()
    } else if offset == 0 {
        vec![json!({"index": 0})]
    } else if offset == 1 {
        vec![json!({"index": 1})]
    } else {
        Vec::new()
    };
    let version = if state.change_lineage && offset > 0 {
        2
    } else {
        1
    };
    Json(json!({
        "dataset_id": "d1",
        "title": "Rows",
        "description": "",
        "equation": null,
        "data_tier": "public",
        "fields": [],
        "filters": [],
        "can_view": true,
        "visibility": "public",
        "meta": null,
        "language": "en",
        "rows": rows,
        "offset": offset,
        "limit": 5000,
        "total_rows": if state.too_many_rows { 5001 } else { 2 },
        "applied_filters": {},
        "lineage": {
            "calculation_id": "calc1",
            "calculation_version": version,
            "inputs": [{"schema": "items", "version": 1}],
            "source_watermark": {"items": {"records": 5001, "last_received_at": "2026-11-24T00:00:00Z"}},
            "filters": {},
            "calculated_at": "2026-11-24T00:00:00Z"
        },
        "visual": null
    }))
}

async fn report_view(
    Path((_project, report)): Path<(String, String)>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let user = headers
        .get("x-uuid")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if report == "missing" {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"})));
    }
    if user == "denied" {
        return (StatusCode::FORBIDDEN, Json(json!({"error": "forbidden"})));
    }
    (
        StatusCode::OK,
        Json(json!({
            "datasets": [
                {"dataset_id":"d1", "title":"Readable", "description":"", "equation":null, "data_tier":"public", "fields":[], "filters":[], "can_view":true, "visibility":"public"},
                {"dataset_id":"d2", "title":"Hidden", "description":"", "equation":null, "data_tier":"restricted", "fields":[], "filters":[], "can_view":false, "visibility":"restricted"}
            ]
        })),
    )
}

#[tokio::test]
async fn report_source_uses_existing_authorized_dataset_route_and_only_canonical_rows() {
    let seen = Seen::default();
    let app = Router::new()
        .route(
            "/apps/reports/v1/projects/{project}/reports/{report}/datasets/{dataset}",
            get(dataset),
        )
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HttpReports::client(
        &format!("http://{address}/apps/reports/v1"),
        "",
        reqwest::Client::new(),
    );
    let material = client
        .fetch_report_dataset("caller-1", "proj", "rep", "d1")
        .await
        .unwrap();

    assert_eq!(material.bytes, br#"[{"a":2,"z":{"a":"x","b":1}}]"#);
    assert!(!material.is_restricted);
    assert_eq!(
        seen.0.lock().unwrap().as_ref().unwrap(),
        &(
            "caller-1".into(),
            "proj".into(),
            "rep".into(),
            "d1".into(),
            "en:0:5000".into()
        )
    );

    server.abort();
}

#[tokio::test]
async fn report_picker_uses_caller_scoped_descriptors_and_hides_unreadable_datasets() {
    let app = Router::new().route(
        "/apps/reports/v1/projects/{project}/reports/{report}",
        get(report_view),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = HttpReports::client(
        &format!("http://{address}/apps/reports/v1"),
        "",
        reqwest::Client::new(),
    );

    let items = client
        .list_readable_datasets("caller-1", "proj", "rep")
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].project, "proj");
    assert_eq!(items[0].report, "rep");
    assert_eq!(items[0].dataset["dataset_id"], "d1");
    assert_eq!(items[0].dataset["can_view"], true);

    let denied = client
        .list_readable_datasets("denied", "proj", "rep")
        .await
        .unwrap_err();
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    let missing = client
        .list_readable_datasets("caller-1", "proj", "missing")
        .await
        .unwrap_err();
    assert_eq!(missing.0, StatusCode::NOT_FOUND);

    server.abort();
}

#[tokio::test]
async fn report_source_fetches_all_pages_under_limit_rejects_lineage_drift_and_caps_rows() {
    for (change_lineage, too_many_rows) in [(false, false), (true, false), (false, true)] {
        let state = PageBehavior {
            change_lineage,
            too_many_rows,
            ..Default::default()
        };
        let app = Router::new()
            .route(
                "/apps/reports/v1/projects/{project}/reports/{report}/datasets/{dataset}",
                get(paged_dataset),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = HttpReports::client(
            &format!("http://{address}/apps/reports/v1"),
            "",
            reqwest::Client::new(),
        );

        let result = client
            .fetch_report_dataset("caller-1", "proj", "rep", "d1")
            .await;
        if too_many_rows {
            let error = result.unwrap_err();
            assert_eq!(error.0, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(*state.offsets.lock().unwrap(), vec![0]);
        } else if change_lineage {
            let error = result.unwrap_err();
            assert_eq!(error.0, axum::http::StatusCode::CONFLICT);
        } else {
            let material = result.unwrap();
            let rows: Vec<Value> = serde_json::from_slice(&material.bytes).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(material.row_count, 2);
            assert_eq!(*state.offsets.lock().unwrap(), vec![0, 1]);
        }
        server.abort();
    }
}

#[tokio::test]
async fn reports_render_job_sends_trusted_service_key_and_dataset_query_does_not() {
    use axum::routing::post;
    use certifications_api::dal::reports::CreateRenderJobInput;
    use std::collections::BTreeMap;

    #[derive(Clone, Default)]
    struct AuthSeen {
        render_job_key: Arc<Mutex<Option<String>>>,
        render_job_user: Arc<Mutex<Option<String>>>,
        delete_job_key: Arc<Mutex<Option<String>>>,
        delete_job_user: Arc<Mutex<Option<String>>>,
        dataset_key: Arc<Mutex<Option<String>>>,
    }

    let auth_seen = AuthSeen::default();
    let auth_seen_clone = auth_seen.clone();

    let app = Router::new()
        .route(
            "/apps/reports/v1/render_jobs",
            post({
                let state = auth_seen.clone();
                move |headers: HeaderMap, Json(body): Json<CreateRenderJobInput>| {
                    let state = state.clone();
                    async move {
                        *state.render_job_key.lock().unwrap() = headers
                            .get("x-reports-service-key")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        *state.render_job_user.lock().unwrap() = headers
                            .get("x-uuid")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        (
                            StatusCode::CREATED,
                            Json(json!({
                                "job_id": "job-123",
                                "template_id": body.template_id,
                                "template_version": body.template_version,
                                "mapping": body.mapping,
                                "config": body.config,
                                "rows": body.rows,
                                "width": body.width,
                                "height": body.height,
                                "scale": body.scale,
                                "expires_at": 1893456000,
                                "status": "pending",
                                "created_at": 1893455000
                            })),
                        )
                    }
                }
            }),
        )
        .route(
            "/apps/reports/v1/render_jobs/{id}",
            axum::routing::delete({
                let state = auth_seen.clone();
                move |headers: HeaderMap, Path(_id): Path<String>| {
                    let state = state.clone();
                    async move {
                        *state.delete_job_key.lock().unwrap() = headers
                            .get("x-reports-service-key")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        *state.delete_job_user.lock().unwrap() = headers
                            .get("x-uuid")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        StatusCode::OK
                    }
                }
            }),
        )
        .route(
            "/apps/reports/v1/projects/{project}/reports/{report}/datasets/{dataset}",
            get({
                let state = auth_seen.clone();
                move |headers: HeaderMap| {
                    let state = state.clone();
                    async move {
                        *state.dataset_key.lock().unwrap() = headers
                            .get("x-reports-service-key")
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        Json(json!({
                            "dataset_id": "d1",
                            "title": "T",
                            "description": "",
                            "equation": null,
                            "data_tier": "public",
                            "fields": [],
                            "filters": [],
                            "can_view": true,
                            "visibility": "public",
                            "meta": null,
                            "language": "en",
                            "rows": [{"a": 1}],
                            "offset": 0,
                            "limit": 5000,
                            "total_rows": 1,
                            "applied_filters": {},
                            "lineage": {"calculation_id": "c1", "calculation_version": 1, "inputs": [], "source_watermark": {}, "filters": {}, "calculated_at": "2026-11-24T00:00:00Z"},
                            "visual": null
                        }))
                    }
                }
            }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = HttpReports::client(
        &format!("http://{address}/apps/reports/v1"),
        "trusted-reports-secret-999",
        reqwest::Client::new(),
    );

    let input = CreateRenderJobInput {
        template_id: "tmpl_1".into(),
        template_version: 1,
        mapping: BTreeMap::new(),
        config: json!({}),
        rows: vec![json!({"x": 1})],
        width: 800,
        height: 600,
        scale: 2,
        ttl_seconds: Some(300),
        originating_scope: None,
    };

    let desc = client
        .create_render_job("author-owner", &input)
        .await
        .unwrap();
    assert_eq!(desc.job_id, "job-123");
    assert_eq!(
        auth_seen_clone.render_job_key.lock().unwrap().as_deref(),
        Some("trusted-reports-secret-999")
    );
    assert_eq!(
        auth_seen_clone.render_job_user.lock().unwrap().as_deref(),
        Some("author-owner")
    );

    client
        .delete_render_job("author-owner", "job-123", true)
        .await
        .unwrap();
    assert_eq!(
        auth_seen_clone.delete_job_key.lock().unwrap().as_deref(),
        Some("trusted-reports-secret-999")
    );
    assert_eq!(
        auth_seen_clone.delete_job_user.lock().unwrap().as_deref(),
        Some("author-owner")
    );

    let mat = client
        .fetch_report_dataset("author-owner", "p", "r", "d")
        .await
        .unwrap();
    assert_eq!(mat.row_count, 1);
    assert_eq!(auth_seen_clone.dataset_key.lock().unwrap().as_deref(), None);

    server.abort();
}
