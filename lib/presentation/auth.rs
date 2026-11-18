use crate::{
    core::Error,
    domain::auth::{ADMIN_ACCESS_LEVEL, CallerIdentity, require_admin},
};
use axum::{
    extract::{FromRequestParts, Request},
    http::{HeaderMap, request::Parts},
    middleware::Next,
    response::Response,
};

pub fn decode_display_name(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = (*bytes.get(i + 1)? as char).to_digit(16)?;
            let lo = (*bytes.get(i + 2)? as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    let trimmed = decoded.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 200 || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn parse_access_level(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("x-access-level")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
}

pub struct SessionUser(pub CallerIdentity);

impl<S: Send + Sync> FromRequestParts<S> for SessionUser {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user_id = parts
            .headers
            .get("x-uuid")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
            .ok_or_else(|| Error::unauthorized("identity_required: Authentication required"))?
            .to_string();

        let access_level = parse_access_level(&parts.headers).unwrap_or(3);
        let display_name = parts
            .headers
            .get("x-display-name")
            .and_then(|v| v.to_str().ok())
            .and_then(decode_display_name);

        Ok(Self(CallerIdentity {
            user_id,
            access_level,
            display_name,
        }))
    }
}

pub struct AdminUser(pub Option<CallerIdentity>);

impl<S: Send + Sync> FromRequestParts<S> for AdminUser {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let level = parse_access_level(&parts.headers);
        require_admin(level)?;
        let caller = parts
            .headers
            .get("x-uuid")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
            .map(|uid| CallerIdentity {
                user_id: uid.to_string(),
                access_level: ADMIN_ACCESS_LEVEL,
                display_name: parts
                    .headers
                    .get("x-display-name")
                    .and_then(|v| v.to_str().ok())
                    .and_then(decode_display_name),
            });
        Ok(Self(caller))
    }
}

/// Middleware guard for `/admin/*` paths.
pub async fn admin_guard(request: Request, next: Next) -> Result<Response, Error> {
    if request.uri().path().starts_with("/admin") {
        require_admin(parse_access_level(request.headers()))?;
    }
    Ok(next.run(request).await)
}
