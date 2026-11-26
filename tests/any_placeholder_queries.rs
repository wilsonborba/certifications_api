use certifications_api::{
    dal::Store,
    domain::{
        attempts::{self, AttemptHistoryFilter},
        auth::CallerIdentity,
        authoring,
        catalog::{self, CatalogQuestionQuery},
        studies::{self, StudyHistoryFilter},
    },
};
use serde_json::json;
use sqlx::any::AnyPoolOptions;

const POSTGRES_MIGRATIONS: [&str; 16] = [
    include_str!("../migrations/001_v2_schema.sql"),
    include_str!("../migrations/002_v2_studies_sources.sql"),
    include_str!("../migrations/003_v2_generation_events.sql"),
    include_str!("../migrations/004_v2_authoring_origin.sql"),
    include_str!("../migrations/005_v2_attempt_activity.sql"),
    include_str!("../migrations/006_v2_certificates_snapshot.sql"),
    include_str!("../migrations/007_v2_drop_legacy_waitlist.sql"),
    include_str!("../migrations/008_v2_quiz_question_provenance.sql"),
    include_str!("../migrations/009_v2_public_question_search.sql"),
    include_str!("../migrations/010_v2_source_provenance.sql"),
    include_str!("../migrations/011_v2_study_prepared_config.sql"),
    include_str!("../migrations/012_v2_generation_source_snapshot.sql"),
    include_str!("../migrations/013_v2_generation_source_external_ref.sql"),
    include_str!("../migrations/014_v2_report_source_snapshot.sql"),
    include_str!("../migrations/015_v2_one_active_generation_per_owner.sql"),
    include_str!("../migrations/016_v2_question_visual_jobs.sql"),
];

async fn seed_and_exercise(store: &Store) {
    sqlx::raw_sql(
        "INSERT INTO schema_versions(schema_name, version, status, description, created_at, published_at)
             VALUES ('compat-question', 1, 'published', '', 1, 1);
         INSERT INTO studies(id, owner_id, title, description, created_at, updated_at,
                             source_origin, status, quiz_id, expires_at)
             VALUES ('compat-study', 'compat-owner', 'Biology study', '', 150, 160,
                     'direct', 'active', 'compat-quiz', NULL),
                    ('compat-study-2', 'compat-owner', 'Biology study 2', '', 170, 180,
                     'direct', 'active', 'compat-quiz', NULL),
                    ('other-study', 'other-owner', 'Biology study', '', 150, 160,
                     'direct', 'active', 'compat-quiz', NULL);
         INSERT INTO tags(id, slug, label, created_at)
             VALUES ('compat-tag', 'science', 'Science', 1);
         INSERT INTO quizzes(id, owner_id, study_id, visibility, active_version_id,
                             created_at, updated_at, origin)
             VALUES ('compat-quiz', 'compat-owner', 'compat-study', 'public', 'compat-version', 1, 1,
                     'supplied_by_creator'),
                    ('private-quiz', 'other-owner', 'other-study', 'private', 'private-version', 1, 1,
                     'supplied_by_creator');
         INSERT INTO quiz_tags(quiz_id, tag_id) VALUES ('compat-quiz', 'compat-tag');
         INSERT INTO quiz_versions(id, quiz_id, version_num, title, created_at)
             VALUES ('compat-version', 'compat-quiz', 1, 'Biology Basics', 1),
                    ('private-version', 'private-quiz', 1, 'Biology Basics', 1);
         INSERT INTO questions(id, owner_id, created_at, origin)
             VALUES ('compat-question', 'compat-owner', 1, 'supplied_by_creator'),
                    ('compat-question-2', 'compat-owner', 2, 'supplied_by_creator'),
                    ('private-question', 'other-owner', 1, 'supplied_by_creator');
         INSERT INTO question_versions(id, question_id, version_num, language, schema_name,
                                       schema_version, couch_doc_id, created_at)
             VALUES ('compat-qv', 'compat-question', 1, 'en', 'compat-question', 1, 'compat-doc', 1),
                    ('compat-qv-2', 'compat-question-2', 1, 'en', 'compat-question', 1, 'compat-doc-2', 2),
                    ('private-qv', 'private-question', 1, 'en', 'compat-question', 1, 'private-doc', 1);
         INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id)
             VALUES ('compat-version', 0, 'compat-qv'), ('compat-version', 1, 'compat-qv-2'),
                    ('private-version', 0, 'private-qv');
         INSERT INTO public_question_search(question_version_id, quiz_id, normalized_prompt,
                                           normalized_title, normalized_tags)
             VALUES ('compat-qv', 'compat-quiz', 'biology cell', 'biology basics', 'science'),
                    ('compat-qv-2', 'compat-quiz', 'biology cell', 'biology basics', 'science'),
                    ('private-qv', 'private-quiz', 'biology cell', 'biology basics', 'science');
         INSERT INTO attempts(id, owner_id, quiz_id, quiz_version_id, status, score_correct,
                              score_total, started_at)
             VALUES ('compat-attempt', 'compat-owner', 'compat-quiz', 'compat-version', 'completed', 1, 1, 150),
                    ('compat-attempt-2', 'compat-owner', 'compat-quiz', 'compat-version', 'completed', 1, 1, 175),
                    ('other-attempt', 'other-owner', 'compat-quiz', 'compat-version', 'completed', 1, 1, 150);",
    )
    .execute(&store.db)
    .await
    .unwrap();
    store
        .docs
        .put_immutable(
            "compat-doc",
            &json!({"prompt":"Biology cell", "alternatives":[]}),
        )
        .await
        .unwrap();
    store
        .docs
        .put_immutable(
            "compat-doc-2",
            &json!({"prompt":"Biology cell two", "alternatives":[]}),
        )
        .await
        .unwrap();

    let caller = CallerIdentity {
        user_id: "compat-owner".into(),
        access_level: 0,
        display_name: None,
    };
    let attempts = attempts::list_my_attempts(
        store,
        &caller,
        &AttemptHistoryFilter {
            search: Some("biology".into()),
            started_from: Some(100),
            started_to: Some(200),
            quiz_id: Some("compat-quiz".into()),
            tag: Some("science".into()),
            status: None,
        },
        1,
        1,
    )
    .await
    .unwrap();
    assert_eq!(attempts.total, 2);
    assert_eq!(attempts.items.len(), 1);
    assert_eq!(attempts.items[0].id, "compat-attempt");

    let studies = studies::list_studies(
        store,
        &caller,
        &StudyHistoryFilter {
            search: Some("biology".into()),
            created_from: Some(100),
            created_to: Some(200),
            status: Some("active".into()),
            quiz_id: Some("compat-quiz".into()),
            tag: Some("science".into()),
        },
        1,
        1,
    )
    .await
    .unwrap();
    assert_eq!(studies.total, 2);
    assert_eq!(studies.items.len(), 1);
    assert_eq!(studies.items[0].id, "compat-study");

    let questions = catalog::list_catalog_questions(
        store,
        CatalogQuestionQuery {
            q: Some("biology".into()),
            language: Some("en".into()),
            origin: Some("supplied_by_creator".into()),
            limit: Some(1),
            offset: Some(1),
        },
    )
    .await
    .unwrap();
    assert_eq!(questions.items.len(), 1);
    assert_eq!(questions.items[0].question_version_id, "compat-qv");
    assert_eq!(questions.items[0].source_quiz_tags, ["Science"]);

    let reused = authoring::reuse_public_question_versions(
        store,
        &caller,
        "compat-study",
        &["compat-qv".into()],
    )
    .await
    .unwrap();
    assert_eq!(reused.already_present_count, 1);
    assert_eq!(reused.added_count, 0);

    let rebuild = catalog::rebuild_public_question_search_batch(store, Some("compat-0"), 10, false)
        .await
        .unwrap();
    assert_eq!(rebuild.scanned_quiz_count, 2);
    assert_eq!(rebuild.public_active_quiz_count, 1);
    assert_eq!(rebuild.rows_to_index, 2);
}

