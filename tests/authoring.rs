use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
};
use certifications_api::{
    core::{Error as CoreError, Result as CoreResult, Settings},
    dal::{
        Store,
        documents::Documents,
        fsm::MemoryFsm,
        reports::{
            BoxFuture, CreateRenderJobInput, ReadableReportDataset, RenderJobDescriptor,
            RenderedVisual, ReportDatasetMaterial, ReportPickerItem, ReportsClient,
        },
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        App,
        generations::{QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION},
        quotas::QuotaCache,
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

struct MockReports;

#[derive(Clone, Default)]
struct CouchTombstoneFixture {
    documents: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    tombstones: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    delete_count: Arc<AtomicUsize>,
}

async fn couch_fixture_put(
    State(couch): State<CouchTombstoneFixture>,
    Path((_database, doc_id)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> StatusCode {
    if couch.documents.lock().unwrap().contains_key(&doc_id)
        || couch.tombstones.lock().unwrap().contains_key(&doc_id)
    {
        return StatusCode::CONFLICT;
    }
    body["_id"] = json!(doc_id);
    body["_rev"] = json!("1-created");
    couch.documents.lock().unwrap().insert(doc_id, body);
    StatusCode::CREATED
}

async fn couch_fixture_get(
    State(couch): State<CouchTombstoneFixture>,
    Path((_database, doc_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    if let Some(body) = couch.documents.lock().unwrap().get(&doc_id).cloned() {
        return (StatusCode::OK, Json(body));
    }
    if couch.tombstones.lock().unwrap().contains_key(&doc_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"not_found","reason":"deleted","id":doc_id})),
        );
    }
    (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"})))
}

async fn couch_fixture_delete(
    State(couch): State<CouchTombstoneFixture>,
    Path((_database, doc_id)): Path<(String, String)>,
) -> StatusCode {
    let Some(mut body) = couch.documents.lock().unwrap().remove(&doc_id) else {
        return StatusCode::NOT_FOUND;
    };
    body["_rev"] = json!("2-deleted");
    body["_deleted"] = json!(true);
    couch.tombstones.lock().unwrap().insert(doc_id, body);
    couch.delete_count.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

impl ReportsClient for MockReports {
    fn list_readable_reports<'a>(
        &'a self,
        _user_id: &'a str,
        _project: Option<&'a str>,
    ) -> BoxFuture<'a, CoreResult<Vec<ReportPickerItem>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn check_report_access<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
    ) -> BoxFuture<'a, CoreResult<ReportPickerItem>> {
        Box::pin(async { Err(CoreError::missing()) })
    }
    fn list_readable_datasets<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
    ) -> BoxFuture<'a, CoreResult<Vec<ReadableReportDataset>>> {
        Box::pin(async { Err(CoreError::missing()) })
    }
    fn fetch_report_dataset<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
        _dataset: &'a str,
    ) -> BoxFuture<'a, CoreResult<ReportDatasetMaterial>> {
        Box::pin(async { Err(CoreError::missing()) })
    }
    fn render_question_visual<'a>(
        &'a self,
        _user_id: &'a str,
        _prompt: &'a str,
        _visual_spec: &'a Value,
    ) -> BoxFuture<'a, CoreResult<RenderedVisual>> {
        Box::pin(async {
            Ok(RenderedVisual {
                bytes: b"PNG_VISUAL_BYTES".to_vec(),
                mime_type: "image/png".into(),
            })
        })
    }
    fn create_render_job<'a>(
        &'a self,
        _user_id: &'a str,
        input: &'a CreateRenderJobInput,
    ) -> BoxFuture<'a, CoreResult<RenderJobDescriptor>> {
        let desc = RenderJobDescriptor {
            job_id: format!("test-render-job-{}", certifications_api::dal::new_id()),
            template_id: input.template_id.clone(),
            template_version: input.template_version,
            mapping: input.mapping.clone(),
            config: input.config.clone(),
            rows: input.rows.clone(),
            width: input.width,
            height: input.height,
            scale: input.scale,
            expires_at: certifications_api::core::now() + 600,
        };
        Box::pin(async move { Ok(desc) })
    }
    fn delete_render_job<'a>(
        &'a self,
        _user_id: &'a str,
        _job_id: &'a str,
        _confirmed: bool,
    ) -> BoxFuture<'a, CoreResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn test_settings() -> Settings {
    Settings {
        host: "127.0.0.1".into(),
        port: 0,
        development: false,
        database_url: String::new(),
        redis_url: String::new(),
        redis_namespace: "certifications_api:".into(),
        couch_url: String::new(),
        couch_database: "certifications".into(),
        couch_user: String::new(),
        couch_password: String::new(),
        fsm_url: String::new(),
        fsm_key: String::new(),
        cortex_url: String::new(),
        reports_url: String::new(),
        gateway_url: String::new(),
        reports_service_key: String::new(),
        certifications_service_key: String::new(),
        clamav: Default::default(),
    }
}

async fn publish_question_schema(store: &Store) {
    let draft = DraftSchema {
        schema_name: QUESTION_SCHEMA_NAME.into(),
        version: QUESTION_SCHEMA_VERSION,
        description: "Question v2".into(),
        fields: vec![
            SchemaField {
                field_path: "prompt".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "alternatives".into(),
                field_type: "array".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "correct_id".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "explanation".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "citation_ref".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "visual_ids".into(),
                field_type: "array".into(),
                is_required: true,
                description: "Canonical visual references".into(),
            },
        ],
    };
    schemas::save_draft(&store.db, &draft).await.unwrap();
    schemas::publish(&store.db, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION)
        .await
        .unwrap();
}

