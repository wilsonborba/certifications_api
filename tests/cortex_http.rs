use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use certifications_api::dal::cortex::{
    GeneratedAlternative, GeneratedQuestion, GenerationRequest, GenerationSource, HttpCortex,
};
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
            selection_kind: "all".into(),
            selection_spec: String::new(),
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
    let selection_kind = b"name=\"selection_kind\"\r\n\r\nall";
    let selection_spec = b"name=\"selection_spec\"\r\n\r\n";
    assert!(seen[0].1.windows(temporary.len()).any(|w| w == temporary));
    assert!(seen[0].1.windows(app_id.len()).any(|w| w == app_id));
    assert!(
        seen[0]
            .1
            .windows(selection_kind.len())
            .any(|w| w == selection_kind)
    );
    assert!(
        seen[0]
            .1
            .windows(selection_spec.len())
            .any(|w| w == selection_spec)
    );
    let chat: Value = serde_json::from_slice(&seen[1].1).unwrap();
    assert_eq!(
        chat,
        json!({
            "app_id":"certifications", "model_id":"cortex-t0", "text":chat["text"], "file_ids":["temporary-file-1"],
            "stream":false, "needs_web":false, "web_mode":"fast", "use_memory":false,
            "memory_topic":null, "thinking":false, "persona_id":null, "temporary":true
        })
    );
    assert!(chat["text"].as_str().unwrap().contains("p1:0-3"));
    server.abort();
}

#[tokio::test]
async fn language_correction_pins_the_existing_t0_model() {
    let seen = Seen::default();
    let app = Router::new()
        .route("/chats", post(chat))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = HttpCortex::client(&format!("http://{address}"), reqwest::Client::new());
    let question = GeneratedQuestion {
        prompt: "Question?".into(),
        alternatives: vec![GeneratedAlternative {
            id: "a".into(),
            text: "Answer".into(),
        }],
        correct_id: "a".into(),
        explanation: "Reason".into(),
        source_id: "source-1".into(),
        citation_ref: "p1:0-3".into(),
    };

    let corrected = client
        .correct_language("owner-1", "pt-BR", &[question])
        .await
        .unwrap();

    assert_eq!(corrected.len(), 1);
    let seen = seen.0.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let chat: Value = serde_json::from_slice(&seen[0].1).unwrap();
    assert_eq!(chat["app_id"], "certifications");
    assert_eq!(chat["model_id"], "cortex-t0");
    assert_eq!(chat["file_ids"], json!([]));
    server.abort();
}

#[tokio::test]
async fn uploads_original_media_and_selection_fields_to_the_cortex_files_bridge() {
    let seen = Seen::default();
    let app = Router::new()
        .route("/files", post(upload))
        .route("/chats", post(chat))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = HttpCortex::client(&format!("http://{address}"), reqwest::Client::new());
    let pdf_bytes = b"%PDF-1.4\noriginal selected PDF bytes".to_vec();
    let request = GenerationRequest {
        owner_id: "owner-1".into(),
        language: "en".into(),
        count: 1,
        source_ids: vec!["source-pdf".into()],
        sources: vec![GenerationSource {
            source_id: "source-pdf".into(),
            filename: "source-source-pdf.pdf".into(),
            mime_type: "application/pdf".into(),
            selection: "pages:1-2".into(),
            selection_kind: "pages".into(),
            selection_spec: "1-2".into(),
            bytes: pdf_bytes.clone(),
        }],
        existing_prompts: vec![],
    };
    client.generate_questions(&request).await.unwrap();

    let seen = seen.0.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0]
            .1
            .windows(pdf_bytes.len())
            .any(|window| window == pdf_bytes)
    );
    let selection_kind = b"name=\"selection_kind\"\r\n\r\npages";
    let selection_spec = b"name=\"selection_spec\"\r\n\r\n1-2";
    assert!(
        seen[0]
            .1
            .windows(selection_kind.len())
            .any(|w| w == selection_kind)
    );
    assert!(
        seen[0]
            .1
            .windows(selection_spec.len())
            .any(|w| w == selection_spec)
    );
    server.abort();
}
