/// Varlink handler for `aileron.Models`.
use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects
use aileron_varlink::aileron_Models::{
    Call_AssignUseCase, Call_Delete, Call_List, Call_Pull, ModelInfo, PullProgress,
    VarlinkCallError, VarlinkInterface,
};

fn io_err(_msg: impl std::fmt::Display) -> varlink::Error {
    varlink::Error::from(varlink::ErrorKind::Io(std::io::ErrorKind::Other))
}

pub struct ModelsHandler {
    state: SharedState,
}

impl ModelsHandler {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }
}

impl VarlinkInterface for ModelsHandler {
    fn list(&self, call: &mut dyn Call_List) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let output = tokio::process::Command::new("podman")
                .args(["images", "--format", "json"])
                .output()
                .await
                .map_err(io_err)?;

            let images: Vec<PodmanImage> =
                serde_json::from_slice(&output.stdout).unwrap_or_default();

            let guard = self.state.0.lock().await;
            let assignments = guard.assignments.all();

            let models: Vec<ModelInfo> = images
                .into_iter()
                .map(|img| {
                    let image_ref = img
                        .names
                        .as_ref()
                        .and_then(|n| n.first())
                        .cloned()
                        .unwrap_or_else(|| img.id.clone());
                    let use_cases: Vec<String> = assignments
                        .iter()
                        .filter(|(_, v)| v.as_str() == image_ref)
                        .map(|(k, _)| k.clone())
                        .collect();
                    ModelInfo {
                        image_ref,
                        use_cases,
                        size_bytes: img.size.unwrap_or(0) as i64,
                        pulled_at: img.created.clone().unwrap_or_default(),
                    }
                })
                .collect();

            call.reply(models)
        })
    }

    fn pull(&self, call: &mut dyn Call_Pull, image_ref: String) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let status = tokio::process::Command::new("podman")
                .args(["pull", &image_ref])
                .status()
                .await
                .map_err(io_err)?;

            if !status.success() {
                return call.reply_pull_failed(image_ref, "podman pull failed".to_string());
            }

            call.reply(PullProgress {
                image_ref,
                bytes_pulled: 0,
                total_bytes: 0,
                done: true,
            })
        })
    }

    fn delete(&self, call: &mut dyn Call_Delete, image_ref: String) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let status = tokio::process::Command::new("podman")
                .args(["rmi", &image_ref])
                .status()
                .await
                .map_err(io_err)?;

            if !status.success() {
                return call.reply_image_not_found(image_ref);
            }

            let mut guard = self.state.0.lock().await;
            let _ = guard.assignments.remove_image(&image_ref);
            call.reply()
        })
    }

    fn assign_use_case(
        &self,
        call: &mut dyn Call_AssignUseCase,
        image_ref: String,
        use_case: String,
    ) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            guard
                .assignments
                .assign(use_case, image_ref)
                .map_err(io_err)?;
            call.reply()
        })
    }
}

#[derive(serde::Deserialize, Default)]
struct PodmanImage {
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Names")]
    names: Option<Vec<String>>,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "Created")]
    created: Option<String>,
}
