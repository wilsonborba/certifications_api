pub mod admin;
pub mod auth;
pub mod logs;
pub mod studies;

use crate::{core::Result, domain::App};
use axum::{Json, Router, extract::State, routing::get};

pub fn router(app: App) -> Router {
    Router::new()
        .route("/logs/stream", get(logs::stream))
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route("/ready", get(ready))
        .merge(studies::routes())
        .merge(admin::routes())
        .layer(axum::middleware::from_fn(auth::admin_guard))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response
                    .headers_mut()
                    .insert("cache-control", "no-store".parse().unwrap());
                response
            },
        ))
        .layer(axum::middleware::from_fn(logs::request_log))
        .with_state(app)
}

async fn ready(State(app): State<App>) -> Result<Json<serde_json::Value>> {
    app.store.ready().await?;
    Ok(Json(serde_json::json!({ "status": "ready" })))
}
