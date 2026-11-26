use certifications_api::{
    dal::{
        SQLITE_MIGRATIONS, Store, memory_docs,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::guardrail,
};
use serde_json::json;
use sqlx::Row;
use std::sync::atomic::Ordering;

const TABLES: [&str; 22] = [
    "schema_versions",
    "schema_fields",
    "runtime_settings",
    "studies",
    "tags",
    "study_tags",
    "sources",
    "source_ledger",
    "source_extractions",
    "quizzes",
    "quiz_tags",
    "questions",
    "question_versions",
    "question_citations",
    "quiz_versions",
    "quiz_version_questions",
    "generation_jobs",
    "generation_steps",
    "generation_job_sources",
    "attempts",
    "attempt_answers",
    "owner_activity",
];

const EXTRA_TABLES: [&str; 6] = [
    "attempt_statistics",
    "estimation_statistics",
    "certificate_holders",
    "certificates",
    "cleanup_tasks",
    "public_question_search",
];

#[tokio::test]
async fn migrations_apply_cleanly_rerun_safely_and_have_no_json_or_array_columns() {
    let store = Store::fixture().await;
    for migration in SQLITE_MIGRATIONS {
        let rerun: String = migration
            .lines()
            .filter(|l| {
                !l.starts_with("ALTER TABLE studies ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE sources ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE generation_jobs ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE generation_job_sources ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE questions ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE quizzes ADD COLUMN ")
                    && !l.starts_with("ALTER TABLE quiz_version_questions")
                    && !l.starts_with("ALTER TABLE certificates ADD COLUMN ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::raw_sql(&rerun).execute(&store.db).await.unwrap();
    }
    assert!(
        include_str!("../migrations/002_v2_studies_sources.sql")
            .contains("ALTER TABLE studies ADD COLUMN IF NOT EXISTS source_origin")
    );
    for table in TABLES.iter().chain(EXTRA_TABLES.iter()) {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
        ))
        .fetch_one(&store.db)
        .await
        .unwrap();
        assert_eq!(count, 1, "missing table {table}");

        let ddl: String = sqlx::query_scalar(&format!(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='{table}'"
        ))
        .fetch_one(&store.db)
        .await
        .unwrap();
        let lower = ddl.to_ascii_lowercase();
        assert!(!lower.contains("jsonb"), "jsonb in {table}: {ddl}");
        assert!(!lower.contains(" json"), "json in {table}: {ddl}");
        assert!(
            !lower.contains("[]") && !lower.contains(" array"),
            "array in {table}: {ddl}"
        );
    }
}

#[tokio::test]
async fn check_constraints_reject_invalid_values() {
    let store = Store::fixture().await;
    // Invalid generation requested_count (21 > 20)
    assert!(
            sqlx::query(
                "INSERT INTO generation_jobs(id, owner_id, status, language, requested_count, queued_at) VALUES('j1', 'o1', 'queued', 'en', 21, 1)"
            )
            .execute(&store.db)
            .await
            .is_err()
        );
    // Invalid language
    assert!(
            sqlx::query(
                "INSERT INTO generation_jobs(id, owner_id, status, language, requested_count, queued_at) VALUES('j1', 'o1', 'queued', 'fr', 5, 1)"
            )
            .execute(&store.db)
            .await
            .is_err()
        );
    // Negative size_bytes
    sqlx::query(
            "INSERT INTO studies(id, owner_id, title, created_at, updated_at) VALUES('s1', 'o1', 'Study', 1, 1)"
        )
        .execute(&store.db)
        .await
        .unwrap();
    assert!(
            sqlx::query(
                "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, status, created_at, updated_at) VALUES('src1', 'o1', 's1', 'a.pdf', 'application/pdf', -1, 'reserved', 1, 1)"
            )
            .execute(&store.db)
            .await
            .is_err()
        );
}

#[tokio::test]
async fn published_schema_is_immutable_and_guardrail_rejects_invalid_before_write() {
    let store = Store::fixture().await;
    let docs = memory_docs(&store.docs).unwrap();

    let draft = DraftSchema {
        schema_name: "question-v2".into(),
        version: 1,
        description: "Multiple choice question".into(),
        fields: vec![
            SchemaField {
                field_path: "prompt".into(),
                field_type: "string".into(),
                is_required: true,
                description: "Question prompt".into(),
            },
            SchemaField {
                field_path: "alternatives".into(),
                field_type: "array".into(),
                is_required: true,
                description: "Answer options".into(),
            },
            SchemaField {
                field_path: "meta.source_page".into(),
                field_type: "integer".into(),
                is_required: false,
                description: "Optional source page".into(),
            },
        ],
    };
    schemas::save_draft(&store.db, &draft).await.unwrap();

    // Draft schema cannot validate documents before publishing.
    let unpub = guardrail::store_validated_document(
        &store,
        "question-v2",
        1,
        "q-unpub",
        &json!({"prompt": "Hi", "alternatives": ["A", "B"]}),
    )
    .await;
    assert!(unpub.is_err());
    assert_eq!(docs.len(), 0);

    schemas::publish(&store.db, "question-v2", 1).await.unwrap();

    // Published schema is immutable.
    assert!(schemas::save_draft(&store.db, &draft).await.is_err());

    // Missing required field is rejected before CouchDB write.
    assert!(
        guardrail::store_validated_document(
            &store,
            "question-v2",
            1,
            "q-bad-1",
            &json!({"prompt": "Hi"}),
        )
        .await
        .is_err()
    );
    // Wrong field type is rejected before CouchDB write.
    assert!(
        guardrail::store_validated_document(
            &store,
            "question-v2",
            1,
            "q-bad-2",
            &json!({"prompt": 123, "alternatives": ["A"]}),
        )
        .await
        .is_err()
    );
    // Undeclared field is rejected before CouchDB write.
    assert!(
        guardrail::store_validated_document(
            &store,
            "question-v2",
            1,
            "q-bad-3",
            &json!({"prompt": "Hi", "alternatives": ["A"], "extra": true}),
        )
        .await
        .is_err()
    );
    assert_eq!(docs.len(), 0);

    // Valid payload succeeds.
    guardrail::store_validated_document(
        &store,
        "question-v2",
        1,
        "q-ok-1",
        &json!({"prompt": "What is 2+2?", "alternatives": ["3", "4"], "meta": {"source_page": 2}}),
    )
    .await
    .unwrap();
    assert_eq!(docs.len(), 1);
    assert!(docs.contains("q-ok-1"));

    // Orphan cleanup removes doc when delete succeeds, or records cleanup_tasks when delete fails.
    guardrail::cleanup_orphan_document(&store, "q-ok-1").await;
    assert!(!docs.contains("q-ok-1"));

    guardrail::store_validated_document(
        &store,
        "question-v2",
        1,
        "q-ok-2",
        &json!({"prompt": "What is 3+3?", "alternatives": ["5", "6"]}),
    )
    .await
    .unwrap();
    docs.fail_delete.store(true, Ordering::SeqCst);
    guardrail::cleanup_orphan_document(&store, "q-ok-2").await;
    let row = sqlx::query("SELECT target_kind, target_ref, status FROM cleanup_tasks")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let kind: String = row.get("target_kind");
    let tref: String = row.get("target_ref");
    let st: String = row.get("status");
    assert_eq!(kind, "couch_doc");
    assert_eq!(tref, "q-ok-2");
    assert_eq!(st, "pending");
}

#[tokio::test]
async fn no_local_plans_waitlist_or_ticket_tables_remain() {
    let store = Store::fixture().await;
    let forbidden = ["plan_waitlist", "waitlist", "plans", "support_tickets"];
    for tbl in forbidden {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = $1",
        )
        .bind(tbl)
        .fetch_one(&store.db)
        .await
        .unwrap();
        assert_eq!(count, 0, "forbidden local table {tbl} exists");
    }
}
