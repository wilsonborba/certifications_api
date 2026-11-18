pub mod logs;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error(pub StatusCode, pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self(StatusCode::UNAUTHORIZED, message.into())
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, message.into())
    }
    pub fn missing() -> Self {
        Self(
            StatusCode::NOT_FOUND,
            "not_found: Resource not found".into(),
        )
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, message.into())
    }
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self(StatusCode::UNPROCESSABLE_ENTITY, message.into())
    }
    pub fn unavailable() -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable: Required service is unavailable".into(),
        )
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (code, msg) = match self.1.split_once(": ") {
            Some((c, m)) if !c.is_empty() && !c.contains(' ') => (c, m),
            _ => (
                match self.0 {
                    StatusCode::BAD_REQUEST => "invalid_request",
                    StatusCode::UNAUTHORIZED => "unauthorized",
                    StatusCode::FORBIDDEN => "forbidden",
                    StatusCode::NOT_FOUND => "not_found",
                    StatusCode::CONFLICT => "conflict",
                    StatusCode::UNPROCESSABLE_ENTITY => "validation_failed",
                    StatusCode::TOO_MANY_REQUESTS => "rate_limited",
                    _ => "service_unavailable",
                },
                self.1.as_str(),
            ),
        };
        (self.0, Json(json!({ "error": code, "message": msg }))).into_response()
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        tracing::error!(module = "dal", "Database error: {err}");
        Self::unavailable()
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub development: bool,
    pub database_url: String,
    pub redis_url: String,
    pub redis_namespace: String,
    pub couch_url: String,
    pub couch_database: String,
    pub couch_user: String,
    pub couch_password: String,
    pub fsm_url: String,
    pub fsm_key: String,
    pub cortex_url: String,
    pub reports_url: String,
    pub gateway_url: String,
    pub certifications_service_key: String,
}

impl Settings {
    pub fn from_env() -> Self {
        let db_url = env("DATABASE_URL", "");
        let database_url = if !db_url.is_empty() {
            db_url
        } else {
            let host = env("CERTIFICATIONS_DB_HOST", "");
            let port = env("CERTIFICATIONS_DB_PORT", "5432");
            let user = env("CERTIFICATIONS_DB_USER", "");
            let pass = env("CERTIFICATIONS_DB_PASSWORD", "");
            let name = env("CERTIFICATIONS_DB_NAME", "");
            let ssl = env("CERTIFICATIONS_DB_SSLMODE", "prefer");
            if !host.is_empty() && !user.is_empty() && !name.is_empty() {
                format!("postgresql://{user}:{pass}@{host}:{port}/{name}?sslmode={ssl}")
            } else {
                String::new()
            }
        };
        Self {
            host: env("HOST", "0.0.0.0"),
            port: env("PORT", "8000").parse().unwrap_or(8000),
            development: matches!(
                env("DEVELOPMENT", "false").to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            ),
            database_url,
            redis_url: env("REDIS_URL", ""),
            redis_namespace: env("REDIS_NAMESPACE", "certifications_api:"),
            couch_url: env("COUCHDB_URL", ""),
            couch_database: env("COUCHDB_DATABASE", "certifications"),
            couch_user: env("COUCHDB_USER", ""),
            couch_password: env("COUCHDB_PASSWORD", ""),
            fsm_url: env("FSM_URL", ""),
            fsm_key: env("FSM_KEY", ""),
            cortex_url: env("CORTEX_URL", ""),
            reports_url: env("REPORTS_URL", ""),
            gateway_url: env("GATEWAY_URL", ""),
            certifications_service_key: env("CERTIFICATIONS_SERVICE_KEY", ""),
        }
    }
}
