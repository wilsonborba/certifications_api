use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::auth::{CallerIdentity, require_owner},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{BTreeSet, HashSet};

pub const MAX_QUIZ_TAGS: usize = 5;

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
    pub typical_duration_sec: Option<i64>,
    pub basis: String,
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
    tx.commit().await?;

    get_public_quiz(store, &quiz_id).await
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

pub async fn get_public_quiz_statistics(store: &Store, quiz_id: &str) -> Result<PublicQuizStats> {
    let vis: Option<String> = sqlx::query_scalar("SELECT visibility FROM quizzes WHERE id = $1")
        .bind(quiz_id)
        .fetch_optional(&store.db)
        .await?;
    if vis.as_deref() != Some("public") {
        return Err(Error::missing());
    }

    let row = sqlx::query(
        "SELECT COUNT(*) AS completed_cnt,
                COUNT(DISTINCT owner_id) AS distinct_users,
                AVG(CASE WHEN score_total > 0 THEN (score_correct * 100.0) / score_total END) AS avg_pct,
                AVG(CASE WHEN completed_at IS NOT NULL THEN completed_at - started_at END) AS avg_dur
         FROM attempts
         WHERE quiz_id = $1 AND status = 'completed'",
    )
    .bind(quiz_id)
    .fetch_one(&store.db)
    .await?;

    let completed_attempt_count: i64 = row.get("completed_cnt");
    let distinct_participants: i64 = row.get("distinct_users");
    let average_score_pct: Option<f64> = if completed_attempt_count > 0 {
        row.try_get("avg_pct").ok()
    } else {
        None
    };
    let typical_duration_sec: Option<i64> = if completed_attempt_count > 0 {
        row.try_get::<f64, _>("avg_dur")
            .ok()
            .map(|v| v.round() as i64)
    } else {
        None
    };

    Ok(PublicQuizStats {
        quiz_id: quiz_id.to_string(),
        completed_attempt_count,
        distinct_participants,
        average_score_pct,
        typical_duration_sec,
        basis: "all_completed_attempts".into(),
    })
}

pub async fn get_public_quiz(store: &Store, quiz_id: &str) -> Result<PublicQuizDetail> {
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
    let performance = get_public_quiz_statistics(store, quiz_id).await?;

    let q_rows = sqlx::query(
        "SELECT qu.id AS question_id, qu.origin, qver.id AS qv_id, qver.language, qver.couch_doc_id
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
        let stats = get_public_quiz_statistics(store, &quiz_id).await?;

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

pub async fn list_catalog_questions(
    store: &Store,
    query: CatalogQuestionQuery,
) -> Result<Vec<PublicQuestionPreview>> {
    let rows = sqlx::query(
        "SELECT DISTINCT qu.id AS question_id, qu.origin, qver.language, qver.couch_doc_id
         FROM quizzes q
         JOIN quiz_version_questions qvq ON qvq.quiz_version_id = q.active_version_id
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         JOIN questions qu ON qu.id = qver.question_id
         WHERE q.visibility = 'public'
         ORDER BY qver.created_at DESC",
    )
    .fetch_all(&store.db)
    .await?;

    let q_filter = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let limit = query.limit.unwrap_or(20).clamp(1, 50) as usize;
    let mut out = Vec::new();

    for r in rows {
        let language: String = r.get("language");
        let origin: String = r.get("origin");
        if let Some(ref lf) = query.language
            && &language != lf
        {
            continue;
        }
        if let Some(ref of) = query.origin
            && &origin != of
        {
            continue;
        }
        let doc_id: String = r.get("couch_doc_id");
        if let Ok(doc) = store.docs.get(&doc_id).await {
            let prompt = doc["prompt"].as_str().unwrap_or("").to_string();
            if let Some(ref qf) = q_filter
                && !prompt.to_lowercase().contains(qf)
            {
                continue;
            }
            out.push(PublicQuestionPreview {
                id: r.get("question_id"),
                language,
                origin,
                prompt,
                alternatives: doc["alternatives"].clone(),
            });
            if out.len() >= limit {
                break;
            }
        }
    }

    Ok(out)
}
