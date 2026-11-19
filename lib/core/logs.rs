//! One line goes to journald and the live stream. Debug requires development mode.
use crate::core::Settings;
use std::{
    fmt,
    io::{self, Write},
    sync::Arc,
};
use tokio::sync::broadcast;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

pub const APP: &str = "certifications";
pub const MAX_LOG_LINE_BYTES: usize = 16_384;

#[derive(Clone)]
pub struct Redactor {
    secrets: Arc<Vec<String>>,
}
impl Redactor {
    pub fn new(settings: &Settings) -> Self {
        let mut secrets: Vec<String> = vec![
            settings.database_url.clone(),
            settings.redis_url.clone(),
            settings.couch_url.clone(),
            settings.couch_password.clone(),
            settings.fsm_key.clone(),
            settings.certifications_service_key.clone(),
        ];
        secrets.retain(|value| value.len() >= 3);
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self {
            secrets: Arc::new(secrets),
        }
    }
    pub fn clean(&self, raw: &str) -> String {
        let mut value = raw.replace(['\r', '\n'], "\\n");
        for secret in self.secrets.iter() {
            value = value.replace(secret, "[redacted]");
        }
        static PATTERNS: std::sync::LazyLock<Vec<(regex::Regex, &'static str)>> =
            std::sync::LazyLock::new(|| {
                vec![
                    (
                        regex::Regex::new(r"(?i)(bearer\s+)[^\s,;]+").unwrap(),
                        "${1}[redacted]",
                    ),
                    (
                        regex::Regex::new(r#"(?i)((?:"|')?(?:password|passwd|secret|token|authorization|cookie|csrf|(?:api|service|app)[_-]?key)(?:"|')?\s*[=:]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#).unwrap(),
                        "${1}[redacted]",
                    ),
                    (
                        regex::Regex::new(r"(https?://|postgres(?:ql)?://|redis://)[^/@\s]+@")
                            .unwrap(),
                        "${1}[redacted]@",
                    ),
                    (
                        regex::Regex::new(
                            r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
                        )
                        .unwrap(),
                        "[redacted]",
                    ),
                ]
            });
        for (pattern, replacement) in PATTERNS.iter() {
            value = pattern.replace_all(&value, *replacement).into_owned();
        }
        if value.len() > MAX_LOG_LINE_BYTES {
            let mut end = MAX_LOG_LINE_BYTES;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
        }
        value
    }
}

#[derive(Clone)]
pub struct LineFormat {
    pub development: bool,
    pub redactor: Redactor,
}

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<String>,
    module: Option<String>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let key = field.name();
        let value = format!("{value:?}");
        if key == "message" {
            self.message = value;
        } else if key == "module" {
            self.module = Some(value.trim_matches('"').to_owned());
        } else if [
            "password",
            "token",
            "secret",
            "cookie",
            "authorization",
            "api_key",
            "service_key",
        ]
        .iter()
        .any(|word| key.to_ascii_lowercase().contains(word))
        {
            self.fields.push(format!("{key}=[redacted]"));
        } else {
            self.fields.push(format!("{key}={value}"));
        }
    }
}

impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        if *event.metadata().level() == tracing::Level::DEBUG && !self.development {
            return Ok(());
        }
        let level = if *event.metadata().level() == tracing::Level::WARN {
            "WARNING"
        } else {
            event.metadata().level().as_str()
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let module = fields
            .module
            .as_deref()
            .unwrap_or(event.metadata().module_path().unwrap_or(APP));
        let module = self
            .redactor
            .clean(module)
            .replace(char::is_whitespace, "_");
        let message = if fields.fields.is_empty() {
            fields.message
        } else {
            format!("{} {}", fields.message, fields.fields.join(" "))
        };
        writeln!(
            writer,
            "{} {level} {APP} {module} {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            self.redactor.clean(&message)
        )
    }
}

#[derive(Clone)]
pub struct LogWriter(pub broadcast::Sender<String>);

pub struct LineWriter {
    sender: broadcast::Sender<String>,
    buffer: Vec<u8>,
}

impl Write for LineWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::stderr().write_all(bytes)?;
        for &byte in bytes {
            if byte == b'\n' {
                if !self.buffer.is_empty() {
                    let _ = self
                        .sender
                        .send(String::from_utf8_lossy(&self.buffer).into_owned());
                }
                self.buffer.clear();
            } else if self.buffer.len() < MAX_LOG_LINE_BYTES + 1024 {
                self.buffer.push(byte);
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LineWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LineWriter {
            sender: self.0.clone(),
            buffer: Vec::new(),
        }
    }
}

pub fn sender() -> broadcast::Sender<String> {
    static SENDER: std::sync::OnceLock<broadcast::Sender<String>> = std::sync::OnceLock::new();
    SENDER.get_or_init(|| broadcast::channel(1024).0).clone()
}

pub fn init(settings: &Settings) {
    let filter = if settings.development {
        "certifications_api=debug,info"
    } else {
        "certifications_api=info,warn"
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .event_format(LineFormat {
            development: settings.development,
            redactor: Redactor::new(settings),
        })
        .with_writer(LogWriter(sender()))
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone)]
    struct VecWriter(Arc<Mutex<Vec<u8>>>);
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
        type Writer = VecWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_settings(development: bool) -> Settings {
        Settings {
            host: "127.0.0.1".into(),
            port: 8000,
            development,
            database_url: "postgres://user:supersecret@db.internal:5432/cert".into(),
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
            certifications_service_key: "svc-secret-key".into(),
            clamav: Default::default(),
        }
    }

    #[test]
    fn line_format_gates_debug_normalizes_warning_and_redacts_before_output() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let fmt_layer = tracing_subscriber::fmt::layer()
            .event_format(LineFormat {
                development: false,
                redactor: Redactor::new(&test_settings(false)),
            })
            .with_writer(VecWriter(buf.clone()));
        let sub = tracing_subscriber::registry().with(fmt_layer);
        tracing::subscriber::with_default(sub, || {
            tracing::debug!(module = "test", "hidden debug");
            tracing::warn!(
                module = "test",
                "warn with svc-secret-key and Bearer abc.def"
            );
        });
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("hidden debug"));
        assert!(out.contains(" WARNING certifications test "));
        assert!(!out.contains("svc-secret-key"));
        assert!(!out.contains("abc.def"));
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn development_emits_debug() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let fmt_layer = tracing_subscriber::fmt::layer()
            .event_format(LineFormat {
                development: true,
                redactor: Redactor::new(&test_settings(true)),
            })
            .with_writer(VecWriter(buf.clone()));
        let sub = tracing_subscriber::registry().with(fmt_layer);
        tracing::subscriber::with_default(sub, || {
            tracing::debug!(module = "test", "visible debug");
        });
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains(" DEBUG certifications test visible debug"));
    }
}
