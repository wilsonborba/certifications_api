pub mod admin;
pub mod attempts;
pub mod auth;
pub mod authoring;
pub mod catalog;
pub mod certificates;
pub mod generations;
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
        .merge(generations::routes())
        .merge(authoring::routes())
        .merge(catalog::routes())
        .merge(attempts::routes())
        .merge(certificates::routes())
        .merge(admin::routes())
        .layer(axum::middleware::from_fn(auth::admin_guard))
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                let cache_control =
                    safe_cache_control(response.headers().get(axum::http::header::CACHE_CONTROL));
                response
                    .headers_mut()
                    .insert(axum::http::header::CACHE_CONTROL, cache_control);
                response
            },
        ))
        .layer(axum::middleware::from_fn(logs::request_log))
        .with_state(app)
}

fn safe_cache_control(value: Option<&axum::http::HeaderValue>) -> axum::http::HeaderValue {
    let already_no_store = value
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-store"))
        });
    if already_no_store {
        value.unwrap().clone()
    } else {
        axum::http::HeaderValue::from_static("no-store")
    }
}

async fn ready(State(app): State<App>) -> Result<Json<serde_json::Value>> {
    app.store.ready().await?;
    let signatures = app.signatures.get();
    let scanner_binary = if std::path::Path::new(&app.settings.clamav.clamscan_bin).is_file() {
        "present"
    } else {
        "missing"
    };
    Ok(Json(serde_json::json!({
        "status": "ready",
        "signatures": signatures,
        "scanner_binary": scanner_binary,
    })))
}

#[cfg(test)]
mod cache_control_tests {
    use super::safe_cache_control;
    use axum::http::HeaderValue;

    #[test]
    fn defaults_to_no_store_and_preserves_only_strict_no_store_directives() {
        assert_eq!(safe_cache_control(None), "no-store");
        assert_eq!(
            safe_cache_control(Some(&HeaderValue::from_static("private, max-age=300"))),
            "no-store"
        );
        assert_eq!(
            safe_cache_control(Some(&HeaderValue::from_static("private, no-store"))),
            "private, no-store"
        );
    }
}
