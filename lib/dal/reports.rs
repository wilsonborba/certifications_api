//! Reports service adapter using real Reports viewer routes.
use crate::core::{Error, Result};
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Serialize)]
pub struct ReportPickerItem {
    pub project: String,
    pub report: String,
    pub title: String,
    pub is_restricted: bool,
}

#[derive(Clone, Debug)]
pub struct RenderedVisual {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

pub trait ReportsClient: Send + Sync {
    fn list_readable_reports<'a>(
        &'a self,
        user_id: &'a str,
        project: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<ReportPickerItem>>>;

    fn check_report_access<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, Result<ReportPickerItem>>;

    fn render_question_visual<'a>(
        &'a self,
        user_id: &'a str,
        prompt: &'a str,
        visual_spec: &'a Value,
    ) -> BoxFuture<'a, Result<RenderedVisual>>;
}

pub struct DisabledReports;

impl ReportsClient for DisabledReports {
    fn list_readable_reports<'a>(
        &'a self,
        _user_id: &'a str,
        _project: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<ReportPickerItem>>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
    fn check_report_access<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
    ) -> BoxFuture<'a, Result<ReportPickerItem>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
    fn render_question_visual<'a>(
        &'a self,
        _user_id: &'a str,
        _prompt: &'a str,
        _visual_spec: &'a Value,
    ) -> BoxFuture<'a, Result<RenderedVisual>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
}

pub struct HttpReports {
    base_url: String,
    http: reqwest::Client,
}

impl HttpReports {
    pub fn client(base_url: &str, http: reqwest::Client) -> Arc<dyn ReportsClient> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Arc::new(DisabledReports);
        }
        Arc::new(Self {
            base_url: base.to_string(),
            http,
        })
    }

    async fn fetch_project_reports(
        &self,
        user_id: &str,
        project: &str,
    ) -> Result<Vec<ReportPickerItem>> {
        let resp = self
            .http
            .get(format!("{}/me/projects/{project}/reports", self.base_url))
            .header("x-uuid", user_id)
            .send()
            .await
            .map_err(|_| Error::unavailable())?;
        if resp.status() == StatusCode::FORBIDDEN || resp.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::forbidden(
                "report_access_denied: Caller lacks read permission for this project",
            ));
        }
        if !resp.status().is_success() {
            return Err(Error::unavailable());
        }
        let body: Value = resp.json().await.map_err(|_| Error::unavailable())?;
        let arr = body["items"]
            .as_array()
            .or_else(|| body.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .into_iter()
            .filter_map(|item| {
                let slug = item["slug"]
                    .as_str()
                    .or_else(|| item["report"].as_str())?
                    .to_string();
                let title = item["title"].as_str().unwrap_or(&slug).to_string();
                let visibility = item["visibility"].as_str().unwrap_or("restricted");
                Some(ReportPickerItem {
                    project: project.to_string(),
                    report: slug,
                    title,
                    is_restricted: visibility != "public",
                })
            })
            .collect())
    }
}

impl ReportsClient for HttpReports {
    fn list_readable_reports<'a>(
        &'a self,
        user_id: &'a str,
        project: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Vec<ReportPickerItem>>> {
        Box::pin(async move {
            if let Some(proj) = project.filter(|p| !p.trim().is_empty()) {
                return self.fetch_project_reports(user_id, proj.trim()).await;
            }
            let resp = self
                .http
                .get(format!("{}/projects", self.base_url))
                .header("x-uuid", user_id)
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            let body: Value = resp.json().await.map_err(|_| Error::unavailable())?;
            let projects = body["items"]
                .as_array()
                .or_else(|| body.as_array())
                .cloned()
                .unwrap_or_default();
            let mut out = Vec::new();
            for proj in projects.into_iter().take(10) {
                if let Some(slug) = proj["slug"].as_str().or_else(|| proj["id"].as_str())
                    && let Ok(mut items) = self.fetch_project_reports(user_id, slug).await
                {
                    out.append(&mut items);
                }
            }
            Ok(out)
        })
    }

    fn check_report_access<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, Result<ReportPickerItem>> {
        Box::pin(async move {
            let resp = self
                .http
                .get(format!(
                    "{}/projects/{project}/reports/{report}",
                    self.base_url
                ))
                .header("x-uuid", user_id)
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if resp.status() == StatusCode::UNAUTHORIZED || resp.status() == StatusCode::FORBIDDEN {
                return Err(Error::forbidden(
                    "report_access_denied: Caller lacks read permission for this report",
                ));
            }
            if resp.status() == StatusCode::NOT_FOUND {
                return Err(Error::missing());
            }
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            let body: Value = resp.json().await.map_err(|_| Error::unavailable())?;
            let title = body["title"].as_str().unwrap_or(report).to_string();
            let visibility = body["visibility"].as_str().unwrap_or("restricted");
            Ok(ReportPickerItem {
                project: project.to_string(),
                report: report.to_string(),
                title,
                is_restricted: visibility != "public",
            })
        })
    }

    fn render_question_visual<'a>(
        &'a self,
        user_id: &'a str,
        prompt: &'a str,
        visual_spec: &'a Value,
    ) -> BoxFuture<'a, Result<RenderedVisual>> {
        Box::pin(async move {
            let resp = self
                .http
                .post(format!("{}/jobs/temporary-render", self.base_url))
                .header("x-uuid", user_id)
                .json(&serde_json::json!({
                    "kind": "question_visual",
                    "prompt": prompt,
                    "spec": visual_spec,
                }))
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            let mime_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/png")
                .to_string();
            let bytes = resp
                .bytes()
                .await
                .map_err(|_| Error::unavailable())?
                .to_vec();
            Ok(RenderedVisual { bytes, mime_type })
        })
    }
}
