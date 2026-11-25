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
    /// Immutable membership denominator used by the quiz scorer; unanswered items count here.
    pub question_count: i64,
    pub correct_question_count: i64,
    pub answered_question_count: i64,
    pub unanswered_question_count: i64,
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
    /// Completed attempts excluded because membership or answer records do not reconcile to grading.
    pub unavailable_attempt_count: i64,
}

impl ProvenanceGroupStats {
    fn from_counts(
        attempt_count: i64,
        question_count: i64,
        correct_question_count: i64,
        answered_question_count: i64,
        unanswered_question_count: i64,
    ) -> Self {
        Self {
            attempt_count,
            question_count,
            correct_question_count,
            answered_question_count,
            unanswered_question_count,
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
        "WITH scoped_attempts AS (
             SELECT a.id, a.quiz_version_id, a.score_total, a.score_correct
             FROM attempts a
             WHERE a.status = 'completed'
               AND (($1 = 'owner' AND a.owner_id = $2) OR ($1 = 'quiz' AND a.quiz_id = $2))
         ), attempt_integrity AS (
             SELECT a.id, a.quiz_version_id, a.score_total, a.score_correct,
                    COUNT(qvq.question_version_id) AS membership_count,
                    COUNT(aa.id) AS matched_answer_count,
                    (SELECT COUNT(*) FROM attempt_answers any_answer WHERE any_answer.attempt_id = a.id)
                        AS answer_count,
                    COALESCE(SUM(CASE WHEN aa.is_correct = 1 THEN 1 ELSE 0 END), 0) AS item_correct_count
             FROM scoped_attempts a
             LEFT JOIN quiz_version_questions qvq ON qvq.quiz_version_id = a.quiz_version_id
             LEFT JOIN attempt_answers aa
               ON aa.attempt_id = a.id AND aa.question_version_id = qvq.question_version_id
             GROUP BY a.id, a.quiz_version_id, a.score_total, a.score_correct
         ), valid_attempts AS (
             SELECT id, quiz_version_id FROM attempt_integrity
             WHERE score_total > 0 AND membership_count = score_total
               AND item_correct_count = score_correct AND matched_answer_count = answer_count
         ), completed_memberships AS (
             SELECT qvq.source_scope, a.id AS attempt_id,
                    CASE WHEN aa.is_correct = 1 THEN 1 ELSE 0 END AS correct,
                    CASE WHEN aa.id IS NULL THEN 0 ELSE 1 END AS answered
             FROM valid_attempts a
             JOIN quiz_version_questions qvq ON qvq.quiz_version_id = a.quiz_version_id
             LEFT JOIN attempt_answers aa
               ON aa.attempt_id = a.id AND aa.question_version_id = qvq.question_version_id
         ), by_scope AS (
             SELECT source_scope,
                    COUNT(DISTINCT attempt_id) AS attempt_count,
                    COUNT(*) AS question_count,
                    COALESCE(SUM(correct), 0) AS correct_count,
                    COALESCE(SUM(answered), 0) AS answered_count,
                    COUNT(*) - COALESCE(SUM(answered), 0) AS unanswered_count
             FROM completed_memberships
             GROUP BY source_scope
         ), unavailable AS (
             SELECT COUNT(*) AS unavailable_attempt_count FROM attempt_integrity
             WHERE score_total <= 0 OR membership_count <> score_total
                OR item_correct_count <> score_correct OR matched_answer_count <> answer_count
         )
         SELECT source_scope, attempt_count, question_count, correct_count,
                answered_count, unanswered_count, unavailable_attempt_count
         FROM by_scope CROSS JOIN unavailable
         UNION ALL
         SELECT '__combined__' AS source_scope,
                COUNT(DISTINCT attempt_id) AS attempt_count,
                COUNT(*) AS question_count,
                COALESCE(SUM(correct), 0) AS correct_count,
                COALESCE(SUM(answered), 0) AS answered_count,
                COUNT(*) - COALESCE(SUM(answered), 0) AS unanswered_count,
                (SELECT unavailable_attempt_count FROM unavailable) AS unavailable_attempt_count
         FROM completed_memberships",
    )
    .bind(scope_kind)
    .bind(scope_id)
    .fetch_all(&store.db)
    .await?;

    let mut result = QuestionProvenanceStats::default();
    for row in rows {
        result.unavailable_attempt_count = row.try_get("unavailable_attempt_count")?;
        let category: String = row.try_get("source_scope")?;
        let group = ProvenanceGroupStats::from_counts(
            row.try_get("attempt_count")?,
            row.try_get("question_count")?,
            row.try_get("correct_count")?,
            row.try_get("answered_count")?,
            row.try_get("unanswered_count")?,
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
