use crate::core::{Error, Result, Settings};
use sqlx::{AnyPool, ConnectOptions, any::AnyPoolOptions};

#[derive(Clone)]
pub struct Store {
    pub db: AnyPool,
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
        Ok(Self { db })
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
        Self { db }
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
