use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod assignments;
mod config;
mod container;
mod handlers;
mod permissions;
mod service;
mod state;

pub use config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("aileron_daemon=info".parse()?))
        .init();

    let config = Config::parse();

    if config.allow_all {
        tracing::warn!("AILERON_ALLOW_ALL is set — all permission checks are bypassed");
    }
    if config.auto_grant {
        tracing::warn!("AILERON_AUTO_GRANT is set — first-use permissions are granted automatically");
    }

    info!("aileron-daemon starting");

    aileron_ipc::server::remove_stale_socket()?;

    let shared = state::SharedState::load(config).await?;
    service::run(shared).await?;

    Ok(())
}
