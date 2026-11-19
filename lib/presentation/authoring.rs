use crate::{
    core::{Error, Result},
    domain::{
        App,
        authoring::{self, CreateQuestionInput, UpdateQuestionInput},
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct CsvImportBody {
    csv_text: String,
    #[serde(default)]
    confirm: bool,
    origin: Option<Value>,
}

#[derive(Deserialize)]
struct VisualRenderBody {
    visual_spec: Value,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/studies/{study_id}/questions",
            get(list_questions).post(create_question),
        )
        .route(
            "/studies/{study_id}/questions/import",
            post(import_questions),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}",
            patch(update_question).delete(remove_question),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}/visual",
            post(render_visual),
        )
}

async fn list_questions(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    let items = authoring::list_study_questions(&app.store, &caller, &study_id).await?;
    Ok(Json(json!({ "items": items })))
}

async fn create_question(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(body): Json<CreateQuestionInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let q = authoring::create_question(&app.store, &caller, &study_id, body).await?;
    Ok((StatusCode::CREATED, Json(json!(q))))
}

async fn update_question(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
    Json(body): Json<UpdateQuestionInput>,
) -> Result<Json<Value>> {
    let q = authoring::update_question(&app.store, &caller, &study_id, &question_id, body).await?;
    Ok(Json(json!(q)))
}

async fn remove_question(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    authoring::remove_question(&app.store, &caller, &study_id, &question_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn import_questions(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(body): Json<CsvImportBody>,
) -> Result<Json<Value>> {
    if body.origin.is_some() {
        return Err(Error::unprocessable(
            "origin_not_allowed: Origin cannot be supplied by callers",
        ));
    }
    let res =
        authoring::import_csv(&app.store, &caller, &study_id, &body.csv_text, body.confirm).await?;
    Ok(Json(json!(res)))
}

async fn render_visual(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
    Json(body): Json<VisualRenderBody>,
) -> Result<(StatusCode, Json<Value>)> {
    let v = authoring::render_question_visual(
        &app.store,
        &caller,
        &study_id,
        &question_id,
        body.visual_spec,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(v))))
}
