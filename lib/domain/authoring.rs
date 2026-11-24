use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::{
        auth::{CallerIdentity, require_owner},
        catalog::public_question_version_sources_in_tx,
        generations::{QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION},
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
    pub fsm_ref: String,
    pub mime_type: String,
    pub created_at: i64,
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

async fn load_visuals(store: &Store, qv_id: &str) -> Result<Vec<QuestionVisual>> {
    let rows = sqlx::query(
        "SELECT id, fsm_ref, mime_type, created_at FROM question_visuals WHERE question_version_id = $1 ORDER BY created_at",
    )
    .bind(qv_id)
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| QuestionVisual {
            id: r.get("id"),
            fsm_ref: r.get("fsm_ref"),
            mime_type: r.get("mime_type"),
            created_at: r.get("created_at"),
        })
        .collect())
}

async fn load_question_detail(
    store: &Store,
    question_id: &str,
    qv_id: &str,
) -> Result<AuthorQuestion> {
    let row = sqlx::query(
        "SELECT q.origin, qv.version_num, qv.language, qv.couch_doc_id
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
    let visuals = load_visuals(store, qv_id).await?;
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
) -> Result<String> {
    let mut tx = store.db.begin().await?;
    let qver_id = write_new_quiz_version_in_tx(&mut tx, quiz_id, items).await?;
    tx.commit().await?;
    Ok(qver_id)
}

async fn write_new_quiz_version_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    quiz_id: &str,
    items: &[QuizQuestionMembership],
) -> Result<String> {
    let next_ver: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_num), 0) + 1 FROM quiz_versions WHERE quiz_id = $1",
    )
    .bind(quiz_id)
    .fetch_one(&mut **tx)
    .await?;
    let qver_id = new_id();
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

    let (owner_id, existing_quiz_id) = load_owned_study_quiz(store, caller, study_id).await?;
    let selected_ids: Vec<String> = question_version_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut tx = store.db.begin().await?;
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
    let quiz_version_id = write_new_quiz_version_in_tx(&mut tx, &quiz_id, &memberships).await?;
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

    let (_, mut existing_items) = current_quiz_version_questions(store, &quiz_id).await?;
    existing_items.push(QuizQuestionMembership {
        question_id: question_id.clone(),
        question_version_id: qv_id.clone(),
        source_scope: "unclassified".into(),
        origin_quiz_id: None,
        origin_quiz_version_id: None,
    });
    write_new_quiz_version(store, &quiz_id, &existing_items).await?;

    if let Some(spec) = input.visual_spec {
        render_question_visual(store, caller, study_id, &question_id, spec).await?;
    }

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
    let (_owner_id, existing_quiz) = load_owned_study_quiz(store, caller, study_id).await?;
    let quiz_id = existing_quiz.ok_or_else(Error::missing)?;
    let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
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

    let current = load_question_detail(store, question_id, &current_qv_id).await?;
    let language = input.language.unwrap_or(current.language);
    let prompt = input.prompt.unwrap_or(current.prompt);
    let current_alts: Vec<QuestionAlternative> =
        serde_json::from_value(current.alternatives).unwrap_or_default();
    let alternatives = input.alternatives.unwrap_or(current_alts);
    let correct_id = input.correct_id.unwrap_or(current.correct_id);
    let explanation = input.explanation.unwrap_or(current.explanation);
    let citation_ref = input.citation_ref.unwrap_or(current.citation_ref);

    validate_question_fields(&language, &prompt, &alternatives, &correct_id, &explanation)?;

    let doc_id = new_id();
    let payload = json!({
        "prompt": prompt.trim(),
        "alternatives": alternatives,
        "correct_id": correct_id.trim(),
        "explanation": explanation.trim(),
        "citation_ref": citation_ref,
    });
    guardrail::store_validated_document(
        store,
        QUESTION_SCHEMA_NAME,
        QUESTION_SCHEMA_VERSION,
        &doc_id,
        &payload,
    )
    .await?;

    let next_ver: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_num), 0) + 1 FROM question_versions WHERE question_id = $1",
    )
    .bind(question_id)
    .fetch_one(&store.db)
    .await?;
    let new_qv_id = new_id();
    let ts = now();

    if let Err(err) = sqlx::query(
        "INSERT INTO question_versions(id, question_id, version_num, language, schema_name, schema_version, couch_doc_id, created_at)
         VALUES($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&new_qv_id)
    .bind(question_id)
    .bind(next_ver)
    .bind(&language)
    .bind(QUESTION_SCHEMA_NAME)
    .bind(QUESTION_SCHEMA_VERSION)
    .bind(&doc_id)
    .bind(ts)
    .execute(&store.db)
    .await
    {
        guardrail::cleanup_orphan_document(store, &doc_id).await;
        return Err(err.into());
    }

    // A new immutable question version carries forward its source evidence.
    // The membership provenance below remains version-bound, so losing these
    // citations would make a source-backed/generated label unverifiable.
    let source_evidence = sqlx::query(
        "SELECT source_id, page_or_offset FROM question_citations WHERE question_version_id = $1",
    )
    .bind(&current_qv_id)
    .fetch_all(&store.db)
    .await?;
    for citation in source_evidence {
        sqlx::query(
            "INSERT INTO question_citations(id, question_version_id, source_id, page_or_offset, created_at)
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(new_id())
        .bind(&new_qv_id)
        .bind(citation.get::<String, _>("source_id"))
        .bind(citation.get::<String, _>("page_or_offset"))
        .bind(ts)
        .execute(&store.db)
        .await?;
    }

    let new_items: Vec<QuizQuestionMembership> = items
        .into_iter()
        .map(|mut item| {
            if item.question_id == question_id {
                item.question_version_id = new_qv_id.clone();
            }
            item
        })
        .collect();
    write_new_quiz_version(store, &quiz_id, &new_items).await?;
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
    let (_, items) = current_quiz_version_questions(store, &quiz_id).await?;
    if !items.iter().any(|item| item.question_id == question_id) {
        return Err(Error::missing());
    }
    let remaining: Vec<QuizQuestionMembership> = items
        .into_iter()
        .filter(|item| item.question_id != question_id)
        .collect();
    write_new_quiz_version(store, &quiz_id, &remaining).await?;
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

pub async fn render_question_visual(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    question_id: &str,
    visual_spec: Value,
) -> Result<QuestionVisual> {
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
    let qv_id = item.question_version_id.clone();
    let detail = load_question_detail(store, question_id, &qv_id).await?;

    let rendered = store
        .reports
        .render_question_visual(&caller.user_id, &detail.prompt, &visual_spec)
        .await?;
    let ref_key = format!("visuals/{qv_id}/{}.png", new_id());
    let fsm_ref = store.fsm.put_bytes(&ref_key, &rendered.bytes).await?;
    let id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO question_visuals(id, question_version_id, fsm_ref, mime_type, created_at)
         VALUES($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(&qv_id)
    .bind(&fsm_ref)
    .bind(&rendered.mime_type)
    .bind(ts)
    .execute(&store.db)
    .await?;

    Ok(QuestionVisual {
        id,
        fsm_ref,
        mime_type: rendered.mime_type,
        created_at: ts,
    })
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
