use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::{
        attempts::{self, ActivitySummary},
        auth::{CallerIdentity, require_owner},
        authoring::ORIGIN_CREATOR,
    },
};
use icu_segmenter::LineSegmenter;
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point};
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::text::{Font, TextDirection};
use krilla::{Data, Document};
use serde::Serialize;
use sqlx::Row;

const REGULAR_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSansThai-Regular.ttf");
const BOLD_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSansThai-Bold.ttf");

#[derive(Clone, Debug, Serialize)]
pub struct HolderIdentity {
    pub holder_id: Option<String>,
    pub display_name: Option<String>,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CertificateRecord {
    pub id: String,
    pub verification_code: String,
    pub holder_public_id: String,
    pub holder_name: String,
    pub quiz_id: String,
    pub quiz_title: String,
    pub attempt_id: String,
    pub score_correct: i64,
    pub score_total: i64,
    pub status: String,
    pub revocation_reason: String,
    pub issued_at: i64,
    pub revoked_at: Option<i64>,
    pub pdf_url: String,
    pub image_url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicCertificateVerification {
    pub verification_code: String,
    pub status: String,
    pub revocation_reason: String,
    pub holder_public_id: String,
    pub holder_name: String,
    pub quiz_title: String,
    pub score_correct: i64,
    pub score_total: i64,
    pub issued_at: i64,
    pub revoked_at: Option<i64>,
    pub activity_summary: ActivitySummary,
}

pub async fn get_my_identity(store: &Store, caller: &CallerIdentity) -> Result<HolderIdentity> {
    let row = sqlx::query("SELECT id, display_name FROM certificate_holders WHERE owner_id = $1")
        .bind(&caller.user_id)
        .fetch_optional(&store.db)
        .await?;
    match row {
        Some(r) => Ok(HolderIdentity {
            holder_id: Some(r.get("id")),
            display_name: Some(r.get("display_name")),
            confirmed: true,
        }),
        None => Ok(HolderIdentity {
            holder_id: None,
            display_name: None,
            confirmed: false,
        }),
    }
}

pub async fn confirm_my_identity(
    store: &Store,
    caller: &CallerIdentity,
    display_name: &str,
) -> Result<HolderIdentity> {
    let clean = display_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() || clean.chars().count() > 120 {
        return Err(Error::unprocessable(
            "invalid_display_name: Display name must be 1 to 120 characters",
        ));
    }

    let existing = sqlx::query("SELECT id FROM certificate_holders WHERE owner_id = $1")
        .bind(&caller.user_id)
        .fetch_optional(&store.db)
        .await?;
    if existing.is_some() {
        return Err(Error::conflict(
            "identity_already_confirmed: Holder identity can only be confirmed once; contact an admin for corrections",
        ));
    }

    let holder_id = format!("hld_{}", new_id());
    let ts = now();
    sqlx::query(
        "INSERT INTO certificate_holders(id, owner_id, display_name, created_at, updated_at)
         VALUES($1, $2, $3, $4, $4)",
    )
    .bind(&holder_id)
    .bind(&caller.user_id)
    .bind(&clean)
    .bind(ts)
    .execute(&store.db)
    .await?;

    Ok(HolderIdentity {
        holder_id: Some(holder_id),
        display_name: Some(clean),
        confirmed: true,
    })
}

pub async fn admin_update_holder(
    store: &Store,
    caller: &CallerIdentity,
    holder_id: &str,
    display_name: &str,
) -> Result<HolderIdentity> {
    if !caller.is_admin() {
        return Err(Error::forbidden("admin_required: Admin access required"));
    }
    let clean = display_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() || clean.chars().count() > 120 {
        return Err(Error::unprocessable(
            "invalid_display_name: Display name must be 1 to 120 characters",
        ));
    }
    let res = sqlx::query(
        "UPDATE certificate_holders SET display_name = $1, updated_at = $2 WHERE id = $3",
    )
    .bind(&clean)
    .bind(now())
    .bind(holder_id)
    .execute(&store.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(Error::missing());
    }
    Ok(HolderIdentity {
        holder_id: Some(holder_id.to_string()),
        display_name: Some(clean),
        confirmed: true,
    })
}

struct CertLabels {
    cert_title: &'static str,
    holder_prefix: &'static str,
    quiz_prefix: &'static str,
    result_prefix: &'static str,
    date_prefix: &'static str,
    code_prefix: &'static str,
    verify_prefix: &'static str,
}

fn to_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(TABLE[b0 >> 2] as char);
        out.push(TABLE[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((b1 & 15) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[b2 & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

static REGULAR_FONT_BASE64: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| to_base64(REGULAR_FONT_BYTES));
static BOLD_FONT_BASE64: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| to_base64(BOLD_FONT_BYTES));

fn resolve_labels(language: Option<&str>) -> CertLabels {
    match language.map(str::trim) {
        Some("th") => CertLabels {
            cert_title: "ใบรับรองการสำเร็จการศึกษา Asodya",
            holder_prefix: "ผู้รับใบรับรอง",
            quiz_prefix: "แบบทดสอบ",
            result_prefix: "ผลคะแนน",
            date_prefix: "วันที่ออก",
            code_prefix: "รหัสยืนยัน",
            verify_prefix: "ตรวจสอบได้ที่",
        },
        Some("pt-BR") | Some("pt") => CertLabels {
            cert_title: "Certificado de Conclusão Asodya",
            holder_prefix: "Titular",
            quiz_prefix: "Questionário",
            result_prefix: "Resultado",
            date_prefix: "Data",
            code_prefix: "Código de Verificação",
            verify_prefix: "Verifique em",
        },
        _ => CertLabels {
            cert_title: "Asodya Completion Certificate",
            holder_prefix: "Holder",
            quiz_prefix: "Quiz",
            result_prefix: "Result",
            date_prefix: "Date",
            code_prefix: "Verification Code",
            verify_prefix: "Verify at",
        },
    }
}

fn measure_str_width(face: &ttf_parser::Face<'_>, text: &str, font_size: f32) -> f32 {
    let upem = face.units_per_em() as f32;
    let mut total_advance = 0.0f32;
    for c in text.chars() {
        let adv = face
            .glyph_index(c)
            .and_then(|gid| face.glyph_hor_advance(gid))
            .unwrap_or(face.units_per_em() / 2) as f32;
        total_advance += adv;
    }
    (total_advance / upem) * font_size
}

fn break_lines(
    text: &str,
    max_width: f32,
    font_bytes: &[u8],
    font_size: f32,
    max_lines: usize,
) -> Vec<String> {
    let face = ttf_parser::Face::parse(font_bytes, 0).expect("valid font bytes");
    let line_segmenter = LineSegmenter::new_auto(Default::default());
    let grapheme_segmenter = icu_segmenter::GraphemeClusterSegmenter::new();

    let mut lines = Vec::new();
    for para in text.split('\n') {
        let breaks: Vec<usize> = line_segmenter.segment_str(para).collect();
        let mut current = String::new();
        let mut current_width = 0.0f32;
        let mut prev = 0usize;

        for &b in &breaks {
            if b <= prev {
                continue;
            }
            let seg = &para[prev..b];
            let seg_width = measure_str_width(&face, seg, font_size);

            if current_width + seg_width <= max_width {
                current.push_str(seg);
                current_width += seg_width;
            } else if seg_width <= max_width {
                let trimmed = current.trim_end();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_string());
                }
                current = seg.to_string();
                current_width = seg_width;
            } else {
                // Emergency grapheme-aware wrap for long unbroken runs:
                let g_breaks: Vec<usize> = grapheme_segmenter.segment_str(seg).collect();
                let mut g_prev = 0usize;
                for &gb in &g_breaks {
                    if gb <= g_prev {
                        continue;
                    }
                    let grapheme = &seg[g_prev..gb];
                    let g_width = measure_str_width(&face, grapheme, font_size);
                    if !current.is_empty() && current_width + g_width > max_width {
                        let trimmed = current.trim_end();
                        if !trimmed.is_empty() {
                            lines.push(trimmed.to_string());
                        }
                        current.clear();
                        current_width = 0.0;
                    }
                    current.push_str(grapheme);
                    current_width += g_width;
                    g_prev = gb;
                }
            }
            prev = b;
        }

        let trimmed = current.trim_end();
        if !trimmed.is_empty() || (para.is_empty() && lines.is_empty()) {
            lines.push(trimmed.to_string());
        }
    }

    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let ellipsis = "…";
            let ell_w = measure_str_width(&face, ellipsis, font_size);
            while !last.is_empty() && measure_str_width(&face, last, font_size) + ell_w > max_width
            {
                last.pop();
            }
            last.push_str(ellipsis);
        }
    }

