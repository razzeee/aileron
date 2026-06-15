#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aileron_daemon::run_main().await
}
