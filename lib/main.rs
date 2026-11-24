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
    let generation_worker = tokio::spawn(
        certifications_api::domain::generations::run_queue_worker(store.clone()),
    );
    let quotas = certifications_api::domain::quotas::QuotaCache::default();
    let _ = quotas.refresh_from_db(&store).await;
    let sig_dir = std::path::PathBuf::from(&settings.clamav.signatures_dir);
    let signatures = certifications_api::dal::scanner::SignatureState::default();
    signatures.refresh_from_dir(&sig_dir);
    let freshclam_task = certifications_api::dal::scanner::spawn_freshclam_supervisor(
        settings.clamav.freshclam_bin.clone(),
        Some(std::path::PathBuf::from(&settings.clamav.freshclam_conf)),
        sig_dir,
        Some(std::path::PathBuf::from(&settings.clamav.lib_dir)),
        Some(std::path::PathBuf::from(&settings.clamav.certs_dir)),
        signatures.clone(),
        certifications_api::dal::scanner::FRESHCLAM_INTERVAL,
    );
    let addr: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(module = "main", "Listening on {}", addr);
    let res = axum::serve(
        listener,
        presentation::router(App {
            store,
            settings,
            quotas,
            signatures,
        }),
    )
    .await;
    freshclam_task.abort();
    generation_worker.abort();
    res?;
    Ok(())
}
