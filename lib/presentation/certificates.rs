use crate::{
    core::Result,
    domain::{App, certificates},
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct IdentityBody {
    display_name: String,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/me/identity", get(get_identity).put(put_identity))
        .route(
            "/attempts/{attempt_id}/certificate",
            post(issue_certificate),
        )
        .route("/certificates/{certificate_code}", get(verify_certificate))
        .route(
            "/certificates/{certificate_code}/files/{format}",
            get(download_certificate_file),
        )
}

async fn get_identity(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
) -> Result<Json<Value>> {
    let id = certificates::get_my_identity(&app.store, &caller).await?;
    Ok(Json(json!(id)))
}

async fn put_identity(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Json(body): Json<IdentityBody>,
) -> Result<Json<Value>> {
    let id = certificates::confirm_my_identity(&app.store, &caller, &body.display_name).await?;
    Ok(Json(json!(id)))
}

async fn issue_certificate(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(attempt_id): Path<String>,
) -> Result<Json<Value>> {
    let cert = certificates::issue_or_get_certificate(&app.store, &caller, &attempt_id).await?;
    Ok(Json(json!(cert)))
}

async fn verify_certificate(
    State(app): State<App>,
    Path(certificate_code): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let accept_language = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|val| val.to_str().ok());
    let info =
        certificates::verify_certificate(&app.store, &certificate_code, accept_language).await?;
    Ok(Json(json!(info)))
}

async fn download_certificate_file(
    State(app): State<App>,
    Path((certificate_code, format)): Path<(String, String)>,
) -> Result<impl IntoResponse> {
    let (content_type, bytes) =
        certificates::get_certificate_file(&app.store, &certificate_code, &format).await?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    ))
}
