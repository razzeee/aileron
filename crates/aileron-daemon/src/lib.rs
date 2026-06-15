pub mod assignments;
pub mod config;
pub mod container;
pub mod handlers;
pub mod hardware;
pub mod llmfit_metadata;
pub mod manifests;
pub mod permissions;
pub mod profiles;
pub mod service;
pub mod state;

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

pub use config::Config;

pub async fn run_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("aileron_daemon=info".parse()?),
        )
        .init();

    let config = Config::parse();

    if config.allow_all {
        tracing::warn!("AILERON_ALLOW_ALL is set — all permission checks are bypassed");
    }
    if config.auto_grant {
        tracing::warn!(
            "AILERON_AUTO_GRANT is set — first-use permissions are granted automatically"
        );
    }

    info!("aileron-daemon starting");

    aileron_ipc::server::remove_stale_socket()?;

    let shared = state::SharedState::load(config).await?;
    service::run(shared).await?;

    Ok(())
}
