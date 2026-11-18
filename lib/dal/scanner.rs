//! Bounded `clamscan` and `freshclam` child-process adapters supervised by the backend.
use serde::Serialize;
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};
use tokio::{process::Command, sync::Semaphore, task::JoinHandle, time::timeout};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_CONCURRENT_SCANS: usize = 2;
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
pub const FRESHCLAM_TIMEOUT: Duration = Duration::from_secs(120);
pub const FRESHCLAM_INTERVAL: Duration = Duration::from_secs(6 * 3600);
pub const SIGNATURE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Infected(String),
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureHealth {
    Fresh,
    Stale,
    Missing,
}

#[derive(Clone, Debug)]
pub struct SignatureState {
    health: Arc<RwLock<SignatureHealth>>,
}

impl Default for SignatureState {
    fn default() -> Self {
        Self::new(SignatureHealth::Missing)
    }
}

impl SignatureState {
    pub fn new(initial: SignatureHealth) -> Self {
        Self {
            health: Arc::new(RwLock::new(initial)),
        }
    }

    pub fn get(&self) -> SignatureHealth {
        self.health.read().unwrap().clone()
    }

    pub fn set(&self, health: SignatureHealth) {
        *self.health.write().unwrap() = health;
    }

    pub fn refresh_from_dir(&self, dir: &Path) -> SignatureHealth {
        let h = inspect_signature_dir(dir);
        self.set(h.clone());
        h
    }
}

pub fn inspect_signature_dir(dir: &Path) -> SignatureHealth {
    let candidates = ["daily.cvd", "daily.cld", "main.cvd", "main.cld"];
    let mut newest: Option<SystemTime> = None;
    for name in candidates {
        let p = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&p)
            && meta.len() > 0
            && let Ok(modified) = meta.modified()
        {
            newest = Some(match newest {
                Some(prev) if prev > modified => prev,
                _ => modified,
            });
        }
    }
    let Some(mtime) = newest else {
        return SignatureHealth::Missing;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) if age > SIGNATURE_MAX_AGE => SignatureHealth::Stale,
        _ => SignatureHealth::Fresh,
    }
}

pub trait VirusScanner: Send + Sync {
    fn scan_bytes<'a>(&'a self, bytes: &'a [u8]) -> BoxFuture<'a, ScanVerdict>;
}

pub struct ClamScanner {
    binary: String,
    semaphore: Arc<Semaphore>,
}

impl ClamScanner {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_SCANS)),
        }
    }
}

impl VirusScanner for ClamScanner {
    fn scan_bytes<'a>(&'a self, bytes: &'a [u8]) -> BoxFuture<'a, ScanVerdict> {
        Box::pin(async move {
            let Ok(_permit) = self.semaphore.acquire().await else {
                return ScanVerdict::Unavailable("scanner_closed".into());
            };
            let Ok(tmp) = tempfile::NamedTempFile::new() else {
                return ScanVerdict::Unavailable("temp_file_failed".into());
            };
            if tokio::fs::write(tmp.path(), bytes).await.is_err() {
                return ScanVerdict::Unavailable("temp_write_failed".into());
            }
            let mut cmd = Command::new(&self.binary);
            cmd.arg("--no-summary").arg("--infected").arg(tmp.path());
            cmd.kill_on_drop(true);
            let output = match timeout(SCAN_TIMEOUT, cmd.output()).await {
                Ok(Ok(out)) => out,
                Ok(Err(_)) => return ScanVerdict::Unavailable("scanner_unavailable".into()),
                Err(_) => return ScanVerdict::Unavailable("scanner_timeout".into()),
            };
            match output.status.code() {
                Some(0) => ScanVerdict::Clean,
                Some(1) => {
                    let sig = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    ScanVerdict::Infected(if sig.is_empty() {
                        "malware_detected".into()
                    } else {
                        sig
                    })
                }
                _ => ScanVerdict::Unavailable("scanner_error".into()),
            }
        })
    }
}

/// Run a bounded one-shot `freshclam` signature update supervised by the backend.
pub async fn run_freshclam_once(binary: &str) -> Result<(), String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--quiet").kill_on_drop(true);
    let output = timeout(FRESHCLAM_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "freshclam_timeout".to_string())?
        .map_err(|_| "freshclam_unavailable".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("freshclam_failed: {:?}", output.status.code()))
    }
}

/// Supervised task that runs `freshclam` once at startup and on a fixed interval,
/// refreshing `SignatureState` after each run. Aborting the handle stops it on shutdown.
pub fn spawn_freshclam_supervisor(
    binary: String,
    sig_dir: std::path::PathBuf,
    signatures: SignatureState,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(err) = run_freshclam_once(&binary).await {
                tracing::warn!(module = "scanner", error = %err, "Freshclam one-shot update failed");
            }
            signatures.refresh_from_dir(&sig_dir);
        }
    })
}
