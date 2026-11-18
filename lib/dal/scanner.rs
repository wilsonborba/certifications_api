//! Bounded `clamscan` and `freshclam` child-process adapters supervised by the backend.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Semaphore, time::timeout};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_CONCURRENT_SCANS: usize = 2;
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
pub const FRESHCLAM_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Infected(String),
    Unavailable(String),
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
