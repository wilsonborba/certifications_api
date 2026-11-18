use certifications_api::{
    core::{Settings, logs},
    dal::Store,
    domain::App,
    presentation,
};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env();
    logs::init(&settings);
    let store = Store::connect(&settings).await?;
    let quotas = certifications_api::domain::quotas::QuotaCache::default();
    let _ = quotas.refresh_from_db(&store).await;
    let addr: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(module = "main", "Listening on {}", addr);
    axum::serve(
        listener,
        presentation::router(App {
            store,
            settings,
            quotas,
        }),
    )
    .await?;
    Ok(())
}
