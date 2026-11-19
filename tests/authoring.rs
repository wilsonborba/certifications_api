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
        reports::{BoxFuture, RenderedVisual, ReportPickerItem, ReportsClient},
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

    // Render visual in author review flow via Reports temp job and save to FSM.
    let (st, vis) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions/{qid}/visual"),
        "author-1",
        None,
        Some(json!({ "visual_spec": { "chart": "bar" } })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let fsm_ref = vis["fsm_ref"].as_str().unwrap().to_string();
    assert!(fsm.has_blob(&fsm_ref));

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
