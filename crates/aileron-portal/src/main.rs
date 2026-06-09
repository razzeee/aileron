use tracing::info;
use tracing_subscriber::EnvFilter;

mod portal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("aileron_portal=info".parse()?),
        )
        .init();

    info!("aileron-portal starting");
    portal::run().await?;
    Ok(())
}
