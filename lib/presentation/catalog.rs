use crate::{
    core::Result,
    domain::{
        App,
        catalog::{self, CatalogQuery, CatalogQuestionQuery},
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct PublishBody {
    title: Option<String>,
    tags: Option<Vec<String>>,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/catalog", get(list_catalog))
        .route("/catalog/questions", get(list_catalog_questions))
        .route("/quizzes/{quiz_id}", get(get_quiz))
        .route("/quizzes/{quiz_id}/statistics", get(get_quiz_statistics))
        .route("/studies/{study_id}/tags/suggestions", get(suggest_tags))
        .route("/studies/{study_id}/publication", post(publish_quiz))
}

async fn list_catalog(
    State(app): State<App>,
    Query(query): Query<CatalogQuery>,
) -> Result<Json<Value>> {
    let page = catalog::list_catalog(&app.store, query).await?;
    Ok(Json(json!(page)))
}

async fn list_catalog_questions(
    State(app): State<App>,
    Query(query): Query<CatalogQuestionQuery>,
) -> Result<Json<Value>> {
    let items = catalog::list_catalog_questions(&app.store, query).await?;
    Ok(Json(json!({ "items": items })))
}

async fn get_quiz(State(app): State<App>, Path(quiz_id): Path<String>) -> Result<Json<Value>> {
    let detail = catalog::get_public_quiz(&app.store, &quiz_id).await?;
    Ok(Json(json!(detail)))
}

async fn get_quiz_statistics(
    State(app): State<App>,
    Path(quiz_id): Path<String>,
) -> Result<Json<Value>> {
    let stats = catalog::get_public_quiz_statistics(&app.store, &quiz_id).await?;
    Ok(Json(json!(stats)))
}

async fn suggest_tags(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    let suggestions = catalog::suggest_study_tags(&app.store, &caller, &study_id).await?;
    Ok(Json(json!({ "suggestions": suggestions })))
}

async fn publish_quiz(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(body): Json<PublishBody>,
) -> Result<Json<Value>> {
    let published =
        catalog::publish_study_quiz(&app.store, &caller, &study_id, body.title, body.tags).await?;
    Ok(Json(json!(published)))
}
