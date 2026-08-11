mod app;
mod async_runtime;
mod pages;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("aileron=info".parse().unwrap()),
        )
        .init();

    app::run();
}
