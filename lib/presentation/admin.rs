use crate::{
    core::Result,
    domain::{App, authoring, certificates, cleanup},
    presentation::auth::AdminUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{delete, get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
struct OriginOverrideBody {
    origin: String,
    #[serde(default)]
    apply_to_questions: bool,
}

#[derive(Deserialize)]
struct HolderUpdateBody {
    display_name: String,
}

#[derive(Deserialize)]
struct RevokeBody {
    reason: String,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin/cleanup", get(list_cleanup))
        .route("/admin/cleanup/{cleanup_id}/retry", post(retry_cleanup))
        .route("/admin/quizzes/{quiz_id}", delete(remove_quiz))
        .route(
            "/admin/quizzes/{quiz_id}/origin",
            patch(override_quiz_origin),
        )
        .route("/admin/holders/{holder_id}", patch(update_holder))
        .route(
            "/admin/certificates/{certificate_code}/revocation",
            post(revoke_certificate),
        )
}

async fn list_cleanup(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Query(q): Query<ListParams>,
) -> Result<Json<Value>> {
    let summary = cleanup::list_cleanup(&app.store, q.limit.unwrap_or(50)).await?;
    Ok(Json(json!(summary)))
}

async fn retry_cleanup(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path(cleanup_id): Path<String>,
) -> Result<Json<Value>> {
    let ok = cleanup::retry_one(&app.store, &cleanup_id).await?;
    Ok(Json(json!({
        "cleanup_id": cleanup_id,
        "status": if ok { "completed" } else { "failed" }
    })))
}

async fn remove_quiz(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
    Path(quiz_id): Path<String>,
) -> Result<Json<Value>> {
    cleanup::delete_quiz(&app.store, admin.as_ref(), &quiz_id, true).await?;
    Ok(Json(json!({ "message": cleanup::REMOVED_MESSAGE })))
}

async fn override_quiz_origin(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
    Path(quiz_id): Path<String>,
    Json(body): Json<OriginOverrideBody>,
) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| crate::core::Error::forbidden("admin_required: Admin access required"))?;
    authoring::admin_override_quiz_origin(
        &app.store,
        caller,
        &quiz_id,
        &body.origin,
        body.apply_to_questions,
    )
    .await?;
    Ok(Json(json!({ "quiz_id": quiz_id, "origin": body.origin })))
}

async fn update_holder(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
    Path(holder_id): Path<String>,
    Json(body): Json<HolderUpdateBody>,
) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| crate::core::Error::forbidden("admin_required: Admin access required"))?;
    let updated =
        certificates::admin_update_holder(&app.store, caller, &holder_id, &body.display_name)
            .await?;
    Ok(Json(json!(updated)))
}

async fn revoke_certificate(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
    Path(certificate_code): Path<String>,
    Json(body): Json<RevokeBody>,
) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| crate::core::Error::forbidden("admin_required: Admin access required"))?;
    certificates::admin_revoke_certificate(&app.store, caller, &certificate_code, &body.reason)
        .await?;
    Ok(Json(
        json!({ "verification_code": certificate_code, "status": "revoked" }),
    ))
}
