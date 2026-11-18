pub mod cortex;
pub mod documents;
pub mod fsm;
pub mod reports;
pub mod scanner;
pub mod schemas;

use crate::core::{Error, Result, Settings};
use sqlx::{AnyPool, ConnectOptions, any::AnyPoolOptions};
use std::sync::Arc;

pub const SQLITE_MIGRATIONS: [&str; 6] = [
    include_str!("../../migrations/001_v2_schema.sql"),
    include_str!("../../tests/sqlite/002_v2_studies_sources.sql"),
    include_str!("../../tests/sqlite/003_v2_generation_events.sql"),
    include_str!("../../tests/sqlite/004_v2_authoring_origin.sql"),
    include_str!("../../tests/sqlite/005_v2_attempt_activity.sql"),
    include_str!("../../tests/sqlite/006_v2_certificates_snapshot.sql"),
];

#[derive(Clone)]
pub struct Store {
    pub db: AnyPool,
    pub docs: documents::Documents,
    pub fsm: Arc<dyn fsm::FsmClient>,
    pub scanner: Arc<dyn scanner::VirusScanner>,
    pub cortex: Arc<dyn cortex::CortexClient>,
    pub reports: Arc<dyn reports::ReportsClient>,
}

impl Store {
    pub async fn connect(settings: &Settings) -> Result<Self> {
        sqlx::any::install_default_drivers();
        if settings.database_url.is_empty() {
            return Err(Error::unavailable());
        }
        let db = AnyPoolOptions::new()
            .max_connections(5)
            .connect(&settings.database_url)
            .await?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| Error::unavailable())?;
        Ok(Self {
            db,
            docs: documents::Documents::remote(settings, http.clone()),
            fsm: fsm::HttpFsm::client(&settings.fsm_url, &settings.fsm_key, http.clone()),
            scanner: Arc::new(scanner::ClamScanner::new("clamscan")),
            cortex: cortex::HttpCortex::client(&settings.cortex_url, http.clone()),
            reports: reports::HttpReports::client(&settings.reports_url, http),
        })
    }

    pub async fn fixture() -> Self {
        sqlx::any::install_default_drivers();
        let db = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&db)
            .await
            .unwrap();
        for migration in SQLITE_MIGRATIONS {
            sqlx::raw_sql(migration).execute(&db).await.unwrap();
        }
        Self {
            db,
            docs: documents::Documents::Memory(documents::Documents::memory()),
            fsm: fsm::MemoryFsm::new(),
            scanner: Arc::new(CleanFixtureScanner),
            cortex: Arc::new(cortex::DisabledCortex),
            reports: Arc::new(reports::DisabledReports),
        }
    }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.db).await?;
        Ok(())
    }

    pub fn postgres(&self) -> bool {
        self.db
            .connect_options()
            .to_url_lossy()
            .scheme()
            .starts_with("postgres")
    }
}

struct CleanFixtureScanner;

impl scanner::VirusScanner for CleanFixtureScanner {
    fn scan_bytes<'a>(&'a self, bytes: &'a [u8]) -> scanner::BoxFuture<'a, scanner::ScanVerdict> {
        Box::pin(async move {
            if bytes.windows(5).any(|w| w == b"EICAR") {
                scanner::ScanVerdict::Infected("Eicar-Test-Signature".into())
            } else {
                scanner::ScanVerdict::Clean
            }
        })
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn memory_docs(docs: &documents::Documents) -> Option<Arc<documents::MemoryDocs>> {
    match docs {
        documents::Documents::Memory(m) => Some(m.clone()),
        documents::Documents::Remote(_) => None,
    }
}
