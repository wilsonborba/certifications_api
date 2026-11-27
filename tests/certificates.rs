use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use certifications_api::{
    core::Settings,
    dal::{
        Store,
        fsm::MemoryFsm,
        schemas::{self, DraftSchema, SchemaField},
    },
    domain::{
        App,
        generations::{QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION},
        quotas::QuotaCache,
    },
    presentation,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn decode_qr_from_svg(svg_bytes: &[u8]) -> Result<String, String> {
    let s = std::str::from_utf8(svg_bytes).map_err(|e| e.to_string())?;
    let start_tag = "<g id=\"qr-code\">";
    let end_tag = "</g>";
    let start = s.find(start_tag).ok_or("no qr-code group in svg")? + start_tag.len();
    let end = s[start..].find(end_tag).ok_or("no qr-code end in svg")? + start;
    let group = &s[start..end];

    let mut rects = Vec::new();
    for line in group.lines() {
        if line.contains("<rect")
            && line.contains("fill=\"#000000\"")
            && let (Some(x_pos), Some(y_pos)) = (line.find("x=\""), line.find("y=\""))
        {
            let x_str = &line[x_pos + 3..];
            let x_end = x_str.find('"').unwrap();
            let x: f64 = x_str[..x_end].parse().unwrap();

            let y_str = &line[y_pos + 3..];
            let y_end = y_str.find('"').unwrap();
            let y: f64 = y_str[..y_end].parse().unwrap();
            rects.push((x, y));
        }
    }
    if rects.is_empty() {
        return Err("no qr rects found in svg".into());
    }

    let min_x = rects.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
    let min_y = rects.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
    let max_x = rects
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = rects
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut xs: Vec<f64> = rects.iter().map(|(x, _)| *x).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    let mut step = f64::INFINITY;
    for i in 1..xs.len() {
        let diff = xs[i] - xs[i - 1];
        if diff > 0.1 && diff < step {
            step = diff;
        }
    }
    let grid_w = ((max_x - min_x) / step).round() as usize + 1;
    let grid_h = ((max_y - min_y) / step).round() as usize + 1;
    assert_eq!(grid_w, grid_h);

    let quiet = 4;
    let scale = 4;
    let total_w = (grid_w + quiet * 2) * scale;
    let mut gray =
        image::GrayImage::from_pixel(total_w as u32, total_w as u32, image::Luma([255u8]));

    for (x, y) in rects {
        let col = ((x - min_x) / step).round() as usize;
        let row = ((y - min_y) / step).round() as usize;
        for dy in 0..scale {
            for dx in 0..scale {
                let px = (col + quiet) * scale + dx;
                let py = (row + quiet) * scale + dy;
                gray.put_pixel(px as u32, py as u32, image::Luma([0u8]));
            }
        }
    }

    let mut prepared = rqrr::PreparedImage::prepare(gray);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("rqrr detected 0 grids in svg qr".into());
    }
    let mut buf = Vec::new();
    grids[0]
        .decode_to(&mut buf)
        .map_err(|e| format!("decode error: {e:?}"))?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

fn decode_qr_from_pdf(pdf_bytes: &[u8]) -> Result<String, String> {
    use std::io::Write;
    let mut temp_pdf = tempfile::Builder::new()
        .suffix(".pdf")
        .tempfile()
        .map_err(|e| e.to_string())?;
    temp_pdf.write_all(pdf_bytes).map_err(|e| e.to_string())?;
    let pdf_path = temp_pdf.path().to_str().unwrap().to_string();

    let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let out_prefix = temp_dir.path().join("out");

    let status = std::process::Command::new("pdftoppm")
        .args([
            "-png",
            "-singlefile",
            "-r",
            "150",
            &pdf_path,
            out_prefix.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("pdftoppm execution failed".into());
    }

    let png_path = temp_dir.path().join("out.png");
    let img = image::open(&png_path).map_err(|e| format!("image::open error: {e:?}"))?;
    let gray = img.into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("rqrr detected 0 grids in rendered pdf image".into());
    }
    let mut buf = Vec::new();
    grids[0]
        .decode_to(&mut buf)
        .map_err(|e| format!("decode error: {e:?}"))?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

fn decode_qr_from_svg_raster(svg_bytes: &[u8]) -> Result<String, String> {
    use std::io::Write;
    let mut temp_svg = tempfile::Builder::new()
        .suffix(".svg")
        .tempfile()
        .map_err(|e| e.to_string())?;
    temp_svg.write_all(svg_bytes).map_err(|e| e.to_string())?;
    let svg_path = temp_svg.path().to_str().unwrap().to_string();

    let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let png_path = temp_dir.path().join("out.png");

    let status = std::process::Command::new("rsvg-convert")
        .args(["-o", png_path.to_str().unwrap(), &svg_path])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("rsvg-convert execution failed".into());
    }

    let img = image::open(&png_path).map_err(|e| format!("image::open error: {e:?}"))?;
    let gray = img.into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("rqrr detected 0 grids in rasterized svg image".into());
    }
    let mut buf = Vec::new();
    grids[0]
        .decode_to(&mut buf)
        .map_err(|e| format!("decode error: {e:?}"))?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

fn extract_pdf_text(pdf_bytes: &[u8]) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("pdftotext")
        .args(["-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn pdftotext");
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(pdf_bytes)
            .expect("failed to write to pdftotext stdin");
    }
    let output = child
        .wait_with_output()
        .expect("failed to wait on pdftotext");
    String::from_utf8_lossy(&output.stdout).to_string()
}

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
        reports_service_key: String::new(),
        certifications_service_key: String::new(),
        clamav: Default::default(),
    }
}

