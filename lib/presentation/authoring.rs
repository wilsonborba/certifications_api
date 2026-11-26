use crate::{
    core::{Error, Result},
    domain::{
        App,
        authoring::{self, CreateQuestionInput, ReuseQuestionsInput, UpdateQuestionInput},
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
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

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/studies/{study_id}/questions",
            get(list_questions).post(create_question),
        )
        .route("/studies/{study_id}/questions/reuse", post(reuse_questions))
        .route(
            "/studies/{study_id}/questions/import",
            post(import_questions),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}",
            patch(update_question).delete(remove_question),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}/visual_jobs",
            post(prepare_visual_job),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}/visuals",
            post(upload_visual).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}/visuals/{visual_id}",
            get(get_question_visual),
        )
        .route(
            "/studies/{study_id}/questions/{question_id}/visual",
            post(legacy_visual_reject),
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

async fn reuse_questions(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(body): Json<ReuseQuestionsInput>,
) -> Result<Json<Value>> {
    let result = authoring::reuse_public_question_versions(
        &app.store,
        &caller,
        &study_id,
        &body.question_version_ids,
    )
    .await?;
    Ok(Json(json!(result)))
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

async fn prepare_visual_job(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
    Json(body): Json<authoring::PrepareVisualJobInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let res =
        authoring::prepare_question_visual_job(&app.store, &caller, &study_id, &question_id, body)
            .await?;
    Ok((StatusCode::CREATED, Json(json!(res))))
}

async fn upload_visual(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
    Json(body): Json<authoring::UploadVisualInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let v = authoring::upload_question_visual(&app.store, &caller, &study_id, &question_id, body)
        .await?;
    Ok((StatusCode::CREATED, Json(json!(v))))
}

async fn get_question_visual(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id, visual_id)): Path<(String, String, String)>,
) -> Result<axum::response::Response> {
    let (bytes, mime_type) =
        authoring::get_question_visual(&app.store, &caller, &study_id, &question_id, &visual_id)
            .await?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, mime_type)
        .header(axum::http::header::CACHE_CONTROL, "private, max-age=300")
        .body(axum::body::Body::from(bytes))
        .map_err(|_| Error::unavailable())
}

async fn legacy_visual_reject(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, question_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Value>)> {
    authoring::check_reused_question_rejection(&app.store, &caller, &study_id, &question_id)
        .await?;
    Err(Error::unprocessable(
        "deprecated_visual_endpoint: direct visual_spec rendering is obsolete; use 2-stage prepare (/visual_jobs) and upload (/visuals)",
    ))
}
