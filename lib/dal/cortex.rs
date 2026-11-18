//! Cortex T0 Easy API adapter for quiz question generation.
use crate::core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{future::Future, pin::Pin, sync::Arc};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedAlternative {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedQuestion {
    pub prompt: String,
    pub alternatives: Vec<GeneratedAlternative>,
    pub correct_id: String,
    pub explanation: String,
    pub source_id: String,
    pub citation_ref: String,
}

#[derive(Clone, Debug)]
pub struct GenerationRequest {
    pub owner_id: String,
    pub job_id: String,
    pub language: String,
    pub count: i64,
    pub source_ids: Vec<String>,
    pub existing_prompts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CortexJobOutcome {
    Pending,
    Completed(String),
    NotFound,
}

pub trait CortexClient: Send + Sync {
    fn is_configured(&self) -> bool;
    fn generate_questions<'a>(
        &'a self,
        req: &'a GenerationRequest,
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>>;
    fn correct_language<'a>(
        &'a self,
        owner_id: &'a str,
        language: &'a str,
        questions: &'a [GeneratedQuestion],
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>>;
    fn check_request_outcome<'a>(
        &'a self,
        cortex_job_id: &'a str,
    ) -> BoxFuture<'a, Result<CortexJobOutcome>>;
}

pub struct DisabledCortex;

impl CortexClient for DisabledCortex {
    fn is_configured(&self) -> bool {
        false
    }
    fn generate_questions<'a>(
        &'a self,
        _req: &'a GenerationRequest,
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
    fn correct_language<'a>(
        &'a self,
        _owner_id: &'a str,
        _language: &'a str,
        _questions: &'a [GeneratedQuestion],
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
    fn check_request_outcome<'a>(
        &'a self,
        _cortex_job_id: &'a str,
    ) -> BoxFuture<'a, Result<CortexJobOutcome>> {
        Box::pin(async { Ok(CortexJobOutcome::NotFound) })
    }
}

pub struct HttpCortex {
    base_url: String,
    http: reqwest::Client,
}

impl HttpCortex {
    pub fn client(base_url: &str, http: reqwest::Client) -> Arc<dyn CortexClient> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Arc::new(DisabledCortex);
        }
        Arc::new(Self {
            base_url: base.to_string(),
            http,
        })
    }
}

impl CortexClient for HttpCortex {
    fn is_configured(&self) -> bool {
        true
    }
    fn generate_questions<'a>(
        &'a self,
        req: &'a GenerationRequest,
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>> {
        Box::pin(async move {
            let resp = self
                .http
                .post(format!("{}/jobs", self.base_url))
                .header("x-uuid", &req.owner_id)
                .json(&json!({
                    "app": "certifications",
                    "tier": "T0",
                    "mode": "easy",
                    "job_id": req.job_id,
                    "language": req.language,
                    "count": req.count,
                    "source_ids": req.source_ids,
                    "existing_prompts": req.existing_prompts,
                }))
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            resp.json::<Vec<GeneratedQuestion>>()
                .await
                .map_err(|_| Error::unavailable())
        })
    }
    fn correct_language<'a>(
        &'a self,
        owner_id: &'a str,
        language: &'a str,
        questions: &'a [GeneratedQuestion],
    ) -> BoxFuture<'a, Result<Vec<GeneratedQuestion>>> {
        Box::pin(async move {
            let resp = self
                .http
                .post(format!("{}/jobs/language_fix", self.base_url))
                .header("x-uuid", owner_id)
                .json(&json!({
                    "language": language,
                    "questions": questions,
                }))
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            resp.json::<Vec<GeneratedQuestion>>()
                .await
                .map_err(|_| Error::unavailable())
        })
    }
    fn check_request_outcome<'a>(
        &'a self,
        cortex_job_id: &'a str,
    ) -> BoxFuture<'a, Result<CortexJobOutcome>> {
        Box::pin(async move {
            let resp = self
                .http
                .get(format!("{}/jobs/{}", self.base_url, cortex_job_id))
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if resp.status() == axum::http::StatusCode::NOT_FOUND {
                return Ok(CortexJobOutcome::NotFound);
            }
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            let body: serde_json::Value = resp.json().await.map_err(|_| Error::unavailable())?;
            match body["status"].as_str() {
                Some("completed") => Ok(CortexJobOutcome::Completed(
                    body["output_id"].as_str().unwrap_or_default().to_string(),
                )),
                Some("queued" | "running") => Ok(CortexJobOutcome::Pending),
                _ => Ok(CortexJobOutcome::NotFound),
            }
        })
    }
}
