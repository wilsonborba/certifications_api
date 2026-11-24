use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use certifications_api::dal::cortex::{GenerationRequest, GenerationSource, HttpCortex};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<RequestRecord>>>);
type RequestRecord = (String, Vec<u8>);

async fn upload(State(seen): State<Seen>, request: Request<Body>) -> (StatusCode, Json<Value>) {
    assert_eq!(request.headers()["x-uuid"], "owner-1");
    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
    seen.0
        .lock()
        .unwrap()
        .push(("/files".into(), body.to_vec()));
    (
        StatusCode::CREATED,
        Json(json!({"data": {"file_id": "temporary-file-1"}})),
    )
}

async fn chat(State(seen): State<Seen>, request: Request<Body>) -> (StatusCode, Json<Value>) {
    assert_eq!(request.headers()["x-uuid"], "owner-1");
    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
    seen.0
        .lock()
        .unwrap()
        .push(("/chats".into(), body.to_vec()));
    (
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "session_id": "session-1", "input_id": "input-1", "output_id": "output-1",
                "status": "done",
                "text": "[{\"prompt\":\"Q?\",\"alternatives\":[{\"id\":\"a\",\"text\":\"A\"}],\"correct_id\":\"a\",\"explanation\":\"because\",\"source_id\":\"source-1\",\"citation_ref\":\"p1:0-3\"}]"
            }
        })),
    )
}

#[tokio::test]
async fn uploads_selected_bytes_then_uses_the_exact_temporary_sync_chat_contract() {
    let seen = Seen::default();
    let app = Router::new()
        .route("/files", post(upload))
        .route("/chats", post(chat))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = HttpCortex::client(&format!("http://{address}"), reqwest::Client::new());
    let request = GenerationRequest {
        owner_id: "owner-1".into(),
        language: "en".into(),
        count: 1,
        source_ids: vec!["source-1".into()],
        sources: vec![GenerationSource {
            source_id: "source-1".into(),
            filename: "source-source-1.txt".into(),
            mime_type: "text/plain".into(),
            selection: "p1:0-3".into(),
            bytes: b"ATP".to_vec(),
        }],
        existing_prompts: vec![],
    };
    let result = client.generate_questions(&request).await.unwrap();
    assert_eq!(result.session_id, "session-1");
    assert_eq!(result.output_id, "output-1");
    assert_eq!(result.questions.len(), 1);

    let seen = seen.0.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].1.windows(3).any(|w| w == b"ATP"));
    let temporary = b"name=\"temporary\"\r\n\r\ntrue";
    let app_id = b"name=\"app_id\"\r\n\r\ncertifications";
    assert!(seen[0].1.windows(temporary.len()).any(|w| w == temporary));
    assert!(seen[0].1.windows(app_id.len()).any(|w| w == app_id));
    let chat: Value = serde_json::from_slice(&seen[1].1).unwrap();
    assert_eq!(
        chat,
        json!({
            "app_id":"certifications", "text":chat["text"], "file_ids":["temporary-file-1"],
            "stream":false, "needs_web":false, "web_mode":"fast", "use_memory":false,
            "memory_topic":null, "thinking":false, "persona_id":null, "temporary":true
        })
    );
    assert!(chat["text"].as_str().unwrap().contains("p1:0-3"));
    server.abort();
}