    lines
}

pub fn render_certificate_pdf(
    holder_name: &str,
    quiz_title: &str,
    score_correct: i64,
    score_total: i64,
    code: &str,
    issued_at: i64,
    language: Option<&str>,
) -> Vec<u8> {
    let regular_font = Font::new(Data::from(REGULAR_FONT_BYTES), 0).expect("regular font");
    let bold_font = Font::new(Data::from(BOLD_FONT_BYTES), 0).expect("bold font");
    let labels = resolve_labels(language);
    let date_str = chrono::DateTime::from_timestamp(issued_at, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "2026-11-25".to_string());
    let verification_url = format!("https://certifications.asodya.com/certificates/{code}");

    let width = 800.0f32;
    let height = 500.0f32;
    let mut doc = Document::new();
    let page_settings = PageSettings::from_wh(width, height).expect("page settings");
    let mut page = doc.start_page_with(page_settings);
    let mut surface = page.surface();

    // Background & Outer Border
    let mut pb = PathBuilder::new();
    pb.move_to(15.0, 15.0);
    pb.line_to(width - 15.0, 15.0);
    pb.line_to(width - 15.0, height - 15.0);
    pb.line_to(15.0, height - 15.0);
    pb.close();
    if let Some(path) = pb.finish() {
        surface.set_fill(Some(Fill {
            paint: rgb::Color::new(255, 255, 255).into(),
            ..Default::default()
        }));
        surface.set_stroke(Some(Stroke {
            paint: rgb::Color::new(31, 41, 55).into(),
            width: 3.0,
            ..Default::default()
        }));
        surface.draw_path(&path);
    }

    // Inner subtle border
    let mut ib = PathBuilder::new();
    ib.move_to(22.0, 22.0);
    ib.line_to(width - 22.0, 22.0);
    ib.line_to(width - 22.0, height - 22.0);
    ib.line_to(22.0, height - 22.0);
    ib.close();
    if let Some(path) = ib.finish() {
        surface.set_fill(None);
        surface.set_stroke(Some(Stroke {
            paint: rgb::Color::new(209, 213, 219).into(),
            width: 1.0,
            ..Default::default()
        }));
        surface.draw_path(&path);
    }

    let left_x = 48.0f32;
    let max_text_width = 470.0f32;

    // Title
    surface.set_fill(Some(Fill {
        paint: rgb::Color::new(17, 24, 39).into(),
        ..Default::default()
    }));
    surface.set_stroke(None);
    surface.draw_text(
        Point::from_xy(left_x, 65.0),
        bold_font.clone(),
        22.0,
        labels.cert_title,
        false,
        TextDirection::Auto,
    );

    let mut current_y = 115.0f32;
    let max_usable_y = 465.0f32;
    let line_height = 22.0f32;

    let draw_field = |surface: &mut krilla::surface::Surface<'_>,
                      label: &str,
                      value: &str,
                      y: &mut f32,
                      max_lines: usize| {
        let full_text = format!("{label}: {value}");
        let lines = break_lines(
            &full_text,
            max_text_width,
            REGULAR_FONT_BYTES,
            16.0,
            max_lines,
        );
        for line in lines {
            if *y > max_usable_y {
                break;
            }
            surface.set_fill(Some(Fill {
                paint: rgb::Color::new(55, 65, 81).into(),
                ..Default::default()
            }));
            surface.set_stroke(None);
            surface.draw_text(
                Point::from_xy(left_x, *y),
                regular_font.clone(),
                16.0,
                &line,
                false,
                TextDirection::Auto,
            );
            *y += line_height;
        }
        *y += 6.0;
    };

    draw_field(
        &mut surface,
        labels.holder_prefix,
        holder_name,
        &mut current_y,
        4,
    );
    draw_field(
        &mut surface,
        labels.quiz_prefix,
        quiz_title,
        &mut current_y,
        4,
    );

    let score_str = format!("{score_correct}/{score_total}");
    draw_field(
        &mut surface,
        labels.result_prefix,
        &score_str,
        &mut current_y,
        1,
    );
    draw_field(
        &mut surface,
        labels.date_prefix,
        &date_str,
        &mut current_y,
        1,
    );

    if current_y <= max_usable_y {
        current_y += 6.0;
        let code_line = format!("{}: {code}", labels.code_prefix);
        surface.set_fill(Some(Fill {
            paint: rgb::Color::new(31, 41, 55).into(),
            ..Default::default()
        }));
        surface.set_stroke(None);
        surface.draw_text(
            Point::from_xy(left_x, current_y),
            bold_font.clone(),
            14.0,
            &code_line,
            false,
            TextDirection::Auto,
        );
        current_y += 20.0;
    }

    if current_y <= max_usable_y {
        let url_line = format!("{}: {verification_url}", labels.verify_prefix);
        let url_lines = break_lines(&url_line, max_text_width, REGULAR_FONT_BYTES, 12.0, 2);
        for uline in url_lines {
            if current_y > max_usable_y {
                break;
            }
            surface.set_fill(Some(Fill {
                paint: rgb::Color::new(107, 114, 128).into(),
                ..Default::default()
            }));
            surface.set_stroke(None);
            surface.draw_text(
                Point::from_xy(left_x, current_y),
                regular_font.clone(),
                12.0,
                &uline,
                false,
                TextDirection::Auto,
            );
            current_y += 16.0;
        }
    }

    // QR Code on the right half (x = 550, y = 110)
    if let Ok(qr) = qrcode::QrCode::new(verification_url.as_bytes()) {
        let qr_width = qr.width();
        let colors = qr.to_colors();
        let module_size = 4.0f32;
        let quiet = 4;
        let total_modules = qr_width + quiet * 2;
        let total_size = (total_modules as f32) * module_size;
        let qr_x = 550.0f32;
        let qr_y = 110.0f32;

        let mut qb = PathBuilder::new();
        qb.move_to(qr_x, qr_y);
        qb.line_to(qr_x + total_size, qr_y);
        qb.line_to(qr_x + total_size, qr_y + total_size);
        qb.line_to(qr_x, qr_y + total_size);
        qb.close();
        if let Some(qpath) = qb.finish() {
            surface.set_fill(Some(Fill {
                paint: rgb::Color::new(255, 255, 255).into(),
                ..Default::default()
            }));
            surface.set_stroke(Some(Stroke {
                paint: rgb::Color::new(229, 231, 235).into(),
                width: 1.0,
                ..Default::default()
            }));
            surface.draw_path(&qpath);
        }

        for y in 0..qr_width {
            for x in 0..qr_width {
                if colors[y * qr_width + x] == qrcode::Color::Dark {
                    let mx = qr_x + ((x + quiet) as f32) * module_size;
                    let my = qr_y + ((y + quiet) as f32) * module_size;
                    let mut mb = PathBuilder::new();
                    mb.move_to(mx, my);
                    mb.line_to(mx + module_size, my);
                    mb.line_to(mx + module_size, my + module_size);
                    mb.line_to(mx, my + module_size);
                    mb.close();
                    if let Some(mpath) = mb.finish() {
                        surface.set_fill(Some(Fill {
                            paint: rgb::Color::new(0, 0, 0).into(),
                            ..Default::default()
                        }));
                        surface.set_stroke(None);
                        surface.draw_path(&mpath);
                    }
                }
            }
        }
    }

    surface.finish();
    page.finish();
    doc.finish().expect("finish pdf")
}

