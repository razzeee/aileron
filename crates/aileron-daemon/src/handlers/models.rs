/// Varlink handler for `aileron.Models`.
use crate::state::SharedState;
#[allow(unused_imports)]
use aileron_varlink::aileron_Models::{
    Call_AssignUseCase, Call_Delete, Call_List, Call_Pull, ModelInfo, PullProgress,
    UseCaseConflict, VarlinkCallError, VarlinkInterface,
};

fn io_err(_msg: impl std::fmt::Display) -> varlink::Error {
    varlink::Error::from(varlink::ErrorKind::Io(std::io::ErrorKind::Other))
}

pub struct ModelsHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

impl ModelsHandler {
    pub fn new(state: SharedState, rt: tokio::runtime::Handle) -> Self {
        Self { state, rt }
    }
}

impl VarlinkInterface for ModelsHandler {
    fn list(&self, call: &mut dyn Call_List) -> varlink::Result<()> {
        self.rt.block_on(async {
            let output = tokio::process::Command::new("podman")
                .args(["images", "--format", "json"])
                .output()
                .await
                .map_err(io_err)?;

            let images: Vec<PodmanImage> = match serde_json::from_slice(&output.stdout) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("failed to parse podman images JSON: {e}");
                    vec![]
                }
            };

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
                        pulled_at: img.created,
                    }
                })
                .collect();

            call.reply(models)
        })
    }

    fn pull(&self, call: &mut dyn Call_Pull, image_ref: String) -> varlink::Result<()> {
        self.rt.block_on(async {
            let status = tokio::process::Command::new("podman")
                .args(["pull", &image_ref])
                .status()
                .await
                .map_err(io_err)?;

            if !status.success() {
                return call.reply_pull_failed(image_ref, "podman pull failed".to_string());
            }

            // Inspect image labels to find suggested use-cases.
            let suggested = read_use_case_label(&image_ref).await;

            // Resolve auto-assignments and conflicts.
            let mut guard = self.state.0.lock().await;
            let mut auto_assigned: Vec<String> = Vec::new();
            let mut conflicts: Vec<UseCaseConflict> = Vec::new();

            for use_case in suggested {
                match guard.assignments.get(&use_case) {
                    None => {
                        // Unassigned — assign automatically.
                        if let Err(e) = guard.assignments.assign(use_case.clone(), image_ref.clone()) {
                            tracing::warn!("auto-assign {use_case} failed: {e}");
                        } else {
                            auto_assigned.push(use_case);
                        }
                    }
                    Some(current) if current == image_ref => {
                        // Already assigned to this image — no-op.
                    }
                    Some(current) => {
                        conflicts.push(UseCaseConflict {
                            use_case,
                            current_image: current.to_string(),
                            new_image: image_ref.clone(),
                        });
                    }
                }
            }

            call.reply(
                PullProgress {
                    image_ref,
                    bytes_pulled: 0,
                    total_bytes: 0,
                    done: true,
                },
                auto_assigned,
                conflicts,
            )
        })
    }

    fn delete(&self, call: &mut dyn Call_Delete, image_ref: String) -> varlink::Result<()> {
        self.rt.block_on(async {
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
        self.rt.block_on(async {
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
    /// podman returns either a Unix timestamp (integer) or an RFC3339 string
    /// depending on the version — accept both.
    #[serde(rename = "Created", default, deserialize_with = "deserialize_created")]
    created: String,
}

fn deserialize_created<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum CreatedField {
        Timestamp(i64),
        Str(String),
    }
    Ok(match CreatedField::deserialize(d)? {
        CreatedField::Timestamp(ts) => {
            // Convert Unix timestamp to RFC3339.
            chrono::DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default()
        }
        CreatedField::Str(s) => s,
    })
}

/// Read the `aileron.use_cases` label from a local image via `podman inspect`.
/// Returns a list of use-case tokens, or an empty vec if the label is absent.
async fn read_use_case_label(image_ref: &str) -> Vec<String> {
    let output = tokio::process::Command::new("podman")
        .args(["inspect", "--format", "{{index .Labels \"aileron.use_cases\"}}", image_ref])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let label = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if label.is_empty() {
                return vec![];
            }
            label
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        _ => vec![],
    }
}
