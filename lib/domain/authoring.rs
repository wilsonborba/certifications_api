use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id, schemas},
    domain::{
        auth::{CallerIdentity, require_owner},
        catalog::{self, public_question_version_sources_in_tx},
        generations::{
            LEGACY_QUESTION_SCHEMA_VERSION, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION,
        },
        guardrail,
        provenance::{REUSED_PUBLIC_VERSION, UNCLASSIFIED},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::HashSet;

pub const ORIGIN_CORTEX: &str = "generated_with_cortex";
pub const ORIGIN_CREATOR: &str = "supplied_by_creator";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuestionAlternative {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionVisual {
    pub id: String,
    #[serde(skip_serializing)]
    pub fsm_ref: String,
    pub mime_type: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadVisualReceipt {
    pub id: String,
    pub mime_type: String,
    pub created_at: i64,
    pub prepared_question_version_id: String,
    pub result_question_version_id: String,
    pub result_quiz_version_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthorQuestion {
    pub id: String,
    pub version_id: String,
    pub version_num: i64,
    pub language: String,
    pub origin: String,
    pub source_scope: String,
    pub origin_quiz_id: Option<String>,
    pub origin_quiz_version_id: Option<String>,
    pub prompt: String,
    pub alternatives: Value,
    pub correct_id: String,
    pub explanation: String,
    pub citation_ref: String,
    pub visuals: Vec<QuestionVisual>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReuseQuestionsInput {
    pub question_version_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReuseQuestionsResult {
    pub quiz_id: String,
    pub quiz_version_id: Option<String>,
    pub added_count: i64,
    pub already_present_count: i64,
    pub total_question_count: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateQuestionInput {
    pub language: Option<String>,
    pub prompt: String,
    pub alternatives: Vec<QuestionAlternative>,
    pub correct_id: String,
    pub explanation: String,
    pub source_id: Option<String>,
    pub citation_ref: Option<String>,
    pub visual_spec: Option<Value>,
    pub origin: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateQuestionInput {
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub alternatives: Option<Vec<QuestionAlternative>>,
    pub correct_id: Option<String>,
    pub explanation: Option<String>,
    pub citation_ref: Option<String>,
    pub origin: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CsvValidRow {
    pub row_number: usize,
    pub language: String,
    pub prompt: String,
    pub correct_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CsvInvalidRow {
    pub row_number: usize,
    pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CsvImportResult {
    pub preview_only: bool,
    pub imported_count: usize,
    pub valid_rows: Vec<CsvValidRow>,
    pub invalid_rows: Vec<CsvInvalidRow>,
    pub questions: Vec<AuthorQuestion>,
}

fn validate_language(lang: &str) -> Result<()> {
    if matches!(lang, "en" | "pt-BR" | "th") {
        Ok(())
    } else {
        Err(Error::unprocessable(
            "invalid_language: Language must be en, pt-BR, or th",
        ))
    }
}

pub fn validate_origin_value(origin: &str) -> Result<()> {
    if origin == ORIGIN_CORTEX || origin == ORIGIN_CREATOR {
        Ok(())
    } else {
        Err(Error::unprocessable(
            "invalid_origin: Origin must be generated_with_cortex or supplied_by_creator",
        ))
    }
}

fn validate_question_fields(
    language: &str,
    prompt: &str,
    alternatives: &[QuestionAlternative],
    correct_id: &str,
    explanation: &str,
) -> Result<()> {
    validate_language(language)?;
    if prompt.trim().is_empty() {
        return Err(Error::unprocessable("invalid_prompt: Prompt is required"));
    }
    if explanation.trim().is_empty() {
        return Err(Error::unprocessable(
            "invalid_explanation: Explanation is required",
        ));
    }
    if alternatives.len() < 2 {
        return Err(Error::unprocessable(
            "invalid_alternatives: At least two alternatives are required",
        ));
    }
    let mut seen = HashSet::new();
    for alt in alternatives {
        if alt.id.trim().is_empty() || alt.text.trim().is_empty() {
            return Err(Error::unprocessable(
                "invalid_alternatives: Alternative id and text must be non-empty",
            ));
        }
        if !seen.insert(alt.id.trim().to_string()) {
            return Err(Error::unprocessable(
                "invalid_alternatives: Alternative ids must be unique",
            ));
        }
    }
    if !seen.contains(correct_id.trim()) {
        return Err(Error::unprocessable(
            "invalid_correct_id: correct_id must match one of the alternatives",
        ));
    }
    Ok(())
}

async fn load_owned_study_quiz(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<(String, Option<String>)> {
    let row = sqlx::query("SELECT owner_id, quiz_id FROM studies WHERE id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;
    let quiz_id: Option<String> = row.get("quiz_id");
    Ok((owner_id, quiz_id.filter(|s| !s.is_empty())))
}

async fn ensure_study_quiz(
    store: &Store,
    owner_id: &str,
    study_id: &str,
    existing_quiz_id: Option<String>,
) -> Result<String> {
    if let Some(qid) = existing_quiz_id {
        return Ok(qid);
    }
    let qid = new_id();
    let ts = now();
    let mut tx = store.db.begin().await?;
    sqlx::query(
        "INSERT INTO quizzes(id, owner_id, study_id, visibility, origin, active_version_id, created_at, updated_at)
         VALUES($1, $2, $3, 'private', $4, '', $5, $5)",
    )
    .bind(&qid)
    .bind(owner_id)
    .bind(study_id)
    .bind(ORIGIN_CREATOR)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE studies SET quiz_id = $1, updated_at = $2 WHERE id = $3")
        .bind(&qid)
        .bind(ts)
        .bind(study_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(qid)
}

async fn ensure_study_quiz_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    owner_id: &str,
    study_id: &str,
) -> Result<String> {
    let qid = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO quizzes(id, owner_id, study_id, visibility, origin, active_version_id, created_at, updated_at)
         VALUES($1, $2, $3, 'private', $4, '', $5, $5)",
    )
    .bind(&qid)
    .bind(owner_id)
    .bind(study_id)
    .bind(ORIGIN_CREATOR)
    .bind(ts)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE studies SET quiz_id = $1, updated_at = $2 WHERE id = $3")
        .bind(&qid)
        .bind(ts)
        .bind(study_id)
        .execute(&mut **tx)
        .await?;
    Ok(qid)
}

fn canonical_visual_ids(doc: &Value) -> Result<Vec<String>> {
    let value = doc
        .get("visual_ids")
        .ok_or_else(|| Error::unprocessable("invalid_visual_ids: schema2 requires visual_ids"))?;
    let ids = value
        .as_array()
        .ok_or_else(|| Error::unprocessable("invalid_visual_ids: visual_ids must be an array"))?;
    if ids.len() > 64 {
        return Err(Error::unprocessable(
            "visual_limit_reached: A question can reference at most 64 visuals",
        ));
    }
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(ids.len());
    for value in ids {
        let id = value.as_str().ok_or_else(|| {
            Error::unprocessable("invalid_visual_ids: visual IDs must be strings")
        })?;
        if id.is_empty() || id.len() > 128 || id.trim() != id || !seen.insert(id) {
            return Err(Error::unprocessable(
                "invalid_visual_ids: visual IDs must be unique, nonempty strings of at most 128 UTF-8 bytes",
            ));
        }
        result.push(id.to_string());
    }
    Ok(result)
}

fn supported_question_visual_ids(
    schema_name: &str,
    schema_version: i64,
    doc: &Value,
) -> Result<Vec<String>> {
    if schema_name != QUESTION_SCHEMA_NAME
        || ![LEGACY_QUESTION_SCHEMA_VERSION, QUESTION_SCHEMA_VERSION].contains(&schema_version)
    {
        return Err(Error::unavailable());
    }
    if schema_version == LEGACY_QUESTION_SCHEMA_VERSION {
        if doc
            .get("visual_ids")
            .is_some_and(|value| value.as_array().is_none_or(|ids| !ids.is_empty()))
        {
            return Err(Error::unavailable());
        }
        return Ok(Vec::new());
    }
    canonical_visual_ids(doc)
}

async fn load_visuals(
    store: &Store,
    ids: &[String],
    question_id: &str,
) -> Result<Vec<QuestionVisual>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut visuals = Vec::with_capacity(ids.len());
    for id in ids {
        let row = sqlx::query("SELECT v.id, v.fsm_ref, v.mime_type, v.created_at FROM question_visuals v JOIN question_versions qv ON qv.id = v.result_question_version_id WHERE v.id = $1 AND v.question_id = $2 AND qv.question_id = $2 AND v.mime_type = 'image/png'")
            .bind(id).bind(question_id).fetch_optional(&store.db).await?.ok_or_else(Error::missing)?;
        visuals.push(QuestionVisual {
            id: row.get("id"),
            fsm_ref: row.get("fsm_ref"),
            mime_type: row.get("mime_type"),
            created_at: row.get("created_at"),
        });
    }
    Ok(visuals)
}

async fn load_visuals_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    ids: &[String],
    question_id: &str,
) -> Result<Vec<QuestionVisual>> {
    let mut visuals = Vec::with_capacity(ids.len());
    for id in ids {
        let row = sqlx::query("SELECT v.id, v.fsm_ref, v.mime_type, v.created_at FROM question_visuals v JOIN question_versions qv ON qv.id = v.result_question_version_id WHERE v.id = $1 AND v.question_id = $2 AND qv.question_id = $2 AND v.mime_type = 'image/png'")
            .bind(id).bind(question_id).fetch_optional(&mut **tx).await?.ok_or_else(Error::missing)?;
        visuals.push(QuestionVisual {
            id: row.get("id"),
            fsm_ref: row.get("fsm_ref"),
            mime_type: row.get("mime_type"),
            created_at: row.get("created_at"),
        });
    }
    Ok(visuals)
}

async fn load_question_detail(
    store: &Store,
    question_id: &str,
    qv_id: &str,
) -> Result<AuthorQuestion> {
    let row = sqlx::query(
        "SELECT q.origin, qv.version_num, qv.language, qv.couch_doc_id, qv.schema_name, qv.schema_version
         FROM questions q
         JOIN question_versions qv ON qv.question_id = q.id
         WHERE q.id = $1 AND qv.id = $2",
    )
    .bind(question_id)
    .bind(qv_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let doc_id: String = row.get("couch_doc_id");
    let doc = store.docs.get(&doc_id).await?;
    let ids = supported_question_visual_ids(
        &row.get::<String, _>("schema_name"),
        row.get("schema_version"),
        &doc,
    )?;
    let visuals = load_visuals(store, &ids, question_id).await?;
    Ok(AuthorQuestion {
        id: question_id.to_string(),
        version_id: qv_id.to_string(),
        version_num: row.get("version_num"),
        language: row.get("language"),
        origin: row.get("origin"),
        source_scope: UNCLASSIFIED.into(),
        origin_quiz_id: None,
        origin_quiz_version_id: None,
        prompt: doc["prompt"].as_str().unwrap_or("").to_string(),
        alternatives: doc["alternatives"].clone(),
        correct_id: doc["correct_id"].as_str().unwrap_or("").to_string(),
        explanation: doc["explanation"].as_str().unwrap_or("").to_string(),
        citation_ref: doc["citation_ref"].as_str().unwrap_or("").to_string(),
        visuals,
    })
}

#[derive(Clone, Debug)]
struct QuizQuestionMembership {
    question_id: String,
    question_version_id: String,
    source_scope: String,
    origin_quiz_id: Option<String>,
    origin_quiz_version_id: Option<String>,
}

async fn current_quiz_version_questions(
    store: &Store,
    quiz_id: &str,
) -> Result<(Option<String>, Vec<QuizQuestionMembership>)> {
    let active_vid: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_one(&store.db)
            .await?;
    if active_vid.is_empty() {
        return Ok((None, Vec::new()));
    }
    let rows = sqlx::query(
        "SELECT qv.question_id, qvq.question_version_id, qvq.source_scope,
                qvq.origin_quiz_id, qvq.origin_quiz_version_id
         FROM quiz_version_questions qvq
         JOIN question_versions qv ON qv.id = qvq.question_version_id
         WHERE qvq.quiz_version_id = $1
         ORDER BY qvq.position",
    )
    .bind(&active_vid)
    .fetch_all(&store.db)
    .await?;
    let items = rows
        .into_iter()
        .map(|r| QuizQuestionMembership {
            question_id: r.get("question_id"),
            question_version_id: r.get("question_version_id"),
            source_scope: r.get("source_scope"),
            origin_quiz_id: r.get("origin_quiz_id"),
            origin_quiz_version_id: r.get("origin_quiz_version_id"),
        })
        .collect();
    Ok((Some(active_vid), items))
}

async fn current_quiz_version_questions_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    quiz_id: &str,
) -> Result<(Option<String>, Vec<QuizQuestionMembership>)> {
    let active_vid: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_one(&mut **tx)
            .await?;
    if active_vid.is_empty() {
        return Ok((None, Vec::new()));
    }
    let rows = sqlx::query(
        "SELECT qv.question_id, qvq.question_version_id, qvq.source_scope,
                qvq.origin_quiz_id, qvq.origin_quiz_version_id
         FROM quiz_version_questions qvq
         JOIN question_versions qv ON qv.id = qvq.question_version_id
         WHERE qvq.quiz_version_id = $1
         ORDER BY qvq.position",
    )
    .bind(&active_vid)
    .fetch_all(&mut **tx)
    .await?;
    let items = rows
        .into_iter()
        .map(|r| QuizQuestionMembership {
            question_id: r.get("question_id"),
            question_version_id: r.get("question_version_id"),
            source_scope: r.get("source_scope"),
            origin_quiz_id: r.get("origin_quiz_id"),
            origin_quiz_version_id: r.get("origin_quiz_version_id"),
        })
        .collect();
    Ok((Some(active_vid), items))
}

async fn write_new_quiz_version(
    store: &Store,
    quiz_id: &str,
    items: &[QuizQuestionMembership],
    expected_active_version_id: Option<&str>,
) -> Result<String> {
    let mut tx = store.db.begin().await?;
    let query = if store.postgres() {
        "SELECT active_version_id FROM quizzes WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT active_version_id FROM quizzes WHERE id = $1"
    };
    let active_version_id: String = sqlx::query_scalar(query)
        .bind(quiz_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    let active_version_id = (!active_version_id.is_empty()).then_some(active_version_id);
    if active_version_id.as_deref() != expected_active_version_id {
        return Err(Error::conflict(
            "quiz_version_changed: Quiz membership changed during question publication",
        ));
    }
    let qver_id = write_new_quiz_version_in_tx(store, &mut tx, quiz_id, items, None).await?;
    tx.commit().await?;
    Ok(qver_id)
}

async fn write_new_quiz_version_in_tx(
    store: &Store,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    quiz_id: &str,
    items: &[QuizQuestionMembership],
    reserved_id: Option<&str>,
) -> Result<String> {
    let next_ver: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_num), 0) + 1 FROM quiz_versions WHERE quiz_id = $1",
    )
    .bind(quiz_id)
    .fetch_one(&mut **tx)
    .await?;
    let qver_id = reserved_id.map(str::to_string).unwrap_or_else(new_id);
    let ts = now();
    sqlx::query(
        "INSERT INTO quiz_versions(id, quiz_id, version_num, title, created_at) VALUES($1, $2, $3, $4, $5)",
    )
    .bind(&qver_id)
    .bind(quiz_id)
    .bind(next_ver)
    .bind(format!("Study Quiz v{next_ver}"))
    .bind(ts)
    .execute(&mut **tx)
    .await?;
    for (pos, item) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO quiz_version_questions(quiz_version_id, position, question_version_id,
                source_scope, origin_quiz_id, origin_quiz_version_id)
             VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(&qver_id)
        .bind(pos as i64)
        .bind(&item.question_version_id)
        .bind(&item.source_scope)
        .bind(&item.origin_quiz_id)
        .bind(&item.origin_quiz_version_id)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query("UPDATE quizzes SET active_version_id = $1, updated_at = $2 WHERE id = $3")
        .bind(&qver_id)
        .bind(ts)
        .bind(quiz_id)
        .execute(&mut **tx)
        .await?;
    catalog::refresh_public_question_search_in_tx(store, tx, quiz_id).await?;
    Ok(qver_id)
}

pub async fn list_study_questions(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<Vec<AuthorQuestion>> {
    let (_owner_id, quiz_id) = load_owned_study_quiz(store, caller, study_id).await?;
    let Some(qid) = quiz_id else {
        return Ok(Vec::new());
    };
    let (_vid, items) = current_quiz_version_questions(store, &qid).await?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut question =
            load_question_detail(store, &item.question_id, &item.question_version_id).await?;
        question.source_scope = item.source_scope.clone();
        question.origin_quiz_id = item.origin_quiz_id.clone();
        question.origin_quiz_version_id = item.origin_quiz_version_id.clone();
        if item.source_scope == REUSED_PUBLIC_VERSION {
            // Source citations and visual storage references remain attached to the
            // canonical version but are not shared through the target study API.
            question.citation_ref.clear();
            question.visuals.clear();
        }
        out.push(question);
    }
    Ok(out)
}

pub async fn reuse_public_question_versions(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_version_ids: &[String],
) -> Result<ReuseQuestionsResult> {
    const MAX_SELECTION: usize = 50;
    if question_version_ids.is_empty() || question_version_ids.len() > MAX_SELECTION {
        return Err(Error::unprocessable(
            "invalid_question_selection: Select between 1 and 50 public question versions",
        ));
    }
    if question_version_ids.iter().any(|id| id.trim().is_empty()) {
        return Err(Error::unprocessable(
            "invalid_question_selection: Question version IDs must be non-empty",
        ));
    }

    let selected_ids: Vec<String> = question_version_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut tx = store.db.begin().await?;
    let lifecycle_query = if store.postgres() {
        "SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1"
    };
    let lifecycle = sqlx::query(lifecycle_query)
        .bind(study_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = lifecycle.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = lifecycle.get("status");
    let expires_at: Option<i64> = lifecycle.get("expires_at");
    if status == "draft" && expires_at.is_some_and(|expires| now() > expires) {
        sqlx::query("UPDATE studies SET status = 'expired' WHERE id = $1 AND status = 'draft'")
            .bind(study_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if status == "expired" {
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    let running: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generation_jobs WHERE study_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(study_id)
    .fetch_one(&mut *tx)
    .await?;
    if running > 0 {
        return Err(Error::conflict(
            "generation_in_progress: Public questions cannot be reused while study generation is queued or running",
        ));
    }
    let existing_quiz_id: Option<String> = lifecycle
        .try_get("quiz_id")
        .ok()
        .filter(|id: &String| !id.is_empty());
    let sources = public_question_version_sources_in_tx(&mut tx, Some(&selected_ids)).await?;
    let mut source_by_version = std::collections::HashMap::new();
    for source in sources {
        source_by_version
            .entry(source.question_version_id.clone())
            .or_insert(source);
    }
    if source_by_version.len() != selected_ids.len() {
        return Err(Error::unprocessable(
            "question_not_reusable: One or more selected question versions are not currently eligible for public reuse",
        ));
    }

    let (quiz_id, current_version_id, mut memberships) = if let Some(quiz_id) = existing_quiz_id {
        let quiz_query = if store.postgres() {
            "SELECT owner_id FROM quizzes WHERE id = $1 FOR UPDATE"
        } else {
            "SELECT owner_id FROM quizzes WHERE id = $1"
        };
        let quiz = sqlx::query(quiz_query)
            .bind(&quiz_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(Error::missing)?;
        require_owner(caller, &quiz.get::<String, _>("owner_id"))?;
        let (version_id, items) = current_quiz_version_questions_in_tx(&mut tx, &quiz_id).await?;
        (quiz_id, version_id, items)
    } else {
        (String::new(), None, Vec::new())
    };
    let mut existing_question_ids: HashSet<String> = memberships
        .iter()
        .map(|membership| membership.question_id.clone())
        .collect();
    let mut requested_ids = HashSet::new();
    let mut already_present_count = 0i64;
    let mut added_count = 0i64;
    for question_version_id in question_version_ids {
        let Some(source) = source_by_version.get(question_version_id) else {
            return Err(Error::unprocessable(
                "question_not_reusable: One or more selected question versions are not currently eligible for public reuse",
            ));
        };
        if !requested_ids.insert(question_version_id.clone())
            || !existing_question_ids.insert(source.question_id.clone())
        {
            already_present_count += 1;
            continue;
        }
        let (origin_quiz_id, origin_quiz_version_id) = if source.source_scope
            == REUSED_PUBLIC_VERSION
        {
            match (
                source.origin_quiz_id.clone(),
                source.origin_quiz_version_id.clone(),
            ) {
                (Some(origin_id), Some(origin_version_id)) => (origin_id, origin_version_id),
                _ => {
                    return Err(Error::unprocessable(
                        "question_not_reusable: One or more selected question versions lack verifiable origin evidence",
                    ));
                }
            }
        } else {
            (
                source.source_quiz_id.clone(),
                source.source_quiz_version_id.clone(),
            )
        };
        memberships.push(QuizQuestionMembership {
            question_id: source.question_id.clone(),
            question_version_id: source.question_version_id.clone(),
            source_scope: REUSED_PUBLIC_VERSION.into(),
            origin_quiz_id: Some(origin_quiz_id),
            origin_quiz_version_id: Some(origin_quiz_version_id),
        });
        added_count += 1;
    }

    if added_count == 0 {
        tx.commit().await?;
        return Ok(ReuseQuestionsResult {
            quiz_id,
            quiz_version_id: current_version_id,
            added_count,
            already_present_count,
            total_question_count: memberships.len() as i64,
        });
    }
    let quiz_id = if quiz_id.is_empty() {
        ensure_study_quiz_in_tx(&mut tx, &owner_id, study_id).await?
    } else {
        quiz_id
    };
    let quiz_version_id =
        write_new_quiz_version_in_tx(store, &mut tx, &quiz_id, &memberships, None).await?;
    tx.commit().await?;
    Ok(ReuseQuestionsResult {
        quiz_id,
        quiz_version_id: Some(quiz_version_id),
        added_count,
        already_present_count,
        total_question_count: memberships.len() as i64,
    })
}

pub async fn create_question(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    input: CreateQuestionInput,
) -> Result<AuthorQuestion> {
    if input.origin.is_some() {
        return Err(Error::unprocessable(
            "origin_not_allowed: Origin is set by the backend and cannot be supplied by callers",
        ));
    }
    if input.visual_spec.is_some() {
        return Err(Error::unprocessable(
            "visual_spec_unsupported: inline visual_spec is obsolete; use visual jobs workflow",
        ));
    }
    let (owner_id, existing_quiz) = load_owned_study_quiz(store, caller, study_id).await?;
    let language = input.language.unwrap_or_else(|| "en".into());
    validate_question_fields(
        &language,
        &input.prompt,
        &input.alternatives,
        &input.correct_id,
        &input.explanation,
    )?;

    if let Some(ref sid) = input.source_id {
        let belongs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sources WHERE id = $1 AND study_id = $2")
                .bind(sid)
                .bind(study_id)
                .fetch_one(&store.db)
                .await?;
        if belongs == 0 {
            return Err(Error::unprocessable(
                "invalid_source: Source does not belong to this study",
            ));
        }
    }

    let citation_ref = input.citation_ref.unwrap_or_default();
    let doc_id = new_id();
    let payload = json!({
        "prompt": input.prompt.trim(),
        "alternatives": input.alternatives,
        "correct_id": input.correct_id.trim(),
        "explanation": input.explanation.trim(),
        "citation_ref": citation_ref,
        "visual_ids": [],
    });
    guardrail::store_validated_document(
        store,
        QUESTION_SCHEMA_NAME,
        QUESTION_SCHEMA_VERSION,
        &doc_id,
        &payload,
    )
    .await?;

    let quiz_id = ensure_study_quiz(store, &owner_id, study_id, existing_quiz).await?;
    let question_id = new_id();
    let qv_id = new_id();
    let ts = now();

    if let Err(err) = async {
        let mut tx = store.db.begin().await?;
        sqlx::query(
            "INSERT INTO questions(id, owner_id, origin, created_at) VALUES($1, $2, $3, $4)",
        )
        .bind(&question_id)
        .bind(&owner_id)
        .bind(ORIGIN_CREATOR)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
             VALUES($1, $2, 1, $3, $4, $5, $6, $7)",
        )
        .bind(&qv_id)
        .bind(&question_id)
        .bind(&language)
        .bind(QUESTION_SCHEMA_NAME)
        .bind(QUESTION_SCHEMA_VERSION)
        .bind(&doc_id)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
        if let Some(ref sid) = input.source_id {
            sqlx::query(
                "INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at)
                 VALUES($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(&qv_id)
            .bind(sid)
            .bind(&citation_ref)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok::<_, Error>(())
    }
    .await
    {
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(err);
    }

    let (expected_active_version_id, mut existing_items) =
        current_quiz_version_questions(store, &quiz_id).await?;
    existing_items.push(QuizQuestionMembership {
        question_id: question_id.clone(),
        question_version_id: qv_id.clone(),
        source_scope: "unclassified".into(),
        origin_quiz_id: None,
        origin_quiz_version_id: None,
    });
    write_new_quiz_version(
        store,
        &quiz_id,
        &existing_items,
        expected_active_version_id.as_deref(),
    )
    .await?;

    load_question_detail(store, &question_id, &qv_id).await
}

pub async fn update_question(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    input: UpdateQuestionInput,
) -> Result<AuthorQuestion> {
    if input.origin.is_some() {
        return Err(Error::unprocessable(
            "origin_not_allowed: Origin is set by the backend and cannot be supplied by callers",
        ));
    }
    let question_schema =
        schemas::get_published(&store.db, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION).await?;
    let mut tx = store.db.begin().await?;
    let study_query = if store.postgres() {
        "SELECT owner_id, quiz_id FROM studies WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT owner_id, quiz_id FROM studies WHERE id = $1"
    };
    let study = sqlx::query(study_query)
        .bind(study_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = study.get("owner_id");
    require_owner(caller, &owner_id)?;
    let quiz_id: String = study
        .try_get::<String, _>("quiz_id")
        .ok()
        .filter(|id| !id.is_empty())
        .ok_or_else(Error::missing)?;
    let quiz_query = if store.postgres() {
        "SELECT owner_id FROM quizzes WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT owner_id FROM quizzes WHERE id = $1"
    };
    let quiz = sqlx::query(quiz_query)
        .bind(&quiz_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    require_owner(caller, &quiz.get::<String, _>("owner_id"))?;
    let (_, items) = current_quiz_version_questions_in_tx(&mut tx, &quiz_id).await?;
    let current_membership = items
        .iter()
        .find(|item| item.question_id == question_id)
        .cloned()
        .ok_or_else(Error::missing)?;
    if current_membership.source_scope == REUSED_PUBLIC_VERSION {
        return Err(Error::conflict(
            "reused_question_immutable: Publicly reused question versions cannot be edited in this quiz",
        ));
    }
    let current_qv_id = current_membership.question_version_id.clone();
    let current_version = sqlx::query(
        "SELECT qv.language, qv.couch_doc_id, qv.schema_name, qv.schema_version FROM question_versions qv WHERE qv.id = $1 AND qv.question_id = $2",
    )
    .bind(&current_qv_id)
    .bind(question_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Error::missing)?;
    let current_doc_id: String = current_version.get("couch_doc_id");
    let current_doc = store.docs.get(&current_doc_id).await?;
    let current_visual_ids = supported_question_visual_ids(
        &current_version.get::<String, _>("schema_name"),
        current_version.get("schema_version"),
        &current_doc,
    )?;
    let current_visuals = load_visuals_in_tx(&mut tx, &current_visual_ids, question_id).await?;
    let current_language: String = current_version.get("language");
    let current_alts: Vec<QuestionAlternative> =
        serde_json::from_value(current_doc["alternatives"].clone()).unwrap_or_default();
    let language = input.language.unwrap_or(current_language);
    let prompt = input
        .prompt
        .unwrap_or_else(|| current_doc["prompt"].as_str().unwrap_or("").to_string());
    let alternatives = input.alternatives.unwrap_or(current_alts);
    let correct_id = input
        .correct_id
        .unwrap_or_else(|| current_doc["correct_id"].as_str().unwrap_or("").to_string());
    let explanation = input.explanation.unwrap_or_else(|| {
        current_doc["explanation"]
            .as_str()
            .unwrap_or("")
            .to_string()
    });
    let citation_ref = input.citation_ref.unwrap_or_else(|| {
        current_doc["citation_ref"]
            .as_str()
            .unwrap_or("")
            .to_string()
    });
    validate_question_fields(&language, &prompt, &alternatives, &correct_id, &explanation)?;

    let doc_id = new_id();
    let payload = json!({
        "prompt": prompt.trim(),
        "alternatives": alternatives,
        "correct_id": correct_id.trim(),
        "explanation": explanation.trim(),
        "citation_ref": citation_ref,
        "visual_ids": current_visuals.iter().map(|visual| visual.id.clone()).collect::<Vec<_>>(),
    });
    guardrail::validate(&question_schema, &payload)?;
    if let Err(error) = store.docs.put_immutable(&doc_id, &payload).await {
        if tx.rollback().await.is_err() {
            return Err(Error::unavailable());
        }
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(error);
    }

    let new_qv_id = new_id();
    let ts = now();
    let publication = async {
        let next_version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version_num), 0) + 1 FROM question_versions WHERE question_id = $1",
        )
        .bind(question_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&new_qv_id)
        .bind(question_id)
        .bind(next_version)
        .bind(&language)
        .bind(QUESTION_SCHEMA_NAME)
        .bind(QUESTION_SCHEMA_VERSION)
        .bind(&doc_id)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
        let source_evidence = sqlx::query(
            "SELECT source_id, page_or_offset FROM question_citations WHERE question_version_id = $1 ORDER BY created_at, id",
        )
        .bind(&current_qv_id)
        .fetch_all(&mut *tx)
        .await?;
        for citation in source_evidence {
            sqlx::query(
                "INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at) VALUES($1,$2,$3,$4,$5)",
            )
            .bind(new_id())
            .bind(&new_qv_id)
            .bind(citation.get::<String, _>("source_id"))
            .bind(citation.get::<String, _>("page_or_offset"))
            .bind(ts)
            .execute(&mut *tx)
            .await?;
        }
        for visual in &current_visuals {
            sqlx::query("INSERT INTO question_version_visual_refs(question_version_id, visual_id) VALUES($1,$2)")
                .bind(&new_qv_id)
                .bind(&visual.id)
                .execute(&mut *tx)
                .await?;
        }
        let mut next_items = items.clone();
        for item in &mut next_items {
            if item.question_id == question_id {
                item.question_version_id = new_qv_id.clone();
            }
        }
        write_new_quiz_version_in_tx(store, &mut tx, &quiz_id, &next_items, None).await?;
        Ok::<(), Error>(())
    }
    .await;
    if let Err(error) = publication {
        if tx.rollback().await.is_err() {
            return Err(Error::unavailable());
        }
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(error);
    }
    if let Err(error) = tx.commit().await {
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(error.into());
    }
    let mut updated = load_question_detail(store, question_id, &new_qv_id).await?;
    updated.source_scope = current_membership.source_scope;
    updated.origin_quiz_id = current_membership.origin_quiz_id;
    updated.origin_quiz_version_id = current_membership.origin_quiz_version_id;
    Ok(updated)
}

pub async fn remove_question(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
) -> Result<()> {
    let (_owner_id, existing_quiz) = load_owned_study_quiz(store, caller, study_id).await?;
    let quiz_id = existing_quiz.ok_or_else(Error::missing)?;
    let (expected_active_version_id, items) =
        current_quiz_version_questions(store, &quiz_id).await?;
    if !items.iter().any(|item| item.question_id == question_id) {
        return Err(Error::missing());
    }
    let remaining: Vec<QuizQuestionMembership> = items
        .into_iter()
        .filter(|item| item.question_id != question_id)
        .collect();
    write_new_quiz_version(
        store,
        &quiz_id,
        &remaining,
        expected_active_version_id.as_deref(),
    )
    .await?;
    Ok(())
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

pub async fn import_csv(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    csv_text: &str,
    confirm: bool,
) -> Result<CsvImportResult> {
    let (_owner_id, _quiz_id) = load_owned_study_quiz(store, caller, study_id).await?;
    let mut lines = csv_text.lines().map(str::trim).filter(|l| !l.is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| Error::unprocessable("empty_csv: CSV header row is required"))?;
    let headers: Vec<String> = parse_csv_line(header_line)
        .into_iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    if headers.iter().any(|h| h == "origin") {
        return Err(Error::unprocessable(
            "origin_not_allowed: CSV cannot contain an origin column",
        ));
    }

    let mut parsed_inputs = Vec::new();
    let mut valid_rows = Vec::new();
    let mut invalid_rows = Vec::new();

    for (idx, line) in lines.enumerate() {
        let row_number = idx + 2;
        let cols = parse_csv_line(line);
        if cols.len() < 8 {
            invalid_rows.push(CsvInvalidRow {
                row_number,
                error: "Expected at least 8 columns: language,prompt,option_a,option_b,option_c,option_d,correct_option,explanation[,citation_ref]".into(),
            });
            continue;
        }
        let language = cols[0].clone();
        let prompt = cols[1].clone();
        let mut alternatives = Vec::new();
        for (id, text) in [
            ("a", &cols[2]),
            ("b", &cols[3]),
            ("c", &cols[4]),
            ("d", &cols[5]),
        ] {
            if !text.is_empty() {
                alternatives.push(QuestionAlternative {
                    id: id.to_string(),
                    text: text.clone(),
                });
            }
        }
        let correct_id = cols[6].to_ascii_lowercase();
        let explanation = cols[7].clone();
        let citation_ref = cols.get(8).cloned().unwrap_or_default();

        match validate_question_fields(&language, &prompt, &alternatives, &correct_id, &explanation)
        {
            Ok(()) => {
                valid_rows.push(CsvValidRow {
                    row_number,
                    language: language.clone(),
                    prompt: prompt.clone(),
                    correct_id: correct_id.clone(),
                });
                parsed_inputs.push(CreateQuestionInput {
                    language: Some(language),
                    prompt,
                    alternatives,
                    correct_id,
                    explanation,
                    source_id: None,
                    citation_ref: Some(citation_ref),
                    visual_spec: None,
                    origin: None,
                });
            }
            Err(err) => {
                invalid_rows.push(CsvInvalidRow {
                    row_number,
                    error: format!("{err:?}"),
                });
            }
        }
    }

    if !confirm {
        return Ok(CsvImportResult {
            preview_only: true,
            imported_count: 0,
            valid_rows,
            invalid_rows,
            questions: Vec::new(),
        });
    }

    if !invalid_rows.is_empty() {
        return Err(Error::unprocessable(
            "csv_invalid_rows: Cannot confirm CSV import while invalid rows remain",
        ));
    }

    let mut questions = Vec::with_capacity(parsed_inputs.len());
    for item in parsed_inputs {
        questions.push(create_question(store, caller, study_id, item).await?);
    }

    Ok(CsvImportResult {
        preview_only: false,
        imported_count: questions.len(),
        valid_rows,
        invalid_rows,
        questions,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct PrepareVisualJobInput {
    pub template_id: String,
    pub template_version: i64,
    pub mapping: std::collections::BTreeMap<String, Vec<String>>,
    pub config: Value,
    pub rows: Vec<Value>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub scale: Option<i64>,
    pub ttl_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreparedVisualJobResult {
    pub question_id: String,
    pub question_version_id: String,
    pub render_job: crate::dal::reports::RenderJobDescriptor,
}

pub async fn prepare_question_visual_job(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    input: PrepareVisualJobInput,
) -> Result<PreparedVisualJobResult> {
    let lifecycle =
        sqlx::query("SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1")
            .bind(study_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
    let owner_id: String = lifecycle.get("owner_id");
    require_owner(caller, &owner_id)?;
    let status: String = lifecycle.get("status");
    let expires_at: Option<i64> = lifecycle.get("expires_at");
    if status == "draft" && expires_at.is_some_and(|expires| now() > expires) {
        sqlx::query("UPDATE studies SET status = 'expired' WHERE id = $1 AND status = 'draft'")
            .bind(study_id)
            .execute(&store.db)
            .await?;
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if status == "expired" {
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    let quiz_id = match lifecycle
        .try_get::<String, _>("quiz_id")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(qid) => qid,
        None => return Err(Error::missing()),
    };
    let quiz_visibility: String =
        sqlx::query_scalar("SELECT visibility FROM quizzes WHERE id = $1")
            .bind(&quiz_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
    if quiz_visibility == "public" {
        return Err(Error::conflict(
            "quiz_already_published: Cannot mutate visuals on a published quiz",
        ));
    }
    let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
    let item = items
        .iter()
        .find(|item| item.question_id == question_id)
        .ok_or_else(Error::missing)?;
    if item.source_scope == REUSED_PUBLIC_VERSION {
        return Err(Error::conflict(
            "reused_question_immutable: Visuals cannot be added to a publicly reused question version",
        ));
    }
    let qv_id = item.question_version_id.clone();

    let width = input.width.unwrap_or(800);
    let height = input.height.unwrap_or(480);
    let scale = input.scale.unwrap_or(2);
    let ttl_seconds = input.ttl_seconds.unwrap_or(900);

    if !(200..=2400).contains(&width) || !(200..=2400).contains(&height) {
        return Err(Error::unprocessable(
            "invalid_dimensions: width and height must be between 200 and 2400",
        ));
    }
    if !(1..=3).contains(&scale) {
        return Err(Error::unprocessable(
            "invalid_scale: scale must be between 1 and 3",
        ));
    }
    if width * scale > 4096 || height * scale > 4096 {
        return Err(Error::unprocessable(
            "invalid_dimensions: width*scale and height*scale must not exceed 4096",
        ));
    }
    if !(60..=3600).contains(&ttl_seconds) {
        return Err(Error::unprocessable(
            "invalid_ttl: ttl_seconds must be between 60 and 3600",
        ));
    }

    let job_input = crate::dal::reports::CreateRenderJobInput {
        template_id: input.template_id,
        template_version: input.template_version,
        mapping: input.mapping,
        config: input.config,
        rows: input.rows,
        width,
        height,
        scale,
        ttl_seconds: Some(ttl_seconds),
        originating_scope: Some(format!("certifications:question:{}", qv_id)),
    };

    let render_job = store
        .reports
        .create_render_job(&caller.user_id, &job_input)
        .await?;

    let now_ts = now();
    sqlx::query(
        "INSERT INTO question_visual_jobs(id, render_job_id, owner_id, study_id, question_id, question_version_id, width, height, scale, expires_at, cleanup_status, cleanup_error, created_at)
         VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending', '', $11)",
    )
    .bind(new_id())
    .bind(&render_job.job_id)
    .bind(&caller.user_id)
    .bind(study_id)
    .bind(question_id)
    .bind(&qv_id)
    .bind(width)
    .bind(height)
    .bind(scale)
    .bind(render_job.expires_at)
    .bind(now_ts)
    .execute(&store.db)
    .await?;

    Ok(PreparedVisualJobResult {
        question_id: question_id.to_string(),
        question_version_id: qv_id,
        render_job,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct UploadVisualInput {
    pub question_version_id: String,
    pub render_job_id: String,
    pub expected_width: Option<i64>,
    pub expected_height: Option<i64>,
    pub expected_scale: Option<i64>,
    pub png_base64: String,
}

pub async fn upload_question_visual(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    input: UploadVisualInput,
) -> Result<UploadVisualReceipt> {
    let render_job_id = input.render_job_id.clone();
    crate::domain::cleanup::with_question_visual_operation_lock(store, &render_job_id, || async {
        upload_question_visual_locked(store, caller, study_id, question_id, input).await
    })
    .await
}

async fn upload_question_visual_locked(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    input: UploadVisualInput,
) -> Result<UploadVisualReceipt> {
    if input.render_job_id.trim().is_empty() {
        return Err(Error::unprocessable(
            "render_job_id_required: render_job_id is required",
        ));
    }

    if input.png_base64.len() > 6 * 1024 * 1024 {
        return Err(Error::unprocessable(
            "payload_too_large: PNG base64 exceeds maximum allowed size",
        ));
    }

    fn from_base64(input: &str) -> Option<Vec<u8>> {
        let s = input.trim();
        if s.is_empty() {
            return Some(Vec::new());
        }
        if !s.len().is_multiple_of(4) {
            return None;
        }
        let decode_char = |b: u8| -> Option<u8> {
            match b {
                b'A'..=b'Z' => Some(b - b'A'),
                b'a'..=b'z' => Some(b - b'a' + 26),
                b'0'..=b'9' => Some(b - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        };

        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity((s.len() / 4) * 3);
        for chunk in bytes.chunks_exact(4) {
            let (c0, c1, c2, c3) = (chunk[0], chunk[1], chunk[2], chunk[3]);
            let d0 = decode_char(c0)?;
            let d1 = decode_char(c1)?;
            if c2 == b'=' {
                if c3 != b'=' {
                    return None;
                }
                out.push((d0 << 2) | (d1 >> 4));
            } else if c3 == b'=' {
                let d2 = decode_char(c2)?;
                out.push((d0 << 2) | (d1 >> 4));
                out.push(((d1 & 0x0f) << 4) | (d2 >> 2));
            } else {
                let d2 = decode_char(c2)?;
                let d3 = decode_char(c3)?;
                out.push((d0 << 2) | (d1 >> 4));
                out.push(((d1 & 0x0f) << 4) | (d2 >> 2));
                out.push(((d2 & 0x03) << 6) | d3);
            }
        }
        Some(out)
    }

    let raw_bytes = from_base64(&input.png_base64)
        .ok_or_else(|| Error::unprocessable("invalid_png: base64 decode failed"))?;

    if raw_bytes.len() < 24 {
        return Err(Error::unprocessable(
            "invalid_png: File too small to be a valid PNG",
        ));
    }
    if raw_bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::unprocessable(
            "invalid_png: PNG raw size exceeds 4MiB limit",
        ));
    }

    if &raw_bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(Error::unprocessable(
            "invalid_png: Missing valid PNG signature",
        ));
    }
    if &raw_bytes[12..16] != b"IHDR" {
        return Err(Error::unprocessable(
            "invalid_png: Missing valid PNG IHDR chunk",
        ));
    }

    let header_w = u32::from_be_bytes([raw_bytes[16], raw_bytes[17], raw_bytes[18], raw_bytes[19]]);
    let header_h = u32::from_be_bytes([raw_bytes[20], raw_bytes[21], raw_bytes[22], raw_bytes[23]]);

    if (header_w as u64).saturating_mul(header_h as u64) > 4096 * 4096 {
        return Err(Error::unprocessable(
            "invalid_dimensions: Total decoded pixels exceed 4096*4096",
        ));
    }

    let img = image::load_from_memory_with_format(&raw_bytes, image::ImageFormat::Png)
        .map_err(|_| Error::unprocessable("invalid_png: Not a valid decodable PNG image"))?;
    let (decoded_w, decoded_h) = image::GenericImageView::dimensions(&img);
    if decoded_w != header_w || decoded_h != header_h {
        return Err(Error::unprocessable(
            "invalid_png: Decoded dimensions mismatch",
        ));
    }

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&raw_bytes);
    let payload_sha256 = format!("{:x}", hasher.finalize());

    let visual_id = format!("vis_{}", input.render_job_id);
    if visual_id.len() > 128 {
        return Err(Error::unprocessable(
            "invalid_visual_ids: render_job_id is too long for a canonical visual ID",
        ));
    }

    // 1. Early check for durable identical replay: does not require active temporary job nor another FSM upload.
    let existing_visual = sqlx::query(
        "SELECT id, question_version_id, fsm_ref, mime_type, payload_sha256, created_at, owner_id, study_id, quiz_id, question_id, prepared_question_version_id, result_question_version_id, result_quiz_version_id FROM question_visuals WHERE id = $1",
    )
    .bind(&visual_id)
    .fetch_optional(&store.db)
    .await?;

    if let Some(existing_visual) = existing_visual {
        let ex_id: String = existing_visual.get("id");
        let ex_fsm: String = existing_visual.get("fsm_ref");
        let ex_mime: String = existing_visual.get("mime_type");
        let ex_hash: String = existing_visual.get("payload_sha256");
        let ex_ts: i64 = existing_visual.get("created_at");
        let prepared_qv: String = existing_visual.get("prepared_question_version_id");
        let result_qv: String = existing_visual.get("result_question_version_id");
        let result_quiz_version: String = existing_visual.get("result_quiz_version_id");
        let study_row = sqlx::query("SELECT owner_id, quiz_id, status FROM studies WHERE id = $1")
            .bind(study_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
        let study_owner: String = study_row.get("owner_id");
        require_owner(caller, &study_owner)?;
        let quiz_id: String = match study_row
            .try_get::<String, _>("quiz_id")
            .ok()
            .filter(|s| !s.is_empty())
        {
            Some(qid) => qid,
            None => return Err(Error::missing()),
        };
        if existing_visual.get::<String, _>("quiz_id") != quiz_id {
            return Err(Error::conflict(
                "visual_conflict: render_job_id belongs to a different quiz",
            ));
        }

        let quiz_row = sqlx::query(
            "SELECT owner_id, visibility, active_version_id FROM quizzes WHERE id = $1",
        )
        .bind(&quiz_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
        let quiz_owner: String = quiz_row.get("owner_id");
        require_owner(caller, &quiz_owner)?;

        let q_owner: String = sqlx::query_scalar("SELECT owner_id FROM questions WHERE id = $1")
            .bind(question_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
        require_owner(caller, &q_owner)?;

        if existing_visual.get::<String, _>("owner_id") != caller.user_id
            || existing_visual.get::<String, _>("study_id") != study_id
            || existing_visual.get::<String, _>("question_id") != question_id
            || prepared_qv != input.question_version_id
        {
            return Err(Error::conflict(
                "visual_conflict: render_job_id belongs to a different request context",
            ));
        }

        let qv_qid: String =
            sqlx::query_scalar("SELECT question_id FROM question_versions WHERE id = $1")
                .bind(&result_qv)
                .fetch_optional(&store.db)
                .await?
                .ok_or_else(Error::missing)?;
        if qv_qid != question_id {
            return Err(Error::conflict(
                "question_mismatch: Visual question version belongs to a different question",
            ));
        }

        let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
        let item = items
            .iter()
            .find(|item| item.question_id == question_id)
            .ok_or_else(Error::missing)?;
        if item.source_scope == REUSED_PUBLIC_VERSION {
            return Err(Error::conflict(
                "reused_question_immutable: Visuals cannot be added to a publicly reused question version",
            ));
        }
        let current_version = sqlx::query(
            "SELECT couch_doc_id, schema_name, schema_version FROM question_versions WHERE id = $1",
        )
        .bind(&item.question_version_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
        let current_doc_id: String = current_version.get("couch_doc_id");
        let current_doc = store.docs.get(&current_doc_id).await?;
        if !supported_question_visual_ids(
            &current_version.get::<String, _>("schema_name"),
            current_version.get("schema_version"),
            &current_doc,
        )?
        .iter()
        .any(|id| id == &ex_id)
        {
            return Err(Error::conflict(
                "conflict: committed visual is no longer present in this question version",
            ));
        }
        if ex_hash != payload_sha256 {
            return Err(Error::conflict(
                "conflict: conflicting PNG payload for same visual render job",
            ));
        }
        let cleanup_res = store
            .reports
            .delete_render_job(&caller.user_id, &input.render_job_id, true)
            .await;
        if cleanup_res.is_ok() {
            let _ = sqlx::query(
                "UPDATE question_visual_jobs SET cleanup_status = 'cleaned', cleanup_error = '' WHERE render_job_id = $1",
            )
            .bind(&input.render_job_id)
            .execute(&store.db)
            .await;
        }
        let _ = (ex_fsm, ex_ts);
        return Ok(UploadVisualReceipt {
            id: ex_id,
            mime_type: ex_mime,
            created_at: ex_ts,
            prepared_question_version_id: prepared_qv,
            result_question_version_id: result_qv,
            result_quiz_version_id: result_quiz_version,
        });
    }

    // 2. Validate study status and expiration before job inspection
    let study_pre =
        sqlx::query("SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1")
            .bind(study_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
    let pre_owner: String = study_pre.get("owner_id");
    require_owner(caller, &pre_owner)?;
    let pre_status: String = study_pre.get("status");
    let pre_expires: Option<i64> = study_pre.get("expires_at");
    if pre_status == "draft" && pre_expires.is_some_and(|expires| now() > expires) {
        sqlx::query("UPDATE studies SET status = 'expired' WHERE id = $1 AND status = 'draft'")
            .bind(study_id)
            .execute(&store.db)
            .await?;
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if pre_status == "expired" {
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }

    // 3. Validate prepared render job descriptor
    let job_row = sqlx::query(
        "SELECT owner_id, study_id, question_id, question_version_id, width, height, scale, expires_at
         FROM question_visual_jobs WHERE render_job_id = $1",
    )
    .bind(&input.render_job_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(|| Error::unprocessable("render_job_not_found: Prepared visual job not found"))?;

    let job_owner_id: String = job_row.get("owner_id");
    let job_study_id: String = job_row.get("study_id");
    let job_question_id: String = job_row.get("question_id");
    let job_qv_id: String = job_row.get("question_version_id");
    let job_width: i64 = job_row.get("width");
    let job_height: i64 = job_row.get("height");
    let job_scale: i64 = job_row.get("scale");
    let job_expires_at: i64 = job_row.get("expires_at");

    if job_owner_id != caller.user_id {
        return Err(Error::forbidden(
            "owner_mismatch: Caller is not the job owner",
        ));
    }
    if job_study_id != study_id {
        return Err(Error::conflict(
            "study_mismatch: Job belongs to a different study",
        ));
    }
    if job_question_id != question_id {
        return Err(Error::conflict(
            "question_mismatch: Job belongs to a different question",
        ));
    }
    if job_qv_id != input.question_version_id {
        return Err(Error::conflict(
            "question_version_mismatch: Job belongs to a different question version",
        ));
    }
    if now() > job_expires_at {
        return Err(Error::unprocessable(
            "render_job_expired: Render job has expired",
        ));
    }

    let expected_w = (job_width
        .checked_mul(job_scale)
        .ok_or_else(|| Error::unprocessable("invalid_dimensions: dimension overflow"))?)
        as u32;
    let expected_h = (job_height
        .checked_mul(job_scale)
        .ok_or_else(|| Error::unprocessable("invalid_dimensions: dimension overflow"))?)
        as u32;

    if header_w != expected_w || header_h != expected_h {
        return Err(Error::unprocessable(format!(
            "invalid_dimensions: Decoded PNG dimensions ({}x{}) do not match expected descriptor bounds ({}x{})",
            header_w, header_h, expected_w, expected_h
        )));
    }

    if input.expected_width.is_some_and(|ew| ew != job_width) {
        return Err(Error::unprocessable(
            "expected_width_mismatch: Client expected_width does not match job descriptor",
        ));
    }
    if input.expected_height.is_some_and(|eh| eh != job_height) {
        return Err(Error::unprocessable(
            "expected_height_mismatch: Client expected_height does not match job descriptor",
        ));
    }
    if input.expected_scale.is_some_and(|es| es != job_scale) {
        return Err(Error::unprocessable(
            "expected_scale_mismatch: Client expected_scale does not match job descriptor",
        ));
    }

    let prepared_quiz_id = study_pre
        .try_get::<String, _>("quiz_id")
        .ok()
        .filter(|id| !id.is_empty())
        .ok_or_else(Error::missing)?;
    let operation_fingerprint = {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}x{}x{}",
            caller.user_id,
            study_id,
            prepared_quiz_id,
            question_id,
            job_qv_id,
            payload_sha256,
            job_width,
            job_height,
            job_scale
        ));
        format!("{:x}", hash.finalize())
    };
    let (reserved_qv_id, reserved_quiz_version_id, reserved_doc_id, reserved_fsm_key) = {
        let mut reserve_tx = store.db.begin().await?;
        let reserve_query = if store.postgres() {
            "SELECT operation_fingerprint, result_question_version_id, result_quiz_version_id, result_document_id, result_fsm_ref_key FROM question_visual_jobs WHERE render_job_id = $1 FOR UPDATE"
        } else {
            "SELECT operation_fingerprint, result_question_version_id, result_quiz_version_id, result_document_id, result_fsm_ref_key FROM question_visual_jobs WHERE render_job_id = $1"
        };
        let existing = sqlx::query(reserve_query)
            .bind(&input.render_job_id)
            .fetch_optional(&mut *reserve_tx)
            .await?
            .ok_or_else(Error::missing)?;
        let old_fingerprint: String = existing.get("operation_fingerprint");
        let values = if old_fingerprint.is_empty() {
            let qv_id = new_id();
            let quiz_version_id = new_id();
            let document_id = new_id();
            let fsm_key = format!("visuals/{qv_id}/{}.png", new_id());
            sqlx::query(
                "UPDATE question_visual_jobs SET operation_fingerprint=$1, result_question_version_id=$2, result_quiz_version_id=$3, result_document_id=$4, result_fsm_ref_key=$5 WHERE render_job_id=$6",
            )
            .bind(&operation_fingerprint)
            .bind(&qv_id)
            .bind(&quiz_version_id)
            .bind(&document_id)
            .bind(&fsm_key)
            .bind(&input.render_job_id)
            .execute(&mut *reserve_tx)
            .await?;
            (qv_id, quiz_version_id, document_id, fsm_key)
        } else {
            if old_fingerprint != operation_fingerprint {
                return Err(Error::conflict(
                    "visual_conflict: render_job_id has a different reserved upload",
                ));
            }
            let reservation: (String, String, String, String) = (
                existing.get("result_question_version_id"),
                existing.get("result_quiz_version_id"),
                existing.get("result_document_id"),
                existing.get("result_fsm_ref_key"),
            );
            if reservation.0.is_empty()
                || reservation.1.is_empty()
                || reservation.2.is_empty()
                || reservation.3.is_empty()
            {
                return Err(Error::unavailable());
            }
            reservation
        };
        reserve_tx.commit().await?;
        values
    };
    let question_schema =
        schemas::get_published(&store.db, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION).await?;

    // 3. Row-locking transaction on study and quiz BEFORE FSM upload
    let mut tx = store.db.begin().await?;

    let lifecycle_query = if store.postgres() {
        "SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT owner_id, quiz_id, status, expires_at FROM studies WHERE id = $1"
    };
    let lifecycle = sqlx::query(lifecycle_query)
        .bind(study_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = lifecycle.get("owner_id");
    require_owner(caller, &owner_id)?;
    let study_status: String = lifecycle.get("status");
    let expires_at: Option<i64> = lifecycle.get("expires_at");
    if study_status == "draft" && expires_at.is_some_and(|expires| now() > expires) {
        sqlx::query("UPDATE studies SET status = 'expired' WHERE id = $1 AND status = 'draft'")
            .bind(study_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }
    if study_status == "expired" {
        return Err(Error(
            axum::http::StatusCode::GONE,
            "study_expired: Abandoned pre-generation study draft expired".into(),
        ));
    }

    let quiz_id = match lifecycle
        .try_get::<String, _>("quiz_id")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(qid) => qid,
        None => return Err(Error::missing()),
    };
    if quiz_id != prepared_quiz_id {
        return Err(Error::conflict(
            "study_mismatch: Study quiz changed during visual upload",
        ));
    }

    let quiz_query = if store.postgres() {
        "SELECT owner_id, visibility, active_version_id FROM quizzes WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT owner_id, visibility, active_version_id FROM quizzes WHERE id = $1"
    };
    let quiz_row = sqlx::query(quiz_query)
        .bind(&quiz_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Error::missing)?;
    let quiz_visibility: String = quiz_row.get("visibility");
    if quiz_visibility == "public" {
        return Err(Error::conflict(
            "quiz_already_published: Cannot mutate visuals on a published quiz",
        ));
    }

    let (_, items) = current_quiz_version_questions_in_tx(&mut tx, &quiz_id).await?;
    let item = match items.iter().find(|item| item.question_id == question_id) {
        Some(it) => it,
        None => return Err(Error::missing()),
    };
    if item.source_scope == REUSED_PUBLIC_VERSION {
        return Err(Error::conflict(
            "reused_question_immutable: Visuals cannot be added to a publicly reused question version",
        ));
    }
    let source_version = sqlx::query(
        "SELECT version_num, language, couch_doc_id, schema_name, schema_version FROM question_versions WHERE id = $1 AND question_id = $2",
    )
    .bind(&job_qv_id)
    .bind(question_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Error::missing)?;
    let source_doc_id: String = source_version.get("couch_doc_id");
    let source_doc = store.docs.get(&source_doc_id).await?;
    let mut visual_ids = supported_question_visual_ids(
        &source_version.get::<String, _>("schema_name"),
        source_version.get("schema_version"),
        &source_doc,
    )?;
    if visual_ids.len() >= 64 {
        return Err(Error::unprocessable(
            "visual_limit_reached: A question can reference at most 64 visuals",
        ));
    }
    if visual_ids.iter().any(|id| id == &visual_id) {
        return Err(Error::conflict(
            "visual_conflict: Visual is already present",
        ));
    }
    for existing_id in &visual_ids {
        let valid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM question_visuals v JOIN question_versions qv ON qv.id = v.result_question_version_id WHERE v.id = $1 AND v.question_id = $2 AND qv.question_id = $2",
        )
        .bind(existing_id)
        .bind(question_id)
        .fetch_one(&mut *tx)
        .await?;
        if valid == 0 {
            return Err(Error::unprocessable(
                "invalid_visual_ids: A carried visual is not owned by this question",
            ));
        }
    }
    visual_ids.push(visual_id.clone());
    let doc_id = reserved_doc_id.clone();
    let next_qv_id = reserved_qv_id.clone();
    let language: String = source_version.get("language");
    let payload = json!({
        "prompt": source_doc["prompt"],
        "alternatives": source_doc["alternatives"],
        "correct_id": source_doc["correct_id"],
        "explanation": source_doc["explanation"],
        "citation_ref": source_doc["citation_ref"],
        "visual_ids": visual_ids,
    });
    // In-transaction check for concurrent insert race
    let in_tx_existing = sqlx::query(
        "SELECT id, question_version_id, fsm_ref, mime_type, payload_sha256, created_at, prepared_question_version_id, result_question_version_id, result_quiz_version_id, owner_id, study_id, question_id FROM question_visuals WHERE id = $1",
    )
    .bind(&visual_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(existing) = in_tx_existing {
        if existing.get::<String, _>("prepared_question_version_id") != job_qv_id
            || existing.get::<String, _>("owner_id") != owner_id
            || existing.get::<String, _>("study_id") != study_id
            || existing.get::<String, _>("question_id") != question_id
        {
            return Err(Error::conflict(
                "visual_conflict: render_job_id belongs to a different request context",
            ));
        }
        if existing.get::<String, _>("payload_sha256") != payload_sha256 {
            return Err(Error::conflict(
                "conflict: conflicting PNG payload for same visual render job",
            ));
        }
        tx.commit().await?;
        let cleanup_res = store
            .reports
            .delete_render_job(&caller.user_id, &input.render_job_id, true)
            .await;
        if cleanup_res.is_ok() {
            let _ = sqlx::query(
                "UPDATE question_visual_jobs SET cleanup_status = 'cleaned', cleanup_error = '' WHERE render_job_id = $1",
            )
            .bind(&input.render_job_id)
            .execute(&store.db)
            .await;
        }
        return Ok(UploadVisualReceipt {
            id: existing.get("id"),
            mime_type: existing.get("mime_type"),
            created_at: existing.get("created_at"),
            prepared_question_version_id: existing.get("prepared_question_version_id"),
            result_question_version_id: existing.get("result_question_version_id"),
            result_quiz_version_id: existing.get("result_quiz_version_id"),
        });
    }
    if item.question_version_id != job_qv_id {
        return Err(Error::conflict(
            "conflict: question_version_id does not match current question version",
        ));
    }

    guardrail::validate(&question_schema, &payload)?;
    if let Err(error) = store.docs.put_immutable(&doc_id, &payload).await {
        if tx.rollback().await.is_err() {
            return Err(Error::unavailable());
        }
        // A conflicting immutable put may be the same stable reserved result
        // from an earlier attempt. Preserve it until the retry resolves it.
        return Err(error);
    }

    // 4. Upload the new image before publishing its immutable question version.
    let ref_key = reserved_fsm_key.clone();
    let fsm_ref = match store.fsm.put_bytes(&ref_key, &raw_bytes).await {
        Ok(reference) => reference,
        Err(error) => {
            if tx.rollback().await.is_err() {
                return Err(Error::unavailable());
            }
            if guardrail::cleanup_orphan_document_with_visual_operation_lock_held(store, &doc_id)
                .await
                .is_err()
                || crate::domain::cleanup::delete_fsm_or_queue_with_visual_operation_lock_held(
                    store,
                    &reserved_fsm_key,
                )
                .await
                .is_err()
            {
                return Err(Error::unavailable());
            }
            return Err(error);
        }
    };
    let ts = now();
    let publish = async {
        let next_ver: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version_num), 0) + 1 FROM question_versions WHERE question_id = $1",
        )
        .bind(question_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&next_qv_id).bind(question_id).bind(next_ver).bind(&language)
        .bind(QUESTION_SCHEMA_NAME).bind(QUESTION_SCHEMA_VERSION).bind(&doc_id).bind(ts)
        .execute(&mut *tx).await?;
        let citations = sqlx::query("SELECT source_id, page_or_offset FROM question_citations WHERE question_version_id = $1 ORDER BY created_at, id")
            .bind(&job_qv_id).fetch_all(&mut *tx).await?;
        for citation in citations {
            sqlx::query("INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at) VALUES($1,$2,$3,$4,$5)")
                .bind(new_id()).bind(&next_qv_id).bind(citation.get::<String,_>("source_id"))
                .bind(citation.get::<String,_>("page_or_offset")).bind(ts).execute(&mut *tx).await?;
        }
        let mut next_items = items.clone();
        for next_item in &mut next_items {
            if next_item.question_id == question_id { next_item.question_version_id = next_qv_id.clone(); }
        }
        let next_quiz_version_id = write_new_quiz_version_in_tx(
            store,
            &mut tx,
            &quiz_id,
            &next_items,
            Some(&reserved_quiz_version_id),
        )
        .await?;
        sqlx::query(
            "INSERT INTO question_visuals(id, question_version_id, fsm_ref, mime_type, payload_sha256, owner_id, study_id, quiz_id, question_id, prepared_question_version_id, result_question_version_id, result_quiz_version_id, created_at) VALUES($1,$2,$3,'image/png',$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(&visual_id).bind(&next_qv_id).bind(&fsm_ref).bind(&payload_sha256)
        .bind(&owner_id).bind(study_id).bind(&quiz_id).bind(question_id).bind(&job_qv_id)
        .bind(&next_qv_id).bind(&next_quiz_version_id).bind(ts).execute(&mut *tx).await?;
        for id in &visual_ids {
            sqlx::query("INSERT INTO question_version_visual_refs(question_version_id, visual_id) VALUES($1,$2)")
                .bind(&next_qv_id).bind(id).execute(&mut *tx).await?;
        }
        Ok::<String, Error>(next_quiz_version_id)
    }.await;
    let result_quiz_version_id = match publish {
        Ok(id) => id,
        Err(err) => {
            if tx.rollback().await.is_err() {
                return Err(Error::unavailable());
            }
            let cleanup_doc =
                guardrail::cleanup_orphan_document_with_visual_operation_lock_held(store, &doc_id)
                    .await;
            let cleanup_fsm =
                crate::domain::cleanup::delete_fsm_or_queue_with_visual_operation_lock_held(
                    store, &fsm_ref,
                )
                .await;
            if cleanup_doc.is_err() || cleanup_fsm.is_err() {
                return Err(Error::unavailable());
            }
            tracing::error!(module = "authoring", question_id, error = ?err, "Question visual publication failed");
            return Err(Error::bad(
                "visual_insert_failed: Could not publish question visual version",
            ));
        }
    };
    let mut committed_result_qv_id = next_qv_id.clone();
    let mut committed_result_quiz_version_id = result_quiz_version_id.clone();
    let mut committed_created_at = ts;
    if let Err(commit_err) = tx.commit().await {
        let committed = sqlx::query(
            "SELECT id, mime_type, created_at, fsm_ref, payload_sha256, owner_id, study_id, quiz_id, question_id, prepared_question_version_id, result_question_version_id, result_quiz_version_id FROM question_visuals WHERE id = $1",
        )
        .bind(&visual_id)
        .fetch_one(&store.db)
        .await;
        match committed {
            Ok(receipt)
                if receipt.get::<String, _>("payload_sha256") == payload_sha256
                    && receipt.get::<String, _>("owner_id") == owner_id
                    && receipt.get::<String, _>("study_id") == study_id
                    && receipt.get::<String, _>("quiz_id") == quiz_id
                    && receipt.get::<String, _>("question_id") == question_id
                    && receipt.get::<String, _>("prepared_question_version_id") == job_qv_id
                    && receipt.get::<String, _>("mime_type") == "image/png"
                    && receipt.get::<String, _>("fsm_ref") == reserved_fsm_key =>
            {
                committed_result_qv_id = receipt.get("result_question_version_id");
                committed_result_quiz_version_id = receipt.get("result_quiz_version_id");
                committed_created_at = receipt.get("created_at");
                if committed_result_qv_id.is_empty() || committed_result_quiz_version_id.is_empty()
                {
                    return Err(Error::unavailable());
                }
            }
            Ok(_) => {
                return Err(Error::conflict(
                    "visual_conflict: committed receipt context differs",
                ));
            }
            Err(_) => return Err(Error::unavailable()),
        }
        let actual_doc_id: String = sqlx::query_scalar(
            "SELECT couch_doc_id FROM question_versions WHERE id = $1 AND question_id = $2",
        )
        .bind(&committed_result_qv_id)
        .bind(question_id)
        .fetch_optional(&store.db)
        .await
        .map_err(|_| Error::unavailable())?
        .ok_or_else(Error::unavailable)?;
        let actual_doc = store
            .docs
            .get(&actual_doc_id)
            .await
            .map_err(|_| Error::unavailable())?;
        if !canonical_visual_ids(&actual_doc)
            .map_err(|_| Error::unavailable())?
            .iter()
            .any(|id| id == &visual_id)
        {
            return Err(Error::unavailable());
        }
        let membership_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1 AND question_version_id = $2",
        )
        .bind(&committed_result_quiz_version_id)
        .bind(&committed_result_qv_id)
        .fetch_one(&store.db)
        .await
        .map_err(|_| Error::unavailable())?;
        if membership_count != 1 {
            return Err(Error::unavailable());
        }
        let _ = commit_err;
    }

    let cleanup_res = store
        .reports
        .delete_render_job(&caller.user_id, &input.render_job_id, true)
        .await;

    match cleanup_res {
        Ok(_) => {
            let _ = sqlx::query(
                "UPDATE question_visual_jobs SET cleanup_status = 'cleaned', cleanup_error = '' WHERE render_job_id = $1",
            )
            .bind(&input.render_job_id)
            .execute(&store.db)
            .await;
        }
        Err(e) => {
            let _ = sqlx::query(
                "UPDATE question_visual_jobs SET cleanup_status = 'failed', cleanup_error = $1 WHERE render_job_id = $2",
            )
            .bind(format!("{e:?}"))
            .bind(&input.render_job_id)
            .execute(&store.db)
            .await;
        }
    }

    Ok(UploadVisualReceipt {
        id: visual_id,
        mime_type: "image/png".into(),
        created_at: committed_created_at,
        prepared_question_version_id: job_qv_id,
        result_question_version_id: committed_result_qv_id,
        result_quiz_version_id: committed_result_quiz_version_id,
    })
}

pub async fn get_question_visual(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    visual_id: &str,
) -> Result<(Vec<u8>, String)> {
    let (_owner_id, existing_quiz) = load_owned_study_quiz(store, caller, study_id).await?;
    let quiz_id = existing_quiz.ok_or_else(Error::missing)?;
    let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
    let item = items
        .iter()
        .find(|item| item.question_id == question_id)
        .ok_or_else(Error::missing)?;
    let qv_id = &item.question_version_id;

    let version = sqlx::query(
        "SELECT couch_doc_id, schema_name, schema_version FROM question_versions WHERE id = $1",
    )
    .bind(qv_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;
    let doc_id: String = version.get("couch_doc_id");
    let doc = store.docs.get(&doc_id).await?;
    if !supported_question_visual_ids(
        &version.get::<String, _>("schema_name"),
        version.get("schema_version"),
        &doc,
    )?
    .iter()
    .any(|id| id == visual_id)
    {
        return Err(Error::missing());
    }

    let row = sqlx::query(
        "SELECT v.fsm_ref, v.mime_type FROM question_visuals v JOIN question_versions qv ON qv.id = v.result_question_version_id WHERE v.id = $1 AND v.question_id = $2 AND qv.question_id = $2 AND v.mime_type = 'image/png'",
    )
    .bind(visual_id)
    .bind(question_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;

    let fsm_ref: String = row.get("fsm_ref");
    let mime_type: String = row.get("mime_type");
    let bytes = store.fsm.get_bytes(&fsm_ref).await?;

    Ok((bytes, mime_type))
}

pub async fn check_reused_question_rejection(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
) -> Result<()> {
    let (_owner_id, existing_quiz) = load_owned_study_quiz(store, caller, study_id).await?;
    let quiz_id = existing_quiz.ok_or_else(Error::missing)?;
    let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
    let item = items
        .iter()
        .find(|item| item.question_id == question_id)
        .ok_or_else(Error::missing)?;
    if item.source_scope == REUSED_PUBLIC_VERSION {
        return Err(Error::conflict(
            "reused_question_immutable: Visuals cannot be added to a publicly reused question version",
        ));
    }
    Ok(())
}

pub async fn admin_override_quiz_origin(
    store: &Store,
    caller: &CallerIdentity,
    quiz_id: &str,
    origin: &str,
    apply_to_questions: bool,
) -> Result<()> {
    if !caller.is_admin() {
        return Err(Error::forbidden("admin_required: Admin access required"));
    }
    validate_origin_value(origin)?;
    let res = sqlx::query("UPDATE quizzes SET origin = $1, updated_at = $2 WHERE id = $3")
        .bind(origin)
        .bind(now())
        .bind(quiz_id)
        .execute(&store.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(Error::missing());
    }
    if apply_to_questions {
        sqlx::query(
            "UPDATE questions SET origin = $1
             WHERE id IN (
                 SELECT DISTINCT qv.question_id
                 FROM question_versions qv
                 JOIN quiz_version_questions qvq ON qvq.question_version_id = qv.id
                 JOIN quiz_versions qver ON qver.id = qvq.quiz_version_id
                 WHERE qver.quiz_id = $2
             )",
        )
        .bind(origin)
        .bind(quiz_id)
        .execute(&store.db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod question_visual_schema_tests {
    use super::{
        LEGACY_QUESTION_SCHEMA_VERSION, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION,
        supported_question_visual_ids,
    };
    use serde_json::json;

    #[test]
    fn schema_one_reads_as_text_only_and_schema_two_requires_bounded_ids() {
        assert_eq!(
            supported_question_visual_ids(
                QUESTION_SCHEMA_NAME,
                LEGACY_QUESTION_SCHEMA_VERSION,
                &json!({"prompt":"legacy"}),
            )
            .unwrap(),
            Vec::<String>::new()
        );
        assert!(
            supported_question_visual_ids(
                QUESTION_SCHEMA_NAME,
                QUESTION_SCHEMA_VERSION,
                &json!({"prompt":"missing list"})
            )
            .is_err()
        );
        assert!(
            supported_question_visual_ids(
                QUESTION_SCHEMA_NAME,
                QUESTION_SCHEMA_VERSION,
                &json!({"visual_ids":["a","a"]})
            )
            .is_err()
        );
        assert!(
            supported_question_visual_ids(
                QUESTION_SCHEMA_NAME,
                QUESTION_SCHEMA_VERSION,
                &json!({"visual_ids":["x".repeat(129)]})
            )
            .is_err()
        );
        let allowed = json!({"visual_ids": (0..64).map(|i| format!("v{i}")).collect::<Vec<_>>()});
        assert_eq!(
            supported_question_visual_ids(QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION, &allowed)
                .unwrap()
                .len(),
            64
        );
        assert!(
            supported_question_visual_ids(
                QUESTION_SCHEMA_NAME,
                QUESTION_SCHEMA_VERSION + 1,
                &allowed
            )
            .is_err()
        );
    }
}
