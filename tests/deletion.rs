use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::{Store, fsm::MemoryFsm},
    domain::{App, cleanup, quotas::QuotaCache},
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
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
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn fsm_outage_keeps_cleanup_intent_across_restart_and_admin_or_daily_retry_cleans_it() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();

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
        None,
        Some(json!({"title": "Deletion Study"})),
    )
    .await;
    let sid = study["id"].as_str().unwrap().to_string();

    let (_, src) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        None,
        Some(json!({
            "filename": "notes.txt",
            "mime_type": "text/plain",
            "content_text": "Private content to remove."
        })),
    )
    .await;
    let src_id = src["id"].as_str().unwrap().to_string();
    let fsm_ref = src["fsm_ref"].as_str().unwrap().to_string();
    assert!(fsm.has_blob(&fsm_ref));

    // Seed an active generation job referencing this source to verify fencing.
    sqlx::query(
        "INSERT INTO generation_jobs(id, owner_id, study_id, status, language, requested_count, queued_at) VALUES('job-1', 'u1', $1, 'running', 'en', 5, 1)",
    )
    .bind(&sid)
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO generation_job_sources(job_id, source_id) VALUES('job-1', $1)")
        .bind(&src_id)
        .execute(&store.db)
        .await
        .unwrap();

    // Simulate FSM outage during deletion.
    fsm.fail_delete.store(true, Ordering::SeqCst);
    let (st, del_body) = call(
        &app,
        "DELETE",
        &format!("/studies/{sid}/sources/{src_id}"),
        Some("u1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(del_body["message"], "Removed from your workspace.");

    // Source row is physically removed and job is fenced.
    let src_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sources WHERE id = $1")
        .bind(&src_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(src_count, 0);
    let job_status: String =
        sqlx::query_scalar("SELECT status FROM generation_jobs WHERE id = 'job-1'")
            .fetch_one(&store.db)
            .await
            .unwrap();
    assert_eq!(job_status, "cancelled");

    // Simulate restart (new router instance with the same DB + FSM) and verify cleanup task persisted.
    let restarted = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    // Normal user cannot access admin cleanup route.
    let (st, _) = call(
        &restarted,
        "GET",
        "/admin/cleanup",
        Some("u1"),
        Some("3"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, pending) = call(&restarted, "GET", "/admin/cleanup", None, Some("1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(pending["pending_count"], 1);
    let cleanup_id = pending["items"][0]["id"].as_str().unwrap().to_string();

    // FSM recovers; admin retry removes blob and drops completed task.
    fsm.fail_delete.store(false, Ordering::SeqCst);
    let (st, retry) = call(
        &restarted,
        "POST",
        &format!("/admin/cleanup/{cleanup_id}/retry"),
        None,
        Some("1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(retry["status"], "completed");
    assert!(!fsm.has_blob(&fsm_ref));

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cleanup_tasks")
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn shared_blobs_are_never_forced_and_published_quizzes_require_admin_to_delete() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    let shared_ref = "shared/blob-1";
    fsm.seed(shared_ref, b"shared bytes");
    store.fsm = fsm.clone();

    sqlx::query(
        "INSERT INTO studies(id, owner_id, title, created_at, updated_at) VALUES('s-other', 'u2', 'Other', 1, 1)",
    )
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, fsm_ref, source_kind, status, created_at, updated_at)
         VALUES('src-other', 'u2', 's-other', 'a.txt', 'text/plain', 12, $1, 'fsm_ref', 'ready', 1, 1)",
    )
    .bind(shared_ref)
    .execute(&store.db)
    .await
    .unwrap();

    cleanup::delete_fsm_or_queue(&store, shared_ref)
        .await
        .unwrap();
    assert!(fsm.has_blob(shared_ref));

    // Owner cannot delete published quiz (403); admin can (200).
    sqlx::query(
        "INSERT INTO quizzes(id, owner_id, visibility, created_at, updated_at) VALUES('q-pub', 'u1', 'public', 1, 1)",
    )
    .execute(&store.db)
    .await
    .unwrap();

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    let (st, _) = call(&app, "DELETE", "/quizzes/q-pub", Some("u1"), None, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, body) = call(
        &app,
        "DELETE",
        "/admin/quizzes/q-pub",
        None,
        Some("1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["message"], "Removed from your workspace.");
}
