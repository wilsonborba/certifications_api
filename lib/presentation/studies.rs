use crate::{
    core::{Error, Result},
    domain::{
        App,
        auth::reject_caller_authority_fields,
        studies::{
            self, CreateStudyInput, PreparedStudyInput, ReferenceSourceInput, SelectionInput,
            UpdateStudyInput,
        },
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadSourceBody {
    pub kind: Option<String>,
    pub filename: String,
    #[serde(default = "default_mime")]
    pub mime_type: String,
    pub content_text: Option<String>,
    pub external_ref: Option<String>,
    pub fsm_ref: Option<String>,
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub is_restricted: bool,
}

fn default_mime() -> String {
    "text/plain".to_string()
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/studies", get(list).post(create))
        .route("/studies/prepared", post(create_prepared))
        .route(
            "/studies/{study_id}",
            get(detail).patch(update).delete(remove),
        )
        .route(
            "/studies/{study_id}/sources",
            post(add_source).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/studies/{study_id}/sources/{source_id}",
            get(get_source).delete(remove_source),
        )
        .route(
            "/studies/{study_id}/sources/{source_id}/content",
            get(content_source),
        )
        .route(
            "/studies/{study_id}/sources/{source_id}/preview",
            get(preview_source),
        )
        .route(
            "/studies/{study_id}/sources/{source_id}/selection",
            patch(update_selection),
        )
        .route(
            "/studies/{study_id}/analysis",
            post(run_analysis).get(get_analysis),
        )
        .route("/reports/picker", get(reports_picker))
        .route("/quizzes/{quiz_id}", axum::routing::delete(remove_quiz))
}

async fn list(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Query(q): Query<ListParams>,
) -> Result<Json<Value>> {
    let items = studies::list_studies(&app.store, &caller, q.limit.unwrap_or(20)).await?;
    Ok(Json(json!({ "items": items })))
}

async fn create(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Json(raw): Json<Value>,
) -> Result<(StatusCode, Json<Value>)> {
    reject_caller_authority_fields(&raw)?;
    let input: CreateStudyInput =
        serde_json::from_value(raw).map_err(|_| Error::unprocessable("invalid_body"))?;
    let study = studies::create_study(&app.store, &caller, input).await?;
    Ok((StatusCode::CREATED, Json(json!(study))))
}

async fn create_prepared(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Json(raw): Json<Value>,
) -> Result<(StatusCode, Json<Value>)> {
    reject_caller_authority_fields(&raw)?;
    let input: PreparedStudyInput =
        serde_json::from_value(raw).map_err(|_| Error::unprocessable("invalid_body"))?;
    let detail = studies::create_prepared_study(&app.store, &caller, input, &app.quotas).await?;
    Ok((StatusCode::CREATED, Json(json!(detail))))
}

async fn detail(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    let detail = studies::get_study(&app.store, &caller, &study_id, &app.quotas).await?;
    Ok(Json(json!(detail)))
}

async fn update(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(raw): Json<Value>,
) -> Result<Json<Value>> {
    reject_caller_authority_fields(&raw)?;
    let input: UpdateStudyInput =
        serde_json::from_value(raw).map_err(|_| Error::unprocessable("invalid_body"))?;
    let study = studies::update_study(&app.store, &caller, &study_id, input).await?;
    Ok(Json(json!(study)))
}

async fn remove(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    studies::delete_study(&app.store, &caller, &study_id).await?;
    Ok(Json(json!({
        "message": crate::domain::cleanup::REMOVED_MESSAGE
    })))
}

async fn add_source(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    req: Request,
) -> Result<(StatusCode, Json<Value>)> {
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/form-data"));

    if is_multipart {
        let _ = studies::get_study(&app.store, &caller, &study_id, &app.quotas).await?;
        let mut multipart = Multipart::from_request(req, &app)
            .await
            .map_err(|_| Error::unprocessable("invalid_multipart: Malformed multipart body"))?;
        let max_file_bytes = app.quotas.get().max_file_bytes.max(0) as usize;
        let mut filename: Option<String> = None;
        let mut mime_type: Option<String> = None;
        let mut file_bytes: Option<Vec<u8>> = None;

        while let Some(mut field) = multipart.next_field().await.map_err(|_| {
            Error::unprocessable("invalid_multipart: Failed reading multipart field")
        })? {
            match field.name() {
                Some("file") => {
                    if filename.is_none()
                        && let Some(name) = field.file_name()
                    {
                        filename = Some(name.to_string());
                    }
                    if mime_type.is_none()
                        && let Some(ct) = field.content_type()
                    {
                        mime_type = Some(ct.to_string());
                    }
                    let mut buf = Vec::new();
                    while let Some(chunk) = field.chunk().await.map_err(|_| {
                        Error::unprocessable("invalid_multipart: Truncated file upload")
                    })? {
                        if buf.len().saturating_add(chunk.len()) > max_file_bytes {
                            return Err(Error::unprocessable(
                                "quota_exceeded: File size exceeds study quota",
                            ));
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    file_bytes = Some(buf);
                }
                Some("filename") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|_| Error::unprocessable("invalid_multipart: Invalid filename"))?;
                    if !val.trim().is_empty() {
                        filename = Some(val);
                    }
                }
                Some("mime_type") => {
                    let val = field.text().await.map_err(|_| {
                        Error::unprocessable("invalid_multipart: Invalid mime_type")
                    })?;
                    if !val.trim().is_empty() {
                        mime_type = Some(val);
                    }
                }
                _ => {}
            }
        }

        let bytes = file_bytes
            .ok_or_else(|| Error::unprocessable("invalid_source: file part is required"))?;
        let fname = filename.unwrap_or_default();
        let mime = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());
        let src = studies::add_source_upload(
            &app.store,
            &caller,
            &study_id,
            &fname,
            &mime,
            &bytes,
            &app.quotas,
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(json!(src))));
    }

    let raw_bytes = axum::body::Bytes::from_request(req, &app)
        .await
        .map_err(|_| Error::unprocessable("invalid_body"))?;
    let raw: Value =
        serde_json::from_slice(&raw_bytes).map_err(|_| Error::unprocessable("invalid_body"))?;
    reject_caller_authority_fields(&raw)?;
    let input: UploadSourceBody =
        serde_json::from_value(raw).map_err(|_| Error::unprocessable("invalid_body"))?;
    if let Some(kind) = input.kind.as_deref()
        && matches!(kind, "fsm_ref" | "link" | "report")
    {
        let src = studies::add_source_reference(
            &app.store,
            &caller,
            &study_id,
            ReferenceSourceInput {
                kind: kind.to_string(),
                filename: input.filename,
                mime_type: input.mime_type,
                external_ref: input.external_ref.unwrap_or_default(),
                fsm_ref: input.fsm_ref.unwrap_or_default(),
                size_bytes: input.size_bytes.unwrap_or(0),
                is_restricted: input.is_restricted,
            },
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(json!(src))));
    }
    let text = input
        .content_text
        .ok_or_else(|| Error::unprocessable("invalid_source: content_text is required"))?;
    let src = studies::add_source_upload(
        &app.store,
        &caller,
        &study_id,
        &input.filename,
        &input.mime_type,
        text.as_bytes(),
        &app.quotas,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(src))))
}