pub fn render_certificate_svg(
    holder_name: &str,
    quiz_title: &str,
    score_correct: i64,
    score_total: i64,
    code: &str,
    issued_at: i64,
    language: Option<&str>,
) -> Vec<u8> {
    let escape_xml = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let labels = resolve_labels(language);
    let date_str = chrono::DateTime::from_timestamp(issued_at, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "2026-11-25".to_string());
    let verification_url = format!("https://certifications.asodya.com/certificates/{code}");

    let left_x = 48;
    let max_text_width = 470.0f32;

    let mut text_elements = String::new();

    // Title
    text_elements.push_str(&format!(
        "<text x=\"{left_x}\" y=\"65\" font-size=\"22\" font-family=\"'IBMPlexSansThai', 'Liberation Sans', sans-serif\" font-weight=\"bold\" fill=\"#111827\">{}</text>\n",
        escape_xml(labels.cert_title)
    ));

    let mut current_y = 115.0f32;
    let max_usable_y = 465.0f32;
    let line_height = 22.0f32;

    let mut add_field = |label: &str, value: &str, y: &mut f32, max_lines: usize| {
        let full_text = format!("{label}: {value}");
        let lines = break_lines(
            &full_text,
            max_text_width,
            REGULAR_FONT_BYTES,
            16.0,
            max_lines,
        );
        for line in lines {
            if *y > max_usable_y {
                break;
            }
            text_elements.push_str(&format!(
                "<text x=\"{left_x}\" y=\"{:.1}\" font-size=\"16\" font-family=\"'IBMPlexSansThai', 'Liberation Sans', sans-serif\" fill=\"#374151\">{}</text>\n",
                *y, escape_xml(&line)
            ));
            *y += line_height;
        }
        *y += 6.0;
    };

    add_field(labels.holder_prefix, holder_name, &mut current_y, 4);
    add_field(labels.quiz_prefix, quiz_title, &mut current_y, 4);
    let score_str = format!("{score_correct}/{score_total}");
    add_field(labels.result_prefix, &score_str, &mut current_y, 1);
    add_field(labels.date_prefix, &date_str, &mut current_y, 1);

    if current_y <= max_usable_y {
        current_y += 6.0;
        let code_line = format!("{}: {code}", labels.code_prefix);
        text_elements.push_str(&format!(
            "<text x=\"{left_x}\" y=\"{:.1}\" font-size=\"14\" font-family=\"'IBMPlexSansThai', monospace\" font-weight=\"bold\" fill=\"#1f2937\">{}</text>\n",
            current_y, escape_xml(&code_line)
        ));
        current_y += 20.0;
    }

    if current_y <= max_usable_y {
        let url_line = format!("{}: {verification_url}", labels.verify_prefix);
        let url_lines = break_lines(&url_line, max_text_width, REGULAR_FONT_BYTES, 12.0, 2);
        for uline in url_lines {
            if current_y > max_usable_y {
                break;
            }
            text_elements.push_str(&format!(
                "<text x=\"{left_x}\" y=\"{:.1}\" font-size=\"12\" font-family=\"'IBMPlexSansThai', monospace\" fill=\"#6b7280\">{}</text>\n",
                current_y, escape_xml(&uline)
            ));
            current_y += 16.0;
        }
    }

    // QR modules
    let mut qr_modules = String::new();
    if let Ok(qr) = qrcode::QrCode::new(verification_url.as_bytes()) {
        let qr_width = qr.width();
        let colors = qr.to_colors();
        let module_size = 4;
        let quiet = 4;
        let total_size = (qr_width + quiet * 2) * module_size;
        let qr_x = 550;
        let qr_y = 110;

        qr_modules.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#ffffff\" stroke=\"#e5e7eb\" stroke-width=\"1\"/>\n",
            qr_x, qr_y, total_size, total_size
        ));
        qr_modules.push_str("<g id=\"qr-code\">\n");
        for y in 0..qr_width {
            for x in 0..qr_width {
                if colors[y * qr_width + x] == qrcode::Color::Dark {
                    let px = qr_x + (x + quiet) * module_size;
                    let py = qr_y + (y + quiet) * module_size;
                    qr_modules.push_str(&format!(
                        "<rect x=\"{px}\" y=\"{py}\" width=\"{module_size}\" height=\"{module_size}\" fill=\"#000000\"/>\n"
                    ));
                }
            }
        }
        qr_modules.push_str("</g>\n");
    }

    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"800\" height=\"500\" viewBox=\"0 0 800 500\">\n\
         <defs>\n\
         <style>\n\
         @font-face {{\n\
           font-family: 'IBMPlexSansThai';\n\
           font-weight: normal;\n\
           font-style: normal;\n\
           src: url('data:font/ttf;base64,{}') format('truetype');\n\
         }}\n\
         @font-face {{\n\
           font-family: 'IBMPlexSansThai';\n\
           font-weight: bold;\n\
           font-style: normal;\n\
           src: url('data:font/ttf;base64,{}') format('truetype');\n\
         }}\n\
         text {{\n\
           font-family: 'IBMPlexSansThai', 'Liberation Sans', sans-serif;\n\
         }}\n\
         </style>\n\
         </defs>\n\
         <rect x=\"15\" y=\"15\" width=\"770\" height=\"470\" rx=\"6\" fill=\"#ffffff\" stroke=\"#1f2937\" stroke-width=\"3\"/>\n\
         <rect x=\"22\" y=\"22\" width=\"756\" height=\"456\" rx=\"4\" fill=\"none\" stroke=\"#d1d5db\" stroke-width=\"1\"/>\n\
         {text_elements}\
         {qr_modules}\
         </svg>",
        *REGULAR_FONT_BASE64, *BOLD_FONT_BASE64
    );
    svg.into_bytes()
}

