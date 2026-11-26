//! Reports service adapter using real Reports viewer routes.
use crate::core::{Error, Result};
use crate::dal::fsm::sha256_hex;
use axum::http::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Serialize)]
pub struct ReportPickerItem {
    pub project: String,
    pub report: String,
    pub title: String,
    pub is_restricted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReadableReportDataset {
    pub project: String,
    pub report: String,
    /// The unchanged caller-readable `DatasetDescriptor` from Reports.
    pub dataset: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateRenderJobInput {
    pub template_id: String,
    pub template_version: i64,
    pub mapping: BTreeMap<String, Vec<String>>,
    pub config: Value,
    pub rows: Vec<Value>,
    pub width: i64,
    pub height: i64,
    pub scale: i64,
    pub ttl_seconds: Option<i64>,
    pub originating_scope: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderJobDescriptor {
    pub job_id: String,
    pub template_id: String,
    pub template_version: i64,
    pub mapping: BTreeMap<String, Vec<String>>,
    pub config: Value,
    pub rows: Vec<Value>,
    pub width: i64,
    pub height: i64,
    pub scale: i64,
    pub expires_at: i64,
}

#[derive(Clone, Debug)]
pub struct RenderedVisual {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

/// Canonical rows from one bounded Reports dataset response. Dataset metadata is
/// intentionally excluded from `bytes`; only the published row values are source
/// material for generation.
#[derive(Clone, Debug)]
pub struct ReportDatasetMaterial {
    pub bytes: Vec<u8>,
    pub is_restricted: bool,
    pub row_count: i64,
    pub lineage: ReportLineage,
    pub lineage_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReportInputRef {
    pub schema: String,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportWatermark {
    pub records: i64,
    pub last_received_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReportLineage {
    pub calculation_id: String,
    pub calculation_version: i64,
    pub inputs: Vec<ReportInputRef>,
    pub source_watermark: BTreeMap<String, ReportWatermark>,
    pub filters: Value,
    pub calculated_at: String,
}

impl ReportLineage {
    pub fn same_source_version(&self, other: &Self) -> bool {
        let mut self_inputs = self.inputs.clone();
        let mut other_inputs = other.inputs.clone();
        self_inputs.sort();
        other_inputs.sort();
        self.calculation_id == other.calculation_id
            && self.calculation_version == other.calculation_version
            && self_inputs == other_inputs
            && self.source_watermark == other.source_watermark
            && self.filters == other.filters
    }

    fn identity_sha256(&self) -> Result<String> {
        let mut inputs = self.inputs.clone();
        inputs.sort();
        let identity = serde_json::json!({
            "calculation_id": &self.calculation_id,
            "calculation_version": self.calculation_version,
            "inputs": inputs,
            "source_watermark": &self.source_watermark,
            "filters": &self.filters,
        });
        let bytes =
            serde_json::to_vec(&sort_json_keys(identity)).map_err(|_| Error::unavailable())?;
        Ok(sha256_hex(&bytes))
    }
}

#[derive(Deserialize)]
struct DatasetPageBody {
    rows: Vec<Value>,
    offset: usize,
    total_rows: usize,
    visibility: String,
    applied_filters: Value,
    lineage: ReportLineage,
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

    fn list_readable_datasets<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReadableReportDataset>>>;

    fn fetch_report_dataset<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
        dataset: &'a str,
    ) -> BoxFuture<'a, Result<ReportDatasetMaterial>>;

    fn render_question_visual<'a>(
        &'a self,
        _user_id: &'a str,
        _prompt: &'a str,
        _visual_spec: &'a Value,
    ) -> BoxFuture<'a, Result<RenderedVisual>> {
        Box::pin(async { Err(Error::unavailable()) })
    }

    fn create_render_job<'a>(
        &'a self,
        _user_id: &'a str,
        _input: &'a CreateRenderJobInput,
    ) -> BoxFuture<'a, Result<RenderJobDescriptor>> {
        Box::pin(async { Err(Error::unavailable()) })
    }

    fn delete_render_job<'a>(
        &'a self,
        _user_id: &'a str,
        _job_id: &'a str,
        _confirmed: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
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
    fn list_readable_datasets<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReadableReportDataset>>> {
        Box::pin(async { Err(Error::unavailable()) })
    }
    fn fetch_report_dataset<'a>(
        &'a self,
        _user_id: &'a str,
        _project: &'a str,
        _report: &'a str,
        _dataset: &'a str,
    ) -> BoxFuture<'a, Result<ReportDatasetMaterial>> {
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
    service_key: String,
    http: reqwest::Client,
}

impl HttpReports {
    const MAX_REPORT_VIEW_BYTES: usize = 1024 * 1024;
    const MAX_DATASET_ROWS: usize = 5000;
    const DATASET_PAGE_ROWS: usize = 5000;
    const MAX_DATASET_BYTES: usize = 25 * 1024 * 1024;
    pub fn client(base_url: &str, service_key: &str, http: reqwest::Client) -> Arc<dyn ReportsClient> {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Arc::new(DisabledReports);
        }
        Arc::new(Self {
            base_url: base.to_string(),
            service_key: service_key.trim().to_string(),
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

    async fn fetch_dataset_page(
        &self,
        user_id: &str,
        project: &str,
        report: &str,
        dataset: &str,
        offset: usize,
    ) -> Result<DatasetPageBody> {
        // Build each path segment separately so a caller-controlled identifier
        // cannot introduce a new path or query parameter.
        let mut url = reqwest::Url::parse(&self.base_url).map_err(|_| Error::unavailable())?;
        url.path_segments_mut()
            .map_err(|_| Error::unavailable())?
            .pop_if_empty()
            .extend(["projects", project, "reports", report, "datasets", dataset]);
        url.query_pairs_mut()
            .append_pair("lang", "en")
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", &Self::DATASET_PAGE_ROWS.to_string());

        let mut response = self
            .http
            .get(url)
            .header("x-uuid", user_id)
            .send()
            .await
            .map_err(|_| Error::unavailable())?;
        if response.status() == StatusCode::FORBIDDEN
            || response.status() == StatusCode::UNAUTHORIZED
        {
            return Err(Error::forbidden(
                "report_access_denied: Caller lacks read permission for this dataset",
            ));
        }
        if response.status() == StatusCode::NOT_FOUND {
            return Err(Error::missing());
        }
        if !response.status().is_success() {
            return Err(Error::unavailable());
        }
        if response
            .content_length()
            .is_some_and(|length| length > Self::MAX_DATASET_BYTES as u64)
        {
            return Err(Error::unprocessable(
                "report_source_too_large: Dataset exceeds the supported source size",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
            if body.len().saturating_add(chunk.len()) > Self::MAX_DATASET_BYTES {
                return Err(Error::unprocessable(
                    "report_source_too_large: Dataset exceeds the supported source size",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let page: DatasetPageBody =
            serde_json::from_slice(&body).map_err(|_| Error::unavailable())?;
        if page.offset != offset || page.rows.len() > Self::DATASET_PAGE_ROWS {
            return Err(Error::unavailable());
        }
        Ok(page)
    }

    async fn fetch_readable_datasets(
        &self,
        user_id: &str,
        project: &str,
        report: &str,
    ) -> Result<Vec<ReadableReportDataset>> {
        let mut url = reqwest::Url::parse(&self.base_url).map_err(|_| Error::unavailable())?;
        url.path_segments_mut()
            .map_err(|_| Error::unavailable())?
            .pop_if_empty()
            .extend(["projects", project, "reports", report]);
        url.query_pairs_mut().append_pair("lang", "en");
        let mut response = self
            .http
            .get(url)
            .header("x-uuid", user_id)
            .send()
            .await
            .map_err(|_| Error::unavailable())?;
        if response.status() == StatusCode::FORBIDDEN
            || response.status() == StatusCode::UNAUTHORIZED
        {
            return Err(Error::forbidden(
                "report_access_denied: Caller lacks read permission for this report",
            ));
        }
        if response.status() == StatusCode::NOT_FOUND {
            return Err(Error::missing());
        }
        if !response.status().is_success() {
            return Err(Error::unavailable());
        }
        if response
            .content_length()
            .is_some_and(|length| length > Self::MAX_REPORT_VIEW_BYTES as u64)
        {
            return Err(Error::unavailable());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::unavailable())? {
            if bytes.len().saturating_add(chunk.len()) > Self::MAX_REPORT_VIEW_BYTES {
                return Err(Error::unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| Error::unavailable())?;
        let datasets = body["datasets"].as_array().ok_or_else(Error::unavailable)?;
        Ok(datasets
            .iter()
            .filter(|dataset| dataset["can_view"].as_bool() == Some(true))
            .filter(|dataset| dataset["dataset_id"].as_str().is_some())
            .cloned()
            .map(|dataset| ReadableReportDataset {
                project: project.to_string(),
                report: report.to_string(),
                dataset,
            })
            .collect())
    }

    async fn fetch_dataset(
        &self,
        user_id: &str,
        project: &str,
        report: &str,
        dataset: &str,
    ) -> Result<ReportDatasetMaterial> {
        let mut rows = Vec::new();
        let mut offset = 0usize;
        let mut total_rows = None;
        let mut baseline: Option<DatasetPageBody> = None;
        loop {
            let page = self
                .fetch_dataset_page(user_id, project, report, dataset, offset)
                .await?;
            if page.total_rows > Self::MAX_DATASET_ROWS {
                return Err(Error::unprocessable(
                    "report_source_too_large: Dataset exceeds the supported 5000 row source limit",
                ));
            }
            let first = baseline.get_or_insert_with(|| DatasetPageBody {
                rows: Vec::new(),
                offset: page.offset,
                total_rows: page.total_rows,
                visibility: page.visibility.clone(),
                applied_filters: page.applied_filters.clone(),
                lineage: page.lineage.clone(),
            });
            let valid_lineage = !page.lineage.calculation_id.trim().is_empty()
                && page.lineage.calculation_version > 0
                && !page.lineage.calculated_at.trim().is_empty()
                && page
                    .lineage
                    .inputs
                    .iter()
                    .all(|input| !input.schema.trim().is_empty() && input.version > 0)
                && page
                    .lineage
                    .source_watermark
                    .values()
                    .all(|watermark| watermark.records >= 0);
            if total_rows.is_some_and(|expected| expected != page.total_rows)
                || page.total_rows < page.offset.saturating_add(page.rows.len())
                || !valid_lineage
                || !page.lineage.same_source_version(&first.lineage)
                || page.visibility != first.visibility
                || !is_empty_filter(&page.applied_filters)
                || !is_empty_filter(&page.lineage.filters)
            {
                return Err(Error::conflict(
                    "report_snapshot_changed: Dataset paging returned inconsistent snapshot metadata",
                ));
            }
            total_rows = Some(page.total_rows);
            let page_len = page.rows.len();
            rows.extend(page.rows.into_iter().map(sort_json_keys));
            offset = offset.saturating_add(page_len);
            if offset == page.total_rows {
                break;
            }
            if page_len == 0 || offset > page.total_rows {
                return Err(Error::conflict(
                    "report_snapshot_changed: Dataset paging did not cover a stable row set",
                ));
            }
        }
        let first = baseline.expect("a dataset request always fetches at least one page");
        let row_count = total_rows.unwrap_or_default();
        if rows.len() != row_count {
            return Err(Error::conflict(
                "report_snapshot_changed: Dataset paging did not return all rows",
            ));
        }
        let bytes = serde_json::to_vec(&rows).map_err(|_| Error::unavailable())?;
        if bytes.len() > Self::MAX_DATASET_BYTES {
            return Err(Error::unprocessable(
                "report_source_too_large: Dataset exceeds the supported source size",
            ));
        }
        Ok(ReportDatasetMaterial {
            bytes,
            is_restricted: first.visibility != "public",
            row_count: i64::try_from(row_count).map_err(|_| {
                Error::unprocessable("report_source_too_large: Dataset row count is invalid")
            })?,
            lineage_sha256: first.lineage.identity_sha256()?,
            lineage: first.lineage,
        })
    }
}

fn is_empty_filter(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json_keys).collect()),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, sort_json_keys(value));
            }
            Value::Object(sorted)
        }
        scalar => scalar,
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

    fn list_readable_datasets<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReadableReportDataset>>> {
        Box::pin(async move { self.fetch_readable_datasets(user_id, project, report).await })
    }

    fn fetch_report_dataset<'a>(
        &'a self,
        user_id: &'a str,
        project: &'a str,
        report: &'a str,
        dataset: &'a str,
    ) -> BoxFuture<'a, Result<ReportDatasetMaterial>> {
        Box::pin(async move { self.fetch_dataset(user_id, project, report, dataset).await })
    }

    fn create_render_job<'a>(
        &'a self,
        user_id: &'a str,
        input: &'a CreateRenderJobInput,
    ) -> BoxFuture<'a, Result<RenderJobDescriptor>> {
        Box::pin(async move {
            let mut req = self
                .http
                .post(format!("{}/render_jobs", self.base_url))
                .header("x-uuid", user_id);
            if !self.service_key.is_empty() {
                req = req.header("x-reports-service-key", &self.service_key);
            }
            let resp = req
                .json(input)
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() {
                return Err(Error::unavailable());
            }
            resp.json::<RenderJobDescriptor>()
                .await
                .map_err(|_| Error::unavailable())
        })
    }

    fn delete_render_job<'a>(
        &'a self,
        user_id: &'a str,
        job_id: &'a str,
        confirmed: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut req = self
                .http
                .delete(format!(
                    "{}/render_jobs/{}?confirmed={}",
                    self.base_url, job_id, confirmed
                ))
                .header("x-uuid", user_id);
            if !self.service_key.is_empty() {
                req = req.header("x-reports-service-key", &self.service_key);
            }
            let resp = req
                .send()
                .await
                .map_err(|_| Error::unavailable())?;
            if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
                return Err(Error::unavailable());
            }
            Ok(())
        })
    }
}
