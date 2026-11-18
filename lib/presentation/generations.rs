use crate::{
    core::{Error, Result},
    domain::{
        App,
        auth::reject_caller_authority_fields,
        generations::{self, StartGenerationInput},
    },
    presentation::auth::SessionUser,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, time::Duration};

#[derive(Deserialize)]
pub struct EventsQuery {
    pub after: Option<i64>,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/studies/{study_id}/generations", post(start))
        .route("/generations/{job_id}", get(detail))
        .route("/generations/{job_id}/events", get(events))
        .route("/generations/{job_id}/stop", post(stop))
        .route("/generations/{job_id}/resume", post(resume))
}

async fn start(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(study_id): Path<String>,
    Json(raw): Json<Value>,
) -> Result<(StatusCode, Json<Value>)> {
    reject_caller_authority_fields(&raw)?;
    let input: StartGenerationInput =
        serde_json::from_value(raw).map_err(|_| Error::unprocessable("invalid_body"))?;
    let job = generations::start_generation(&app.store, &caller, &study_id, input).await?;
    Ok((StatusCode::CREATED, Json(json!(job))))
}

async fn detail(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>> {
    let job = generations::get_job(&app.store, &caller, &job_id).await?;
    Ok(Json(json!(job)))
}

async fn events(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(job_id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Response> {
    let rows = generations::list_events(&app.store, &caller, &job_id, q.after.unwrap_or(0)).await?;
    let items: Vec<std::result::Result<Event, Infallible>> = rows
        .into_iter()
        .map(|ev| {
            let data = serde_json::to_string(&ev).unwrap_or_default();
            Ok(Event::default()
                .id(ev.seq.to_string())
                .event(ev.event_type)
                .data(data))
        })
        .collect();
    let mut resp = Sse::new(stream::iter(items))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    resp.headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    resp.headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(resp)
}

async fn stop(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>> {
    let job = generations::stop_generation(&app.store, &caller, &job_id).await?;
    Ok(Json(json!(job)))
}

async fn resume(
    State(app): State<App>,
    SessionUser(caller): SessionUser,
    Path(job_id): Path<String>,
) -> Result<(StatusCode, Json<Value>)> {
    let job = generations::resume_generation(&app.store, &caller, &job_id).await?;
    Ok((StatusCode::CREATED, Json(json!(job))))
}
