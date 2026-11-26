use certifications_api::{
    core::{Settings, logs},
    dal::{
        Store,
        scanner::{SignatureHealth, SignatureState},
    },
    domain::App,
    presentation,
};
use futures_util::StreamExt;
use std::io::Write;
use std::time::Duration;
use tracing_subscriber::fmt::MakeWriter;

fn fixture_settings() -> Settings {
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

#[tokio::test]
async fn service_starts_on_test_port_and_answers_health_ready_and_log_stream() {
    let store = Store::fixture().await;
    let settings = fixture_settings();
    let signatures = SignatureState::new(SignatureHealth::Missing);
    let app = presentation::router(App {
        store,
        settings,
        quotas: Default::default(),
        signatures: signatures.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let health = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"status": "ok"})
    );

    let ready = client
        .get(format!("http://{addr}/ready"))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), 200);
    assert_eq!(
        ready.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"status": "ready", "signatures": "missing", "scanner_binary": "missing"})
    );
    signatures.set(SignatureHealth::Fresh);
    let ready_fresh = client
        .get(format!("http://{addr}/ready"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ready_fresh.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"status": "ready", "signatures": "fresh", "scanner_binary": "missing"})
    );

    let stream_resp = client
        .get(format!("http://{addr}/logs/stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream_resp.status(), 200);
    assert_eq!(stream_resp.headers()["content-type"], "text/event-stream");
    assert_eq!(stream_resp.headers()["cache-control"], "no-store");
    assert_eq!(stream_resp.headers()["x-accel-buffering"], "no");

    let mut bytes = stream_resp.bytes_stream();
    let writer = logs::LogWriter(logs::sender());
    writeln!(
        writer.make_writer(),
        "2026-11-18T00:00:00Z INFO certifications test gateway-sse-check"
    )
    .unwrap();
    let chunk = tokio::time::timeout(Duration::from_secs(2), bytes.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("gateway-sse-check"));

    server.abort();
}