async fn publish_question_schema(store: &Store) {
    let draft = DraftSchema {
        schema_name: QUESTION_SCHEMA_NAME.into(),
        version: QUESTION_SCHEMA_VERSION,
        description: "Question v2".into(),
        fields: vec![
            SchemaField {
                field_path: "prompt".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "alternatives".into(),
                field_type: "array".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "correct_id".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "explanation".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "citation_ref".into(),
                field_type: "string".into(),
                is_required: true,
                description: "".into(),
            },
            SchemaField {
                field_path: "visual_ids".into(),
                field_type: "array".into(),
                is_required: true,
                description: "Canonical visual references".into(),
            },
        ],
    };
    schemas::save_draft(&store.db, &draft).await.unwrap();
    schemas::publish(&store.db, QUESTION_SCHEMA_NAME, QUESTION_SCHEMA_VERSION)
        .await
        .unwrap();
}

async fn setup_app() -> Router {
    let mut store = Store::fixture().await;
    store.fsm = MemoryFsm::new();
    publish_question_schema(&store).await;

    presentation::router(App {
        store,
        settings: test_settings(),
        quotas: QuotaCache::default(),
        signatures: Default::default(),
    })
}

async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(l) = level {
        req = req.header("x-access-level", l);
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
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    level: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, bytes) = call_raw(app, method, uri, user, level, body).await;
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

async fn call_with_lang(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    level: Option<&str>,
    accept_lang: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-uuid", u);
    }
    if let Some(l) = level {
        req = req.header("x-access-level", l);
    }
    if let Some(al) = accept_lang {
        req = req.header("accept-language", al);
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
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, val)
}