fn map_cert_row(r: &sqlx::any::AnyRow) -> CertificateRecord {
    let code: String = r.get("verification_code");
    CertificateRecord {
        id: r.get("id"),
        verification_code: code.clone(),
        holder_public_id: r.get("holder_id"),
        holder_name: r.get("holder_name_snapshot"),
        quiz_id: r.get("quiz_id"),
        quiz_title: r.get("quiz_title_snapshot"),
        attempt_id: r.get("attempt_id"),
        score_correct: r.get("score_correct_snapshot"),
        score_total: r.get("score_total_snapshot"),
        status: r.get("status"),
        revocation_reason: r.get("revocation_reason"),
        issued_at: r.get("issued_at"),
        revoked_at: r.try_get("revoked_at").ok(),
        pdf_url: format!("/certificates/{code}/files/pdf"),
        image_url: format!("/certificates/{code}/files/image"),
    }
}

pub async fn issue_or_get_certificate(
    store: &Store,
    caller: &CallerIdentity,
    attempt_id: &str,
) -> Result<CertificateRecord> {
    let att = sqlx::query(
        "SELECT a.owner_id, a.quiz_id, a.quiz_version_id, a.status, a.score_correct, a.score_total,
                q.owner_id AS quiz_owner_id, q.visibility, q.origin, qv.title AS quiz_title
         FROM attempts a
         JOIN quizzes q ON q.id = a.quiz_id
         JOIN quiz_versions qv ON qv.id = a.quiz_version_id
         WHERE a.id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;

    let owner_id: String = att.get("owner_id");
    require_owner(caller, &owner_id)?;

    let status: String = att.get("status");
    if status != "completed" {
        return Err(Error::unprocessable(
            "attempt_not_completed: Only completed attempts can issue a certificate",
        ));
    }
    let visibility: String = att.get("visibility");
    if visibility != "public" {
        return Err(Error::unprocessable(
            "private_quiz: Private quizzes do not issue certificates",
        ));
    }
    let quiz_owner_id: String = att.get("quiz_owner_id");
    let quiz_origin: String = att.get("origin");
    if quiz_owner_id == caller.user_id && quiz_origin == ORIGIN_CREATOR && !caller.is_admin() {
        return Err(Error::forbidden(
            "creator_practice_only: Normal creators can practice their own manual quizzes without a certificate",
        ));
    }

    // Idempotent: return existing certificate if already issued for this attempt.
    if let Some(existing) = sqlx::query(
        "SELECT id, holder_id, quiz_id, attempt_id, verification_code, status, issued_at, revoked_at,
                holder_name_snapshot, quiz_title_snapshot, score_correct_snapshot, score_total_snapshot,
                revocation_reason
         FROM certificates WHERE attempt_id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&store.db)
    .await?
    {
        return Ok(map_cert_row(&existing));
    }

    let holder = sqlx::query("SELECT id, display_name FROM certificate_holders WHERE owner_id = $1")
        .bind(&caller.user_id)
        .fetch_optional(&store.db)
        .await?
        .ok_or_else(|| {
            Error::unprocessable(
                "identity_not_confirmed: Confirm your holder name via PUT /me/identity before issuing a certificate",
            )
        })?;
    let holder_id: String = holder.get("id");
    let holder_name: String = holder.get("display_name");
    let quiz_id: String = att.get("quiz_id");
    let quiz_version_id: String = att.get("quiz_version_id");
    let quiz_title: String = att.get("quiz_title");
    let score_correct: i64 = att.get("score_correct");
    let score_total: i64 = att.get("score_total");

    let quiz_language: Option<String> = sqlx::query_scalar(
        "SELECT qver.language
         FROM quiz_version_questions qvq
         JOIN question_versions qver ON qver.id = qvq.question_version_id
         WHERE qvq.quiz_version_id = $1
         LIMIT 1",
    )
    .bind(&quiz_version_id)
    .fetch_optional(&store.db)
    .await?;

    let cert_id = new_id();
    let code = format!("ASODYA-{}", &new_id()[..12].to_uppercase());
    let ts = now();
    let pdf_bytes = render_certificate_pdf(
        &holder_name,
        &quiz_title,
        score_correct,
        score_total,
        &code,
        ts,
        quiz_language.as_deref(),
    );
    let img_bytes = render_certificate_svg(
        &holder_name,
        &quiz_title,
        score_correct,
        score_total,
        &code,
        ts,
        quiz_language.as_deref(),
    );

    let pdf_ref = store
        .fsm
        .put_bytes(&format!("certificates/{cert_id}.pdf"), &pdf_bytes)
        .await?;
    let img_ref = store
        .fsm
        .put_bytes(&format!("certificates/{cert_id}.svg"), &img_bytes)
        .await?;

    sqlx::query(
        "INSERT INTO certificates(
            id, holder_id, quiz_id, attempt_id, verification_code, status, issued_at,
            holder_name_snapshot, quiz_title_snapshot, score_correct_snapshot, score_total_snapshot,
            pdf_fsm_ref, image_fsm_ref, revocation_reason
         ) VALUES($1, $2, $3, $4, $5, 'active', $6, $7, $8, $9, $10, $11, $12, '')",
    )
    .bind(&cert_id)
    .bind(&holder_id)
    .bind(&quiz_id)
    .bind(attempt_id)
    .bind(&code)
    .bind(ts)
    .bind(&holder_name)
    .bind(&quiz_title)
    .bind(score_correct)
    .bind(score_total)
    .bind(&pdf_ref)
    .bind(&img_ref)
    .execute(&store.db)
    .await?;

    let row = sqlx::query(
        "SELECT id, holder_id, quiz_id, attempt_id, verification_code, status, issued_at, revoked_at,
                holder_name_snapshot, quiz_title_snapshot, score_correct_snapshot, score_total_snapshot,
                revocation_reason
         FROM certificates WHERE id = $1",
    )
    .bind(&cert_id)
    .fetch_one(&store.db)
    .await?;
    Ok(map_cert_row(&row))
}

