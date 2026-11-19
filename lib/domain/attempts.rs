use crate::{
    core::{Error, Result, now},
    dal::{
        Store, new_id,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        auth::{CallerIdentity, require_owner},
        authoring::{ORIGIN_CORTEX, ORIGIN_CREATOR},
        guardrail,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;

pub const ATTEMPT_STATS_SCHEMA_NAME: &str = "attempt-stats-v1";
pub const ATTEMPT_STATS_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct TakingQuestion {
    pub question_id: String,
    pub question_version_id: String,
    pub language: String,
    pub origin: String,
    pub prompt: String,
    pub alternatives: Value,
    pub selected_option_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttemptView {
    pub id: String,
    pub quiz_id: String,
    pub quiz_version_id: String,
    pub status: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub monitoring_notice: String,
    pub questions: Vec<TakingQuestion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttemptHistoryItem {
    pub id: String,
    pub quiz_id: String,
    pub quiz_version_id: String,
    pub status: String,
    pub score_correct: i64,
    pub score_total: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ActivityEventInput {
    pub dedup_key: Option<String>,
    pub event_kind: String,
    #[serde(default)]
    pub duration_ms: i64,
    pub occurred_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActivityEventRecord {
    pub id: String,
    pub event_kind: String,
    pub duration_ms: i64,
    pub occurred_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActivitySummary {
    pub focus_lost_count: i64,
    pub page_hidden_count: i64,
    pub page_return_count: i64,
    pub known_away_ms: i64,
    pub reconnect_count: i64,
    pub refresh_count: i64,
    pub unknown_gap_count: i64,
    pub completeness: String,
    pub disclaimer: String,
    pub events: Vec<ActivityEventRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatsGroup {
    pub correct: i64,
    pub total: i64,
    pub percentage: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreeGroupStats {
    pub generated_with_cortex: StatsGroup,
    pub supplied_by_creator: StatsGroup,
    pub overall: StatsGroup,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewedQuestion {
    pub question_id: String,
    pub question_version_id: String,
    pub language: String,
    pub origin: String,
    pub prompt: String,
    pub alternatives: Value,
    pub selected_option_id: Option<String>,
    pub correct_id: String,
    pub is_correct: bool,
    pub explanation: String,
    pub citation_ref: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttemptResults {
    pub id: String,
    pub quiz_id: String,
    pub quiz_version_id: String,
    pub status: String,
    pub score_correct: i64,
    pub score_total: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub stats: ThreeGroupStats,
    pub activity_summary: ActivitySummary,
    pub questions: Vec<ReviewedQuestion>,
}

fn make_stats_group(correct: i64, total: i64) -> StatsGroup {
    StatsGroup {
        correct,
        total,
        percentage: if total > 0 {
            Some(((correct as f64) * 100.0) / (total as f64))
        } else {
            None
        },
    }
}

pub async fn ensure_attempt_stats_schema(store: &Store) -> Result<()> {
    if schemas::get_published(
        &store.db,
        ATTEMPT_STATS_SCHEMA_NAME,
        ATTEMPT_STATS_SCHEMA_VERSION,
    )
    .await
    .is_ok()
    {
        return Ok(());
    }
    let draft = DraftSchema {
        schema_name: ATTEMPT_STATS_SCHEMA_NAME.into(),
        version: ATTEMPT_STATS_SCHEMA_VERSION,
        description: "Attempt grading and browser activity summary".into(),
        fields: vec![
            SchemaField {
                field_path: "score_correct".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Correct answer count".into(),
            },
            SchemaField {
                field_path: "score_total".into(),
                field_type: "integer".into(),
                is_required: true,
                description: "Total question count".into(),
            },
            SchemaField {
                field_path: "completeness".into(),
                field_type: "string".into(),
                is_required: true,
                description: "Activity completeness".into(),
            },
        ],
    };
    let _ = schemas::save_draft(&store.db, &draft).await;
    schemas::publish(
        &store.db,
        ATTEMPT_STATS_SCHEMA_NAME,
        ATTEMPT_STATS_SCHEMA_VERSION,
    )
    .await?;
    Ok(())
}

async fn load_taking_questions(
    store: &Store,
    attempt_id: &str,
    quiz_version_id: &str,
) -> Result<Vec<TakingQuestion>> {
    let rows = sqlx::query(
        "SELECT qu.id AS question_id, qver.id AS qv_id, qver.language, qu.origin, qver.couch_doc_id,
                aa.selected_option_id
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         JOIN questions qu ON qu.id = qver.question_id
         LEFT JOIN attempt_answers aa ON aa.attempt_id = $1 AND aa.question_version_id = qver.id
         WHERE qvq.quiz_version_id = $2
         ORDER BY qvq.position",
    )
    .bind(attempt_id)
    .bind(quiz_version_id)
    .fetch_all(&store.db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let doc_id: String = r.get("couch_doc_id");
        let doc = store.docs.get(&doc_id).await?;
        // Taking API NEVER returns correct_id or explanation before submission.
        out.push(TakingQuestion {
            question_id: r.get("question_id"),
            question_version_id: r.get("qv_id"),
            language: r.get("language"),
            origin: r.get("origin"),
            prompt: doc["prompt"].as_str().unwrap_or("").to_string(),
            alternatives: doc["alternatives"].clone(),
            selected_option_id: r.try_get("selected_option_id").ok(),
        });
    }
    Ok(out)
}

pub async fn start_attempt(
    store: &Store,
    caller: &CallerIdentity,
    quiz_id: &str,
) -> Result<AttemptView> {
    let row =
        sqlx::query("SELECT owner_id, visibility, active_version_id FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    let visibility: String = row.get("visibility");
    let active_vid: String = row.get("active_version_id");

    if visibility != "public" && owner_id != caller.user_id && !caller.is_admin() {
        return Err(Error::missing());
    }
    if active_vid.is_empty() {
        return Err(Error::unprocessable(
            "empty_quiz: Quiz has no active version",
        ));
    }
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
    )
    .bind(&active_vid)
    .fetch_one(&store.db)
    .await?;
    if total == 0 {
        return Err(Error::unprocessable("empty_quiz: Quiz has no questions"));
    }

    let attempt_id = new_id();
    let ts = now();
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "INSERT INTO attempts(id, owner_id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at)
         VALUES($1, $2, $3, $4, 'in_progress', 0, $5, $6)",
    )
    .bind(&attempt_id)
    .bind(&caller.user_id)
    .bind(quiz_id)
    .bind(&active_vid)
    .bind(total)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO owner_activity(owner_id, total_attempts, completed_attempts, total_correct, total_answered, updated_at)
         VALUES($1, 1, 0, 0, 0, $2)
         ON CONFLICT(owner_id) DO UPDATE SET total_attempts = owner_activity.total_attempts + 1, updated_at = $2",
    )
    .bind(&caller.user_id)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    get_attempt(store, caller, &attempt_id).await
}

pub async fn get_attempt(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
) -> Result<AttemptView> {
    let row = sqlx::query(
        "SELECT id, owner_id, quiz_id, quiz_version_id, status, started_at, completed_at
         FROM attempts WHERE id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;

    let quiz_version_id: String = row.get("quiz_version_id");
    let questions = load_taking_questions(store, attempt_id, &quiz_version_id).await?;
    Ok(AttemptView {
        id: row.get("id"),
        quiz_id: row.get("quiz_id"),
        quiz_version_id,
        status: row.get("status"),
        started_at: row.get("started_at"),
        completed_at: row.try_get("completed_at").ok(),
        monitoring_notice: "Observable browser focus and visibility events are recorded during active attempts without automated cheating verdicts.".into(),
        questions,
    })
}

pub async fn list_my_attempts(
    store: &Store,
    caller: &CallerIdentity,
    limit: i64,
    offset: i64,
) -> Result<Vec<AttemptHistoryItem>> {
    let rows = sqlx::query(
        "SELECT id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at, completed_at
         FROM attempts
         WHERE owner_id = $1
         ORDER BY started_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&caller.user_id)
    .bind(limit.clamp(1, 50))
    .bind(offset.max(0))
    .fetch_all(&store.db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AttemptHistoryItem {
            id: r.get("id"),
            quiz_id: r.get("quiz_id"),
            quiz_version_id: r.get("quiz_version_id"),
            status: r.get("status"),
            score_correct: r.get("score_correct"),
            score_total: r.get("score_total"),
            started_at: r.get("started_at"),
            completed_at: r.try_get("completed_at").ok(),
        })
        .collect())
}

pub async fn save_answer(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
    question_id: &str,
    selected_option_id: &str,
) -> Result<()> {
    let row = sqlx::query("SELECT owner_id, quiz_version_id, status FROM attempts WHERE id = $1")
        .bind(attempt_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = row.get("status");
    if status != "in_progress" {
        return Err(Error::conflict(
            "attempt_already_submitted: Answers cannot be modified after submission",
        ));
    }
    let quiz_version_id: String = row.get("quiz_version_id");

    let q_row = sqlx::query(
        "SELECT qver.id AS qv_id, qver.couch_doc_id
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         WHERE qvq.quiz_version_id = $1 AND qver.question_id = $2",
    )
    .bind(&quiz_version_id)
    .bind(question_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let qv_id: String = q_row.get("qv_id");
    let doc_id: String = q_row.get("couch_doc_id");
    let doc = store.docs.get(&doc_id).await?;
    let valid_opt = doc["alternatives"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|a| a["id"].as_str() == Some(selected_option_id.trim()))
        })
        .unwrap_or(false);
    if !valid_opt {
        return Err(Error::unprocessable(
            "invalid_option: Selected option is not valid for this question",
        ));
    }

    let ts = now();
    sqlx::query(
        "INSERT INTO attempt_answers(id, attempt_id, question_version_id, selected_option_id, is_correct, answered_at)
         VALUES($1, $2, $3, $4, 0, $5)
         ON CONFLICT(attempt_id, question_version_id) DO UPDATE SET selected_option_id = $4, answered_at = $5",
    )
    .bind(new_id())
    .bind(attempt_id)
    .bind(&qv_id)
    .bind(selected_option_id.trim())
    .bind(ts)
    .execute(&store.db)
    .await?;
    Ok(())
}

pub async fn record_activity_events(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
    events: Vec<ActivityEventInput>,
) -> Result<usize> {
    let row = sqlx::query("SELECT owner_id, status FROM attempts WHERE id = $1")
        .bind(attempt_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = row.get("status");
    if status != "in_progress" {
        return Err(Error::conflict(
            "attempt_already_submitted: Activity events only apply to active attempts",
        ));
    }

    let ts = now();
    let mut inserted = 0usize;
    for ev in events {
        if !matches!(
            ev.event_kind.as_str(),
            "focus_lost"
                | "page_hidden"
                | "page_return"
                | "away_duration"
                | "reconnect"
                | "refresh"
                | "gap_unknown"
        ) {
            return Err(Error::unprocessable(
                "invalid_event_kind: Unsupported attempt activity event kind",
            ));
        }
        let occurred_at = ev.occurred_at.unwrap_or(ts).max(0);
        let dedup_key = ev
            .dedup_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{}:{occurred_at}", ev.event_kind));
        let res = sqlx::query(
            "INSERT INTO attempt_activity_events(id, attempt_id, dedup_key, event_kind, duration_ms, occurred_at, created_at)
             VALUES($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT(attempt_id, dedup_key) DO NOTHING",
        )
        .bind(new_id())
        .bind(attempt_id)
        .bind(&dedup_key)
        .bind(&ev.event_kind)
        .bind(ev.duration_ms.max(0))
        .bind(occurred_at)
        .bind(ts)
        .execute(&store.db)
        .await?;
        if res.rows_affected() > 0 {
            inserted += 1;
        }
    }
    Ok(inserted)
}

pub async fn load_activity_summary(store: &Store, attempt_id: &str) -> Result<ActivitySummary> {
    let rows = sqlx::query(
        "SELECT id, event_kind, duration_ms, occurred_at
         FROM attempt_activity_events
         WHERE attempt_id = $1
         ORDER BY occurred_at ASC, created_at ASC",
    )
    .bind(attempt_id)
    .fetch_all(&store.db)
    .await?;

    let mut focus_lost_count = 0i64;
    let mut page_hidden_count = 0i64;
    let mut page_return_count = 0i64;
    let mut known_away_ms = 0i64;
    let mut reconnect_count = 0i64;
    let mut refresh_count = 0i64;
    let mut unknown_gap_count = 0i64;
    let mut events = Vec::with_capacity(rows.len());

    for r in rows {
        let kind: String = r.get("event_kind");
        let dur: i64 = r.get("duration_ms");
        match kind.as_str() {
            "focus_lost" => focus_lost_count += 1,
            "page_hidden" => page_hidden_count += 1,
            "page_return" => page_return_count += 1,
            "away_duration" => known_away_ms += dur,
            "reconnect" => reconnect_count += 1,
            "refresh" => refresh_count += 1,
            "gap_unknown" => unknown_gap_count += 1,
            _ => {}
        }
        events.push(ActivityEventRecord {
            id: r.get("id"),
            event_kind: kind,
            duration_ms: dur,
            occurred_at: r.get("occurred_at"),
        });
    }

    let completeness = if unknown_gap_count > 0 || reconnect_count > 0 || refresh_count > 0 {
        "partial_with_interruptions"
    } else if events.is_empty() {
        "no_events_recorded"
    } else {
        "complete_observed"
    };

    Ok(ActivitySummary {
        focus_lost_count,
        page_hidden_count,
        page_return_count,
        known_away_ms,
        reconnect_count,
        refresh_count,
        unknown_gap_count,
        completeness: completeness.into(),
        disclaimer: "Observed browser lifecycle summary only; absence of events or presence of interruptions is not a cheating verdict.".into(),
        events,
    })
}

pub async fn submit_attempt(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
) -> Result<AttemptResults> {
    let row = sqlx::query("SELECT owner_id, quiz_version_id, status FROM attempts WHERE id = $1")
        .bind(attempt_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = row.get("status");
    if status == "completed" {
        return get_results(store, caller, attempt_id).await;
    }

    let quiz_version_id: String = row.get("quiz_version_id");
    let q_rows = sqlx::query(
        "SELECT qver.id AS qv_id, qver.couch_doc_id, aa.selected_option_id
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         LEFT JOIN attempt_answers aa ON aa.attempt_id = $1 AND aa.question_version_id = qver.id
         WHERE qvq.quiz_version_id = $2
         ORDER BY qvq.position",
    )
    .bind(attempt_id)
    .bind(&quiz_version_id)
    .fetch_all(&store.db)
    .await?;

    let mut score_correct = 0i64;
    let score_total = q_rows.len() as i64;
    let mut answered_count = 0i64;
    let mut updates = Vec::new();

    for qr in q_rows {
        let qv_id: String = qr.get("qv_id");
        let doc_id: String = qr.get("couch_doc_id");
        let selected: Option<String> = qr.try_get("selected_option_id").ok();
        let doc = store.docs.get(&doc_id).await?;
        let correct_id = doc["correct_id"].as_str().unwrap_or("");
        let is_correct = selected.as_deref() == Some(correct_id);
        if selected.is_some() {
            answered_count += 1;
        }
        if is_correct {
            score_correct += 1;
        }
        updates.push((qv_id, if is_correct { 1i64 } else { 0i64 }));
    }

    let ts = now();
    let mut tx = store.db.begin().await?;
    for (qv_id, correct_flag) in updates {
        sqlx::query(
            "UPDATE attempt_answers SET is_correct = $1 WHERE attempt_id = $2 AND question_version_id = $3",
        )
        .bind(correct_flag)
        .bind(attempt_id)
        .bind(qv_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE attempts SET status = 'completed', score_correct = $1, score_total = $2, completed_at = $3 WHERE id = $4",
    )
    .bind(score_correct)
    .bind(score_total)
    .bind(ts)
    .bind(attempt_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE owner_activity
         SET completed_attempts = completed_attempts + 1,
             total_correct = total_correct + $1,
             total_answered = total_answered + $2,
             updated_at = $3
         WHERE owner_id = $4",
    )
    .bind(score_correct)
    .bind(answered_count)
    .bind(ts)
    .bind(&owner_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let activity = load_activity_summary(store, attempt_id).await?;
    let _ = ensure_attempt_stats_schema(store).await;
    let doc_id = new_id();
    let stats_payload = json!({
        "score_correct": score_correct,
        "score_total": score_total,
        "completeness": activity.completeness,
    });
    if guardrail::store_validated_document(
        store,
        ATTEMPT_STATS_SCHEMA_NAME,
        ATTEMPT_STATS_SCHEMA_VERSION,
        &doc_id,
        &stats_payload,
    )
    .await
    .is_ok()
    {
        let _ = sqlx::query(
            "INSERT INTO attempt_statistics(id, attempt_id, schema_name, schema_version, couch_doc_id, created_at)
             VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(new_id())
        .bind(attempt_id)
        .bind(ATTEMPT_STATS_SCHEMA_NAME)
        .bind(ATTEMPT_STATS_SCHEMA_VERSION)
        .bind(&doc_id)
        .bind(ts)
        .execute(&store.db)
        .await;
    }

    get_results(store, caller, attempt_id).await
}

pub async fn get_results(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
) -> Result<AttemptResults> {
    let row = sqlx::query(
        "SELECT id, owner_id, quiz_id, quiz_version_id, status, score_correct, score_total, started_at, completed_at
         FROM attempts WHERE id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = row.get("status");
    if status != "completed" {
        return Err(Error::unprocessable(
            "attempt_not_completed: Results and answer keys are only available after submission",
        ));
    }

    let quiz_version_id: String = row.get("quiz_version_id");
    let q_rows = sqlx::query(
        "SELECT qu.id AS question_id, qver.id AS qv_id, qver.language, qu.origin, qver.couch_doc_id,
                aa.selected_option_id, aa.is_correct
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         JOIN questions qu ON qu.id = qver.question_id
         LEFT JOIN attempt_answers aa ON aa.attempt_id = $1 AND aa.question_version_id = qver.id
         WHERE qvq.quiz_version_id = $2
         ORDER BY qvq.position",
    )
    .bind(attempt_id)
    .bind(&quiz_version_id)
    .fetch_all(&store.db)
    .await?;

    let mut questions = Vec::with_capacity(q_rows.len());
    let mut cortex_correct = 0i64;
    let mut cortex_total = 0i64;
    let mut creator_correct = 0i64;
    let mut creator_total = 0i64;

    for r in q_rows {
        let doc_id: String = r.get("couch_doc_id");
        let origin: String = r.get("origin");
        let selected: Option<String> = r.try_get("selected_option_id").ok();
        let doc = store.docs.get(&doc_id).await?;
        let correct_id = doc["correct_id"].as_str().unwrap_or("").to_string();
        let is_correct = selected.as_deref() == Some(correct_id.as_str());

        if origin == ORIGIN_CORTEX {
            cortex_total += 1;
            if is_correct {
                cortex_correct += 1;
            }
        } else if origin == ORIGIN_CREATOR {
            creator_total += 1;
            if is_correct {
                creator_correct += 1;
            }
        }

        questions.push(ReviewedQuestion {
            question_id: r.get("question_id"),
            question_version_id: r.get("qv_id"),
            language: r.get("language"),
            origin,
            prompt: doc["prompt"].as_str().unwrap_or("").to_string(),
            alternatives: doc["alternatives"].clone(),
            selected_option_id: selected,
            correct_id,
            is_correct,
            explanation: doc["explanation"].as_str().unwrap_or("").to_string(),
            citation_ref: doc["citation_ref"].as_str().unwrap_or("").to_string(),
        });
    }

    let score_correct: i64 = row.get("score_correct");
    let score_total: i64 = row.get("score_total");
    let stats = ThreeGroupStats {
        generated_with_cortex: make_stats_group(cortex_correct, cortex_total),
        supplied_by_creator: make_stats_group(creator_correct, creator_total),
        overall: make_stats_group(score_correct, score_total),
    };
    let activity_summary = load_activity_summary(store, attempt_id).await?;

    Ok(AttemptResults {
        id: row.get("id"),
        quiz_id: row.get("quiz_id"),
        quiz_version_id,
        status,
        score_correct,
        score_total,
        started_at: row.get("started_at"),
        completed_at: row.try_get("completed_at").ok(),
        stats,
        activity_summary,
        questions,
    })
}