#[tokio::test]
async fn affected_dynamic_queries_work_with_sqlite_any() {
    sqlx::any::install_default_drivers();
    let store = Store::fixture().await;
    seed_and_exercise(&store).await;
    store.db.close().await;
}

#[tokio::test]
#[ignore = "requires the isolated PostgreSQL test fixture"]
async fn affected_dynamic_queries_work_with_postgres_any() {
    let url = std::env::var("CERTIFICATIONS_TEST_DATABASE_URL")
        .expect("CERTIFICATIONS_TEST_DATABASE_URL must point at the isolated fixture");
    let socket_dir = std::env::var("CERTIFICATIONS_TEST_SOCKET_DIR")
        .expect("CERTIFICATIONS_TEST_SOCKET_DIR must name the fixture socket directory");
    let socket_path = std::path::Path::new(&socket_dir);
    assert!(socket_path.is_absolute());
    assert!(socket_path.is_dir());
    let options = url.parse::<sqlx::any::AnyConnectOptions>().unwrap();
    let parsed = &options.database_url;
    assert_eq!(parsed.scheme(), "postgresql");
    assert_eq!(parsed.username(), "certifications_api_app");
    assert!(parsed.password().is_none());
    assert_eq!(parsed.path(), "/certifications_api_test");
    assert_eq!(parsed.host_str(), Some("localhost"));
    let dsn_socket = parsed
        .query_pairs()
        .find(|(key, _)| key == "host")
        .map(|(_, value)| value.into_owned())
        .expect("test DSN must use its private Unix socket");
    assert_eq!(dsn_socket, socket_dir);
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    for migration in POSTGRES_MIGRATIONS {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }
    let mut store = Store::fixture().await;
    store.db.close().await;
    store.db = pool;
    seed_and_exercise(&store).await;
    store.db.close().await;
}
