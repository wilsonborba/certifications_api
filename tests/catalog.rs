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
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt;

struct MockReports {
    restricted: AtomicBool,
}

impl MockReports {
    fn new(restricted: bool) -> Arc<Self> {
        Arc::new(Self {
            restricted: AtomicBool::new(restricted),
        })
    }
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
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, CoreResult<ReportPickerItem>> {
        let restricted = self.restricted.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(ReportPickerItem {
                project: project.to_string(),
                report: report.to_string(),
                title: "Safety Report".into(),
                is_restricted: restricted,
            })
        })
    }
    fn render_question_visual<'a>(
        &'a self,
        _user_id: &'a str,
        _prompt: &'a str,
        _visual_spec: &'a Value,
    ) -> BoxFuture<'a, CoreResult<RenderedVisual>> {
        Box::pin(async { Err(CoreError::unavailable()) })
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

async fn setup_app(reports: Arc<MockReports>) -> (Router, Store) {
    let mut store = Store::fixture().await;
    store.fsm = MemoryFsm::new();
    store.reports = reports;
    publish_question_schema(&store).await;

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    (app, store)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
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
async fn catalog_discovery_tags_no_answer_leaks_and_restricted_report_refusal() {
    let reports = MockReports::new(true);
    let (app, store) = setup_app(reports.clone()).await;

    // Create study with a restricted report source and a private upload filename.
    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        Some("author-1"),
        Some(json!({
            "title": "Cardiology Basics",
            "description": "Heart anatomy and clinical ECG review"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap().to_string();

    call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("author-1"),
        Some(json!({
            "filename": "secret-internal-draft-filename.txt",
            "mime_type": "text/plain",
            "content_text": "Private draft text."
        })),
    )
    .await;

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("author-1"),
        Some(json!({
            "kind": "report",
            "filename": "Safety Report",
            "mime_type": "application/json",
            "external_ref": "clinical/restricted-metrics"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        Some("author-1"),
        Some(json!({
            "language": "en",
            "prompt": "Which chamber pumps oxygenated blood to the aorta?",
            "alternatives": [
                { "id": "a", "text": "Left ventricle" },
                { "id": "b", "text": "Right atrium" }
            ],
            "correct_id": "a",
            "explanation": "The left ventricle ejects blood through the aortic valve."
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // Private quiz never appears in anonymous catalog or quiz endpoints.
    let (st, cat_before) = call(&app, "GET", "/catalog", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cat_before["total"], 0);

    // Tag suggestions are deterministic, <= 5, and never leak private filename.
    let (st, sugg) = call(
        &app,
        "GET",
        &format!("/studies/{sid}/tags/suggestions"),
        Some("author-1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let sugg_arr = sugg["suggestions"].as_array().unwrap();
    assert!(!sugg_arr.is_empty() && sugg_arr.len() <= 5);
    assert!(!sugg.to_string().contains("secret-internal-draft-filename"));

    // Publishing with a restricted report source is refused with 422.
    let (st, err_pub) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        Some("author-1"),
        Some(json!({ "tags": ["Cardiology", "ECG"] })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err_pub.to_string().contains("restricted_report_source"));

    // Once report is public/permitted, >5 tags is still rejected.
    reports.restricted.store(false, Ordering::SeqCst);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        Some("author-1"),
        Some(json!({ "tags": ["t1", "t2", "t3", "t4", "t5", "t6"] })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Publish with valid tags.
    let (st, pub_quiz) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        Some("author-1"),
        Some(json!({ "tags": ["Cardiology", "Clinical ECG"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let quiz_id = pub_quiz["id"].as_str().unwrap().to_string();

    // Anonymous discovery & search work, with no answer-key or private filename leaks.
    let (st, cat) = call(
        &app,
        "GET",
        "/catalog?q=Cardiology&language=en&tag=clinical-ecg&origin=supplied_by_creator&suggest=true",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cat["total"], 1);
    assert_eq!(cat["suggestions"][0], "Cardiology Basics");

    let (st, detail) = call(&app, "GET", &format!("/quizzes/{quiz_id}"), None, None).await;
    assert_eq!(st, StatusCode::OK);
    let detail_str = detail.to_string();
    assert!(!detail_str.contains("correct_id"));
    assert!(!detail_str.contains("aortic valve"));
    assert!(!detail_str.contains("secret-internal-draft-filename"));
    assert_eq!(detail["performance"]["basis"], "all_completed_attempts");
    assert_eq!(detail["performance"]["score_distribution_denominator"], 0);
    assert!(
        detail["performance"]["score_distribution"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        detail["performance"]["score_distribution_report_view"]["visual"]["mapping"]["bucket"][0],
        "score_band"
    );

    let (st, pub_qs) = call(
        &app,
        "GET",
        "/catalog/questions?q=aorta&language=en",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(pub_qs["items"].as_array().unwrap().len(), 1);
    assert!(!pub_qs.to_string().contains("correct_id"));

    // Real completed attempt updates public statistics.
    let qver_id: String = sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
        .bind(&quiz_id)
        .fetch_one(&store.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO attempts(id, owner_id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at, completed_at)
         VALUES('att-1', 'learner-1', $1, $2, 'completed', 1, 1, 100, 145)",
    )
    .bind(&quiz_id)
    .bind(&qver_id)
    .execute(&store.db)
    .await
    .unwrap();

    for (id, user, correct, started) in [
        ("att-2", "learner-2", 49, 200),
        ("att-3", "learner-3", 50, 300),
        ("att-4", "learner-4", 79, 400),
        ("att-5", "learner-5", 80, 500),
    ] {
        sqlx::query(
            "INSERT INTO attempts(id, owner_id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at, completed_at)
             VALUES($1, $2, $3, $4, 'completed', $5, 100, $6, $6 + 45)",
        )
        .bind(id)
        .bind(user)
        .bind(&quiz_id)
        .bind(&qver_id)
        .bind(correct)
        .bind(started)
        .execute(&store.db)
        .await
        .unwrap();
    }

    let (st, stats) = call(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(stats["completed_attempt_count"], 5);
    assert_eq!(stats["distinct_participants"], 5);
    assert_eq!(stats["average_score_pct"], 71.6);
    assert_eq!(stats["average_score_denominator"], 5);
    assert_eq!(stats["typical_duration_sec"], 45);
    assert_eq!(stats["duration_denominator"], 5);
    assert_eq!(stats["score_distribution_denominator"], 5);
    assert_eq!(stats["score_distribution"][0]["score_band"], "0-49%");
    assert_eq!(stats["score_distribution"][0]["attempt_count"], 1);
    assert_eq!(stats["score_distribution"][1]["score_band"], "50-79%");
    assert_eq!(stats["score_distribution"][1]["attempt_count"], 2);
    assert_eq!(stats["score_distribution"][2]["score_band"], "80-100%");
    assert_eq!(stats["score_distribution"][2]["attempt_count"], 2);
    assert_eq!(
        stats["score_distribution_report_view"]["rows"][0]["score_band"],
        "0-49%"
    );

    sqlx::query("UPDATE quizzes SET visibility = 'private' WHERE id = $1")
        .bind(&quiz_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, private_stats) = call(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(private_stats.get("score_distribution").is_none());
}
