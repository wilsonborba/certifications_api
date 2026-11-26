use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::{Error as CoreError, Result as CoreResult, Settings},
    dal::{
        Store,
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
use std::sync::Arc;
use tower::ServiceExt;

struct MockReports;

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
            job_id: "test-render-job-40".into(),
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
    let fsm_ref = vis["fsm_ref"].as_str().unwrap().to_string();
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
    assert_eq!(vis_retry["fsm_ref"], fsm_ref);

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
    assert!(!body_bytes.is_empty());

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
    assert_eq!(ver_count, 2);
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