pub async fn verify_certificate(
    store: &Store,
    code: &str,
) -> Result<PublicCertificateVerification> {
    let row = sqlx::query(
        "SELECT holder_id, attempt_id, verification_code, status, issued_at, revoked_at,
                holder_name_snapshot, quiz_title_snapshot, score_correct_snapshot, score_total_snapshot,
                revocation_reason
         FROM certificates WHERE verification_code = $1",
    )
    .bind(code)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;

    let attempt_id: String = row.get("attempt_id");
    let activity_summary = attempts::load_activity_summary(store, &attempt_id).await?;

    Ok(PublicCertificateVerification {
        verification_code: row.get("verification_code"),
        status: row.get("status"),
        revocation_reason: row.get("revocation_reason"),
        holder_public_id: row.get("holder_id"),
        holder_name: row.get("holder_name_snapshot"),
        quiz_title: row.get("quiz_title_snapshot"),
        score_correct: row.get("score_correct_snapshot"),
        score_total: row.get("score_total_snapshot"),
        issued_at: row.get("issued_at"),
        revoked_at: row.try_get("revoked_at").ok(),
        activity_summary,
    })
}

pub async fn get_certificate_file(
    store: &Store,
    code: &str,
    format: &str,
) -> Result<(&'static str, Vec<u8>)> {
    let row = sqlx::query(
        "SELECT pdf_fsm_ref, image_fsm_ref FROM certificates WHERE verification_code = $1",
    )
    .bind(code)
    .fetch_optional(&store.db)
    .await?
    .ok_or_else(Error::missing)?;

    let (content_type, fsm_ref): (&'static str, String) = match format {
        "pdf" => ("application/pdf", row.get("pdf_fsm_ref")),
        "image" | "svg" => ("image/svg+xml", row.get("image_fsm_ref")),
        _ => {
            return Err(Error::unprocessable(
                "invalid_format: Format must be pdf or image",
            ));
        }
    };
    let bytes = store.fsm.get_bytes(&fsm_ref).await?;
    Ok((content_type, bytes))
}

pub async fn admin_revoke_certificate(
    store: &Store,
    caller: &CallerIdentity,
    code: &str,
    reason: &str,
) -> Result<()> {
    if !caller.is_admin() {
        return Err(Error::forbidden("admin_required: Admin access required"));
    }
    let clean = reason.trim();
    if clean.is_empty() {
        return Err(Error::unprocessable(
            "invalid_reason: Revocation reason is required",
        ));
    }
    let res = sqlx::query(
        "UPDATE certificates SET status = 'revoked', revoked_at = $1, revocation_reason = $2 WHERE verification_code = $3",
    )
    .bind(now())
    .bind(clean)
    .bind(code)
    .execute(&store.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(Error::missing());
    }
    Ok(())
}
