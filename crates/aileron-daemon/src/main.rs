use tracing::info;
use tracing_subscriber::EnvFilter;

mod assignments;
mod container;
mod handlers;
mod permissions;
mod service;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("aileron_daemon=info".parse()?))
        .init();

    info!("aileron-daemon starting");

    // Remove any stale socket from a previous crash.
    aileron_ipc::server::remove_stale_socket()?;

    let shared = state::SharedState::load().await?;
    service::run(shared).await?;

    Ok(())
}