async fn get_source(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, source_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let src = studies::get_source(&app.store, &caller, &study_id, &source_id).await?;
    Ok(Json(json!(src)))
}

async fn content_source(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, source_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let (src, bytes) =
        studies::get_source_content(&app.store, &caller, &study_id, &source_id).await?;
    let total = bytes.len();
    let content_type = header::HeaderValue::from_str(&src.mime_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));

    if let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        let Some((start, end)) = studies::parse_byte_range(range_hdr, total) else {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .unwrap());
        };
        let slice = bytes[start..=end].to_vec();
        return Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )
            .header(header::CONTENT_LENGTH, slice.len().to_string())
            .body(Body::from(slice))
            .unwrap());
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total.to_string())
        .body(Body::from(bytes))
        .unwrap())
}

async fn preview_source(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, source_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let p = studies::preview_source(&app.store, &caller, &study_id, &source_id).await?;
    Ok(Json(p))
}

async fn update_selection(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, source_id)): Path<(String, String)>,
    Json(input): Json<SelectionInput>,
) -> Result<Json<Value>> {
    let src = studies::update_selection(&app.store, &caller, &study_id, &source_id, input).await?;
    Ok(Json(json!(src)))
}

async fn remove_source(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path((study_id, source_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    studies::delete_source(&app.store, &caller, &study_id, &source_id).await?;
    Ok(Json(json!({
        "message": crate::domain::cleanup::REMOVED_MESSAGE
    })))
}

async fn remove_quiz(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(quiz_id): Path<String>,
) -> Result<Json<Value>> {
    crate::domain::cleanup::delete_quiz(&app.store, Some(&caller), &quiz_id, false).await?;
    Ok(Json(json!({
        "message": crate::domain::cleanup::REMOVED_MESSAGE
    })))
}

async fn run_analysis(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    let a = studies::analyze_study(&app.store, &caller, &study_id).await?;
    Ok(Json(json!(a)))
}

async fn get_analysis(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
) -> Result<Json<Value>> {
    let a = studies::get_analysis(&app.store, &caller, &study_id).await?;
    Ok(Json(json!(a)))
}

#[derive(Deserialize)]
pub struct PickerQuery {
    pub project: Option<String>,
}

async fn reports_picker(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Query(q): Query<PickerQuery>,
) -> Result<Json<Value>> {
    let items = app
        .store
        .reports
        .list_readable_reports(&caller.user_id, q.project.as_deref())
        .await?;
    Ok(Json(json!({ "items": items })))
}
