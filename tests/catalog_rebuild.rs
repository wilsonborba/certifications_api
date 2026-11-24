use certifications_api::{
    dal::{Store, new_id},
    domain::catalog::{self, CatalogQuestionQuery},
};
use serde_json::json;
use sqlx::Row;

async fn seed_quiz(
    store: &Store,
    quiz_id: &str,
    visibility: &str,
    title: &str,
    prompt: &str,
    active: bool,
) -> String {
    let ts = 50_000;
    let quiz_version_id = new_id();
    let question_id = new_id();
    let question_version_id = new_id();
    let document_id = new_id();
    sqlx::query(
        "INSERT INTO quizzes(id, owner_id, study_id, visibility, active_version_id, created_at, updated_at)
         VALUES($1, 'rebuild-fixture', NULL, $2, '', $3, $3)",
    )
    .bind(quiz_id)
    .bind(visibility)
    .bind(ts)
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO quiz_versions(id, quiz_id, version_num, title, created_at) VALUES($1, $2, 1, $3, $4)",
    )
    .bind(&quiz_version_id)
    .bind(quiz_id)
    .bind(title)
    .bind(ts)
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO questions(id, owner_id, created_at) VALUES($1, 'rebuild-fixture', $2)",
    )
    .bind(&question_id)
    .bind(ts)
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
         VALUES($1, $2, 1, 'en', 'v2_question', 1, $3, $4)",
    )
    .bind(&question_version_id)
    .bind(&question_id)
    .bind(&document_id)
    .bind(ts)
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
    if active {
        sqlx::query("UPDATE quizzes SET active_version_id = $1 WHERE id = $2")
            .bind(&quiz_version_id)
            .bind(quiz_id)
            .execute(&store.db)
            .await
            .unwrap();
    }
    store
        .docs
        .put_immutable(
            &document_id,
            &json!({
                "prompt": prompt,
                "alternatives": [{"id":"a","text":"One"},{"id":"b","text":"Two"}],
                "correct_id":"a",
                "explanation":"Fixture only"
            }),
        )
        .await
        .unwrap();
    question_version_id
}

async fn search(store: &Store, query: &str) -> usize {
    catalog::list_catalog_questions(
        store,
        CatalogQuestionQuery {
            q: Some(query.to_string()),
            ..CatalogQuestionQuery::default()
        },
    )
    .await
    .unwrap()
    .items
    .len()
}

