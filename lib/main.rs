use certifications_api::{
    core::{Settings, logs},
    dal::Store,
    domain::{App, catalog},
    presentation,
};
use std::net::SocketAddr;

struct SearchRebuildArgs {
    write: bool,
    batch_size: usize,
    after_quiz_id: Option<String>,
}

fn parse_search_rebuild_args(args: &[String]) -> anyhow::Result<Option<SearchRebuildArgs>> {
    if args.first().map(String::as_str) != Some("--rebuild-public-question-search") {
        return Ok(None);
    }
    let mut parsed = SearchRebuildArgs {
        write: false,
        batch_size: catalog::DEFAULT_PUBLIC_SEARCH_REBUILD_BATCH,
        after_quiz_id: None,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--write" => parsed.write = true,
            "--batch-size" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--batch-size requires a value"))?;
                parsed.batch_size = value
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--batch-size must be an integer"))?;
            }
            "--after-quiz-id" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--after-quiz-id requires a value"))?;
                if value.trim().is_empty() {
                    anyhow::bail!("--after-quiz-id must not be empty");
                }
                parsed.after_quiz_id = Some(value.clone());
            }
            unsupported => anyhow::bail!("unsupported rebuild option: {unsupported}"),
        }
        index += 1;
    }
    Ok(Some(parsed))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env();
    logs::init(&settings);
    let store = Store::connect(&settings).await?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(rebuild) = parse_search_rebuild_args(&args)? {
        let summary = catalog::rebuild_public_question_search_batch(
            &store,
            rebuild.after_quiz_id.as_deref(),
            rebuild.batch_size,
            rebuild.write,
        )
        .await?;
        println!("{}", serde_json::to_string(&summary)?);
        if let Some(failed_quiz_id) = summary.failed_quiz_id {
            anyhow::bail!(
                "search projection rebuild stopped at quiz {failed_quiz_id}; resume after cursor {:?}",
                summary.next_cursor
            );
        }
        return Ok(());
    }
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
    res?;
    Ok(())
}