async fn setup_app() -> (Router, Store, Arc<MemoryFsm>) {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();
    store.reports = Arc::new(MockReports);
    publish_question_schema(&store).await;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    (app, store, fsm)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: &str,
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-uuid", user);
    if let Some(l) = level {
        req = req.header("x-access-level", l);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    user: &str,
    level: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-uuid", user);
    if let Some(l) = level {
        req = req.header("x-access-level", l);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

fn to_base64_str(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(TABLE[b0 >> 2] as char);
        out.push(TABLE[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((b1 & 15) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[b2 & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn make_test_png(width: u32, height: u32) -> String {
    let img = image::RgbaImage::new(width, height);
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    to_base64_str(&bytes)
}

fn make_colored_test_png(width: u32, height: u32, color: [u8; 4]) -> String {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    to_base64_str(&bytes)
}

#[tokio::test]
async fn manual_authoring_edit_delete_visual_and_origin_guards() {
    let (app, store, fsm) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-1",
        None,
        Some(json!({ "title": "Authoring Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    // Ordinary caller cannot supply origin in payload.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-1",
        None,
        Some(json!({
            "language": "en",
            "prompt": "What is 2+2?",
            "alternatives": [
                { "id": "a", "text": "4" },
                { "id": "b", "text": "5" }
            ],
            "correct_id": "a",
            "explanation": "Basic arithmetic",
            "origin": "generated_with_cortex"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Valid manual creation sets supplied_by_creator.
    let (st, q) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-1",
        None,
        Some(json!({
            "language": "en",
            "prompt": "What is 2+2?",
            "alternatives": [
                { "id": "a", "text": "4" },
                { "id": "b", "text": "5" }
            ],
            "correct_id": "a",
            "explanation": "Basic arithmetic"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(q["origin"], "supplied_by_creator");
    let qid = q["id"].as_str().unwrap().to_string();

    // Edit creates version 2 and keeps origin.
    let (st, q_v2) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/questions/{qid}"),
        "author-1",
        None,
        Some(json!({
            "explanation": "Updated arithmetic explanation",
            "correct_id": "a"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(q_v2["version_num"], 2);
    assert_eq!(q_v2["origin"], "supplied_by_creator");

    // Legacy direct visual endpoint is rejected with 422.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual"),
        "author-1",
        None,
        Some(json!({ "visual_spec": { "chart": "bar" } })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Stage 1: Prepare visual render job through Reports descriptor API.
    let (st, prep) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual_jobs"),
        "author-1",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": { "x": ["label"], "y": ["val"] },
            "config": {},
            "rows": [{ "label": "A", "val": 10 }],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(prep["question_id"], qid);
    let qv_id = prep["question_version_id"].as_str().unwrap().to_string();
    assert_eq!(qv_id, q_v2["version_id"].as_str().unwrap());
    let job_id = prep["render_job"]["job_id"].as_str().unwrap().to_string();
    let old_quiz_id: String = sqlx::query_scalar("SELECT quiz_id FROM studies WHERE id = $1")
        .bind(&sid)
        .fetch_one(&store.db)
        .await
        .unwrap();
    let (st, old_attempt) = call(
        &app,
        "POST",
        &format!("/quizzes/{old_quiz_id}/attempts"),
        "author-1",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let old_attempt_id = old_attempt["id"].as_str().unwrap().to_string();

    // Stage 2a: Decoded dimension mismatch rejected with 422 (100x100 vs expected 1600x960).
    let bad_png = make_test_png(100, 100);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": bad_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Stage 2b: Valid PNG upload persists to FSM and records in question_visuals.
    let valid_png = make_test_png(1600, 960);
    let (st, vis) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let vis_id = vis["id"].as_str().unwrap().to_string();
    assert_eq!(vis["prepared_question_version_id"], qv_id);
    let result_qv_id = vis["result_question_version_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reserved_receipt = sqlx::query("SELECT operation_fingerprint, result_question_version_id, result_quiz_version_id, result_document_id, result_fsm_ref_key FROM question_visual_jobs WHERE render_job_id = $1")
        .bind(&job_id).fetch_one(&store.db).await.unwrap();
    assert!(
        !reserved_receipt
            .get::<String, _>("operation_fingerprint")
            .is_empty()
    );
    assert_eq!(
        reserved_receipt.get::<String, _>("result_question_version_id"),
        result_qv_id
    );
    assert_eq!(
        reserved_receipt.get::<String, _>("result_quiz_version_id"),
        vis["result_quiz_version_id"]
    );
    let fsm_ref: String = sqlx::query_scalar("SELECT fsm_ref FROM question_visuals WHERE id = $1")
        .bind(&vis_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    let result_doc_id: String =
        sqlx::query_scalar("SELECT couch_doc_id FROM question_versions WHERE id = $1")
            .bind(&result_qv_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    assert_eq!(
        reserved_receipt.get::<String, _>("result_document_id"),
        result_doc_id
    );
    assert_eq!(
        reserved_receipt.get::<String, _>("result_fsm_ref_key"),
        fsm_ref
    );
    assert!(fsm.has_blob(&fsm_ref));

    // Stage 2c: Idempotent replay returns same visual descriptor.
    let (st, vis_retry) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(vis_retry["id"], vis_id);
    assert_eq!(vis_retry["result_question_version_id"], result_qv_id);
    assert!(vis_retry.get("fsm_ref").is_none());

    // Stage 3a: Author reads visual preview bytes.
    let (st, headers, body_bytes) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/questions/{qid}/visuals/{vis_id}"),
        "author-1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "image/png");
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    assert!(!body_bytes.is_empty());
    let old_attempt_visual_path =
        format!("/attempts/{old_attempt_id}/question_versions/{qv_id}/visuals/{vis_id}");
    let (st, _, _) = call_raw(&app, "GET", &old_attempt_visual_path, "author-1", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Stage 3b: Unauthorized caller receives FORBIDDEN.
    let (st, _, _) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/questions/{qid}/visuals/{vis_id}"),
        "author-2",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // The upload publishes a successor question document and leaves the prepared version unchanged.
    let old_doc_id: String =
        sqlx::query_scalar("SELECT couch_doc_id FROM question_versions WHERE id = $1")
            .bind(&qv_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let old_doc = store.docs.get(&old_doc_id).await.unwrap();
    assert_eq!(old_doc["visual_ids"], json!([]));
    let result_doc_id: String =
        sqlx::query_scalar("SELECT couch_doc_id FROM question_versions WHERE id = $1")
            .bind(&result_qv_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let result_doc = store.docs.get(&result_doc_id).await.unwrap();
    assert_eq!(result_doc["visual_ids"], json!([vis_id]));
    let visual_ref_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM question_version_visual_refs WHERE question_version_id = $1 AND visual_id = $2",
    )
    .bind(&result_qv_id)
    .bind(&vis_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(visual_ref_count, 1);
    assert!(
        sqlx::query("DELETE FROM question_visuals WHERE id = $1")
            .bind(&vis_id)
            .execute(&store.db)
            .await
            .is_err()
    );

    // A later text edit carries the exact visual reference into a new immutable version.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/questions/{qid}"),
        "author-1",
        None,
        Some(json!({"prompt": "Edited after image publication"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let latest_qv: String = sqlx::query_scalar(
        "SELECT question_version_id FROM quiz_version_questions WHERE quiz_version_id = (SELECT active_version_id FROM quizzes WHERE id = (SELECT quiz_id FROM studies WHERE id = $1)) AND question_version_id <> $2 ORDER BY position LIMIT 1",
    )
    .bind(&sid).bind(&result_qv_id).fetch_one(&store.db).await.unwrap();
    let latest_doc_id: String =
        sqlx::query_scalar("SELECT couch_doc_id FROM question_versions WHERE id = $1")
            .bind(&latest_qv)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let latest_doc = store.docs.get(&latest_doc_id).await.unwrap();
    assert_eq!(latest_doc["visual_ids"], json!([vis_id]));

    // A lost success response can replay after local job deletion and a later text edit.
    sqlx::query("DELETE FROM question_visual_jobs WHERE render_job_id = $1")
        .bind(&job_id)
        .execute(&store.db)
        .await
        .unwrap();
    let before_replay_qvs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM question_versions WHERE question_id = $1")
            .bind(&qid)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let before_replay_quiz_versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quiz_versions WHERE quiz_id = (SELECT quiz_id FROM studies WHERE id = $1)")
        .bind(&sid).fetch_one(&store.db).await.unwrap();
    let puts_before_replay = fsm.put_count();
    let (st, replayed) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(
            json!({"question_version_id": qv_id, "render_job_id": job_id, "png_base64": valid_png,
            "expected_width": 800, "expected_height": 480, "expected_scale": 2}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(replayed["id"], vis_id);
    assert_eq!(replayed["result_question_version_id"], result_qv_id);
    assert_eq!(fsm.put_count(), puts_before_replay);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM question_versions WHERE question_id = $1"
        )
        .bind(&qid)
        .fetch_one(&store.db)
        .await
        .unwrap(),
        before_replay_qvs
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quiz_versions WHERE quiz_id = (SELECT quiz_id FROM studies WHERE id = $1)").bind(&sid).fetch_one(&store.db).await.unwrap(), before_replay_quiz_versions);

    // Attempts expose only safe visual metadata and use the exact pinned question version for PNG reads.
    let active_quiz_id: String = sqlx::query_scalar("SELECT quiz_id FROM studies WHERE id = $1")
        .bind(&sid)
        .fetch_one(&store.db)
        .await
        .unwrap();
    let (st, attempt) = call(
        &app,
        "POST",
        &format!("/quizzes/{active_quiz_id}/attempts"),
        "author-1",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let attempt_id = attempt["id"].as_str().unwrap();
    let (st, taking) = call(
        &app,
        "GET",
        &format!("/attempts/{attempt_id}"),
        "author-1",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let pinned_qv = taking["questions"][0]["question_version_id"]
        .as_str()
        .unwrap();
    assert_eq!(taking["questions"][0]["visuals"][0]["id"], vis_id);
    assert!(taking["questions"][0].get("correct_id").is_none());
    assert!(taking["questions"][0].get("explanation").is_none());
    let attempt_visual_path =
        format!("/attempts/{attempt_id}/question_versions/{pinned_qv}/visuals/{vis_id}");
    let (st, headers, body_bytes) =
        call_raw(&app, "GET", &attempt_visual_path, "author-1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "private, no-store");
    assert!(!body_bytes.is_empty());
    let (st, _, _) = call_raw(&app, "GET", &attempt_visual_path, "author-2", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let stale_attempt_visual_path =
        format!("/attempts/{attempt_id}/question_versions/{result_qv_id}/visuals/{vis_id}");
    let (st, _, _) = call_raw(&app, "GET", &stale_attempt_visual_path, "author-1", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Conflict: Stale or mismatched question version ID is rejected with 409.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": "stale-version-id",
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Conflict: Conflicting PNG payload for same render_job_id is rejected with 409.
    let conflicting_png = make_colored_test_png(1600, 960, [255, 0, 0, 255]);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": conflicting_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Unprocessable: Unknown render job ID is rejected with 422.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-1",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": "unknown-nonexistent-job",
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Forbidden: Upload by non-owner is rejected with 403.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-2",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Quiz id for admin origin override.
    let quiz_id: String = sqlx::query_scalar("SELECT quiz_id FROM studies WHERE id = $1")
        .bind(&sid)
        .fetch_one(&store.db)
        .await
        .unwrap();

    // Non-admin cannot override quiz origin.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/admin/quizzes/{quiz_id}/origin"),
        "author-1",
        Some("0"),
        Some(json!({ "origin": "generated_with_cortex" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Admin overrides quiz and question origin.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/admin/quizzes/{quiz_id}/origin"),
        "admin-1",
        Some("1"),
        Some(json!({ "origin": "generated_with_cortex", "apply_to_questions": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let row = sqlx::query("SELECT origin FROM quizzes WHERE id = $1")
        .bind(&quiz_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("origin"), "generated_with_cortex");

    // Removing membership from study quiz never deletes question_versions.
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/studies/{sid}/questions/{qid}"),
        "author-1",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    let ver_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM question_versions WHERE question_id = $1")
            .bind(&qid)
            .fetch_one(&store.db)
            .await
            .unwrap();
    assert_eq!(ver_count, 4);
}

#[tokio::test]
async fn csv_import_previews_and_never_silently_imports_invalid_rows() {
    let (app, _store, _fsm) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-2",
        None,
        Some(json!({ "title": "CSV Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let mixed_csv = "\
language,prompt,option_a,option_b,option_c,option_d,correct_option,explanation,citation_ref
en,Capital of France?,Paris,Lyon,Nice,Marseille,a,Paris is the capital,p.1
en,Bad row missing correct,One,Two,,,z,Missing option z,p.2
";

    // Preview shows 1 valid and 1 invalid row, imports 0.
    let (st, preview) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/import"),
        "author-2",
        None,
        Some(json!({ "csv_text": mixed_csv, "confirm": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(preview["preview_only"], true);
    assert_eq!(preview["imported_count"], 0);
    assert_eq!(preview["valid_rows"].as_array().unwrap().len(), 1);
    assert_eq!(preview["invalid_rows"].as_array().unwrap().len(), 1);

    // Confirming while invalid rows exist is rejected (never silently drops invalid rows).
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/import"),
        "author-2",
        None,
        Some(json!({ "csv_text": mixed_csv, "confirm": true })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let clean_csv = "\
language,prompt,option_a,option_b,option_c,option_d,correct_option,explanation,citation_ref
en,Capital of France?,Paris,Lyon,Nice,Marseille,a,Paris is the capital,p.1
";
    let (st, imported) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/import"),
        "author-2",
        None,
        Some(json!({ "csv_text": clean_csv, "confirm": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(imported["preview_only"], false);
    assert_eq!(imported["imported_count"], 1);
    assert_eq!(imported["questions"][0]["origin"], "supplied_by_creator");
}

#[tokio::test]
async fn create_question_rejects_visual_spec_without_side_effects() {
    let (app, store, _) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-side-effects",
        None,
        Some(json!({ "title": "Side Effects Guard Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let q_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM questions")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let qv_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_versions")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let quiz_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quizzes")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let qver_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quiz_versions")
        .fetch_one(&store.db)
        .await
        .unwrap();

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-side-effects",
        None,
        Some(json!({
            "prompt": "What is 2 + 2?",
            "alternatives": [
                { "id": "a", "text": "4" },
                { "id": "b", "text": "5" }
            ],
            "correct_id": "a",
            "explanation": "Basic math",
            "visual_spec": { "chart": "bar" }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let q_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM questions")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let qv_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_versions")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let quiz_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quizzes")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let qver_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quiz_versions")
        .fetch_one(&store.db)
        .await
        .unwrap();

    assert_eq!(q_count_before, q_count_after, "no question rows created");
    assert_eq!(
        qv_count_before, qv_count_after,
        "no question_version rows created"
    );
    assert_eq!(quiz_count_before, quiz_count_after, "no quiz rows created");
    assert_eq!(
        qver_count_before, qver_count_after,
        "no quiz_version rows created"
    );
}

#[tokio::test]
async fn test_schema_016_upgrade_preserves_historical_visuals_with_unknown_payload_sha256() {
    sqlx::any::install_default_drivers();
    let pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    // Run historical baseline migrations up to 004 (which has question_visuals without payload_sha256)
    for migration in &certifications_api::dal::SQLITE_MIGRATIONS[..4] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }

    // Insert baseline schema_version, question and question_version
    sqlx::query("INSERT INTO schema_versions(schema_name, version, status, description, created_at, published_at) VALUES('question', 1, 'published', '', 1, 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO questions(id, owner_id, origin, created_at) VALUES('q1', 'u1', 'supplied_by_creator', 1)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at) VALUES('qv1', 'q1', 1, 'en', 'question', 1, 'doc1', 1)")
        .execute(&pool)
        .await
        .unwrap();

    // Insert historical question_visual row without payload_sha256 column
    sqlx::query("INSERT INTO question_visuals(id, question_version_id, fsm_ref, mime_type, created_at) VALUES('vis-hist-1', 'qv1', 'visuals/qv1/blob1.png', 'image/png', 1000)")
        .execute(&pool)
        .await
        .unwrap();

    // Run remaining migrations up to 016
    for migration in &certifications_api::dal::SQLITE_MIGRATIONS[4..] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }

    // Verify existing row survived and payload_sha256 defaulted to 'unknown' (not empty SHA)
    let (id, payload_sha256, fsm_ref): (String, String, String) = sqlx::query_as(
        "SELECT id, payload_sha256, fsm_ref FROM question_visuals WHERE id = 'vis-hist-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(id, "vis-hist-1");
    assert_eq!(payload_sha256, "unknown");
    assert_eq!(fsm_ref, "visuals/qv1/blob1.png");
}

#[tokio::test]
async fn test_visual_publication_serialization_and_rejection() {
    let (app, store, _) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-pub-guard",
        None,
        Some(json!({ "title": "Publication Guard Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let (st, question) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-pub-guard",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Test publication lock",
            "alternatives": [
                { "id": "a", "text": "1" },
                { "id": "b", "text": "2" }
            ],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid = question["id"].as_str().unwrap().to_string();
    let qv_id = question["version_id"].as_str().unwrap().to_string();

    // Prepare visual job while private
    let (st, prep) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual_jobs"),
        "author-pub-guard",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id = prep["render_job"]["job_id"].as_str().unwrap().to_string();

    // Publish the quiz directly
    sqlx::query("UPDATE quizzes SET visibility = 'public' WHERE study_id = $1")
        .bind(&sid)
        .execute(&store.db)
        .await
        .unwrap();

    // Stage 1 prepare visual job must fail with 409 quiz_already_published
    let (st, resp) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual_jobs"),
        "author-pub-guard",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or_default()
            .contains("quiz_already_published")
    );

    // Stage 2 visual upload must also fail with 409 quiz_already_published
    let valid_png = make_test_png(1600, 960);
    let (st, resp) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-pub-guard",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or_default()
            .contains("quiz_already_published")
    );
}

#[tokio::test]
async fn test_visual_durable_identical_replay_without_active_job() {
    let (app, store, _) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-replay",
        None,
        Some(json!({ "title": "Replay Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let (st, question) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-replay",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Test replay",
            "alternatives": [
                { "id": "a", "text": "1" },
                { "id": "b", "text": "2" }
            ],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid = question["id"].as_str().unwrap().to_string();
    let qv_id = question["version_id"].as_str().unwrap().to_string();

    let (st, prep) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual_jobs"),
        "author-replay",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id = prep["render_job"]["job_id"].as_str().unwrap().to_string();

    let valid_png = make_test_png(1600, 960);
    let (st, vis_first) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-replay",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let first_vis_id = vis_first["id"].as_str().unwrap().to_string();

    // Expire/delete temporary job record
    sqlx::query("DELETE FROM question_visual_jobs WHERE render_job_id = $1")
        .bind(&job_id)
        .execute(&store.db)
        .await
        .unwrap();

    // Replay identical upload: should succeed with existing visual receipt even without active job
    let (st, vis_replay) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-replay",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(vis_replay["id"], first_vis_id);
}

#[tokio::test]
async fn test_visual_fsm_compensation_outside_sql_transaction() {
    let (app, store, fsm) = setup_app().await;

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        "author-compensate",
        None,
        Some(json!({ "title": "Compensation Study" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    let (st, question) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        "author-compensate",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Test compensation",
            "alternatives": [
                { "id": "a", "text": "1" },
                { "id": "b", "text": "2" }
            ],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid = question["id"].as_str().unwrap().to_string();
    let qv_id = question["version_id"].as_str().unwrap().to_string();

    let (st, prep) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual_jobs"),
        "author-compensate",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id = prep["render_job"]["job_id"].as_str().unwrap().to_string();

    let puts_before = fsm.put_count();
    let docs_before = certifications_api::dal::memory_docs(&store.docs)
        .unwrap()
        .len();

    // Create trigger that forces INSERT on question_visuals to fail
    sqlx::query("CREATE TRIGGER abort_qv_insert BEFORE INSERT ON question_visuals BEGIN SELECT RAISE(ABORT, 'forced insert failure'); END;")
        .execute(&store.db)
        .await
        .unwrap();

    // Force both compensations into the durable cleanup queue. A same-job
    // retry must be able to reuse these exact reserved resources safely.
    fsm.fail_delete.store(true, Ordering::SeqCst);
    certifications_api::dal::memory_docs(&store.docs)
        .unwrap()
        .fail_delete
        .store(true, Ordering::SeqCst);

    let valid_png = make_test_png(1600, 960);
    let (st, err_resp) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-compensate",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png.clone(),
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;

    // Must return 400 with sanitized message, not raw database error
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(err_resp["error"], "visual_insert_failed");

    // Failed compensation remains pending rather than deleting a reserved key.
    let puts_after = fsm.put_count();
    assert_eq!(puts_after, puts_before + 1);
    assert_eq!(fsm.blob_count(), 1);
    assert_eq!(
        certifications_api::dal::memory_docs(&store.docs)
            .unwrap()
            .len(),
        docs_before + 1
    );
    let failed_assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_visuals")
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(failed_assets, 0);

    // Drop trigger
    sqlx::query("DROP TRIGGER abort_qv_insert;")
        .execute(&store.db)
        .await
        .unwrap();

    fsm.fail_delete.store(false, Ordering::SeqCst);
    certifications_api::dal::memory_docs(&store.docs)
        .unwrap()
        .fail_delete
        .store(false, Ordering::SeqCst);
    let (retry_status, retry_receipt) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visuals"),
        "author-compensate",
        None,
        Some(json!({
            "question_version_id": qv_id,
            "render_job_id": job_id,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(retry_status, StatusCode::CREATED);

    let cleanup_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM cleanup_tasks WHERE status = 'pending' ORDER BY created_at, id",
    )
    .fetch_all(&store.db)
    .await
    .unwrap();
    assert_eq!(cleanup_ids.len(), 2);
    for cleanup_id in cleanup_ids {
        assert!(
            certifications_api::domain::cleanup::retry_one(&store, &cleanup_id)
                .await
                .unwrap()
        );
    }
    assert_eq!(fsm.blob_count(), 1);
    assert!(
        certifications_api::dal::memory_docs(&store.docs)
            .unwrap()
            .contains(
                sqlx::query_scalar::<_, String>(
                    "SELECT result_document_id FROM question_visual_jobs WHERE render_job_id = $1",
                )
                .bind(job_id)
                .fetch_one(&store.db)
                .await
                .unwrap()
                .as_str(),
            )
    );
    assert!(!retry_receipt["id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn remote_couch_retry_preserves_reserved_document_until_expiry() {
    let couch = CouchTombstoneFixture::default();
    let couch_app = Router::new()
        .route(
            "/{database}/{doc_id}",
            axum::routing::put(couch_fixture_put)
                .get(couch_fixture_get)
                .delete(couch_fixture_delete),
        )
        .with_state(couch.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let couch_server = tokio::spawn(async move {
        axum::serve(listener, couch_app).await.unwrap();
    });

    let mut settings = test_settings();
    settings.couch_url = format!("http://{address}");
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.docs = Documents::remote(&settings, reqwest::Client::new());
    store.fsm = fsm.clone();
    store.reports = Arc::new(MockReports);
    publish_question_schema(&store).await;
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    let (status, study) = call(
        &app,
        "POST",
        "/studies",
        "couch-retry",
        None,
        Some(json!({"title":"Couch visual retry"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let study_id = study["id"].as_str().unwrap().to_string();
    let (status, question) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions"),
        "couch-retry",
        None,
        Some(json!({
            "language":"en",
            "prompt":"Couch retry question",
            "alternatives":[{"id":"a","text":"One"},{"id":"b","text":"Two"}],
            "correct_id":"a",
            "explanation":"Reason.",
            "citation_ref":""
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{question}");
    let question_id = question["id"].as_str().unwrap().to_string();
    let question_version_id = question["version_id"].as_str().unwrap().to_string();
    let (status, prepared) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions/{question_id}/visual_jobs"),
        "couch-retry",
        None,
        Some(json!({
            "template_id":"chart_template","template_version":1,"mapping":{},
            "config":{},"rows":[],"width":800,"height":480,"scale":2,
            "ttl_seconds":600
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = prepared["render_job"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let png = make_test_png(1600, 960);

    sqlx::query("CREATE TRIGGER abort_couch_retry BEFORE INSERT ON question_visuals BEGIN SELECT RAISE(ABORT, 'forced insert failure'); END;")
        .execute(&store.db)
        .await
        .unwrap();
    let upload = json!({
        "question_version_id":question_version_id,
        "render_job_id":job_id,
        "png_base64":png.clone(),
        "expected_width":800,"expected_height":480,"expected_scale":2
    });
    let (status, body) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions/{question_id}/visuals"),
        "couch-retry",
        None,
        Some(upload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "visual_insert_failed");

    let reserved_doc: String = sqlx::query_scalar(
        "SELECT result_document_id FROM question_visual_jobs WHERE render_job_id = $1",
    )
    .bind(&job_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    let cleanup_id: String = sqlx::query_scalar(
        "SELECT id FROM cleanup_tasks WHERE target_kind='couch_doc' AND target_ref=$1",
    )
    .bind(&reserved_doc)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert!(couch.documents.lock().unwrap().contains_key(&reserved_doc));
    assert_eq!(couch.delete_count.load(Ordering::SeqCst), 0);
    assert!(
        !certifications_api::domain::cleanup::retry_one(&store, &cleanup_id)
            .await
            .unwrap()
    );
    assert!(couch.documents.lock().unwrap().contains_key(&reserved_doc));
    let task_state: String = sqlx::query_scalar("SELECT status FROM cleanup_tasks WHERE id=$1")
        .bind(&cleanup_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(task_state, "pending");

    sqlx::query("DROP TRIGGER abort_couch_retry")
        .execute(&store.db)
        .await
        .unwrap();
    let (status, receipt) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions/{question_id}/visuals"),
        "couch-retry",
        None,
        Some(upload),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        receipt["result_question_version_id"],
        sqlx::query_scalar::<_, String>(
            "SELECT result_question_version_id FROM question_visual_jobs WHERE render_job_id=$1"
        )
        .bind(&job_id)
        .fetch_one(&store.db)
        .await
        .unwrap()
    );
    assert!(
        certifications_api::domain::cleanup::retry_one(&store, &cleanup_id)
            .await
            .unwrap()
    );
    assert_eq!(couch.delete_count.load(Ordering::SeqCst), 0);
    assert!(couch.documents.lock().unwrap().contains_key(&reserved_doc));

    // A separate failed reservation becomes nonretryable after its expiry.
    // Its queued cleanup now performs the real Couch DELETE/tombstone path.
    let (status, next_prepared) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions/{question_id}/visual_jobs"),
        "couch-retry",
        None,
        Some(json!({
            "template_id":"chart_template","template_version":1,"mapping":{},
            "config":{},"rows":[],"width":800,"height":480,"scale":2,
            "ttl_seconds":600
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let expired_job = next_prepared["render_job"]["job_id"].as_str().unwrap();
    let current_qv = next_prepared["question_version_id"].as_str().unwrap();
    sqlx::query("CREATE TRIGGER abort_couch_expired BEFORE INSERT ON question_visuals BEGIN SELECT RAISE(ABORT, 'forced insert failure'); END;")
        .execute(&store.db)
        .await
        .unwrap();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/studies/{study_id}/questions/{question_id}/visuals"),
        "couch-retry",
        None,
        Some(json!({
            "question_version_id":current_qv,"render_job_id":expired_job,
            "png_base64":png,"expected_width":800,"expected_height":480,"expected_scale":2
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    sqlx::query("DROP TRIGGER abort_couch_expired")
        .execute(&store.db)
        .await
        .unwrap();
    let expired_doc: String = sqlx::query_scalar(
        "SELECT result_document_id FROM question_visual_jobs WHERE render_job_id=$1",
    )
    .bind(expired_job)
    .fetch_one(&store.db)
    .await
    .unwrap();
    let expired_cleanup: String = sqlx::query_scalar(
        "SELECT id FROM cleanup_tasks WHERE target_kind='couch_doc' AND target_ref=$1",
    )
    .bind(&expired_doc)
    .fetch_one(&store.db)
    .await
    .unwrap();
    sqlx::query("UPDATE question_visual_jobs SET expires_at=0 WHERE render_job_id=$1")
        .bind(expired_job)
        .execute(&store.db)
        .await
        .unwrap();
    assert!(
        certifications_api::domain::cleanup::retry_one(&store, &expired_cleanup)
            .await
            .unwrap()
    );
    assert_eq!(couch.delete_count.load(Ordering::SeqCst), 1);
    assert!(couch.tombstones.lock().unwrap().contains_key(&expired_doc));
    assert!(store.docs.get(&expired_doc).await.is_err());
    let mut tombstone_body = couch
        .tombstones
        .lock()
        .unwrap()
        .get(&expired_doc)
        .unwrap()
        .clone();
    tombstone_body.as_object_mut().unwrap().remove("_id");
    tombstone_body.as_object_mut().unwrap().remove("_rev");
    tombstone_body.as_object_mut().unwrap().remove("_deleted");
    assert!(
        store
            .docs
            .put_immutable(&expired_doc, &tombstone_body)
            .await
            .is_err()
    );
    couch_server.abort();
}

#[tokio::test]
async fn test_visual_replay_scoping_and_draft_expiry_regressions() {
    let (app, store, fsm) = setup_app().await;

    // 1. Setup Study A with Question A1 and Question A2
    let (st, study_a) = call(
        &app,
        "POST",
        "/studies",
        "author-a",
        None,
        Some(json!({ "title": "Study A" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid_a = study_a["id"].as_str().unwrap().to_string();

    let (st, q_a1) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a}/questions"),
        "author-a",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q A1",
            "alternatives": [{ "id": "a", "text": "1" }, { "id": "b", "text": "2" }],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid_a1 = q_a1["id"].as_str().unwrap().to_string();
    let qv_a1 = q_a1["version_id"].as_str().unwrap().to_string();

    let (st, q_a2) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a}/questions"),
        "author-a",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q A2",
            "alternatives": [{ "id": "a", "text": "1" }, { "id": "b", "text": "2" }],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid_a2 = q_a2["id"].as_str().unwrap().to_string();

    // Prepare and upload visual for Question A1
    let (st, prep_a1) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a}/questions/{qid_a1}/visual_jobs"),
        "author-a",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id_a1 = prep_a1["render_job"]["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let valid_png = make_test_png(1600, 960);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a}/questions/{qid_a1}/visuals"),
        "author-a",
        None,
        Some(json!({
            "question_version_id": qv_a1,
            "render_job_id": job_id_a1,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // Negative Regression 1: Wrong question path (replaying A1 receipt on A2)
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a}/questions/{qid_a2}/visuals"),
        "author-a",
        None,
        Some(json!({
            "question_version_id": qv_a1,
            "render_job_id": job_id_a1,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert!(st == StatusCode::CONFLICT || st == StatusCode::NOT_FOUND);

    // Setup Study A2 (second study owned by Author A)
    let (st, study_a2) = call(
        &app,
        "POST",
        "/studies",
        "author-a",
        None,
        Some(json!({ "title": "Study A2" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid_a2 = study_a2["id"].as_str().unwrap().to_string();

    // Negative Regression 2: Wrong owned study (calling Study A2 with Question A1 and A1 receipt)
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid_a2}/questions/{qid_a1}/visuals"),
        "author-a",
        None,
        Some(json!({
            "question_version_id": qv_a1,
            "render_job_id": job_id_a1,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert!(st == StatusCode::NOT_FOUND || st == StatusCode::CONFLICT);

    // Setup Study B owned by User B with Question B
    let (st, study_b) = call(
        &app,
        "POST",
        "/studies",
        "author-b",
        None,
        Some(json!({ "title": "Study B" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid_b = study_b["id"].as_str().unwrap().to_string();

    let (st, q_b) = call(
        &app,
        "POST",
        &format!("/studies/{sid_b}/questions"),
        "author-b",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q B",
            "alternatives": [{ "id": "a", "text": "1" }, { "id": "b", "text": "2" }],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid_b = q_b["id"].as_str().unwrap().to_string();

    // Negative Regression 3: B-study with A-receipt (User B attempts to replay User A's visual receipt)
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid_b}/questions/{qid_b}/visuals"),
        "author-b",
        None,
        Some(json!({
            "question_version_id": qv_a1,
            "render_job_id": job_id_a1,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert!(
        st == StatusCode::CONFLICT || st == StatusCode::FORBIDDEN || st == StatusCode::NOT_FOUND
    );

    // Negative Regression 4: Timestamp-expired draft
    let past_ts = chrono::Utc::now().timestamp() - 1000;
    sqlx::query("UPDATE studies SET expires_at = $1 WHERE id = $2")
        .bind(past_ts)
        .bind(&sid_b)
        .execute(&store.db)
        .await
        .unwrap();

    // Prepare on expired draft must return 410 GONE
    let (st, exp_prep) = call(
        &app,
        "POST",
        &format!("/studies/{sid_b}/questions/{qid_b}/visual_jobs"),
        "author-b",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert!(
        exp_prep["error"]
            .as_str()
            .unwrap_or_default()
            .contains("study_expired")
    );

    // Upload on expired draft must return 410 GONE
    let (st, exp_upload) = call(
        &app,
        "POST",
        &format!("/studies/{sid_b}/questions/{qid_b}/visuals"),
        "author-b",
        None,
        Some(json!({
            "question_version_id": "nonexistent_qv",
            "render_job_id": "new_job_id",
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert!(
        exp_upload["error"]
            .as_str()
            .unwrap_or_default()
            .contains("study_expired")
    );

    // Direct upload on timestamp-expired draft regression:
    // Setup Study C with Question C while draft is valid
    let (st, study_c) = call(
        &app,
        "POST",
        "/studies",
        "author-c",
        None,
        Some(json!({ "title": "Study C" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid_c = study_c["id"].as_str().unwrap().to_string();

    let (st, q_c) = call(
        &app,
        "POST",
        &format!("/studies/{sid_c}/questions"),
        "author-c",
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q C",
            "alternatives": [{ "id": "a", "text": "1" }, { "id": "b", "text": "2" }],
            "correct_id": "a",
            "explanation": "Exp"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let qid_c = q_c["id"].as_str().unwrap().to_string();
    let qv_c = q_c["version_id"].as_str().unwrap().to_string();

    // Prepare a valid visual job while Study C is valid draft
    let (st, prep_c) = call(
        &app,
        "POST",
        &format!("/studies/{sid_c}/questions/{qid_c}/visual_jobs"),
        "author-c",
        None,
        Some(json!({
            "template_id": "chart_template",
            "template_version": 1,
            "mapping": {},
            "config": {},
            "rows": [],
            "width": 800,
            "height": 480,
            "scale": 2,
            "ttl_seconds": 600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let job_id_c = prep_c["render_job"]["job_id"].as_str().unwrap().to_string();

    // Verify stored status is 'draft' before expiry mutation
    let status_before: String = sqlx::query_scalar("SELECT status FROM studies WHERE id = $1")
        .bind(&sid_c)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(status_before, "draft");

    // Mutate expires_at to past while status is STILL draft
    sqlx::query("UPDATE studies SET expires_at = $1 WHERE id = $2")
        .bind(past_ts)
        .bind(&sid_c)
        .execute(&store.db)
        .await
        .unwrap();

    let status_still_draft: String = sqlx::query_scalar("SELECT status FROM studies WHERE id = $1")
        .bind(&sid_c)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(status_still_draft, "draft");

    let fsm_blobs_before = fsm.blob_count();
    let fsm_puts_before = fsm.put_count();

    // Directly call upload without calling prepare after expiry
    let (st, direct_exp_upload) = call(
        &app,
        "POST",
        &format!("/studies/{sid_c}/questions/{qid_c}/visuals"),
        "author-c",
        None,
        Some(json!({
            "question_version_id": qv_c,
            "render_job_id": job_id_c,
            "png_base64": valid_png,
            "expected_width": 800,
            "expected_height": 480,
            "expected_scale": 2
        })),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert!(
        direct_exp_upload["error"]
            .as_str()
            .unwrap_or_default()
            .contains("study_expired")
    );

    // Assert zero new FSM blobs created and zero attempted FSM puts
    assert_eq!(fsm.blob_count(), fsm_blobs_before);
    assert_eq!(fsm.put_count(), fsm_puts_before);

    // Assert status transitioned from 'draft' to 'expired' directly by upload
    let status_after: String = sqlx::query_scalar("SELECT status FROM studies WHERE id = $1")
        .bind(&sid_c)
        .fetch_one(&store.db)
        .await
        .unwrap();
    assert_eq!(status_after, "expired");
}