async fn insert_stale_search_row(store: &Store, quiz_id: &str, question_version_id: &str) {
    sqlx::query(
        "INSERT INTO public_question_search(question_version_id, quiz_id, normalized_prompt, normalized_title, normalized_tags)
         VALUES($1, $2, 'stale private prompt', 'stale private title', '')",
    )
    .bind(question_version_id)
    .bind(quiz_id)
    .execute(&store.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn bounded_rebuild_is_read_only_by_default_and_repairs_legacy_public_rows() {
    let store = Store::fixture().await;
    sqlx::query(
        "INSERT INTO schema_versions(schema_name, version, status, created_at) VALUES('v2_question', 1, 'published', 1)",
    )
    .execute(&store.db)
    .await
    .unwrap();

    let legacy_version = seed_quiz(
        &store,
        "legacy-public",
        "public",
        "Public Café Review",
        "How does a CAFé cell function?",
        true,
    )
    .await;
    sqlx::query("INSERT INTO tags(id, slug, label, created_at) VALUES('tag-clinica', 'clinica', 'Clínica', 1)")
        .execute(&store.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO quiz_tags(quiz_id, tag_id) VALUES('legacy-public', 'tag-clinica')")
        .execute(&store.db)
        .await
        .unwrap();
    let private_version = seed_quiz(
        &store,
        "private-quiz",
        "private",
        "Private Excluded",
        "Needle in a private prompt",
        true,
    )
    .await;
    let revoked_version = seed_quiz(
        &store,
        "revoked-quiz",
        "public",
        "Revoked Excluded",
        "Needle in a revoked prompt",
        true,
    )
    .await;
    sqlx::query("UPDATE quizzes SET visibility = 'private' WHERE id = 'revoked-quiz'")
        .execute(&store.db)
        .await
        .unwrap();
    let inactive_version = seed_quiz(
        &store,
        "inactive-quiz",
        "public",
        "Inactive Excluded",
        "Needle in an inactive prompt",
        false,
    )
    .await;
    insert_stale_search_row(&store, "private-quiz", &private_version).await;
    insert_stale_search_row(&store, "revoked-quiz", &revoked_version).await;
    insert_stale_search_row(&store, "inactive-quiz", &inactive_version).await;

    assert_eq!(search(&store, "café").await, 0);
    let before_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM public_question_search")
        .fetch_one(&store.db)
        .await
        .unwrap();
    let dry = catalog::rebuild_public_question_search_batch(&store, None, 10, false)
        .await
        .unwrap();
    assert_eq!(dry.mode, "dry-run");
    assert_eq!(dry.rows_to_index, 1);
    assert_eq!(dry.stale_rows_to_remove, 3);
    assert_eq!(dry.next_cursor.as_deref(), Some("revoked-quiz"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM public_question_search")
            .fetch_one(&store.db)
            .await
            .unwrap(),
        before_rows
    );
    assert_eq!(search(&store, "café").await, 0);

    let written = catalog::rebuild_public_question_search_batch(&store, None, 10, true)
        .await
        .unwrap();
    assert_eq!(written.mode, "write");
    assert_eq!(written.rows_to_index, 1);
    assert_eq!(written.stale_rows_to_remove, 3);
    assert_eq!(search(&store, "café").await, 1);
    assert_eq!(search(&store, "cafe\u{301}").await, 1);
    assert_eq!(search(&store, "cli\u{301}nica").await, 1);
    for (quiz_id, question_version_id) in [
        ("private-quiz", private_version),
        ("revoked-quiz", revoked_version),
        ("inactive-quiz", inactive_version),
    ] {
        let remaining: i64 = sqlx::query(
            "SELECT COUNT(*) FROM public_question_search WHERE quiz_id = $1 AND question_version_id = $2",
        )
        .bind(quiz_id)
        .bind(question_version_id)
        .fetch_one(&store.db)
        .await
        .unwrap()
        .get(0);
        assert_eq!(remaining, 0, "stale row retained for {quiz_id}");
    }
    let indexed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM public_question_search WHERE quiz_id = 'legacy-public' AND question_version_id = $1",
    )
    .bind(legacy_version)
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(indexed, 1);
}

#[tokio::test]
async fn rebuild_batches_use_a_stable_cursor_and_validate_caps_without_advancing_on_failure() {
    let store = Store::fixture().await;
    for id in ["a-first", "z-last"] {
        sqlx::query(
            "INSERT INTO quizzes(id, owner_id, study_id, visibility, active_version_id, created_at, updated_at)
             VALUES($1, 'rebuild-fixture', NULL, 'private', '', 1, 1)",
        )
        .bind(id)
        .execute(&store.db)
        .await
        .unwrap();
    }
    let first = catalog::rebuild_public_question_search_batch(&store, None, 1, false)
        .await
        .unwrap();
    assert_eq!(first.scanned_quiz_count, 1);
    assert_eq!(first.has_more, true);
    assert_eq!(first.next_cursor.as_deref(), Some("a-first"));
    let second = catalog::rebuild_public_question_search_batch(
        &store,
        first.next_cursor.as_deref(),
        1,
        false,
    )
    .await
    .unwrap();
    assert_eq!(second.scanned_quiz_count, 1);
    assert_eq!(second.has_more, false);
    assert_eq!(second.next_cursor.as_deref(), Some("z-last"));
    assert!(
        catalog::rebuild_public_question_search_batch(&store, None, 101, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_quiz_fails_closed_and_returns_the_previous_stable_cursor() {
    let store = Store::fixture().await;
    sqlx::query(
        "INSERT INTO schema_versions(schema_name, version, status, created_at) VALUES('v2_question', 1, 'published', 1)",
    )
    .execute(&store.db)
    .await
    .unwrap();
    let _ = seed_quiz(&store, "a-first", "private", "First", "unused", false).await;
    let _ = seed_quiz(&store, "b-oversized", "public", "Oversized", "unused", true).await;
    let quiz_version: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = 'b-oversized'")
            .fetch_one(&store.db)
            .await
            .unwrap();
    for index in 0..catalog::MAX_PUBLIC_SEARCH_REBUILD_QUIZ_QUESTIONS {
        let ts = 90_000 + index as i64;
        let question_id = new_id();
        let version_id = new_id();
        let document_id = new_id();
        sqlx::query("INSERT INTO questions(id, owner_id, created_at) VALUES($1, 'fixture', $2)")
            .bind(&question_id)
            .bind(ts)
            .execute(&store.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
             VALUES($1, $2, 1, 'en', 'v2_question', 1, $3, $4)",
        )
        .bind(&version_id)
        .bind(&question_id)
        .bind(&document_id)
        .bind(ts)
        .execute(&store.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id) VALUES($1, $2, $3)",
        )
        .bind(&quiz_version)
        .bind(index as i64 + 1)
        .bind(version_id)
        .execute(&store.db)
        .await
        .unwrap();
    }
    let result = catalog::rebuild_public_question_search_batch(&store, None, 5, false)
        .await
        .unwrap();
    assert_eq!(result.scanned_quiz_count, 1);
    assert_eq!(result.failed_quiz_id.as_deref(), Some("b-oversized"));
    assert_eq!(result.next_cursor.as_deref(), Some("a-first"));
    let unchanged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM public_question_search WHERE quiz_id = 'b-oversized'",
    )
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(unchanged, 0);
}
