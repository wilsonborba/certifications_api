use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::{Store, fsm::MemoryFsm},
    domain::{
        App,
        quotas::{QuotaCache, QuotaConfig},
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn test_settings() -> Settings {
    Settings {
        host: "127.0.0.1".into(),
        port: 0,
        development: false,
        database_url: String::new(),
        redis_url: String::new(),
        redis_namespace: "certifications_api:".into(),
        couch_url: String::new(),
        couch_database: "certifications".into(),
        couch_user: String::new(),
        couch_password: String::new(),
        fsm_url: String::new(),
        fsm_key: String::new(),
        cortex_url: String::new(),
        reports_url: String::new(),
        gateway_url: String::new(),
        certifications_service_key: String::new(),
        clamav: Default::default(),
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn quota_enforces_limits_and_releases_failed_uploads() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();

    let quotas = QuotaCache::default();
    quotas
        .update_in_db(
            &store,
            QuotaConfig {
                max_file_bytes: 70,
                max_study_bytes: 80,
                max_account_bytes: 80,
            },
            "admin-1",
        )
        .await
        .unwrap();
    let refreshed = QuotaCache::default();
    let loaded = refreshed.refresh_from_db(&store).await.unwrap();
    assert_eq!(loaded.max_file_bytes, 70);

    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: refreshed,
        signatures: Default::default(),
    });

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Quota Study"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap();

    // Failed upload (EICAR) is rejected (422) and releases its byte reservation.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "bad.txt",
            "mime_type": "text/plain",
            "content_text": "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Valid upload of 45 bytes succeeds.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "notes.txt",
            "mime_type": "text/plain",
            "content_text": "123456789012345678901234567890123456789012345"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // Second upload of 40 bytes exceeds 80-byte account/study quota (413).
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "filename": "notes2.txt",
            "mime_type": "text/plain",
            "content_text": "1234567890123456789012345678901234567890"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);

    let (st, detail) = call(&app, "GET", &format!("/studies/{sid}"), Some("u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["quota"]["used_bytes"], 45);
    assert_eq!(detail["quota"]["reserved_bytes"], 0);
    assert_eq!(detail["quota"]["remaining_account_bytes"], 35);
}

async fn call_with_service_key(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    service_key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(k) = service_key {
        req = req.header("x-service-key", k);
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn selections_survive_refresh_and_prepared_studies_reuse_fsm_without_upload() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    fsm.seed("annotations/u1/file-1", b"Cell respiration uses ATP.");
    store.fsm = fsm.clone() as Arc<dyn certifications_api::dal::fsm::FsmClient>;

    let mut settings = test_settings();
    settings.certifications_service_key = "ann-svc-key".into();
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    // Prepared study from Annotations arrives filled and reuses stored FSM refs without putting bytes.
    let (st, prepared) = call_with_service_key(
        &app,
        "POST",
        "/studies/prepared",
        Some("u1"),
        Some("ann-svc-key"),
        Some(json!({
            "title": "Biology Prep",
            "sources": [{
                "filename": "bio.txt",
                "mime_type": "text/plain",
                "size_bytes": 26,
                "fsm_ref": "annotations/u1/file-1",
                "selection_kind": "passages",
                "selection_spec": "p1:0-25"
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(fsm.put_count(), 0);
    let sid = prepared["study"]["id"].as_str().unwrap();
    let src_id = prepared["sources"][0]["id"].as_str().unwrap();

    // Cross-user access fails with 403.
    let (st, _) = call(&app, "GET", &format!("/studies/{sid}"), Some("u2"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Update text passages and verify the selection survives refresh.
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/studies/{sid}/sources/{src_id}/selection"),
        Some("u1"),
        Some(json!({"kind": "passages", "spec": "p1:0-10,p1:15-25"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, refreshed) = call(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{src_id}"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(refreshed["selection_kind"], "passages");
    assert_eq!(refreshed["selection_spec"], "p1:0-10,p1:15-25");

    // Analysis summary reflects sources.
    let (st, analysis) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/analysis"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(analysis["total_sources"], 1);
    assert_eq!(analysis["ready_sources"], 1);

    // Expired abandoned draft returns 410 Gone.
    sqlx::query("UPDATE studies SET expires_at = 1 WHERE id = $1")
        .bind(sid)
        .execute(&store.db)
        .await
        .unwrap();
    let (st, _) = call(&app, "GET", &format!("/studies/{sid}"), Some("u1"), None).await;
    assert_eq!(st, StatusCode::GONE);
}

#[tokio::test]
async fn reports_picker_and_report_source_enforce_permissions_and_unconfigured_503() {
    use certifications_api::{
        core::{Error as CoreError, Result as CoreResult},
        dal::reports::{
            BoxFuture, ReadableReportDataset, ReportDatasetMaterial, ReportInputRef, ReportLineage,
            ReportPickerItem, ReportWatermark, ReportsClient,
        },
    };

    struct MockReports;
    impl ReportsClient for MockReports {
        fn list_readable_reports<'a>(
            &'a self,
            user_id: &'a str,
            _project: Option<&'a str>,
        ) -> BoxFuture<'a, CoreResult<Vec<ReportPickerItem>>> {
            Box::pin(async move {
                if user_id == "u1" {
                    Ok(vec![ReportPickerItem {
                        project: "proj1".into(),
                        report: "rep1".into(),
                        title: "Q4 Metrics".into(),
                        is_restricted: true,
                    }])
                } else {
                    Ok(vec![])
                }
            })
        }
        fn check_report_access<'a>(
            &'a self,
            user_id: &'a str,
            project: &'a str,
            report: &'a str,
        ) -> BoxFuture<'a, CoreResult<ReportPickerItem>> {
            Box::pin(async move {
                if user_id == "u1" && project == "proj1" && report == "rep1" {
                    Ok(ReportPickerItem {
                        project: "proj1".into(),
                        report: "rep1".into(),
                        title: "Q4 Metrics".into(),
                        is_restricted: true,
                    })
                } else {
                    Err(CoreError::forbidden(
                        "report_access_denied: Caller lacks read permission for this report",
                    ))
                }
            })
        }
        fn list_readable_datasets<'a>(
            &'a self,
            user_id: &'a str,
            project: &'a str,
            report: &'a str,
        ) -> BoxFuture<'a, CoreResult<Vec<ReadableReportDataset>>> {
            Box::pin(async move {
                if user_id == "u1" && project == "proj1" && report == "rep1" {
                    Ok(vec![ReadableReportDataset {
                        project: project.into(),
                        report: report.into(),
                        dataset: json!({
                            "dataset_id": "data1",
                            "title": "Counts",
                            "description": "",
                            "equation": null,
                            "data_tier": "public",
                            "fields": [],
                            "filters": [],
                            "can_view": true,
                            "visibility": "public",
                            "meta": null
                        }),
                    }])
                } else if user_id == "u1" && report == "missing" {
                    Err(CoreError::missing())
                } else if user_id == "u2" {
                    Ok(Vec::new())
                } else {
                    Err(CoreError::forbidden(
                        "report_access_denied: Caller lacks read permission for this report",
                    ))
                }
            })
        }
        fn fetch_report_dataset<'a>(
            &'a self,
            user_id: &'a str,
            project: &'a str,
            report: &'a str,
            dataset: &'a str,
        ) -> BoxFuture<'a, CoreResult<ReportDatasetMaterial>> {
            Box::pin(async move {
                if user_id == "u1" && project == "proj1" && report == "rep1" && dataset == "data1" {
                    Ok(ReportDatasetMaterial {
                        bytes: br#"[{"count":2,"name":"one"}]"#.to_vec(),
                        is_restricted: true,
                        row_count: 1,
                        lineage_sha256: "b".repeat(64),
                        lineage: ReportLineage {
                            calculation_id: "calc1".into(),
                            calculation_version: 1,
                            inputs: vec![ReportInputRef {
                                schema: "items".into(),
                                version: 1,
                            }],
                            source_watermark: [(
                                "items".into(),
                                ReportWatermark {
                                    records: 2,
                                    last_received_at: None,
                                },
                            )]
                            .into(),
                            filters: json!({}),
                            calculated_at: "2026-11-24T00:00:00Z".into(),
                        },
                    })
                } else {
                    Err(CoreError::forbidden(
                        "report_access_denied: Caller lacks read permission for this dataset",
                    ))
                }
            })
        }
        fn render_question_visual<'a>(
            &'a self,
            _user_id: &'a str,
            _prompt: &'a str,
            _visual_spec: &'a Value,
        ) -> BoxFuture<'a, CoreResult<certifications_api::dal::reports::RenderedVisual>> {
            Box::pin(async { Err(CoreError::unavailable()) })
        }
    }

    // Unconfigured Reports returns 503 instead of a stubbed empty list.
    let disabled_store = Store::fixture().await;
    let disabled_app = presentation::router(App {
        store: disabled_store,
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });
    let (st, _) = call(&disabled_app, "GET", "/reports/picker", Some("u1"), None).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);

    // Configured Reports returns readable reports and enforces read permission on source attach.
    let mut store = Store::fixture().await;
    store.reports = Arc::new(MockReports);
    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    let (st, picker) = call(&app, "GET", "/reports/picker", Some("u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(picker["items"].as_array().unwrap().len(), 1);
    assert_eq!(picker["items"][0]["report"], "rep1");
    assert!(picker["items"][0]["dataset"].is_null());

    // Existing picker can fetch caller-readable dataset descriptors for one explicit report.
    let (st, datasets) = call(
        &app,
        "GET",
        "/reports/picker?project=proj1&report=rep1",
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(datasets["items"].as_array().unwrap().len(), 1);
    assert_eq!(datasets["items"][0]["dataset"]["dataset_id"], "data1");
    assert_eq!(datasets["items"][0]["project"], "proj1");
    assert_eq!(datasets["items"][0]["report"], "rep1");

    let (st, no_datasets) = call(
        &app,
        "GET",
        "/reports/picker?project=proj1&report=rep1",
        Some("u2"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(no_datasets["items"].as_array().unwrap().len(), 0);

    let (st, _) = call(
        &app,
        "GET",
        "/reports/picker?project=proj1&report=missing",
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &app,
        "GET",
        "/reports/picker?project=proj1&report=rep1",
        Some("u3"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&app, "GET", "/reports/picker?report=rep1", Some("u1"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "GET",
        "/reports/picker?project=proj1&report=../rep1",
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (_, s1) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "U1 Study"})),
    )
    .await;
    let sid1 = s1["id"].as_str().unwrap();
    let (st, src) = call(
        &app,
        "POST",
        &format!("/studies/{sid1}/sources"),
        Some("u1"),
        Some(json!({
            "kind": "report",
            "filename": "Q4 Metrics",
            "external_ref": "proj1/rep1/data1",
            "is_restricted": false
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(src["source_kind"], "report");
    assert_eq!(src["selection_kind"], "all");
    assert_eq!(src["selection_spec"], "");
    assert_eq!(src["is_restricted"], true);
    assert_eq!(
        src["size_bytes"],
        br#"[{"count":2,"name":"one"}]"#.len() as i64
    );
    assert!(!src["sha256"].as_str().unwrap().is_empty());

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid1}/sources"),
        Some("u1"),
        Some(json!({
            "kind": "report",
            "filename": "Invalid",
            "external_ref": "proj1/../data1"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid1}/sources"),
        Some("u1"),
        Some(json!({
            "kind": "report",
            "filename": "Invalid",
            "external_ref": "proj1/rep1/data1",
            "fsm_ref": "foreign/blob"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, s2) = call(
        &app,
        "POST",
        "/studies",
        Some("u2"),
        Some(json!({"title": "U2 Study"})),
    )
    .await;
    let sid2 = s2["id"].as_str().unwrap();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{sid2}/sources"),
        Some("u2"),
        Some(json!({
            "kind": "report",
            "filename": "Q4 Metrics",
            "external_ref": "proj1/rep1/data1"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

fn multipart_body(boundary: &str, filename: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(data);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

fn minimal_wav(duration_secs: u32) -> Vec<u8> {
    let sample_rate: u32 = 8000;
    let channels: u16 = 1;
    let bits_per_sample: u16 = 8;
    let byte_rate: u32 = sample_rate * (channels as u32) * (bits_per_sample as u32 / 8);
    let data_len: u32 = byte_rate * duration_secs;
    let riff_len: u32 = 36 + data_len;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // block align
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0x80);
    wav
}

async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    content_type: Option<&str>,
    range: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(ct) = content_type {
        req = req.header("content-type", ct);
    }
    if let Some(r) = range {
        req = req.header("range", r);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn multipart_pdf_and_wav_uploads_stream_with_range_preview_metadata_and_quota() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();
    let quotas = QuotaCache::default();
    quotas
        .update_in_db(
            &store,
            QuotaConfig {
                max_file_bytes: 32_768,
                max_study_bytes: 65_536,
                max_account_bytes: 131_072,
            },
            "admin-1",
        )
        .await
        .unwrap();
    let app = presentation::router(App {
        store: store.clone(),
        settings: test_settings(),
        quotas,
        signatures: Default::default(),
    });

    let (st, study) = call(
        &app,
        "POST",
        "/studies",
        Some("u1"),
        Some(json!({"title": "Binary Sources"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = study["id"].as_str().unwrap();

    // 1. Multipart PDF upload: valid 1-page PDF with `% /Type /Page` comment & `/Type /Page` in stream -> page_count == 1
    let pdf_bytes = b"%PDF-1.4\n% /Type /Page\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Count 1 /Kids[3 0 R]>>endobj\n3 0 obj<</Type /Page /Contents 4 0 R>>endobj\n4 0 obj<</Length 15>>stream\n/Type /Page\nendstream\nendobj\ntrailer<</Root 1 0 R>>\n%%EOF".to_vec();
    let boundary = "----asodya35boundary";
    let body = multipart_body(boundary, "slides.pdf", "application/pdf", &pdf_bytes);
    let (st, _, raw) = call_raw(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        body,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let pdf_src: Value = serde_json::from_slice(&raw).unwrap();
    let pdf_id = pdf_src["id"].as_str().unwrap();
    assert_eq!(pdf_src["mime_type"], "application/pdf");
    assert_eq!(pdf_src["size_bytes"], pdf_bytes.len() as i64);
    assert_eq!(fsm.put_count(), 1);

    // Preview returns cheap PDF page_count + content_url + supports_range
    let (st, preview) = call(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{pdf_id}/preview"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(preview["page_count"], 1);
    assert_eq!(preview["supports_range"], true);
    assert_eq!(
        preview["content_url"],
        format!("/studies/{sid}/sources/{pdf_id}/content")
    );
    assert!(preview["duration_seconds"].is_null());

    // Full content + Range 206 + invalid Range 416 + cross-user 403
    let (st, hdrs, bytes) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{pdf_id}/content"),
        Some("u1"),
        None,
        None,
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(hdrs.get("accept-ranges").unwrap(), "bytes");
    assert_eq!(hdrs.get("cache-control").unwrap(), "no-store");
    assert_eq!(hdrs.get("content-type").unwrap(), "application/pdf");
    assert_eq!(bytes, pdf_bytes);

    let (st, hdrs, slice) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{pdf_id}/content"),
        Some("u1"),
        None,
        Some("bytes=0-4"),
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(hdrs.get("cache-control").unwrap(), "no-store");
    assert_eq!(
        hdrs.get("content-range").unwrap().to_str().unwrap(),
        format!("bytes 0-4/{}", pdf_bytes.len())
    );
    assert_eq!(slice, b"%PDF-");

    let (st, hdrs, _) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{pdf_id}/content"),
        Some("u1"),
        None,
        Some("bytes=99999-100000"),
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(hdrs.get("cache-control").unwrap(), "no-store");

    let (st, _, _) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{pdf_id}/content"),
        Some("u2"),
        None,
        None,
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 2. Multipart WAV audio upload + duration_seconds metadata + interval selection
    let wav_bytes = minimal_wav(2);
    let wav_body = multipart_body(boundary, "lecture.wav", "audio/wav", &wav_bytes);
    let (st, _, raw) = call_raw(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        wav_body,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let wav_src: Value = serde_json::from_slice(&raw).unwrap();
    let wav_id = wav_src["id"].as_str().unwrap();

    let (st, wav_preview) = call(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{wav_id}/preview"),
        Some("u1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!((wav_preview["duration_seconds"].as_f64().unwrap() - 2.0).abs() < 0.01);
    assert!(wav_preview["page_count"].is_null());

    // 3. Invalid magic bytes rejected without consuming FSM or quota
    let bad_pdf = multipart_body(boundary, "evil.pdf", "application/pdf", b"not-a-pdf-header");
    let (st, _, _) = call_raw(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        bad_pdf,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fsm.put_count(), 2);

    let (st, detail) = call(&app, "GET", &format!("/studies/{sid}"), Some("u1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        detail["quota"]["used_bytes"],
        (pdf_bytes.len() + wav_bytes.len()) as i64
    );
    assert_eq!(detail["quota"]["reserved_bytes"], 0);
}

#[tokio::test]
async fn prepared_study_fsm_ref_streams_content_and_link_returns_404() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    let pdf_bytes = b"%PDF-1.4\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Count 1 /Kids[3 0 R]>>endobj\n3 0 obj<</Type /Page>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF".to_vec();
    fsm.seed("shared/ann-1/doc.pdf", &pdf_bytes);
    store.fsm = fsm.clone();
    let mut settings = test_settings();
    settings.certifications_service_key = "ann-svc-key".into();
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    let (st, study) = call_with_service_key(
        &app,
        "POST",
        "/studies/prepared",
        Some("u1"),
        Some("ann-svc-key"),
        Some(json!({
            "title": "Prepared Study",
            "sources": [
                {
                    "filename": "doc.pdf",
                    "mime_type": "application/pdf",
                    "fsm_ref": "shared/ann-1/doc.pdf",
                    "size_bytes": pdf_bytes.len(),
                    "selection_kind": "pages",
                    "selection_spec": "1"
                }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(fsm.put_count(), 0);
    let sid = study["study"]["id"].as_str().unwrap();
    let fsm_src_id = study["sources"][0]["id"].as_str().unwrap();

    let (st, link_src) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/sources"),
        Some("u1"),
        Some(json!({
            "kind": "link",
            "filename": "notes.html",
            "mime_type": "text/html",
            "external_ref": "https://example.org/notes"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let link_src_id = link_src["id"].as_str().unwrap();

    let (st, _, bytes) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{fsm_src_id}/content"),
        Some("u1"),
        None,
        Some("bytes=0-4"),
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, b"%PDF-");

    let (st, _, _) = call_raw(
        &app,
        "GET",
        &format!("/studies/{sid}/sources/{link_src_id}/content"),
        Some("u1"),
        None,
        None,
        vec![],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cross_owner_fsm_ref_rejected_with_zero_fsm_reads_and_bounds_enforced() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();
    let quotas = QuotaCache::default();
    quotas
        .update_in_db(
            &store,
            QuotaConfig {
                max_file_bytes: 1024,
                max_study_bytes: 4096,
                max_account_bytes: 8192,
            },
            "admin-1",
        )
        .await
        .unwrap();
    let mut settings = test_settings();
    settings.certifications_service_key = "ann-svc-key".into();
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas,
        signatures: Default::default(),
    });

    // Owner B uploads a private PDF.
    let (_, b_study) = call(
        &app,
        "POST",
        "/studies",
        Some("owner-b"),
        Some(json!({"title": "B Private"})),
    )
    .await;
    let b_sid = b_study["id"].as_str().unwrap();
    let pdf_b = b"%PDF-1.4\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Count 1>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
    let boundary = "----secboundary35";
    let (st, _, raw_b) = call_raw(
        &app,
        "POST",
        &format!("/studies/{b_sid}/sources"),
        Some("owner-b"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        multipart_body(boundary, "secret.pdf", "application/pdf", pdf_b),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let b_src: Value = serde_json::from_slice(&raw_b).unwrap();
    let b_fsm_ref = b_src["fsm_ref"].as_str().unwrap();

    // Owner A creates a study and tries to reference Owner B's fsm_ref via POST /sources AND /studies/prepared.
    let (_, a_study) = call(
        &app,
        "POST",
        "/studies",
        Some("owner-a"),
        Some(json!({"title": "A Study"})),
    )
    .await;
    let a_sid = a_study["id"].as_str().unwrap();

    let gets_before = fsm.get_count();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(json!({
            "kind": "fsm_ref",
            "filename": "stolen.pdf",
            "mime_type": "application/pdf",
            "fsm_ref": b_fsm_ref,
            "size_bytes": 100
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(fsm.get_count(), gets_before);

    // Untrusted /studies/prepared without service key fails closed even for arbitrary fsm_ref.
    let (st, _) = call(
        &app,
        "POST",
        "/studies/prepared",
        Some("owner-a"),
        Some(json!({
            "title": "Fake Prep",
            "sources": [{
                "filename": "stolen.pdf",
                "mime_type": "application/pdf",
                "fsm_ref": b_fsm_ref,
                "size_bytes": 100
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(fsm.get_count(), gets_before);

    // Even with service key, referencing Owner B's owned blob for Owner A fails closed!
    let (st, _) = call_with_service_key(
        &app,
        "POST",
        "/studies/prepared",
        Some("owner-a"),
        Some("ann-svc-key"),
        Some(json!({
            "title": "Cross-owner Prep",
            "sources": [{
                "filename": "stolen.pdf",
                "mime_type": "application/pdf",
                "fsm_ref": b_fsm_ref,
                "size_bytes": 100
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(fsm.get_count(), gets_before);

    // Multipart duplicate file part, authority field, and oversized metadata fail without FSM writes or quota leaks.
    let puts_before = fsm.put_count();
    let dup_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhi\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"b.txt\"\r\nContent-Type: text/plain\r\n\r\nhi2\r\n--{boundary}--\r\n"
    );
    let (st, _, _) = call_raw(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        dup_body.into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let auth_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"owner_id\"\r\n\r\nowner-b\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhi\r\n--{boundary}--\r\n"
    );
    let (st, _, _) = call_raw(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        auth_body.into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let big_name = "x".repeat(600);
    let meta_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"filename\"\r\n\r\n{big_name}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhi\r\n--{boundary}--\r\n"
    );
    let (st, _, _) = call_raw(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        meta_body.into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Oversized multipart body (> max_file_bytes + 64 KiB) fails with 413.
    let too_big = vec![b'a'; 70_000];
    let (st, _, _) = call_raw(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        multipart_body(boundary, "big.txt", "text/plain", &too_big),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(fsm.put_count(), puts_before);

    let (_, detail_a) = call(
        &app,
        "GET",
        &format!("/studies/{a_sid}"),
        Some("owner-a"),
        None,
    )
    .await;
    assert_eq!(detail_a["quota"]["used_bytes"], 0);
    assert_eq!(detail_a["quota"]["reserved_bytes"], 0);

    // Encrypted/compressed PDF returns null page_count honestly instead of guessing.
    let enc_pdf = b"%PDF-1.4\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Count 5>>endobj\n3 0 obj<</Encrypt 4 0 R>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
    let (st, _, raw_enc) = call_raw(
        &app,
        "POST",
        &format!("/studies/{a_sid}/sources"),
        Some("owner-a"),
        Some(&format!("multipart/form-data; boundary={boundary}")),
        None,
        multipart_body(boundary, "enc.pdf", "application/pdf", enc_pdf),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let enc_src: Value = serde_json::from_slice(&raw_enc).unwrap();
    let enc_id = enc_src["id"].as_str().unwrap();
    let (st, enc_prev) = call(
        &app,
        "GET",
        &format!("/studies/{a_sid}/sources/{enc_id}/preview"),
        Some("owner-a"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(enc_prev["page_count"].is_null());

    // Historical/raw unproven fsm_ref row in a non-prepared study fails closed with zero FSM reads.
    let raw_src_id = "legacy-raw-src";
    let raw_fsm = "legacy/unproven/blob.pdf";
    certifications_api::dal::fsm::FsmClient::put_bytes(&*fsm, raw_fsm, b"%PDF-1.4\n%%EOF")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at)
         VALUES($1, 'owner-a', $2, 'legacy.pdf', 'application/pdf', 15, '', $3, 'fsm_ref', 'ready', 'all', '', 0, '', 1, 1)",
    )
    .bind(raw_src_id)
    .bind(a_sid)
    .bind(raw_fsm)
    .execute(&store.db)
    .await
    .unwrap();
    let gets_before_legacy = fsm.get_count();
    let (st, _) = call(
        &app,
        "GET",
        &format!("/studies/{a_sid}/sources/{raw_src_id}/content"),
        Some("owner-a"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(fsm.get_count(), gets_before_legacy);

    // Cross-reference /ObjStm PDF stays unknown (null page_count).
    let objstm_pdf = b"%PDF-1.5\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Count 7>>endobj\n3 0 obj<</Type /ObjStm /N 1 /First 5>>stream\nabc\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
    assert!(certifications_api::domain::studies::extract_pdf_page_count(objstm_pdf).is_none());
}

#[tokio::test]
async fn legacy_annotations_prepared_row_with_unknown_provenance_fails_closed_with_zero_fsm_reads()
{
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();
    let mut settings = test_settings();
    settings.certifications_service_key = "ann-svc-key".into();
    let app = presentation::router(App {
        store: store.clone(),
        settings,
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    });

    // Historical study with source_origin = 'annotations' and a raw fsm_ref row whose
    // source_provenance defaulted to 'unknown' (migration 010) MUST fail closed with zero FSM reads.
    let legacy_sid = "legacy-prep-study";
    let legacy_src = "legacy-prep-src";
    let legacy_fsm = "annotations/legacy/raw.pdf";
    certifications_api::dal::fsm::FsmClient::put_bytes(&*fsm, legacy_fsm, b"%PDF-1.4\n%%EOF")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO studies(id, owner_id, title, description, source_origin, status, quiz_id, expires_at, created_at, updated_at)
         VALUES($1, 'owner-a', 'Legacy Prep', '', 'annotations', 'draft', '', 9999999999, 1, 1)",
    )
    .bind(legacy_sid)
    .execute(&store.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sources(id, owner_id, study_id, filename, mime_type, size_bytes, sha256, fsm_ref, source_kind, status, selection_kind, selection_spec, is_restricted, external_ref, created_at, updated_at)
         VALUES($1, 'owner-a', $2, 'legacy.pdf', 'application/pdf', 15, '', $3, 'fsm_ref', 'ready', 'all', '', 0, '', 1, 1)",
    )
    .bind(legacy_src)
    .bind(legacy_sid)
    .bind(legacy_fsm)
    .execute(&store.db)
    .await
    .unwrap();

    let gets_before = fsm.get_count();
    let (st, _) = call(
        &app,
        "GET",
        &format!("/studies/{legacy_sid}/sources/{legacy_src}/content"),
        Some("owner-a"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(fsm.get_count(), gets_before);

    // Verified service-key prepared study (source_provenance = 'prepared_service') still reads.
    let (st, prep) = call_with_service_key(
        &app,
        "POST",
        "/studies/prepared",
        Some("owner-a"),
        Some("ann-svc-key"),
        Some(json!({
            "title": "Verified Prep",
            "sources": [{
                "filename": "good.pdf",
                "mime_type": "application/pdf",
                "fsm_ref": legacy_fsm,
                "size_bytes": 15
            }]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let new_sid = prep["study"]["id"].as_str().unwrap();
    let new_src = prep["sources"][0]["id"].as_str().unwrap();
    let (st_ok, _, body) = call_raw(
        &app,
        "GET",
        &format!("/studies/{new_sid}/sources/{new_src}/content"),
        Some("owner-a"),
        None,
        None,
        Vec::new(),
    )
    .await;
    assert_eq!(st_ok, StatusCode::OK);
    assert_eq!(body, b"%PDF-1.4\n%%EOF");
}

#[test]
fn selection_grammar_validates_pages_passages_and_ms_intervals() {
    use certifications_api::domain::studies::canonicalize_selection;

    // Pages
    assert_eq!(
        canonicalize_selection("pages", "1-3, 5", Some("application/pdf"), Some(10), None).unwrap(),
        "1-3,5"
    );
    assert!(canonicalize_selection("pages", "0-2", Some("application/pdf"), None, None).is_err());
    assert!(canonicalize_selection("pages", "3,1", Some("application/pdf"), None, None).is_err());
    assert!(
        canonicalize_selection("pages", "1-5", Some("application/pdf"), Some(4), None).is_err()
    );
    assert!(canonicalize_selection("pages", "1-2", Some("audio/wav"), None, None).is_err());

    // Passages
    assert_eq!(
        canonicalize_selection(
            "passages",
            "p1:0-25, p1:25-40",
            Some("text/plain"),
            None,
            None
        )
        .unwrap(),
        "p1:0-25,p1:25-40"
    );
    assert!(canonicalize_selection("passages", "0-25", Some("text/plain"), None, None).is_err());
    assert!(
        canonicalize_selection("passages", "p1:10-10", Some("text/plain"), None, None).is_err()
    );

    // Audio intervals (ms:start-end, adjacent normalized, overlaps/floats/exponents/bounds rejected)
    assert_eq!(
        canonicalize_selection(
            "intervals",
            "ms:0-12500,30000-45000",
            Some("audio/wav"),
            None,
            Some(60_000)
        )
        .unwrap(),
        "ms:0-12500,30000-45000"
    );
    assert_eq!(
        canonicalize_selection(
            "intervals",
            "ms:0-1000,1000-2000",
            Some("audio/wav"),
            None,
            Some(5000)
        )
        .unwrap(),
        "ms:0-2000"
    );
    assert!(canonicalize_selection("intervals", "0-1000", Some("audio/wav"), None, None).is_err());
    assert!(
        canonicalize_selection("intervals", "ms:1e2-500", Some("audio/wav"), None, None).is_err()
    );
    assert!(
        canonicalize_selection(
            "intervals",
            "ms:0-5000,4000-6000",
            Some("audio/wav"),
            None,
            None
        )
        .is_err()
    );
    assert!(
        canonicalize_selection(
            "intervals",
            "ms:0-5000",
            Some("audio/wav"),
            None,
            Some(4000)
        )
        .is_err()
    );
    assert!(
        canonicalize_selection(
            "intervals",
            "ms:0-5000",
            Some("application/pdf"),
            None,
            None
        )
        .is_err()
    );

    // Cross-service Cortex#136 alignment:
    // 1. Canonical noncontiguous multi-audio fixture (`ms:0-100,200-300`)
    assert_eq!(
        canonicalize_selection(
            "intervals",
            "ms:0-100, 200-300",
            Some("audio/wav; rate=16000"),
            None,
            None
        )
        .unwrap(),
        "ms:0-100,200-300"
    );
    assert!(
        canonicalize_selection(
            "intervals",
            "ms:0-100,ms:200-300",
            Some("audio/wav"),
            None,
            None
        )
        .is_err()
    );
    // 2. PDF total selected pages <= 100 (single & multi-range boundary + overflow) and MIME parameter normalization
    assert_eq!(
        canonicalize_selection(
            "pages",
            "1-50,51-100",
            Some("application/pdf; charset=binary"),
            None,
            None
        )
        .unwrap(),
        "1-50,51-100"
    );
    assert!(canonicalize_selection("pages", "1-101", Some("application/pdf"), None, None).is_err());
    assert!(
        canonicalize_selection("pages", "1-50,51-101", Some("application/pdf"), None, None)
            .is_err()
    );
    assert!(
        canonicalize_selection("pages", "1-5,5-10", Some("application/pdf"), None, None).is_err()
    );
    // 3. Passages requires text/* (with optional MIME params); DOCX allows `all` only, not `passages`
    assert_eq!(
        canonicalize_selection(
            "passages",
            "p1:0-10,p1:10-20",
            Some("text/plain; charset=utf-8"),
            None,
            None
        )
        .unwrap(),
        "p1:0-10,p1:10-20"
    );
    assert!(
        canonicalize_selection(
            "passages",
            "p1:0-10,p1:5-15",
            Some("text/plain"),
            None,
            None
        )
        .is_err()
    );
    assert!(
        canonicalize_selection(
            "passages",
            "p1:0-10",
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            None,
            None
        )
        .is_err()
    );
    assert_eq!(
        canonicalize_selection(
            "all",
            "",
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            None,
            None
        )
        .unwrap(),
        ""
    );
}
