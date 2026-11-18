use crate::{
    core::Result,
    domain::{App, cleanup},
    presentation::auth::AdminUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin/cleanup", get(list_cleanup))
        .route("/admin/cleanup/{cleanup_id}/retry", post(retry_cleanup))
        .route("/admin/quizzes/{quiz_id}", delete(remove_quiz))
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
