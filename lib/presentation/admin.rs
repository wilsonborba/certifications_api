use crate::{
    core::{Error, Result},
    dal::schemas::{self, DraftSchema, SchemaField},
    domain::{
        App,
        admin::{self, UpdateSettingInput},
        authoring, certificates, cleanup,
    },
    presentation::auth::AdminUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
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

#[derive(Deserialize)]
struct SettingsPatchBody {
    items: Vec<UpdateSettingInput>,
}

#[derive(Deserialize)]
struct SchemaDraftBody {
    version: Option<i64>,
    #[serde(default)]
    description: String,
    fields: Vec<SchemaField>,
}

#[derive(Deserialize)]
struct SchemaStatusBody {
    status: String,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin/settings", get(get_settings).patch(patch_settings))
        .route(
            "/admin/schemas/{schema_name}/versions",
            get(list_schema_versions).post(create_schema_version),
        )
        .route(
            "/admin/schemas/{schema_name}/versions/{version}",
            get(get_schema_version)
                .patch(update_schema_version)
                .delete(delete_schema_version),
        )
        .route(
            "/admin/schemas/{schema_name}/versions/{version}/status",
            patch(update_schema_status),
        )
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
        .route("/admin/statistics", get(get_statistics))
}

async fn get_settings(State(app): State<App>, AdminUser(admin): AdminUser) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
    let items = admin::get_settings(&app.store, caller).await?;
    Ok(Json(json!({ "items": items })))
}

async fn patch_settings(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
    Json(body): Json<SettingsPatchBody>,
) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
    let items = admin::patch_settings(&app.store, caller, body.items).await?;
    Ok(Json(json!({ "items": items })))
}

async fn list_schema_versions(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path(schema_name): Path<String>,
) -> Result<Json<Value>> {
    let items = schemas::list_versions(&app.store.db, &schema_name).await?;
    Ok(Json(json!({ "items": items })))
}

async fn create_schema_version(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path(schema_name): Path<String>,
    Json(body): Json<SchemaDraftBody>,
) -> Result<(StatusCode, Json<Value>)> {
    let version = body.version.ok_or_else(|| {
        Error::unprocessable("invalid_schema_version: Positive version is required")
    })?;
    let draft = DraftSchema {
        schema_name: schema_name.clone(),
        version,
        description: body.description,
        fields: body.fields,
    };
    schemas::save_draft(&app.store.db, &draft).await?;
    let detail = schemas::get_version(&app.store.db, &schema_name, version).await?;
    Ok((StatusCode::CREATED, Json(json!(detail))))
}

async fn get_schema_version(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path((schema_name, version)): Path<(String, i64)>,
) -> Result<Json<Value>> {
    let detail = schemas::get_version(&app.store.db, &schema_name, version).await?;
    Ok(Json(json!(detail)))
}

async fn update_schema_version(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path((schema_name, version)): Path<(String, i64)>,
    Json(body): Json<SchemaDraftBody>,
) -> Result<Json<Value>> {
    let draft = DraftSchema {
        schema_name: schema_name.clone(),
        version,
        description: body.description,
        fields: body.fields,
    };
    schemas::save_draft(&app.store.db, &draft).await?;
    let detail = schemas::get_version(&app.store.db, &schema_name, version).await?;
    Ok(Json(json!(detail)))
}

async fn delete_schema_version(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path((schema_name, version)): Path<(String, i64)>,
) -> Result<StatusCode> {
    schemas::delete_draft(&app.store.db, &schema_name, version).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_schema_status(
    State(app): State<App>,
    AdminUser(_admin): AdminUser,
    Path((schema_name, version)): Path<(String, i64)>,
    Json(body): Json<SchemaStatusBody>,
) -> Result<Json<Value>> {
    match body.status.as_str() {
        "published" => schemas::publish(&app.store.db, &schema_name, version).await?,
        "retired" => schemas::retire(&app.store.db, &schema_name, version).await?,
        _ => {
            return Err(Error::unprocessable(
                "invalid_schema_status: Status must be published or retired",
            ));
        }
    }
    let detail = schemas::get_version(&app.store.db, &schema_name, version).await?;
    Ok(Json(json!(detail)))
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
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
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
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
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
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
    certificates::admin_revoke_certificate(&app.store, caller, &certificate_code, &body.reason)
        .await?;
    Ok(Json(
        json!({ "verification_code": certificate_code, "status": "revoked" }),
    ))
}

async fn get_statistics(
    State(app): State<App>,
    AdminUser(admin): AdminUser,
) -> Result<Json<Value>> {
    let caller = admin
        .as_ref()
        .ok_or_else(|| Error::forbidden("admin_required: Admin access required"))?;
    let stats = admin::get_statistics(&app.store, caller).await?;
    Ok(Json(json!(stats)))
}
