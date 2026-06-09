/// Varlink service entry point for the daemon.

use anyhow::Result;
use tracing::info;

use crate::state::SharedState;
use crate::handlers::{InferenceHandler, ModelsHandler, PermissionsHandler, SessionsHandler};

pub async fn run(state: SharedState) -> Result<()> {
    let addr = aileron_ipc::varlink_address();
    info!("listening on {}", addr);

    // Spawn idle container eviction every 60 s.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut guard = state.0.lock().await;
                guard.containers.evict_idle();
            }
        });
    }

    let state_for_thread = state.clone();
    tokio::task::spawn_blocking(move || run_varlink_service(state_for_thread, &addr))
        .await??;

    Ok(())
}

fn run_varlink_service(state: SharedState, addr: &str) -> Result<()> {
    use aileron_varlink::aileron_Inference;
    use aileron_varlink::aileron_Models;
    use aileron_varlink::aileron_Permissions;
    use aileron_varlink::aileron_Sessions;

    let service = varlink::VarlinkService::new(
        "aileron",
        "Aileron local AI daemon",
        env!("CARGO_PKG_VERSION"),
        "https://github.com/aileron-project/aileron",
        vec![
            Box::new(aileron_Inference::new(Box::new(
                InferenceHandler::new(state.clone()),
            ))),
            Box::new(aileron_Models::new(Box::new(
                ModelsHandler::new(state.clone()),
            ))),
            Box::new(aileron_Permissions::new(Box::new(
                PermissionsHandler::new(state.clone()),
            ))),
            Box::new(aileron_Sessions::new(Box::new(
                SessionsHandler::new(state.clone()),
            ))),
        ],
    );

    varlink::listen(service, addr, &varlink::ListenConfig::default())?;
    Ok(())
}
