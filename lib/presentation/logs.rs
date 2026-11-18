//! Trusted LAN stream. Public authentication belongs to api_for_apps.
use axum::{
    extract::Request,
    middleware::Next,
    response::{
        Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use futures_util::{Stream, stream};
use std::{convert::Infallible, time::Duration};
use tokio::sync::broadcast::error::RecvError;

pub async fn stream() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = crate::core::logs::sender().subscribe();
    let events = stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Ok(line) => Some((Ok(Event::default().event("log").data(line)), receiver)),
            Err(RecvError::Lagged(count)) => Some((
                Ok(Event::default()
                    .event("gap")
                    .data(format!("{count} log events skipped"))),
                receiver,
            )),
            Err(RecvError::Closed) => None,
        }
    });
    Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// Never record query strings, headers or request/response bodies.
pub async fn request_log(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    tracing::info!(module = "http", %method, %path, status = response.status().as_u16(), "Request completed");
    if path == "/logs/stream" {
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().unwrap());
        response
            .headers_mut()
            .insert("x-accel-buffering", "no".parse().unwrap());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, routing::get};
    use futures_util::StreamExt;
    use std::io::Write;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    #[tokio::test]
    async fn lan_stream_needs_no_auth_and_uses_process_writer() {
        let router = Router::new()
            .route("/logs/stream", get(stream))
            .layer(axum::middleware::from_fn(request_log));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/logs/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        let mut body = response.into_body().into_data_stream();
        let writer = crate::core::logs::LogWriter(crate::core::logs::sender());
        writeln!(
            writer.make_writer(),
            "2026-10-27T00:00:00Z INFO certifications test actual-writer"
        )
        .unwrap();
        let chunk = tokio::time::timeout(Duration::from_secs(1), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8_lossy(&chunk);
        assert!(text.contains("actual-writer"));
    }
}
