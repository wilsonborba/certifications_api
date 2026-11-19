use crate::{
    core::{Error, Result, now},
    dal::{Store, new_id},
    domain::{
        attempts::{self, ActivitySummary},
        auth::{CallerIdentity, require_owner},
        authoring::ORIGIN_CREATOR,
    },
};
use serde::Serialize;
use sqlx::Row;

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

fn escape_pdf_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn render_certificate_pdf(
    holder_name: &str,
    quiz_title: &str,
    score_correct: i64,
    score_total: i64,
    code: &str,
) -> Vec<u8> {
    let line1 = escape_pdf_text(&format!("Asodya Completion Certificate ({code})"));
    let line2 = escape_pdf_text(&format!("Holder: {holder_name}"));
    let line3 = escape_pdf_text(&format!("Quiz: {quiz_title}"));
    let line4 = escape_pdf_text(&format!("Score: {score_correct}/{score_total}"));
    let stream = format!(
        "BT /F1 14 Tf 50 740 Td ({line1}) Tj 0 -24 Td ({line2}) Tj 0 -24 Td ({line3}) Tj 0 -24 Td ({line4}) Tj ET"
    );
    let pdf = format!(
        "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
         2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
         3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n\
         4 0 obj << /Length {} >> stream\n{stream}\nendstream endobj\n\
         5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
         trailer << /Root 1 0 R >>\n%%EOF\n",
        stream.len()
    );
    pdf.into_bytes()
}

fn render_certificate_svg(
    holder_name: &str,
    quiz_title: &str,
    score_correct: i64,
    score_total: i64,
    code: &str,
) -> Vec<u8> {
    let escape_xml = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"800\" height=\"500\">\
         <rect width=\"800\" height=\"500\" fill=\"#ffffff\" stroke=\"#1f2937\" stroke-width=\"4\"/>\
         <text x=\"40\" y=\"70\" font-size=\"28\" font-family=\"sans-serif\">Asodya Completion Certificate</text>\
         <text x=\"40\" y=\"130\" font-size=\"22\" font-family=\"sans-serif\">Holder: {}</text>\
         <text x=\"40\" y=\"180\" font-size=\"20\" font-family=\"sans-serif\">Quiz: {}</text>\
         <text x=\"40\" y=\"230\" font-size=\"20\" font-family=\"sans-serif\">Result: {}/{}</text>\
         <text x=\"40\" y=\"280\" font-size=\"16\" font-family=\"monospace\">Verification Code: {}</text>\
         </svg>",
        escape_xml(holder_name),
        escape_xml(quiz_title),
        score_correct,
        score_total,
        escape_xml(code)
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
    let quiz_title: String = att.get("quiz_title");
    let score_correct: i64 = att.get("score_correct");
    let score_total: i64 = att.get("score_total");

    let cert_id = new_id();
    let code = format!("ASODYA-{}", &new_id()[..12].to_uppercase());
    let pdf_bytes =
        render_certificate_pdf(&holder_name, &quiz_title, score_correct, score_total, &code);
    let img_bytes =
        render_certificate_svg(&holder_name, &quiz_title, score_correct, score_total, &code);

    let pdf_ref = store
        .fsm
        .put_bytes(&format!("certificates/{cert_id}.pdf"), &pdf_bytes)
        .await?;
    let img_ref = store
        .fsm
        .put_bytes(&format!("certificates/{cert_id}.svg"), &img_bytes)
        .await?;

    let ts = now();
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
