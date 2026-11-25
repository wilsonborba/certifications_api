use crate::{
    core::{Error, Result, now},
    dal::{AnyQuery, Store, new_id},
    domain::{
        auth::{CallerIdentity, require_owner, require_owner_or_admin},
        provenance::{QuestionProvenanceStats, question_provenance_stats},
    },
};
use serde::{Deserialize, Serialize};
use sqlx::{AnyConnection, Row};
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

pub const MAX_QUIZ_TAGS: usize = 5;
pub const DEFAULT_PUBLIC_SEARCH_REBUILD_BATCH: usize = 25;
pub const MAX_PUBLIC_SEARCH_REBUILD_BATCH: usize = 100;
pub const MAX_PUBLIC_SEARCH_REBUILD_QUIZ_QUESTIONS: usize = 1000;

#[derive(Clone, Debug, Serialize)]
pub struct PublicSearchRebuildSummary {
    pub mode: String,
    pub batch_size: usize,
    pub after_quiz_id: Option<String>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub scanned_quiz_count: usize,
    pub public_active_quiz_count: usize,
    pub existing_index_row_count: usize,
    pub rows_to_index: usize,
    pub stale_rows_to_remove: usize,
    pub failed_quiz_id: Option<String>,
    pub failure_status: Option<u16>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TagInfo {
    pub slug: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicQuizStats {
    pub quiz_id: String,
    pub completed_attempt_count: i64,
    pub distinct_participants: i64,
    pub average_score_pct: Option<f64>,
    pub average_score_denominator: i64,
    pub typical_duration_sec: Option<i64>,
    pub duration_denominator: i64,
    pub basis: String,
    pub score_distribution_denominator: i64,
    pub score_distribution: Vec<ScoreBandCount>,
    pub score_distribution_report_view: serde_json::Value,
    pub question_provenance: QuestionProvenanceStats,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScoreBandCount {
    pub score_band: String,
    pub attempt_count: i64,
}

fn normalize_stats_language(language: Option<&str>) -> &'static str {
    language
        .unwrap_or("")
        .split(',')
        .map(|part| {
            part.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .find_map(|value| match value.as_str() {
            "pt" | "pt-br" => Some("pt-BR"),
            "th" | "th-th" => Some("th"),
            "en" | "en-us" | "en-gb" => Some("en"),
            _ => None,
        })
        .unwrap_or("en")
}

fn score_band_labels(language: &str) -> [&'static str; 3] {
    match language {
        "pt-BR" => ["0% a <50%", "50% a <80%", "80% a 100%"],
        "th" => ["0% ถึงน้อยกว่า 50%", "50% ถึงน้อยกว่า 80%", "80% ถึง 100%"],
        _ => ["0% to <50%", "50% to <80%", "80% to 100%"],
    }
}

fn score_distribution_page(
    quiz_id: &str,
    rows: &[ScoreBandCount],
    language: &str,
    visibility: &str,
) -> serde_json::Value {
    let (title, description, band_label, attempts_label, sample_definition) = match language {
        "pt-BR" => (
            "Distribuição das pontuações do questionário",
            "Pontuações de tentativas concluídas agrupadas em faixas percentuais fixas",
            "Faixa de pontuação",
            "Tentativas",
            "Tentativas concluídas com pontuação total maior que zero; cada tentativa conta uma vez.",
        ),
        "th" => (
            "การกระจายคะแนนแบบทดสอบ",
            "คะแนนจากความพยายามที่เสร็จสิ้นแล้วแบ่งตามช่วงเปอร์เซ็นต์คงที่",
            "ช่วงคะแนน",
            "จำนวนครั้ง",
            "ความพยายามที่เสร็จสิ้นและมีคะแนนรวมมากกว่าศูนย์ โดยนับแต่ละครั้งหนึ่งครั้ง",
        ),
        _ => (
            "Quiz score distribution",
            "Completed attempt scores grouped into fixed percentage bands",
            "Score band",
            "Attempts",
            "Completed attempts with a positive score total; each attempt counts once.",
        ),
    };
    let sample_size: i64 = rows.iter().map(|row| row.attempt_count).sum();
    serde_json::json!({
        "descriptor": {
            "dataset_id": format!("cert:quiz:{quiz_id}:score-distribution"),
            "title": title,
            "description": description,
            "equation": null,
            "fields": [
                {"name":"score_band", "label":band_label, "type":"string", "role":"dimension"},
                {"name":"attempt_count", "label":attempts_label, "type":"integer", "role":"measure", "unit":"attempts"}
            ],
            "filters": [],
            "data_tier": "live",
            "can_view": true,
            "visibility": visibility,
            "meta": {
                "source":"Certifications API",
                "tier":"live",
                "freshness_mode":"live",
                "language":language,
                "sample_size":sample_size,
                "sample_unit":"completed_attempts",
                "sample_definition":sample_definition
            }
        },
        "rows": rows,
        "offset": 0,
        "limit": rows.len(),
        "total_rows": rows.len(),
        "visual": {
            "template":"cert-score-distribution",
            "version":1,
            "mapping":{"bucket":["score_band"],"attempts":["attempt_count"]},
            "options":{}
        },
        "template_definition": {
            "template_id":"cert-score-distribution",
            "version":1,
            "kind":"chart",
            "visual":"bar",
            "title":title,
            "slots":[
                {
                    "slot_name":"bucket",
                    "role":"dimension",
                    "is_required":true,
                    "min_count":1,
                    "max_count":1,
                    "accepted_types":["string"]
                },
                {
                    "slot_name":"attempts",
                    "role":"measure",
                    "is_required":true,
                    "min_count":1,
                    "max_count":2,
                    "accepted_types":["integer","float"]
                }
            ],
            "options":[]
        }
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogQuizSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub origin: String,
    pub question_count: i64,
    pub languages: Vec<String>,
    pub tags: Vec<TagInfo>,
    pub completed_attempt_count: i64,
    pub distinct_participants: i64,
    pub average_score_pct: Option<f64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogPage {
    pub items: Vec<CatalogQuizSummary>,
    pub suggestions: Vec<String>,
    pub total: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicSourceMaterial {
    pub source_kind: String,
    pub reports_project: Option<String>,
    pub reports_report: Option<String>,
    pub citation_refs: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicQuestionPreview {
    pub id: String,
    pub language: String,
    pub origin: String,
    pub prompt: String,
    pub alternatives: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogQuestionPreview {
    pub id: String,
    pub question_version_id: String,
    pub source_quiz_title: String,
    pub source_quiz_tags: Vec<String>,
    pub language: String,
    pub origin: String,
    pub prompt: String,
    pub alternatives: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicQuizDetail {
    pub id: String,
    pub title: String,
    pub description: String,
    pub author_name: String,
    pub origin: String,
    pub tags: Vec<TagInfo>,
    pub question_count: i64,
    pub study_material: Vec<PublicSourceMaterial>,
    pub questions: Vec<PublicQuestionPreview>,
    pub performance: PublicQuizStats,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct CatalogQuery {
    pub q: Option<String>,
    pub language: Option<String>,
    pub tag: Option<String>,
    pub origin: Option<String>,
    pub suggest: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct CatalogQuestionQuery {
    pub q: Option<String>,
    pub language: Option<String>,
    pub origin: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicQuestionPage {
    pub items: Vec<CatalogQuestionPreview>,
    pub limit: i64,
    pub offset: i64,
    pub has_more: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PublicQuestionVersionSource {
    pub question_id: String,
    pub question_version_id: String,
    pub source_quiz_id: String,
    pub source_quiz_version_id: String,
    pub source_scope: String,
    pub origin_quiz_id: Option<String>,
    pub origin_quiz_version_id: Option<String>,
}

pub fn normalize_tag(raw: &str) -> Option<(String, String)> {
    let label: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 40 {
        return None;
    }
    let slug: String = trimmed
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        return None;
    }
    Some((slug, trimmed.to_string()))
}

pub fn deterministic_tag_suggestions(
    title: &str,
    description: &str,
    prompts: &[String],
) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "that",
        "this",
        "from",
        "what",
        "which",
        "when",
        "where",
        "how",
        "are",
        "was",
        "were",
        "have",
        "has",
        "about",
        "into",
        "your",
        "study",
        "quiz",
        "question",
        "questions",
        "para",
        "como",
        "sobre",
        "qual",
        "quais",
        "uma",
        "uns",
    ];
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for text in std::iter::once(title)
        .chain(std::iter::once(description))
        .chain(prompts.iter().map(String::as_str))
    {
        for token in text.split(|c: char| !c.is_alphanumeric()) {
            let lower = token.trim().to_lowercase();
            if lower.chars().count() < 3 || stop.contains(lower.as_str()) {
                continue;
            }
            *counts.entry(lower).or_insert(0) += 1;
        }
    }

    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(MAX_QUIZ_TAGS)
        .map(|(word, _)| word)
        .collect()
}

pub async fn suggest_study_tags(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
) -> Result<Vec<String>> {
    let row =
        sqlx::query("SELECT owner_id, title, description, quiz_id FROM studies WHERE id = $1")
            .bind(study_id)
            .fetch_optional(&store.db)
            .await?
            .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    require_owner(caller, &owner_id)?;

    let title: String = row.get("title");
    let description: String = row.get("description");
    let quiz_id: Option<String> = row.get("quiz_id");
    let mut prompts = Vec::new();
    if let Some(qid) = quiz_id.filter(|s| !s.is_empty()) {
        let active_vid: String =
            sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
                .bind(&qid)
                .fetch_one(&store.db)
                .await?;
        if !active_vid.is_empty() {
            let docs = sqlx::query(
                "SELECT qv.couch_doc_id FROM quiz_version_questions qvq
                 JOIN question_versions qv ON qv.id = qvq.question_version_id
                 WHERE qvq.quiz_version_id = $1",
            )
            .bind(&active_vid)
            .fetch_all(&store.db)
            .await?;
            for d in docs {
                let doc_id: String = d.get("couch_doc_id");
                if let Ok(doc) = store.docs.get(&doc_id).await
                    && let Some(p) = doc["prompt"].as_str()
                {
                    prompts.push(p.to_string());
                }
            }
        }
    }

    Ok(deterministic_tag_suggestions(
        &title,
        &description,
        &prompts,
    ))
}

pub async fn publish_study_quiz(
    store: &Store,
    caller: &CallerIdentity,
    study_id: &str,
    title: Option<String>,
    tags: Option<Vec<String>>,
    language: Option<&str>,
) -> Result<PublicQuizDetail> {
    let row = sqlx::query("SELECT owner_id, title, quiz_id FROM studies WHERE id = $1")
        .bind(study_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.get("owner_id");
    let study_title: String = row.get("title");
    require_owner(caller, &owner_id)?;

    let quiz_id: String = row
        .get::<Option<String>, _>("quiz_id")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::unprocessable("no_quiz: Study has no quiz ready to publish"))?;

    let active_vid: String =
        sqlx::query_scalar("SELECT active_version_id FROM quizzes WHERE id = $1")
            .bind(&quiz_id)
            .fetch_one(&store.db)
            .await?;
    if active_vid.is_empty() {
        return Err(Error::unprocessable(
            "no_questions: Quiz has no active version to publish",
        ));
    }
    let q_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
    )
    .bind(&active_vid)
    .fetch_one(&store.db)
    .await?;
    if q_count == 0 {
        return Err(Error::unprocessable(
            "no_questions: Quiz must contain at least one question before publishing",
        ));
    }

    // Manager correction: check every source; refuse publication if any source is a restricted report.
    let source_rows = sqlx::query(
        "SELECT source_kind, external_ref, is_restricted FROM sources WHERE study_id = $1",
    )
    .bind(study_id)
    .fetch_all(&store.db)
    .await?;
    for sr in source_rows {
        let kind: String = sr.get("source_kind");
        let is_restricted_col: i64 = sr.get("is_restricted");
        if kind == "report" {
            let ext: String = sr.get("external_ref");
            let (project, report) = ext.trim().split_once('/').ok_or_else(|| {
                Error::unprocessable(
                    "restricted_report_source: Cannot verify report permission; restricted report sources cannot be published",
                )
            })?;
            let access = store
                .reports
                .check_report_access(&caller.user_id, project, report)
                .await
                .map_err(|_| {
                    Error::unprocessable(
                        "restricted_report_source: Cannot verify report permission; restricted report sources cannot be published",
                    )
                })?;
            if access.is_restricted || is_restricted_col != 0 {
                // If Reports now permits it, allow publication; if Reports still restricts it, refuse.
                if access.is_restricted {
                    return Err(Error::unprocessable(
                        "restricted_report_source: A quiz with a restricted report source cannot be published; permit or replace the source first",
                    ));
                }
            }
        }
    }

    // Validate and normalize up to 5 relational tags.
    let mut normalized_tags = Vec::new();
    let mut seen_slugs = HashSet::new();
    if let Some(raw_tags) = tags {
        for raw in raw_tags {
            if let Some((slug, label)) = normalize_tag(&raw)
                && seen_slugs.insert(slug.clone())
            {
                normalized_tags.push((slug, label));
            }
        }
    }
    if normalized_tags.len() > MAX_QUIZ_TAGS {
        return Err(Error::unprocessable(
            "too_many_tags: A quiz can have at most 5 tags",
        ));
    }

    let ts = now();
    let mut tx = store.db.begin().await?;
    let effective_title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or(study_title);
    sqlx::query("UPDATE quiz_versions SET title = $1 WHERE id = $2")
        .bind(effective_title)
        .bind(&active_vid)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM quiz_tags WHERE quiz_id = $1")
        .bind(&quiz_id)
        .execute(&mut *tx)
        .await?;
    for (slug, label) in normalized_tags {
        let existing_id: Option<String> = sqlx::query_scalar("SELECT id FROM tags WHERE slug = $1")
            .bind(&slug)
            .fetch_optional(&mut *tx)
            .await?;
        let tag_id = match existing_id {
            Some(id) => id,
            None => {
                let id = new_id();
                sqlx::query("INSERT INTO tags(id, slug, label, created_at) VALUES($1, $2, $3, $4)")
                    .bind(&id)
                    .bind(&slug)
                    .bind(&label)
                    .bind(ts)
                    .execute(&mut *tx)
                    .await?;
                id
            }
        };
        sqlx::query("INSERT INTO quiz_tags(quiz_id, tag_id) VALUES($1, $2)")
            .bind(&quiz_id)
            .bind(&tag_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE quizzes SET visibility = 'public', updated_at = $1 WHERE id = $2")
        .bind(ts)
        .bind(&quiz_id)
        .execute(&mut *tx)
        .await?;
    refresh_public_question_search_in_tx(store, &mut tx, &quiz_id).await?;
    tx.commit().await?;

    get_public_quiz(store, &quiz_id, language).await
}

async fn load_quiz_tags(store: &Store, quiz_id: &str) -> Result<Vec<TagInfo>> {
    let rows = sqlx::query(
        "SELECT t.slug, t.label
         FROM quiz_tags qt
         JOIN tags t ON t.id = qt.tag_id
         WHERE qt.quiz_id = $1
         ORDER BY t.label",
    )
    .bind(quiz_id)
    .fetch_all(&store.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| TagInfo {
            slug: r.get("slug"),
            label: r.get("label"),
        })
        .collect())
}

async fn calculate_quiz_statistics(
    store: &Store,
    quiz_id: &str,
    language: Option<&str>,
    visibility: &str,
) -> Result<PublicQuizStats> {
    let question_provenance = question_provenance_stats(store, "quiz", quiz_id).await?;

    let row = sqlx::query(
        "SELECT COUNT(*) AS completed_cnt,
                COUNT(DISTINCT owner_id) AS distinct_users,
                AVG(CAST(CASE WHEN score_total > 0 THEN (score_correct * 100.0) / score_total END AS DOUBLE PRECISION)) AS avg_pct,
                COALESCE(SUM(CASE WHEN score_total > 0 THEN 1 ELSE 0 END), 0) AS score_n,
                AVG(CAST(CASE WHEN completed_at >= started_at THEN completed_at - started_at END AS DOUBLE PRECISION)) AS avg_dur,
                COALESCE(SUM(CASE WHEN completed_at >= started_at THEN 1 ELSE 0 END), 0) AS duration_n
         FROM attempts
         WHERE quiz_id = $1 AND status = 'completed'",
    )
    .bind(quiz_id)
    .fetch_one(&store.db)
    .await?;

    let completed_attempt_count: i64 = row.try_get("completed_cnt")?;
    let distinct_participants: i64 = row.try_get("distinct_users")?;
    let average_score_denominator: i64 = row.try_get("score_n")?;
    let average_score_pct: Option<f64> = row.try_get("avg_pct")?;
    let average_duration: Option<f64> = row.try_get("avg_dur")?;
    let typical_duration_sec = average_duration.map(|value| value.round() as i64);
    let duration_denominator: i64 = row.try_get("duration_n")?;

    let distribution_rows = sqlx::query(
        "SELECT CASE
                    WHEN score_correct * 100.0 / score_total < 50 THEN '0-49%'
                    WHEN score_correct * 100.0 / score_total < 80 THEN '50-79%'
                    ELSE '80-100%'
                END AS score_band,
                COUNT(*) AS attempt_count
         FROM attempts
         WHERE quiz_id = $1 AND status = 'completed' AND score_total > 0
         GROUP BY CASE
                    WHEN score_correct * 100.0 / score_total < 50 THEN '0-49%'
                    WHEN score_correct * 100.0 / score_total < 80 THEN '50-79%'
                    ELSE '80-100%'
                  END",
    )
    .bind(quiz_id)
    .fetch_all(&store.db)
    .await?;
    let band_order = ["0-49%", "50-79%", "80-100%"];
    let mut score_distribution = distribution_rows
        .into_iter()
        .map(|r| {
            Ok(ScoreBandCount {
                score_band: r.try_get("score_band")?,
                attempt_count: r.try_get("attempt_count")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    score_distribution.sort_by_key(|r| {
        band_order
            .iter()
            .position(|band| *band == r.score_band)
            .unwrap_or(3)
    });
    let score_distribution_denominator = score_distribution.iter().map(|r| r.attempt_count).sum();
    let language = normalize_stats_language(language);
    let localized_bands = score_band_labels(language);
    for row in &mut score_distribution {
        let band_index = band_order.iter().position(|band| *band == row.score_band);
        if let Some(label) = band_index.and_then(|index| localized_bands.get(index)) {
            row.score_band = (*label).to_string();
        }
    }
    let score_distribution_report_view =
        score_distribution_page(quiz_id, &score_distribution, language, visibility);

    Ok(PublicQuizStats {
        quiz_id: quiz_id.to_string(),
        completed_attempt_count,
        distinct_participants,
        average_score_pct,
        average_score_denominator,
        typical_duration_sec,
        duration_denominator,
        basis: "all_completed_attempts".into(),
        score_distribution_denominator,
        score_distribution,
        score_distribution_report_view,
        question_provenance,
    })
}

pub async fn get_public_quiz_statistics(
    store: &Store,
    quiz_id: &str,
    language: Option<&str>,
) -> Result<PublicQuizStats> {
    let visibility: Option<String> =
        sqlx::query_scalar("SELECT visibility FROM quizzes WHERE id = $1")
            .bind(quiz_id)
            .fetch_optional(&store.db)
            .await?;
    if visibility.as_deref() != Some("public") {
        return Err(Error::missing());
    }
    calculate_quiz_statistics(store, quiz_id, language, "public").await
}

pub async fn get_owner_quiz_statistics(
    store: &Store,
    caller: &CallerIdentity,
    quiz_id: &str,
    language: Option<&str>,
) -> Result<PublicQuizStats> {
    let row = sqlx::query("SELECT owner_id, visibility FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(Error::missing)?;
    let owner_id: String = row.try_get("owner_id")?;
    let visibility: String = row.try_get("visibility")?;
    require_owner_or_admin(caller, &owner_id)?;
    let report_visibility = if visibility == "public" {
        "public"
    } else {
        "restricted"
    };
    calculate_quiz_statistics(store, quiz_id, language, report_visibility).await
}

pub async fn get_public_quiz(
    store: &Store,
    quiz_id: &str,
    language: Option<&str>,
) -> Result<PublicQuizDetail> {
    let row = sqlx::query(
        "SELECT q.id, q.owner_id, q.study_id, q.origin, q.active_version_id,
                qv.title, COALESCE(s.description, '') AS description,
                COALESCE(ch.display_name, 'Anonymous author') AS author_name
         FROM quizzes q
         JOIN quiz_versions qv ON qv.id = q.active_version_id
         LEFT JOIN studies s ON s.id = q.study_id
         LEFT JOIN certificate_holders ch ON ch.owner_id = q.owner_id
         WHERE q.id = $1 AND q.visibility = 'public'",
    )
    .bind(quiz_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;

    let active_vid: String = row.get("active_version_id");
    let tags = load_quiz_tags(store, quiz_id).await?;
    let performance = get_public_quiz_statistics(store, quiz_id, language).await?;

    let q_rows = sqlx::query(
        "SELECT qu.id AS question_id, qu.origin, qver.id AS qv_id, qver.language,
                qver.couch_doc_id, qvq.source_scope
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         JOIN questions qu ON qu.id = qver.question_id
         WHERE qvq.quiz_version_id = $1
         ORDER BY qvq.position",
    )
    .bind(&active_vid)
    .fetch_all(&store.db)
    .await?;

    let mut questions = Vec::with_capacity(q_rows.len());
    let mut material_map: std::collections::BTreeMap<String, PublicSourceMaterial> =
        std::collections::BTreeMap::new();

    for qr in q_rows {
        let doc_id: String = qr.get("couch_doc_id");
        let qv_id: String = qr.get("qv_id");
        let source_scope: String = qr.get("source_scope");
        if let Ok(doc) = store.docs.get(&doc_id).await {
            // Public preview NEVER leaks correct_id or explanation.
            questions.push(PublicQuestionPreview {
                id: qr.get("question_id"),
                language: qr.get("language"),
                origin: qr.get("origin"),
                prompt: doc["prompt"].as_str().unwrap_or("").to_string(),
                alternatives: doc["alternatives"].clone(),
            });
        }
        if source_scope != "reused_public_question_version" {
            let cit_rows = sqlx::query(
                "SELECT s.id, s.source_kind, s.external_ref, qc.page_or_offset
                 FROM question_citations qc
                 JOIN sources s ON s.id = qc.source_id
                 WHERE qc.question_version_id = $1",
            )
            .bind(&qv_id)
            .fetch_all(&store.db)
            .await?;
            for cr in cit_rows {
                let sid: String = cr.get("id");
                let kind: String = cr.get("source_kind");
                let ext: String = cr.get("external_ref");
                let page: String = cr.get("page_or_offset");
                let (proj, rep) = if kind == "report" {
                    match ext.split_once('/') {
                        Some((p, r)) => (Some(p.to_string()), Some(r.to_string())),
                        None => (None, None),
                    }
                } else {
                    (None, None)
                };
                let entry = material_map
                    .entry(sid)
                    .or_insert_with(|| PublicSourceMaterial {
                        source_kind: kind.clone(),
                        reports_project: proj,
                        reports_report: rep,
                        citation_refs: Vec::new(),
                    });
                if !page.is_empty() && !entry.citation_refs.contains(&page) {
                    entry.citation_refs.push(page);
                }
            }
        }
    }

    Ok(PublicQuizDetail {
        id: row.get("id"),
        title: row.get("title"),
        description: row.get("description"),
        author_name: row.get("author_name"),
        origin: row.get("origin"),
        tags,
        question_count: questions.len() as i64,
        study_material: material_map.into_values().collect(),
        questions,
        performance,
    })
}

pub async fn list_catalog(store: &Store, query: CatalogQuery) -> Result<CatalogPage> {
    let rows = sqlx::query(
        "SELECT q.id, q.origin, q.active_version_id, q.updated_at,
                qv.title, COALESCE(s.description, '') AS description,
                COALESCE(ch.display_name, 'Anonymous author') AS author_name
         FROM quizzes q
         JOIN quiz_versions qv ON qv.id = q.active_version_id
         LEFT JOIN studies s ON s.id = q.study_id
         LEFT JOIN certificate_holders ch ON ch.owner_id = q.owner_id
         WHERE q.visibility = 'public'
         ORDER BY q.updated_at DESC",
    )
    .fetch_all(&store.db)
    .await?;

    let q_filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let tag_filter = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);

    let mut matched = Vec::new();
    let mut suggestions = BTreeSet::new();

    for r in rows {
        let quiz_id: String = r.get("id");
        let title: String = r.get("title");
        let description: String = r.get("description");
        let origin: String = r.get("origin");
        let active_vid: String = r.get("active_version_id");

        if let Some(ref orig) = query.origin
            && &origin != orig
        {
            continue;
        }

        let tags = load_quiz_tags(store, &quiz_id).await?;
        if let Some(ref tf) = tag_filter
            && !tags
                .iter()
                .any(|t| t.slug.to_lowercase() == *tf || t.label.to_lowercase() == *tf)
        {
            continue;
        }

        let lang_rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT qver.language
             FROM quiz_version_questions qvq
             JOIN question_versions qver ON qver.id = qvq.question_version_id
             WHERE qvq.quiz_version_id = $1
             ORDER BY qver.language",
        )
        .bind(&active_vid)
        .fetch_all(&store.db)
        .await?;

        if let Some(ref lf) = query.language
            && !lang_rows.iter().any(|l| l == lf)
        {
            continue;
        }

        if let Some(ref qf) = q_filter {
            let in_title = title.to_lowercase().contains(qf);
            let in_desc = description.to_lowercase().contains(qf);
            let in_tags = tags.iter().any(|t| t.label.to_lowercase().contains(qf));
            if !in_title && !in_desc && !in_tags {
                continue;
            }
        }

        if query.suggest.unwrap_or(false) {
            suggestions.insert(title.clone());
        }

        let question_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM quiz_version_questions WHERE quiz_version_id = $1",
        )
        .bind(&active_vid)
        .fetch_one(&store.db)
        .await?;
        let stats = get_public_quiz_statistics(store, &quiz_id, None).await?;

        matched.push(CatalogQuizSummary {
            id: quiz_id,
            title,
            description,
            author_name: r.get("author_name"),
            origin,
            question_count,
            languages: lang_rows,
            tags,
            completed_attempt_count: stats.completed_attempt_count,
            distinct_participants: stats.distinct_participants,
            average_score_pct: stats.average_score_pct,
            updated_at: r.get("updated_at"),
        });
    }

    let total = matched.len() as i64;
    let offset = query.offset.unwrap_or(0).max(0) as usize;
    let limit = query.limit.unwrap_or(20).clamp(1, 50) as usize;
    let items: Vec<CatalogQuizSummary> = matched.into_iter().skip(offset).take(limit).collect();

    Ok(CatalogPage {
        items,
        suggestions: suggestions.into_iter().take(10).collect(),
        total,
    })
}

pub(crate) async fn public_question_version_sources_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    question_version_ids: Option<&[String]>,
) -> Result<Vec<PublicQuestionVersionSource>> {
    let query = public_question_version_sources_query(question_version_ids)?;
    let rows = query.query().fetch_all(&mut **tx).await?;
    Ok(rows
        .into_iter()
        .map(|row| PublicQuestionVersionSource {
            question_id: row.get("question_id"),
            question_version_id: row.get("question_version_id"),
            source_quiz_id: row.get("source_quiz_id"),
            source_quiz_version_id: row.get("source_quiz_version_id"),
            source_scope: row.get("source_scope"),
            origin_quiz_id: row.get("origin_quiz_id"),
            origin_quiz_version_id: row.get("origin_quiz_version_id"),
        })
        .collect())
}

fn public_question_version_sources_query(
    question_version_ids: Option<&[String]>,
) -> Result<AnyQuery> {
    if question_version_ids.is_some_and(|ids| ids.is_empty()) {
        return Ok(AnyQuery::new(
            "SELECT '' AS question_id, '' AS question_version_id, '' AS source_quiz_id, '' AS source_quiz_version_id, '' AS source_scope, NULL AS origin_quiz_id, NULL AS origin_quiz_version_id, '' AS couch_doc_id, '' AS language, '' AS question_origin, '' AS source_quiz_title WHERE 1 = 0",
        ));
    }
    let mut query = AnyQuery::new(
        "WITH public_candidates AS (
             SELECT q.id AS source_quiz_id, q.active_version_id AS source_quiz_version_id,
                    qversion.title AS source_quiz_title,
                    qvq.question_version_id, question_version.question_id,
                    question_version.language, question_version.couch_doc_id,
                    question_version.created_at, question.origin AS question_origin,
                    qvq.source_scope, qvq.origin_quiz_id, qvq.origin_quiz_version_id
             FROM quizzes q
             JOIN quiz_versions qversion ON qversion.id = q.active_version_id
             JOIN quiz_version_questions qvq ON qvq.quiz_version_id = qversion.id
             JOIN question_versions question_version ON question_version.id = qvq.question_version_id
             JOIN questions question ON question.id = question_version.question_id
             WHERE q.visibility = 'public'
               AND (qvq.source_scope <> 'reused_public_question_version'
                    OR (qvq.origin_quiz_id IS NOT NULL
                        AND qvq.origin_quiz_version_id IS NOT NULL
                        AND EXISTS (
                            SELECT 1 FROM quiz_versions origin_version
                            JOIN quiz_version_questions origin_membership
                              ON origin_membership.quiz_version_id = origin_version.id
                            WHERE origin_version.id = qvq.origin_quiz_version_id
                              AND origin_version.quiz_id = qvq.origin_quiz_id
                              AND origin_membership.question_version_id = qvq.question_version_id
                        )))",
    );
    if let Some(ids) = question_version_ids {
        query.push(" AND qvq.question_version_id IN (");
        for (index, id) in ids.iter().enumerate() {
            if index > 0 {
                query.push(", ");
            }
            query.push_bind_text(id.clone());
        }
        query.push(")");
    }
    query.push(
        ") SELECT source_quiz_id, source_quiz_version_id, source_quiz_title,
                 question_version_id, question_id, language, couch_doc_id, question_origin,
                 source_scope, origin_quiz_id, origin_quiz_version_id
          FROM public_candidates
          ORDER BY created_at DESC, question_version_id ASC, source_quiz_id ASC",
    );
    Ok(query)
}

async fn public_quiz_tags(
    store: &Store,
    quiz_ids: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    if quiz_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut query = AnyQuery::new(
        "SELECT qt.quiz_id, t.label FROM quiz_tags qt JOIN tags t ON t.id = qt.tag_id WHERE qt.quiz_id IN (",
    );
    for (index, id) in quiz_ids.iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query.push_bind_text(id.clone());
    }
    query.push(") ORDER BY t.label");
    let rows = query.query().fetch_all(&store.db).await?;
    let mut tags: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        tags.entry(row.get("quiz_id"))
            .or_default()
            .push(row.get("label"));
    }
    Ok(tags)
}

pub(crate) async fn refresh_public_question_search_in_tx(
    store: &Store,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    quiz_id: &str,
) -> Result<()> {
    let projection = build_public_question_search_projection(store, tx, quiz_id, None).await?;
    replace_public_question_search_projection(tx, quiz_id, &projection.rows).await
}

#[derive(Clone, Debug)]
struct PublicQuestionSearchRow {
    question_version_id: String,
    normalized_prompt: String,
    normalized_title: String,
    normalized_tags: String,
}

#[derive(Clone, Debug, Default)]
struct PublicQuestionSearchProjection {
    is_public_active: bool,
    rows: Vec<PublicQuestionSearchRow>,
}

fn normalize_public_search_text(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect()
}

async fn build_public_question_search_projection(
    store: &Store,
    connection: &mut AnyConnection,
    quiz_id: &str,
    max_questions: Option<usize>,
) -> Result<PublicQuestionSearchProjection> {
    let quiz = sqlx::query("SELECT visibility, active_version_id FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .fetch_optional(&mut *connection)
        .await?;
    let Some(quiz) = quiz else {
        return Ok(PublicQuestionSearchProjection::default());
    };
    let active_version_id: String = quiz.get("active_version_id");
    if quiz.get::<String, _>("visibility") != "public" || active_version_id.is_empty() {
        return Ok(PublicQuestionSearchProjection::default());
    }
    let title: Option<String> =
        sqlx::query_scalar("SELECT title FROM quiz_versions WHERE id = $1 AND quiz_id = $2")
            .bind(&active_version_id)
            .bind(quiz_id)
            .fetch_optional(&mut *connection)
            .await?;
    let Some(title) = title else {
        return Err(Error::unavailable());
    };
    let tag_rows = sqlx::query(
        "SELECT t.label FROM quiz_tags qt JOIN tags t ON t.id = qt.tag_id
         WHERE qt.quiz_id = $1 ORDER BY t.label",
    )
    .bind(quiz_id)
    .fetch_all(&mut *connection)
    .await?;
    let normalized_tags = tag_rows
        .iter()
        .map(|row| normalize_public_search_text(&row.get::<String, _>("label")))
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let mut question_query = AnyQuery::new(
        "SELECT qv.id AS question_version_id, qv.couch_doc_id
         FROM quiz_version_questions qvq JOIN question_versions qv ON qv.id = qvq.question_version_id
         WHERE qvq.quiz_version_id = ",
    );
    question_query.push_bind_text(active_version_id.clone());
    question_query.push(" ORDER BY qvq.position, qv.id");
    if let Some(limit) = max_questions {
        question_query.push(" LIMIT ");
        question_query.push_bind_i64(i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX));
    }
    let question_rows = question_query.query().fetch_all(&mut *connection).await?;
    if max_questions.is_some_and(|limit| question_rows.len() > limit) {
        return Err(Error::unprocessable(
            "search_rebuild_quiz_too_large: Quiz exceeds the bounded per-quiz question limit",
        ));
    }
    let mut projection = PublicQuestionSearchProjection {
        is_public_active: true,
        rows: Vec::with_capacity(question_rows.len()),
    };
    for row in question_rows {
        let question_version_id: String = row.get("question_version_id");
        let doc_id: String = row.get("couch_doc_id");
        let doc = store.docs.get(&doc_id).await?;
        let prompt = doc["prompt"].as_str().ok_or_else(Error::unavailable)?;
        projection.rows.push(PublicQuestionSearchRow {
            question_version_id,
            normalized_prompt: normalize_public_search_text(prompt),
            normalized_title: normalize_public_search_text(&title),
            normalized_tags: normalized_tags.clone(),
        });
    }
    Ok(projection)
}

async fn existing_public_question_search_question_ids(
    connection: &mut AnyConnection,
    quiz_id: &str,
) -> Result<HashSet<String>> {
    let rows =
        sqlx::query("SELECT question_version_id FROM public_question_search WHERE quiz_id = $1")
            .bind(quiz_id)
            .fetch_all(&mut *connection)
            .await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("question_version_id"))
        .collect())
}

async fn replace_public_question_search_projection(
    connection: &mut AnyConnection,
    quiz_id: &str,
    rows: &[PublicQuestionSearchRow],
) -> Result<()> {
    sqlx::query("DELETE FROM public_question_search WHERE quiz_id = $1")
        .bind(quiz_id)
        .execute(&mut *connection)
        .await?;
    for row in rows {
        sqlx::query(
            "INSERT INTO public_question_search
             (question_version_id, quiz_id, normalized_prompt, normalized_title, normalized_tags)
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&row.question_version_id)
        .bind(quiz_id)
        .bind(&row.normalized_prompt)
        .bind(&row.normalized_title)
        .bind(&row.normalized_tags)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

async fn rebuild_one_public_question_search_quiz(
    store: &Store,
    quiz_id: &str,
    write: bool,
) -> Result<(PublicQuestionSearchProjection, HashSet<String>)> {
    if write {
        let mut tx = store.db.begin().await?;
        let projection = build_public_question_search_projection(
            store,
            &mut tx,
            quiz_id,
            Some(MAX_PUBLIC_SEARCH_REBUILD_QUIZ_QUESTIONS),
        )
        .await?;
        let existing = existing_public_question_search_question_ids(&mut tx, quiz_id).await?;
        replace_public_question_search_projection(&mut tx, quiz_id, &projection.rows).await?;
        tx.commit().await.map_err(|_| Error::unavailable())?;
        Ok((projection, existing))
    } else {
        let mut connection = store.db.acquire().await?;
        let projection = build_public_question_search_projection(
            store,
            &mut connection,
            quiz_id,
            Some(MAX_PUBLIC_SEARCH_REBUILD_QUIZ_QUESTIONS),
        )
        .await?;
        let existing =
            existing_public_question_search_question_ids(&mut connection, quiz_id).await?;
        Ok((projection, existing))
    }
}

pub async fn rebuild_public_question_search_batch(
    store: &Store,
    after_quiz_id: Option<&str>,
    batch_size: usize,
    write: bool,
) -> Result<PublicSearchRebuildSummary> {
    if !(1..=MAX_PUBLIC_SEARCH_REBUILD_BATCH).contains(&batch_size) {
        return Err(Error::bad(
            "invalid_batch_size: Batch size must be between 1 and 100",
        ));
    }
    let mut quiz_query = AnyQuery::new("SELECT id FROM quizzes WHERE 1 = 1");
    if let Some(cursor) = after_quiz_id.filter(|value| !value.is_empty()) {
        quiz_query.push(" AND id > ");
        quiz_query.push_bind_text(cursor.to_owned());
    }
    quiz_query.push(" ORDER BY id LIMIT ");
    quiz_query.push_bind_i64(i64::try_from(batch_size.saturating_add(1)).unwrap_or(i64::MAX));
    let quiz_rows = quiz_query.query().fetch_all(&store.db).await?;
    let has_more = quiz_rows.len() > batch_size;
    let quiz_ids = quiz_rows
        .into_iter()
        .take(batch_size)
        .map(|row| row.get::<String, _>("id"))
        .collect::<Vec<_>>();
    let mut summary = PublicSearchRebuildSummary {
        mode: if write { "write" } else { "dry-run" }.into(),
        batch_size,
        after_quiz_id: after_quiz_id.map(str::to_owned),
        next_cursor: after_quiz_id.map(str::to_owned),
        has_more,
        scanned_quiz_count: 0,
        public_active_quiz_count: 0,
        existing_index_row_count: 0,
        rows_to_index: 0,
        stale_rows_to_remove: 0,
        failed_quiz_id: None,
        failure_status: None,
    };
    for quiz_id in quiz_ids {
        let (projection, existing) =
            match rebuild_one_public_question_search_quiz(store, &quiz_id, write).await {
                Ok(result) => result,
                Err(error) => {
                    summary.failed_quiz_id = Some(quiz_id.clone());
                    summary.failure_status = Some(error.0.as_u16());
                    break;
                }
            };
        summary.scanned_quiz_count += 1;
        if projection.is_public_active {
            summary.public_active_quiz_count += 1;
        }
        summary.existing_index_row_count += existing.len();
        summary.rows_to_index += projection.rows.len();
        let current_ids = projection
            .rows
            .iter()
            .map(|row| row.question_version_id.as_str())
            .collect::<HashSet<_>>();
        summary.stale_rows_to_remove += existing
            .iter()
            .filter(|question_version_id| !current_ids.contains(question_version_id.as_str()))
            .count();
        summary.next_cursor = Some(quiz_id);
    }
    if summary.failed_quiz_id.is_some() {
        summary.has_more = true;
    }
    Ok(summary)
}

pub async fn list_catalog_questions(
    store: &Store,
    query: CatalogQuestionQuery,
) -> Result<PublicQuestionPage> {
    let q_filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let escaped = normalize_public_search_text(s)
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            format!("%{escaped}%")
        });
    let limit = query.limit.unwrap_or(20).clamp(1, 50) as usize;
    let offset = query.offset.unwrap_or(0).max(0) as usize;
    let mut sql = AnyQuery::new(
        "WITH matching_origins AS (
           SELECT q.id AS source_quiz_id, q.active_version_id AS source_quiz_version_id,
                  qv.title AS source_quiz_title, qvq.question_version_id,
                  qquestion.question_id, qquestion.language, qquestion.couch_doc_id,
                  qquestion.created_at AS question_created_at, question.origin AS question_origin,
                  ROW_NUMBER() OVER (PARTITION BY qvq.question_version_id
                                     ORDER BY qquestion.created_at DESC, q.id ASC) AS origin_rank
           FROM quizzes q
           JOIN quiz_versions qv ON qv.id = q.active_version_id
           JOIN quiz_version_questions qvq ON qvq.quiz_version_id = qv.id
           JOIN question_versions qquestion ON qquestion.id = qvq.question_version_id
           JOIN questions question ON question.id = qquestion.question_id
           WHERE q.visibility = 'public'
             AND (qvq.source_scope <> 'reused_public_question_version'
                  OR (qvq.origin_quiz_id IS NOT NULL AND qvq.origin_quiz_version_id IS NOT NULL
                      AND EXISTS (SELECT 1 FROM quiz_versions ov
                                  JOIN quiz_version_questions om ON om.quiz_version_id = ov.id
                                  WHERE ov.id = qvq.origin_quiz_version_id
                                    AND ov.quiz_id = qvq.origin_quiz_id
                                    AND om.question_version_id = qvq.question_version_id)))",
    );
    if let Some(language) = query.language.as_deref() {
        sql.push(" AND qquestion.language = ");
        sql.push_bind_text(language.to_string());
    }
    if let Some(origin) = query.origin.as_deref() {
        sql.push(" AND question.origin = ");
        sql.push_bind_text(origin.to_string());
    }
    if let Some(ref q) = q_filter {
        sql.push(
            " AND EXISTS (SELECT 1 FROM public_question_search pqs
                               WHERE pqs.question_version_id = qquestion.id
                                 AND pqs.quiz_id = q.id
                                 AND (pqs.normalized_prompt LIKE ",
        );
        sql.push_bind_text(q.clone());
        sql.push(" ESCAPE '!' OR pqs.normalized_title LIKE ");
        sql.push_bind_text(q.clone());
        sql.push(" ESCAPE '!' OR pqs.normalized_tags LIKE ");
        sql.push_bind_text(q.clone());
        sql.push(" ESCAPE '!'))");
    }
    sql.push(
        ") SELECT source_quiz_id, source_quiz_version_id, source_quiz_title,
                 question_version_id, question_id, language, couch_doc_id, question_origin
          FROM matching_origins WHERE origin_rank = 1
          ORDER BY question_created_at DESC, question_version_id ASC LIMIT ",
    );
    sql.push_bind_i64((limit + 1) as i64);
    sql.push(" OFFSET ");
    sql.push_bind_i64(offset as i64);
    let rows = sql.query().fetch_all(&store.db).await?;
    let has_more = rows.len() > limit;
    let page_rows = rows.into_iter().take(limit).collect::<Vec<_>>();
    let quiz_ids = page_rows
        .iter()
        .map(|row| row.get::<String, _>("source_quiz_id"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let tags_by_quiz = public_quiz_tags(store, &quiz_ids).await?;
    let mut items = Vec::with_capacity(page_rows.len());
    for row in page_rows {
        let quiz_id: String = row.get("source_quiz_id");
        let doc_id: String = row.get("couch_doc_id");
        let doc = store.docs.get(&doc_id).await?;
        items.push(CatalogQuestionPreview {
            id: row.get("question_id"),
            question_version_id: row.get("question_version_id"),
            source_quiz_title: row.get("source_quiz_title"),
            source_quiz_tags: tags_by_quiz.get(&quiz_id).cloned().unwrap_or_default(),
            language: row.get("language"),
            origin: row.get("question_origin"),
            prompt: doc["prompt"].as_str().unwrap_or("").to_string(),
            alternatives: doc["alternatives"].clone(),
        });
    }
    Ok(PublicQuestionPage {
        items,
        limit: limit as i64,
        offset: offset as i64,
        has_more,
    })
}
