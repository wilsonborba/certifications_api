use crate::{core::Result, dal::Store};
use serde::Serialize;
use sqlx::Row;

pub const GENERATED_CURRENT_SOURCES: &str = "generated_from_current_selected_sources";
pub const REUSED_PUBLIC_VERSION: &str = "reused_public_question_version";
pub const UNCLASSIFIED: &str = "unclassified";

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProvenanceGroupStats {
    /// Number of distinct completed attempts represented by this group.
    pub attempt_count: i64,
    /// Completed attempt-question memberships; retries are separate samples.
    pub question_count: i64,
    pub correct_question_count: i64,
    /// Correct questions divided by question_count, expressed as a percentage.
    pub correct_percentage: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct QuestionProvenanceStats {
    pub generated_from_current_selected_sources: ProvenanceGroupStats,
    pub reused_public_question_version: ProvenanceGroupStats,
    pub unclassified: ProvenanceGroupStats,
    /// Every completed attempt-question membership once, including unclassified rows.
    pub combined: ProvenanceGroupStats,
}

impl ProvenanceGroupStats {
    fn from_counts(attempt_count: i64, question_count: i64, correct_question_count: i64) -> Self {
        Self {
            attempt_count,
            question_count,
            correct_question_count,
            correct_percentage: (question_count > 0)
                .then(|| correct_question_count as f64 * 100.0 / question_count as f64),
        }
    }
}

/// Aggregate completed attempt-question rows by immutable quiz-version membership.
/// The caller supplies either "owner" or "quiz" and the already-authorized identifier.
pub async fn question_provenance_stats(
    store: &Store,
    scope_kind: &str,
    scope_id: &str,
) -> Result<QuestionProvenanceStats> {
    let rows = sqlx::query(
        "WITH completed_memberships AS (
             SELECT qvq.source_scope, a.id AS attempt_id,
                    CASE WHEN aa.is_correct = 1 THEN 1 ELSE 0 END AS correct
             FROM attempts a
             JOIN quiz_version_questions qvq ON qvq.quiz_version_id = a.quiz_version_id
             LEFT JOIN attempt_answers aa
               ON aa.attempt_id = a.id AND aa.question_version_id = qvq.question_version_id
             WHERE a.status = 'completed' AND a.score_total > 0
               AND (($1 = 'owner' AND a.owner_id = $2) OR ($1 = 'quiz' AND a.quiz_id = $2))
         ), by_scope AS (
             SELECT source_scope,
                    COUNT(DISTINCT attempt_id) AS attempt_count,
                    COUNT(*) AS question_count,
                    COALESCE(SUM(correct), 0) AS correct_count
             FROM completed_memberships
             GROUP BY source_scope
         )
         SELECT source_scope, attempt_count, question_count, correct_count FROM by_scope
         UNION ALL
         SELECT '__combined__' AS source_scope,
                COUNT(DISTINCT attempt_id) AS attempt_count,
                COUNT(*) AS question_count,
                COALESCE(SUM(correct), 0) AS correct_count
         FROM completed_memberships",
    )
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_all(&store.db)
    .await?;

    let mut result = QuestionProvenanceStats::default();
    for row in rows {
        let category: String = row.try_get("source_scope")?;
        let group = ProvenanceGroupStats::from_counts(
            row.try_get("attempt_count")?,
            row.try_get("question_count")?,
            row.try_get("correct_count")?,
        );
        match category.as_str() {
            GENERATED_CURRENT_SOURCES => result.generated_from_current_selected_sources = group,
            REUSED_PUBLIC_VERSION => result.reused_public_question_version = group,
            UNCLASSIFIED => result.unclassified = group,
            "__combined__" => result.combined = group,
            _ => return Err(crate::core::Error::unavailable()),
        }
    }
    Ok(result)
}
