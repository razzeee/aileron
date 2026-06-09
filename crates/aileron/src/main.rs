mod app;
mod pages;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("aileron=info".parse().unwrap()),
        )
        .init();

    libadwaita::init().expect("failed to initialise libadwaita");
    let application = app::build_app();
    use gio::prelude::ApplicationExtManual;
    application.run();
}
