use crate::{
    core::Result,
    domain::{
        App,
        attempts::{self, ActivityEventInput, AttemptHistoryFilter},
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct ListParams {
    limit: Option<i64>,
    offset: Option<i64>,
    q: Option<String>,
    started_from: Option<i64>,
    started_to: Option<i64>,
    quiz_id: Option<String>,
    tag: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct AnswerBody {
    selected_option_id: String,
}

#[derive(Deserialize)]
struct EventsBody {
    events: Vec<ActivityEventInput>,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/quizzes/{quiz_id}/attempts", post(start_attempt))
        .route("/attempts", get(list_attempts))
        .route("/attempts/overview", get(get_personal_statistics))
        .route("/attempts/{attempt_id}", get(get_attempt))
        .route(
            "/attempts/{attempt_id}/answers/{question_id}",
            put(save_answer),
        )
        .route("/attempts/{attempt_id}/events", post(record_events))
        .route("/attempts/{attempt_id}/submit", post(submit_attempt))
        .route("/attempts/{attempt_id}/results", get(get_results))
        .route(
            "/attempts/{attempt_id}/question_versions/{question_version_id}/visuals/{visual_id}",
            get(get_attempt_question_visual),
        )
}

async fn start_attempt(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(quiz_id): Path<String>,
) -> Result<(StatusCode, Json<Value>)> {
    let view = attempts::start_attempt(&app.store, &caller, &quiz_id).await?;
    Ok((StatusCode::CREATED, Json(json!(view))))
}

async fn list_attempts(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Query(q): Query<ListParams>,
) -> Result<Json<Value>> {
    let page = attempts::list_my_attempts(
        &app.store,
        &caller,
        &AttemptHistoryFilter {
            search: q.q,
            started_from: q.started_from,
            started_to: q.started_to,
            quiz_id: q.quiz_id,
            tag: q.tag,
            status: q.status,
        },
        q.limit.unwrap_or(20),
        q.offset.unwrap_or(0),
    )
    .await?;
    Ok(Json(json!(page)))
}

async fn get_personal_statistics(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
) -> Result<Json<Value>> {
    let stats = attempts::personal_statistics(&app.store, &caller).await?;
    Ok(Json(json!(stats)))
}

async fn get_attempt(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(attempt_id): Path<String>,
) -> Result<Json<Value>> {
    let view = attempts::get_attempt(&app.store, &caller, &attempt_id).await?;
    Ok(Json(json!(view)))
}

async fn save_answer(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((attempt_id, question_id)): Path<(String, String)>,
    Json(body): Json<AnswerBody>,
) -> Result<Json<Value>> {
    attempts::save_answer(
        &app.store,
        &caller,
        &attempt_id,
        &question_id,
        &body.selected_option_id,
    )
    .await?;
    Ok(Json(json!({ "saved": true })))
}

async fn record_events(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(attempt_id): Path<String>,
    Json(body): Json<EventsBody>,
) -> Result<Json<Value>> {
    let inserted =
        attempts::record_activity_events(&app.store, &caller, &attempt_id, body.events).await?;
    Ok(Json(json!({ "inserted": inserted })))
}

async fn submit_attempt(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(attempt_id): Path<String>,
) -> Result<Json<Value>> {
    let res = attempts::submit_attempt(&app.store, &caller, &attempt_id).await?;
    Ok(Json(json!(res)))
}

async fn get_results(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(attempt_id): Path<String>,
) -> Result<Json<Value>> {
    let res = attempts::get_results(&app.store, &caller, &attempt_id).await?;
    Ok(Json(json!(res)))
}

async fn get_attempt_question_visual(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((attempt_id, question_version_id, visual_id)): Path<(String, String, String)>,
) -> crate::core::Result<axum::response::Response> {
    let (bytes, mime_type) = attempts::get_attempt_question_visual(
        &app.store,
        &caller,
        &attempt_id,
        &question_version_id,
        &visual_id,
    )
    .await?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, mime_type)
        .header(axum::http::header::CACHE_CONTROL, "private, no-store")
        .header(axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(axum::body::Body::from(bytes))
        .map_err(|_| crate::core::Error::unavailable())
}