#[tokio::test]
async fn certificate_identity_idempotent_issuance_snapshot_files_and_revocation() {
    let app = setup_app().await;

    // Publish a manual quiz by author-1.
    let (_, study) = call(
        &app,
        "POST",
        "/studies",
        Some("author-1"),
        None,
        Some(json!({ "title": "Cert Quiz" })),
    )
    .await;
    let sid = study["id"].as_str().unwrap().to_string();

    call(
        &app,
        "POST",
        &format!("/studies/{sid}/questions"),
        Some("author-1"),
        None,
        Some(json!({
            "language": "en",
            "prompt": "Q1?",
            "alternatives": [
                { "id": "a", "text": "Yes" },
                { "id": "b", "text": "No" }
            ],
            "correct_id": "a",
            "explanation": "Yes is right"
        })),
    )
    .await;

    let (_, pub_q) = call(
        &app,
        "POST",
        &format!("/studies/{sid}/publication"),
        Some("author-1"),
        None,
        Some(json!({ "tags": ["cert"] })),
    )
    .await;
    let quiz_id = pub_q["id"].as_str().unwrap().to_string();

    // Normal creator taking own manual quiz cannot issue a certificate (practice only).
    let (_, a_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{quiz_id}/attempts"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    let a_att_id = a_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{a_att_id}/submit"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/attempts/{a_att_id}/certificate"),
        Some("author-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Learner-1 completes attempt with 0 score (no pass/fail threshold: still eligible!).
    let (_, l_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{quiz_id}/attempts"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    let l_att_id = l_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/events"),
        Some("learner-1"),
        None,
        Some(json!({
            "events": [{ "dedup_key": "e1", "event_kind": "page_hidden", "occurred_at": 100 }]
        })),
    )
    .await;
    call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/submit"),
        Some("learner-1"),
        None,
        None,
    )
    .await;

    // Must confirm holder identity before issuing.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Confirm holder identity once; ordinary rename is blocked.
    let (st, holder) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-1"),
        None,
        Some(json!({ "display_name": "Ana Silva" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let holder_id = holder["holder_id"].as_str().unwrap().to_string();
    assert_ne!(holder_id, "learner-1");

    let (st, _) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-1"),
        None,
        Some(json!({ "display_name": "Other Name" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Idempotent issuance returns the exact same certificate.
    let (st, cert1) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, cert2) = call(
        &app,
        "POST",
        &format!("/attempts/{l_att_id}/certificate"),
        Some("learner-1"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cert1["verification_code"], cert2["verification_code"]);
    let code = cert1["verification_code"].as_str().unwrap().to_string();

    // Admin updates holder name, but historic certificate snapshot stays "Ana Silva".
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/admin/holders/{holder_id}"),
        Some("admin-1"),
        Some("1"),
        Some(json!({ "display_name": "Ana M. Silva" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Public verification shows snapshot, activity summary, and no private answers.
    let (st, ver) = call(
        &app,
        "GET",
        &format!("/certificates/{code}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver["holder_name"], "Ana Silva");
    assert_eq!(ver["score_correct"], 0);
    assert_eq!(ver["score_total"], 1);
    assert_eq!(ver["activity_summary"]["page_hidden_count"], 1);
    assert!(!ver.to_string().contains("Yes is right"));

    // PDF and image files are served from FSM.
    let (st, pdf_bytes) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{code}/files/pdf"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(pdf_bytes.starts_with(b"%PDF-"));

    let (st, img_bytes) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{code}/files/image"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(String::from_utf8_lossy(&img_bytes).contains("<svg"));

    // Verify QR payloads decode from both rendered artifacts
    let expected_url = format!("https://certifications.asodya.com/certificates/{code}");
    let decoded_svg_qr = decode_qr_from_svg(&img_bytes).expect("SVG QR decode failed");
    assert_eq!(decoded_svg_qr, expected_url);

    let decoded_pdf_qr = decode_qr_from_pdf(&pdf_bytes).expect("PDF QR decode failed");
    assert_eq!(decoded_pdf_qr, expected_url);

    // Readable text in SVG and PDF
    let svg_str = String::from_utf8_lossy(&img_bytes);
    assert!(svg_str.contains("Asodya Completion Certificate"));
    assert!(svg_str.contains("Holder: Ana Silva"));
    assert!(svg_str.contains("Quiz: Cert Quiz"));
    assert!(svg_str.contains("Result: 0/1"));
    assert!(svg_str.contains("Date:"));
    assert!(!svg_str.contains("page_hidden"));
    assert!(!svg_str.contains("Yes is right"));

    // Purple branding, logo image, ASODYA header, and absence of signature / admin
    assert!(svg_str.contains("stroke=\"#6B21A8\""));
    assert!(svg_str.contains("stroke=\"#E9D5FF\""));
    assert!(svg_str.contains("fill=\"#581C87\""));
    assert!(svg_str.contains("<image href=\"data:image/png;base64,"));
    assert!(svg_str.contains(">ASODYA<"));
    assert!(!svg_str.to_lowercase().contains("signature"));

    let pdf_str = extract_pdf_text(&pdf_bytes);
    assert!(pdf_str.contains("Asodya Completion Certificate"));
    assert!(pdf_str.contains("Holder: Ana Silva"));
    assert!(pdf_str.contains("Quiz: Cert Quiz"));
    assert!(pdf_str.contains("Result: 0/1"));
    assert!(pdf_str.contains("Date:"));
    assert!(!pdf_str.contains("page_hidden"));
    assert!(!pdf_str.contains("Yes is right"));
    assert!(pdf_str.contains("ASODYA"));
    assert!(!pdf_str.to_lowercase().contains("signature"));

    // Admin revocation is reflected on public verification.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/admin/certificates/{code}/revocation"),
        Some("admin-1"),
        Some("1"),
        Some(json!({ "reason": "Issued in error" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, ver_after) = call(
        &app,
        "GET",
        &format!("/certificates/{code}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ver_after["status"], "revoked");
    assert_eq!(ver_after["revocation_reason"], "Issued in error");
}

#[tokio::test]
async fn certificate_qr_and_multilingual_unicode_rendering_en_pt_th() {
    let app = setup_app().await;

    // 1. Portuguese quiz and holder with accents
    let (_, pt_study) = call(
        &app,
        "POST",
        "/studies",
        Some("instructor-pt"),
        None,
        Some(json!({ "title": "Programação Básica em Rust" })),
    )
    .await;
    let pt_sid = pt_study["id"].as_str().unwrap().to_string();

    call(
        &app,
        "POST",
        &format!("/studies/{pt_sid}/questions"),
        Some("instructor-pt"),
        None,
        Some(json!({
            "language": "pt-BR",
            "prompt": "Qual é a saída principal?",
            "alternatives": [
                { "id": "a", "text": "Correto" },
                { "id": "b", "text": "Incorreto" }
            ],
            "correct_id": "a",
            "explanation": "Explicação detalhada"
        })),
    )
    .await;

    let (_, pt_pub) = call(
        &app,
        "POST",
        &format!("/studies/{pt_sid}/publication"),
        Some("instructor-pt"),
        None,
        Some(json!({ "tags": ["pt-test"] })),
    )
    .await;
    let pt_quiz_id = pt_pub["id"].as_str().unwrap().to_string();

    // Confirm Portuguese learner identity
    let (st, _) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-pt"),
        None,
        Some(json!({ "display_name": "João da Silva Oliveira" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Attempt and submit
    let (_, pt_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{pt_quiz_id}/attempts"),
        Some("learner-pt"),
        None,
        None,
    )
    .await;
    let pt_att_id = pt_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{pt_att_id}/submit"),
        Some("learner-pt"),
        None,
        None,
    )
    .await;

    // Issue certificate
    let (st, pt_cert) = call(
        &app,
        "POST",
        &format!("/attempts/{pt_att_id}/certificate"),
        Some("learner-pt"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let pt_code = pt_cert["verification_code"].as_str().unwrap().to_string();
    let pt_expected_url = format!("https://certifications.asodya.com/certificates/{pt_code}");

    // Download PT files
    let (st, pt_pdf) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{pt_code}/files/pdf"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, pt_svg) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{pt_code}/files/image"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Verify PT QR decodes
    assert_eq!(decode_qr_from_svg(&pt_svg).unwrap(), pt_expected_url);
    assert_eq!(decode_qr_from_pdf(&pt_pdf).unwrap(), pt_expected_url);

    // Verify PT readable text
    let pt_svg_str = String::from_utf8_lossy(&pt_svg);
    assert!(pt_svg_str.contains("João da Silva Oliveira"));
    assert!(pt_svg_str.contains("Programação Básica em Rust"));
    assert!(!pt_svg_str.contains("Explicação"));

    let pt_pdf_str = extract_pdf_text(&pt_pdf);
    assert!(pt_pdf_str.contains("João da Silva Oliveira"));
    assert!(pt_pdf_str.contains("Programação Básica em Rust"));
    assert!(!pt_pdf_str.contains("Explicação"));

    // 2. Thai quiz and holder
    let (_, th_study) = call(
        &app,
        "POST",
        "/studies",
        Some("instructor-th"),
        None,
        Some(json!({ "title": "แบบทดสอบความรู้พื้นฐาน" })),
    )
    .await;
    let th_sid = th_study["id"].as_str().unwrap().to_string();

    call(
        &app,
        "POST",
        &format!("/studies/{th_sid}/questions"),
        Some("instructor-th"),
        None,
        Some(json!({
            "language": "th",
            "prompt": "คำถามแรกคืออะไร?",
            "alternatives": [
                { "id": "a", "text": "ถูกต้อง" },
                { "id": "b", "text": "ไม่ถูกต้อง" }
            ],
            "correct_id": "a",
            "explanation": "คำอธิบายที่ถูกต้อง"
        })),
    )
    .await;

    let (_, th_pub) = call(
        &app,
        "POST",
        &format!("/studies/{th_sid}/publication"),
        Some("instructor-th"),
        None,
        Some(json!({ "tags": ["th-test"] })),
    )
    .await;
    let th_quiz_id = th_pub["id"].as_str().unwrap().to_string();

    // Confirm Thai learner identity
    let (st, _) = call(
        &app,
        "PUT",
        "/me/identity",
        Some("learner-th"),
        None,
        Some(json!({ "display_name": "สมชาย ใจดี" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Attempt and submit
    let (_, th_att) = call(
        &app,
        "POST",
        &format!("/quizzes/{th_quiz_id}/attempts"),
        Some("learner-th"),
        None,
        None,
    )
    .await;
    let th_att_id = th_att["id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        &format!("/attempts/{th_att_id}/submit"),
        Some("learner-th"),
        None,
        None,
    )
    .await;

    // Issue certificate
    let (st, th_cert) = call(
        &app,
        "POST",
        &format!("/attempts/{th_att_id}/certificate"),
        Some("learner-th"),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let th_code = th_cert["verification_code"].as_str().unwrap().to_string();
    let th_expected_url = format!("https://certifications.asodya.com/certificates/{th_code}");

    // Download Thai files
    let (st, th_pdf) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{th_code}/files/pdf"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, th_svg) = call_raw(
        &app,
        "GET",
        &format!("/certificates/{th_code}/files/image"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Verify Thai QR decodes
    assert_eq!(decode_qr_from_svg(&th_svg).unwrap(), th_expected_url);
    assert_eq!(decode_qr_from_pdf(&th_pdf).unwrap(), th_expected_url);

    // Verify Thai readable text
    let th_svg_str = String::from_utf8_lossy(&th_svg);
    assert!(th_svg_str.contains("สมชาย ใจดี"));
    assert!(th_svg_str.contains("แบบทดสอบความรู้พื้นฐาน"));
    assert!(!th_svg_str.contains("คำอธิบาย"));

    let th_pdf_str = extract_pdf_text(&th_pdf);
    assert!(th_pdf_str.contains("สมชาย ใจดี"));
    assert!(th_pdf_str.contains("แบบทดสอบความรู้พื้นฐาน"));
    assert!(!th_pdf_str.contains("คำอธิบาย"));

    // 3. Activity interpretation note / disclaimer localization:
    // Thai quiz verification falls back to Thai disclaimer when no Accept-Language header is provided:
    let (st, th_ver_default) = call(
        &app,
        "GET",
        &format!("/certificates/{th_code}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        th_ver_default["activity_summary"]["disclaimer"],
        "สรุปวงจรชีวิตของเบราว์เซอร์ที่สังเกตได้เท่านั้น การไม่มีเหตุการณ์หรือการมีสิ่งรบกวนไม่ใช่คำตัดสินว่ามีการทุจริต"
    );

    // Explicit Accept-Language: th overrides or confirms Thai:
    let (st, th_ver_header) = call_with_lang(
        &app,
        "GET",
        &format!("/certificates/{th_code}"),
        None,
        None,
        Some("th-TH,th;q=0.9"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        th_ver_header["activity_summary"]["disclaimer"],
        "สรุปวงจรชีวิตของเบราว์เซอร์ที่สังเกตได้เท่านั้น การไม่มีเหตุการณ์หรือการมีสิ่งรบกวนไม่ใช่คำตัดสินว่ามีการทุจริต"
    );

    // Explicit Accept-Language: pt returns Portuguese disclaimer:
    let (st, pt_ver_header) = call_with_lang(
        &app,
        "GET",
        &format!("/certificates/{th_code}"),
        None,
        None,
        Some("pt-BR,pt;q=0.9"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        pt_ver_header["activity_summary"]["disclaimer"],
        "Resumo do ciclo de vida do navegador observado apenas; a ausência de eventos ou a presença de interrupções não é um veredito de fraude."
    );

    // Explicit Accept-Language: en returns English disclaimer:
    let (st, en_ver_header) = call_with_lang(
        &app,
        "GET",
        &format!("/certificates/{th_code}"),
        None,
        None,
        Some("en-US,en;q=0.9"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        en_ver_header["activity_summary"]["disclaimer"],
        "Observed browser lifecycle summary only; absence of events or presence of interruptions is not a cheating verdict."
    );
}

#[tokio::test]
async fn certificate_multiline_wrapping_and_no_qr_overlap() {
    let holder = "นายสมชาย ใจดี ผู้เชี่ยวชาญด้านการทดสอบระบบซอฟต์แวร์และการรักษาความปลอดภัยระดับองค์กร";
    let title = "แบบทดสอบทักษะและการพัฒนาโปรแกรมประยุกต์ขั้นสูงสำหรับผู้เชี่ยวชาญรุ่นที่หนึ่ง ประจำปี 2026";
    let code = "ASODYA-TEST-LONG-WRAP-1234";
    let ts = 1795600000;

    let pdf = certifications_api::domain::certificates::render_certificate_pdf(
        holder,
        title,
        95,
        100,
        code,
        ts,
        Some("th"),
    );
    let svg = certifications_api::domain::certificates::render_certificate_svg(
        holder,
        title,
        95,
        100,
        code,
        ts,
        Some("th"),
    );

    assert!(pdf.starts_with(b"%PDF-"));
    let expected_url = format!("https://certifications.asodya.com/certificates/{code}");
    assert_eq!(decode_qr_from_pdf(&pdf).unwrap(), expected_url);
    assert_eq!(decode_qr_from_svg(&svg).unwrap(), expected_url);

    let pdf_text = extract_pdf_text(&pdf);
    assert!(pdf_text.contains("นายสมชาย"));
    assert!(pdf_text.contains("ใจดี"));
    assert!(pdf_text.contains("แบบทดสอบทักษะ"));
    assert!(pdf_text.contains("95/100"));
    assert!(pdf_text.contains(code));

    let svg_text = String::from_utf8_lossy(&svg);
    assert!(svg_text.contains("นายสมชาย"));
    assert!(svg_text.contains("ใจดี"));
    assert!(svg_text.contains("แบบทดสอบทักษะ"));
    assert!(svg_text.contains("95/100"));
    assert!(svg_text.contains(code));
    assert!(svg_text.contains("@font-face"));
    assert!(svg_text.contains("IBMPlexSansThai"));
    assert!(svg_text.contains("data:font/ttf;base64,"));
}

#[tokio::test]
async fn certificate_deterministic_language_labels() {
    let holder = "Maria Silva";
    let title = "Safety Protocols";
    let code = "ASODYA-TEST-LANG-5678";
    let ts = 1795600000;

    let en_svg = certifications_api::domain::certificates::render_certificate_svg(
        holder,
        title,
        10,
        10,
        code,
        ts,
        Some("en"),
    );
    let en_text = String::from_utf8_lossy(&en_svg);
    assert!(en_text.contains("Asodya Completion Certificate"));
    assert!(en_text.contains("Holder:"));

    let pt_svg = certifications_api::domain::certificates::render_certificate_svg(
        holder,
        title,
        10,
        10,
        code,
        ts,
        Some("pt-BR"),
    );
    let pt_text = String::from_utf8_lossy(&pt_svg);
    assert!(pt_text.contains("Certificado de Conclusão Asodya"));
    assert!(pt_text.contains("Titular:"));

    let th_svg = certifications_api::domain::certificates::render_certificate_svg(
        holder,
        title,
        10,
        10,
        code,
        ts,
        Some("th"),
    );
    let th_text = String::from_utf8_lossy(&th_svg);
    assert!(th_text.contains("ใบรับรองการสำเร็จการศึกษา Asodya"));
    assert!(th_text.contains("ผู้รับใบรับรอง:"));
}

#[tokio::test]
async fn certificate_wide_unbroken_text_measured_layout_and_raster_qr_decodable() {
    let holder = "W".repeat(120);
    let title = "W".repeat(120);
    let code = "ASODYA-TEST-LONG-120";
    let ts = 1795600000;

    let pdf = certifications_api::domain::certificates::render_certificate_pdf(
        &holder,
        &title,
        100,
        100,
        code,
        ts,
        Some("en"),
    );
    let svg = certifications_api::domain::certificates::render_certificate_svg(
        &holder,
        &title,
        100,
        100,
        code,
        ts,
        Some("en"),
    );

    assert!(pdf.starts_with(b"%PDF-"));
    let expected_url = format!("https://certifications.asodya.com/certificates/{code}");

    // Both real rasterized PDF (pdftoppm) and real rasterized SVG (rsvg-convert)
    // must successfully detect and decode the QR code without text overpainting.
    let pdf_qr = decode_qr_from_pdf(&pdf).expect("pdf qr decoded via pdftoppm");
    assert_eq!(pdf_qr, expected_url);

    let svg_qr = decode_qr_from_svg_raster(&svg).expect("svg qr decoded via rsvg-convert");
    assert_eq!(svg_qr, expected_url);

    // Verify SVG does not place any text past max_text_width
    let svg_str = String::from_utf8_lossy(&svg);
    for line in svg_str.lines() {
        if line.contains("<text") && line.contains("WWWW") {
            // No individual line contains more than 35 consecutive W characters
            assert!(!line.contains(&"W".repeat(36)));
        }
    }

    // Full 120 W characters must be preserved without truncation
    let pdf_text = extract_pdf_text(&pdf);
    let holder_w_count = pdf_text.chars().filter(|c| *c == 'W').count();
    // 120 Ws in holder + 120 Ws in quiz = 240 Ws
    assert_eq!(holder_w_count, 240);
}

#[tokio::test]
async fn certificate_multilingual_portuguese_thai_mixed_raster_qr_decodable() {
    let code = "ASODYA-TEST-MULTI-9999";
    let ts = 1795600000;
    let expected_url = format!("https://certifications.asodya.com/certificates/{code}");

    // Portuguese with accented characters
    let pt_holder = "João Carlos Mendonça de Alcântara e Silva dos Santos";
    let pt_title = "Avaliação de Proficiência em Gestão e Coordenação Técnica Especializada 2026";
    let pt_pdf = certifications_api::domain::certificates::render_certificate_pdf(
        pt_holder,
        pt_title,
        98,
        100,
        code,
        ts,
        Some("pt-BR"),
    );
    let pt_svg = certifications_api::domain::certificates::render_certificate_svg(
        pt_holder,
        pt_title,
        98,
        100,
        code,
        ts,
        Some("pt-BR"),
    );
    assert_eq!(decode_qr_from_pdf(&pt_pdf).unwrap(), expected_url);
    assert_eq!(decode_qr_from_svg_raster(&pt_svg).unwrap(), expected_url);

    // Mixed English / Thai / Numbers
    let mix_holder = "Dr. Somsak สมศักดิ์ (DevOps & Platform Engineering)";
    let mix_title = "Kubernetes Certified Administrator & Cloud Architecture หลักสูตรเข้มข้น 2026";
    let mix_pdf = certifications_api::domain::certificates::render_certificate_pdf(
        mix_holder,
        mix_title,
        100,
        100,
        code,
        ts,
        Some("th"),
    );
    let mix_svg = certifications_api::domain::certificates::render_certificate_svg(
        mix_holder,
        mix_title,
        100,
        100,
        code,
        ts,
        Some("th"),
    );
    assert_eq!(decode_qr_from_pdf(&mix_pdf).unwrap(), expected_url);
    assert_eq!(decode_qr_from_svg_raster(&mix_svg).unwrap(), expected_url);
}
