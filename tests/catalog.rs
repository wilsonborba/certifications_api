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
        memory_docs, new_id,
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
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;

struct MockReports {
    restricted: AtomicBool,
    calls: AtomicUsize,
}

impl MockReports {
    fn new(restricted: bool) -> Arc<Self> {
        Arc::new(Self {
            restricted: AtomicBool::new(restricted),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReportsClient for MockReports {
    fn list_readable_reports<'a>(
        &'a self,
        _user_id: &'a str,
        _project: Option<&'a str>,
    ) -> BoxFuture<'a, CoreResult<Vec<ReportPickerItem>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Vec::new()) })
    }
    fn check_report_access<'a>(
        &'a self,
        _user_id: &'a str,
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, CoreResult<ReportPickerItem>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
        self.calls.fetch_add(1, Ordering::SeqCst);
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
    call_with_language(app, method, uri, user, body, None).await
}

async fn call_with_language(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
    language: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(language) = language {
        req = req.header("accept-language", language);
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

async fn call_with_access_level(
    app: &Router,
    uri: &str,
    user: &str,
    access_level: i64,
) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("x-uuid", user)
                .header("x-access-level", access_level.to_string())
                .body(Body::empty())
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

    let (st, private_upload) = call(
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
    assert_eq!(st, StatusCode::CREATED);
    let private_source_id = private_upload["id"].as_str().unwrap().to_string();

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
            "explanation": "The left ventricle ejects blood through the aortic valve.",
            "source_id": private_source_id,
            "citation_ref": "private upload page marker"
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
            "prompt": "Which chamber receives oxygenated blood from the lungs?",
            "alternatives": [
                { "id": "a", "text": "Left atrium" },
                { "id": "b", "text": "Right ventricle" }
            ],
            "correct_id": "a",
            "explanation": "The left atrium receives oxygenated blood from the lungs."
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
    let membership_scope: String = sqlx::query_scalar(
        "SELECT source_scope FROM quiz_version_questions qvq
         JOIN quizzes q ON q.active_version_id = qvq.quiz_version_id WHERE q.id = $1",
    )
    .bind(&quiz_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(membership_scope, "unclassified");

    // A matching public prompt after more than fifty nonmatching source rows
    // remains discoverable through database matching rather than a scan cutoff.
    for index in 0..51 {
        let quiz_id = new_id();
        let quiz_version_id = new_id();
        let question_id = new_id();
        let question_version_id = new_id();
        let couch_doc_id = new_id();
        let prompt = if index == 50 {
            "Needle beyond fifty source rows".to_string()
        } else {
            format!("Synthetic source question {index}")
        };
        store
            .docs
            .put_immutable(
                &couch_doc_id,
                &json!({
                    "prompt": prompt,
                    "alternatives": [{"id":"a","text":"A"},{"id":"b","text":"B"}],
                    "correct_id":"a",
                    "explanation":"private explanation"
                }),
            )
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO questions(id, owner_id, origin, created_at) VALUES($1, 'catalog-fixture', 'supplied_by_creator', $2)",
        )
        .bind(&question_id)
        .bind(10_000 + index as i64)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
             VALUES($1, $2, 1, 'en', $3, $4, $5, $6)",
        )
        .bind(&question_version_id)
        .bind(&question_id)
        .bind(QUESTION_SCHEMA_NAME)
        .bind(QUESTION_SCHEMA_VERSION)
        .bind(&couch_doc_id)
        .bind(10_000 + index as i64)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quizzes(id, owner_id, study_id, visibility, active_version_id, created_at, updated_at)
             VALUES($1, 'catalog-fixture', NULL, 'public', '', $2, $2)",
        )
        .bind(&quiz_id)
        .bind(10_000 + index as i64)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quiz_versions(id, quiz_id, version_num, title, created_at) VALUES($1, $2, 1, $3, $4)",
        )
        .bind(&quiz_version_id)
        .bind(&quiz_id)
        .bind(format!("Synthetic quiz {index}"))
        .bind(10_000 + index as i64)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id) VALUES($1, 0, $2)",
        )
        .bind(&quiz_version_id)
        .bind(&question_version_id)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query("UPDATE quizzes SET active_version_id = $1 WHERE id = $2")
            .bind(&quiz_version_id)
            .bind(&quiz_id)
            .execute(&store.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO public_question_search(question_version_id, quiz_id, normalized_prompt, normalized_title, normalized_tags)
             VALUES($1, $2, $3, $4, '')",
        )
        .bind(&question_version_id)
        .bind(&quiz_id)
        .bind(prompt.to_lowercase())
        .bind(format!("synthetic quiz {index}"))
        .execute(&store.db)
        .await
        .unwrap();
    }
    let doc_store = memory_docs(&store.docs).unwrap();
    doc_store.reset_read_count();
    let (st, late_prompt) = call(
        &app,
        "GET",
        "/catalog/questions?q=Needle%20beyond%20fifty",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(late_prompt["items"].as_array().unwrap().len(), 1);
    assert_eq!(late_prompt["has_more"], false);
    assert_eq!(doc_store.read_count(), 1);

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

    let (st, first_public_questions) = call(
        &app,
        "GET",
        "/catalog/questions?q=Cardiology&language=en&limit=1&offset=0",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first_public_questions["items"].as_array().unwrap().len(), 1);
    assert_eq!(first_public_questions["limit"], 1);
    assert_eq!(first_public_questions["offset"], 0);
    assert_eq!(first_public_questions["has_more"], true);
    assert_eq!(
        first_public_questions["items"][0]["source_quiz_title"],
        "Cardiology Basics"
    );
    assert_eq!(
        first_public_questions["items"][0]["source_quiz_tags"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        !first_public_questions
            .to_string()
            .contains("private upload page marker")
    );
    assert!(
        !first_public_questions
            .to_string()
            .contains(&private_source_id)
    );
    assert!(!first_public_questions.to_string().contains("correct_id"));
    assert!(!first_public_questions.to_string().contains("explanation"));
    let (st, questions_by_tag) = call(
        &app,
        "GET",
        "/catalog/questions?q=Clinical%20ECG&limit=5",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(questions_by_tag["items"].as_array().unwrap().len(), 2);
    let (st, second_public_questions) = call(
        &app,
        "GET",
        "/catalog/questions?q=Cardiology&language=en&limit=1&offset=1",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        second_public_questions["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(second_public_questions["has_more"], false);
    assert_ne!(
        first_public_questions["items"][0]["question_version_id"],
        second_public_questions["items"][0]["question_version_id"]
    );
    assert_ne!(
        first_public_questions["items"][0]["id"],
        second_public_questions["items"][0]["id"]
    );
    let (st, second_question) = call(
        &app,
        "GET",
        "/catalog/questions?q=receives%20oxygenated",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let second_question_version_id = second_question["items"][0]["question_version_id"]
        .as_str()
        .unwrap()
        .to_string();

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
    assert!(!pub_qs.to_string().contains("explanation"));
    assert!(!pub_qs.to_string().contains("private upload page marker"));
    assert!(!pub_qs.to_string().contains(&private_source_id));

    let (st, target_study) = call(
        &app,
        "POST",
        "/studies",
        Some("reuse-author"),
        Some(json!({"title":"Reusable public questions"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let target_study_id = target_study["id"].as_str().unwrap().to_string();
    let question_version_id = pub_qs["items"][0]["question_version_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, expired_study) = call(
        &app,
        "POST",
        "/studies",
        Some("reuse-author"),
        Some(json!({"title":"Expired reuse target"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let expired_study_id = expired_study["id"].as_str().unwrap();
    sqlx::query("UPDATE studies SET expires_at = 0 WHERE id = $1")
        .bind(expired_study_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, expired_reuse) = call(
        &app,
        "POST",
        &format!("/studies/{expired_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::GONE, "{expired_reuse}");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM studies WHERE id = $1")
            .bind(expired_study_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        "expired"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
            .bind(expired_study_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        0
    );

    let (st, generating_study) = call(
        &app,
        "POST",
        "/studies",
        Some("reuse-author"),
        Some(json!({"title":"Generating reuse target"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let generating_study_id = generating_study["id"].as_str().unwrap();
    sqlx::query(
        "INSERT INTO generation_jobs(id, owner_id, study_id, status, language, requested_count, queued_at)
         VALUES('queued-reuse-test', 'reuse-author', $1, 'queued', 'en', 1, 1)",
    )
    .bind(generating_study_id)
    .execute(&store.db)
    .await
    .unwrap();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{generating_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
            .bind(generating_study_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        0
    );

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("other-user"),
        Some(json!({"question_version_ids":[question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({
            "question_version_ids":[question_version_id,question_version_id],
            "owner_id":"forged-owner",
            "origin_quiz_id":"forged-origin",
            "origin_quiz_version_id":"forged-version",
            "source_scope":"generated_from_current_selected_sources"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quizzes WHERE study_id = $1")
            .bind(&target_study_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        0
    );

    let (st, reused) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[question_version_id,question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(reused["added_count"], 1);
    assert_eq!(reused["already_present_count"], 1);
    assert_eq!(reused["total_question_count"], 1);
    let target_quiz_id = reused["quiz_id"].as_str().unwrap().to_string();
    let target_version_id = reused["quiz_version_id"].as_str().unwrap().to_string();
    let reused_membership = sqlx::query(
        "SELECT qvq.question_version_id, qvq.source_scope, qvq.origin_quiz_id,
                qvq.origin_quiz_version_id
         FROM quiz_version_questions qvq WHERE qvq.quiz_version_id = $1",
    )
    .bind(&target_version_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(
        reused_membership.get::<String, _>("question_version_id"),
        question_version_id
    );
    assert_eq!(
        reused_membership.get::<String, _>("source_scope"),
        "reused_public_question_version"
    );
    assert_eq!(
        reused_membership.get::<String, _>("origin_quiz_id"),
        quiz_id
    );
    assert_eq!(
        reused_membership.get::<String, _>("origin_quiz_version_id"),
        sqlx::query_scalar::<_, String>("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(&quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap()
    );

    let target_version_count_before_retry: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM quiz_versions WHERE quiz_id = $1")
            .bind(&target_quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let (st, retry) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(retry["added_count"], 0);
    assert_eq!(retry["already_present_count"], 1);
    assert_eq!(retry["quiz_version_id"], target_version_id);
    let target_version_count_after_retry: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM quiz_versions WHERE quiz_id = $1")
            .bind(&target_quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    assert_eq!(
        target_version_count_before_retry,
        target_version_count_after_retry
    );

    sqlx::query(&format!(
        "CREATE TRIGGER fail_reuse_membership BEFORE INSERT ON quiz_version_questions
         WHEN NEW.question_version_id = '{second_question_version_id}'
         BEGIN SELECT RAISE(ABORT, 'injected membership write failure'); END"
    ))
    .execute(&store.db)
    .await
    .unwrap();
    let (st, failed_reuse) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[second_question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{failed_reuse}");
    sqlx::query("DROP TRIGGER fail_reuse_membership")
        .execute(&store.db)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quiz_versions WHERE quiz_id = $1")
            .bind(&target_quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        target_version_count_before_retry
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(&target_quiz_id)
            .fetch_one(&store.db)
            .await
            .unwrap(),
        target_version_id
    );

    let (st, target_questions) = call(
        &app,
        "GET",
        &format!("/studies/{target_study_id}/questions"),
        Some("reuse-author"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        target_questions["items"][0]["source_scope"],
        "reused_public_question_version"
    );
    assert_eq!(target_questions["items"][0]["origin_quiz_id"], quiz_id);
    assert_eq!(target_questions["items"][0]["citation_ref"], "");
    assert!(
        target_questions["items"][0]["visuals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let reused_question_id = target_questions["items"][0]["id"].as_str().unwrap();
    let (st, edit_reused) = call(
        &app,
        "PATCH",
        &format!("/studies/{target_study_id}/questions/{reused_question_id}"),
        Some("reuse-author"),
        Some(json!({"prompt":"Attempt to edit canonical reused version"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(
        edit_reused
            .to_string()
            .contains("reused_question_immutable")
    );
    let (st, add_visual) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/{reused_question_id}/visual"),
        Some("reuse-author"),
        Some(json!({"visual_spec":{"kind":"diagram"}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(add_visual.to_string().contains("reused_question_immutable"));
    let citation_count_after_reuse: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM question_citations WHERE question_version_id = $1",
    )
    .bind(&question_version_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(citation_count_after_reuse, 1);

    let (st, private_study) = call(
        &app,
        "POST",
        "/studies",
        Some("private-author"),
        Some(json!({"title":"Not publicly reusable"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let private_study_id = private_study["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{private_study_id}/questions"),
        Some("private-author"),
        Some(json!({
            "language":"en",
            "prompt":"Private question not in public catalog",
            "alternatives":[{"id":"a","text":"A"},{"id":"b","text":"B"}],
            "correct_id":"a",
            "explanation":"Private explanation"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let private_question_version_id: String = sqlx::query_scalar(
        "SELECT qvq.question_version_id FROM quizzes q
         JOIN quiz_version_questions qvq ON qvq.quiz_version_id = q.active_version_id
         WHERE q.study_id = $1",
    )
    .bind(&private_study_id)
    .fetch_one(&store.db)
    .await
    .unwrap();
    let (st, private_catalog_search) = call(
        &app,
        "GET",
        "/catalog/questions?q=Private%20question%20not%20in%20public%20catalog",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        private_catalog_search["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (st, private_reuse) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/questions/reuse"),
        Some("reuse-author"),
        Some(json!({"question_version_ids":[private_question_version_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(private_reuse.to_string().contains("question_not_reusable"));

    let (st, published_target) = call(
        &app,
        "POST",
        &format!("/studies/{target_study_id}/publication"),
        Some("reuse-author"),
        Some(json!({"tags":["Reuse"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(published_target["id"], target_quiz_id);
    let (st, target_public_detail) = call(
        &app,
        "GET",
        &format!("/quizzes/{target_quiz_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        target_public_detail["study_material"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !target_public_detail
            .to_string()
            .contains("private upload page marker")
    );
    assert!(
        !target_public_detail
            .to_string()
            .contains(&private_source_id)
    );
    assert!(!target_public_detail.to_string().contains("correct_id"));
    let (st, taking) = call(
        &app,
        "POST",
        &format!("/quizzes/{target_quiz_id}/attempts"),
        Some("learner-reuse"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(!taking.to_string().contains("correct_id"));
    assert!(!taking.to_string().contains("private upload page marker"));

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

    for (id, user, correct, total, started) in [
        ("att-6", "learner-6", 99, 200, 600),  // 49.5%
        ("att-7", "learner-7", 159, 200, 700), // 79.5%
        ("att-8", "learner-8", 100, 200, 800), // 50%
        ("att-9", "learner-9", 160, 200, 900), // 80%
        ("att-10", "learner-10", 200, 200, 1000),
        ("att-11", "learner-11", 0, 0, 1100), // undefined score is excluded
        ("att-12", "learner-12", 0, 200, 1200), // 0%
    ] {
        sqlx::query(
            "INSERT INTO attempts(id, owner_id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at, completed_at)
             VALUES($1, $2, $3, $4, 'completed', $5, $6, $7, $7 + 45)",
        )
        .bind(id)
        .bind(user)
        .bind(&quiz_id)
        .bind(&qver_id)
        .bind(correct)
        .bind(total)
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
    assert_eq!(stats["completed_attempt_count"], 12);
    assert_eq!(
        stats["question_provenance"]["unavailable_attempt_count"],
        12
    );
    assert_eq!(stats["question_provenance"]["combined"]["attempt_count"], 0);
    assert_eq!(
        stats["question_provenance"]["combined"]["question_count"],
        0
    );
    assert_eq!(stats["distinct_participants"], 12);
    assert!((stats["average_score_pct"].as_f64().unwrap() - 717.0 / 11.0).abs() < 1e-9);
    assert_eq!(stats["average_score_denominator"], 11);
    assert_eq!(stats["typical_duration_sec"], 45);
    assert_eq!(stats["duration_denominator"], 12);
    assert_eq!(stats["score_distribution_denominator"], 11);
    assert_eq!(stats["score_distribution"][0]["score_band"], "0% to <50%");
    assert_eq!(stats["score_distribution"][0]["attempt_count"], 3);
    assert_eq!(stats["score_distribution"][1]["score_band"], "50% to <80%");
    assert_eq!(stats["score_distribution"][1]["attempt_count"], 4);
    assert_eq!(stats["score_distribution"][2]["score_band"], "80% to 100%");
    assert_eq!(stats["score_distribution"][2]["attempt_count"], 4);
    assert_eq!(
        stats["score_distribution_report_view"]["rows"][0]["score_band"],
        "0% to <50%"
    );
    let report_view = &stats["score_distribution_report_view"];
    assert_eq!(
        report_view["visual"],
        json!({
            "template": "cert-score-distribution",
            "version": 1,
            "mapping": {"bucket": ["score_band"], "attempts": ["attempt_count"]},
            "options": {}
        })
    );
    assert_eq!(
        report_view["template_definition"],
        json!({
            "template_id": "cert-score-distribution",
            "version": 1,
            "kind": "chart",
            "visual": "bar",
            "title": report_view["descriptor"]["title"],
            "slots": [
                {
                    "slot_name": "bucket",
                    "role": "dimension",
                    "is_required": true,
                    "min_count": 1,
                    "max_count": 1,
                    "accepted_types": ["string"]
                },
                {
                    "slot_name": "attempts",
                    "role": "measure",
                    "is_required": true,
                    "min_count": 1,
                    "max_count": 2,
                    "accepted_types": ["integer", "float"]
                }
            ],
            "options": []
        })
    );
    assert_eq!(
        report_view["template_definition"]["slots"][0]["accepted_types"][0],
        report_view["descriptor"]["fields"][0]["type"]
    );
    assert_eq!(
        report_view["template_definition"]["slots"][1]["accepted_types"][0],
        report_view["descriptor"]["fields"][1]["type"]
    );
    assert_eq!(
        report_view["descriptor"]["meta"]["source"],
        "Certifications API"
    );
    assert_eq!(report_view["descriptor"]["meta"]["sample_size"], 11);
    assert_eq!(
        report_view["descriptor"]["meta"]["sample_unit"],
        "completed_attempts"
    );

    let reports_calls_before_owner_stats = reports.calls();
    let (st, public_owner_stats) = call(
        &app,
        "GET",
        &format!("/me/quizzes/{quiz_id}/statistics"),
        Some("author-1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(public_owner_stats, stats);
    let (st, public_other_owner_stats) = call(
        &app,
        "GET",
        &format!("/me/quizzes/{quiz_id}/statistics"),
        Some("other-author"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(public_other_owner_stats.get("score_distribution").is_none());
    let (st, public_admin_stats) = call_with_access_level(
        &app,
        &format!("/me/quizzes/{quiz_id}/statistics"),
        "admin-1",
        1,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(public_admin_stats, stats);
    assert_eq!(reports.calls(), reports_calls_before_owner_stats);

    let (st, pt_stats) = call_with_language(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
        Some("pt-BR,pt;q=0.9,en;q=0.8"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        pt_stats["score_distribution_report_view"]["descriptor"]["title"],
        "Distribuição das pontuações do questionário"
    );
    assert_eq!(
        pt_stats["score_distribution_report_view"]["template_definition"]["title"],
        pt_stats["score_distribution_report_view"]["descriptor"]["title"]
    );
    assert_eq!(pt_stats["score_distribution"][0]["score_band"], "0% a <50%");
    assert_eq!(
        pt_stats["score_distribution_report_view"]["descriptor"]["meta"]["language"],
        "pt-BR"
    );
    let (st, th_stats) = call_with_language(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
        Some("th-TH,th;q=0.9"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        th_stats["score_distribution_report_view"]["descriptor"]["title"],
        "การกระจายคะแนนแบบทดสอบ"
    );
    assert_eq!(
        th_stats["score_distribution_report_view"]["template_definition"]["title"],
        th_stats["score_distribution_report_view"]["descriptor"]["title"]
    );
    assert_eq!(
        th_stats["score_distribution"][1]["score_band"],
        "50% ถึงน้อยกว่า 80%"
    );
    let (st, th_detail) = call_with_language(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}"),
        None,
        None,
        Some("th-TH"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        th_detail["performance"]["score_distribution_report_view"]["descriptor"]["meta"]["language"],
        "th"
    );

    sqlx::query(
        "DELETE FROM attempts
         WHERE quiz_id = $1 AND (
             score_total = 0 OR
             CASE WHEN score_total > 0 THEN score_correct * 100.0 / score_total END < 50 OR
             CASE WHEN score_total > 0 THEN score_correct * 100.0 / score_total END >= 80
         )",
    )
    .bind(&quiz_id)
    .execute(&store.db)
    .await
    .unwrap();
    let (st, sparse_stats) = call_with_language(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
        Some("th-TH"),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        sparse_stats["score_distribution"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        sparse_stats["score_distribution"][0]["score_band"],
        "50% ถึงน้อยกว่า 80%"
    );
    let (st, public_stats_before_private_transition) = call(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let updated_question_id: String =
        sqlx::query_scalar("SELECT question_id FROM question_versions WHERE id = $1")
            .bind(&question_version_id)
            .fetch_one(&store.db)
            .await
            .unwrap();
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/questions/{updated_question_id}"),
        Some("author-1"),
        Some(json!({"prompt":"Which structure directs 100% of ventricular blood into the aorta (cœur)?"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, stale_prompt) = call(
        &app,
        "GET",
        "/catalog/questions?q=pumps%20oxygenated",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        stale_prompt["items"].as_array().unwrap().len(),
        1,
        "{stale_prompt}"
    );
    assert_eq!(
        stale_prompt["items"][0]["source_quiz_title"],
        "Reusable public questions"
    );
    let (st, current_prompt) = call(
        &app,
        "GET",
        "/catalog/questions?q=directs%20100%25",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(current_prompt["items"].as_array().unwrap().len(), 1);
    let (st, unicode_prompt) =
        call(&app, "GET", "/catalog/questions?q=C%C5%92UR", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(unicode_prompt["items"].as_array().unwrap().len(), 1);
    let (st, literal_percent) = call(&app, "GET", "/catalog/questions?q=%25", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(literal_percent["items"].as_array().unwrap().len(), 1);
    let (st, literal_underscore) = call(&app, "GET", "/catalog/questions?q=_", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(literal_underscore["items"].as_array().unwrap().len(), 0);

    sqlx::query("UPDATE quizzes SET visibility = 'private' WHERE id = $1")
        .bind(&quiz_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, revoked_question) = call(
        &app,
        "GET",
        "/catalog/questions?q=pumps%20oxygenated",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(revoked_question["items"].as_array().unwrap().len(), 1);
    sqlx::query("UPDATE quizzes SET visibility = 'private' WHERE id = $1")
        .bind(&target_quiz_id)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, fully_revoked_question) = call(
        &app,
        "GET",
        "/catalog/questions?q=pumps%20oxygenated",
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fully_revoked_question["items"].as_array().unwrap().len(), 0);
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
    let (st, private_stats_with_identity) = call(
        &app,
        "GET",
        &format!("/quizzes/{quiz_id}/statistics"),
        Some("author-1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(
        private_stats_with_identity
            .get("score_distribution")
            .is_none()
    );

    let (st, missing_identity_stats) = call(
        &app,
        "GET",
        &format!("/me/quizzes/{quiz_id}/statistics"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(missing_identity_stats.get("score_distribution").is_none());
    let doc_store = memory_docs(&store.docs).unwrap();
    doc_store.reset_read_count();
    let (st, other_owner_stats) = call(
        &app,
        "GET",
        &format!("/me/quizzes/{quiz_id}/statistics"),
        Some("other-author"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(other_owner_stats.get("score_distribution").is_none());
    assert_eq!(doc_store.read_count(), 0);

    let reports_calls_before_private_stats = reports.calls();
    let (st, private_owner_stats) = call(
        &app,
        "GET",
        &format!("/me/quizzes/{quiz_id}/statistics"),
        Some("author-1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut expected_private_stats = public_stats_before_private_transition;
    expected_private_stats["score_distribution_report_view"]["descriptor"]["visibility"] =
        json!("restricted");
    assert_eq!(private_owner_stats, expected_private_stats);
    let private_stats_text = private_owner_stats.to_string();
    for forbidden in [
        "correct_id",
        "explanation",
        "citation_ref",
        "answer-1",
        "att-1",
        "learner-1",
    ] {
        assert!(!private_stats_text.contains(forbidden));
    }

    let (st, private_admin_stats) = call_with_access_level(
        &app,
        &format!("/me/quizzes/{quiz_id}/statistics"),
        "admin-1",
        1,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(private_admin_stats, expected_private_stats);
    assert_eq!(reports.calls(), reports_calls_before_private_stats);
}
