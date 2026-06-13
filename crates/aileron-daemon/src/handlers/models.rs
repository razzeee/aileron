/// Varlink handler for `aileron.Models`.
use std::path::PathBuf;

use crate::profiles::{ArtifactHash, Profile, RuntimeImage as StoredRuntimeImage};
use crate::state::SharedState;
#[allow(unused_imports)]
use aileron_varlink::aileron_Models::{
    Call_AssignUseCase, Call_DeleteProfile, Call_InstallManifest, Call_InstallUrlProfile,
    Call_List, Call_ListRuntimeManifests, InstallProgress, ProfileInfo, RuntimeImage,
    RuntimeManifestInfo, UseCaseConflict, VarlinkCallError, VarlinkInterface,
};

const USE_CASES: &[&str] = &[
    "llm.summarize",
    "llm.translate",
    "llm.rephrase",
    "llm.classify",
    "llm.extract",
    "llm.analyze",
    "asr.transcribe",
    "vision.describe",
    "vision.segment",
];

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
            let guard = self.state.0.lock().await;
            let profiles: Vec<ProfileInfo> = guard
                .profiles
                .all()
                .map(|profile| {
                    let assigned_use_cases = guard
                        .assignments
                        .all()
                        .iter()
                        .filter(|(_, assigned)| assigned.as_str() == profile.profile_id)
                        .map(|(use_case, _)| use_case.clone())
                        .collect();
                    let runtime_images = if profile.runtime_images.is_empty() {
                        guard.runtimes.images_for(&profile.runtime_id)
                    } else {
                        profile.runtime_images.clone()
                    };
                    ProfileInfo {
                        profile_id: profile.profile_id.clone(),
                        model_id: profile.model_id.clone(),
                        runtime_id: profile.runtime_id.clone(),
                        artifact_path: profile.artifact_path.display().to_string(),
                        runtime_images: runtime_images
                            .iter()
                            .map(|image| RuntimeImage {
                                variant: image.variant.clone(),
                                image_ref: image.image_ref.clone(),
                            })
                            .collect(),
                        use_cases: profile.use_cases.clone(),
                        assigned_use_cases,
                        installed_at: profile.installed_at.clone(),
                    }
                })
                .collect();

            call.reply(profiles)
        })
    }

    fn install_manifest(
        &self,
        call: &mut dyn Call_InstallManifest,
        profile_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let path = match crate::manifests::find_model_manifest(&profile_id) {
                Ok(Some(path)) => path,
                Ok(None) => {
                    return call.reply_install_failed(profile_id, "manifest not found".to_string());
                }
                Err(e) => return call.reply_install_failed(profile_id, e.to_string()),
            };
            let (auto_assigned, conflicts) =
                match install_manifest_path(&self.state, path, Some(profile_id.clone())).await {
                    Ok(result) => result,
                    Err(e) => return call.reply_install_failed(profile_id, e.to_string()),
                };

            call.reply(
                InstallProgress {
                    profile_id,
                    bytes_pulled: 0,
                    total_bytes: 0,
                    done: true,
                },
                auto_assigned,
                conflicts,
            )
        })
    }

    fn list_runtime_manifests(
        &self,
        call: &mut dyn Call_ListRuntimeManifests,
    ) -> varlink::Result<()> {
        let runtimes = self.rt.block_on(async {
            let guard = self.state.0.lock().await;
            guard
                .runtimes
                .all()
                .into_iter()
                .map(|runtime| RuntimeManifestInfo {
                    runtime_id: runtime.runtime_id,
                    variants: runtime.variants,
                })
                .collect()
        });
        call.reply(runtimes)
    }

    fn install_url_profile(
        &self,
        call: &mut dyn Call_InstallUrlProfile,
        runtime_id: String,
        url: String,
        sha256: String,
        use_cases: Vec<String>,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let filename = match filename_from_url(&url) {
                Ok(filename) => filename,
                Err(e) => return call.reply_install_failed(url, e.to_string()),
            };
            let model_id = generated_model_id(&runtime_id, &filename, &sha256);
            let profile_id = model_id.clone();
            let manifest = ModelManifest {
                profile_id: profile_id.clone(),
                model_id,
                runtime_id,
                runtime_images: Vec::new(),
                use_cases,
                artifacts: vec![ManifestArtifact {
                    url,
                    filename,
                    sha256,
                }],
            };
            let (auto_assigned, conflicts) =
                match install_manifest_data(&self.state, manifest).await {
                    Ok(result) => result,
                    Err(e) => return call.reply_install_failed(profile_id, e.to_string()),
                };

            call.reply(
                InstallProgress {
                    profile_id,
                    bytes_pulled: 0,
                    total_bytes: 0,
                    done: true,
                },
                auto_assigned,
                conflicts,
            )
        })
    }

    fn delete_profile(
        &self,
        call: &mut dyn Call_DeleteProfile,
        profile_id: String,
        force: bool,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if guard.profiles.get(&profile_id).is_none() {
                return call.reply_profile_not_found(profile_id);
            }

            let assigned = guard
                .assignments
                .all()
                .values()
                .any(|assigned| assigned == &profile_id);
            let active_sessions: Vec<String> = guard
                .sessions
                .iter()
                .filter(|(_, session)| session.profile_id == profile_id)
                .map(|(session_id, _)| session_id.clone())
                .collect();

            if !force && (assigned || !active_sessions.is_empty()) {
                return call.reply_profile_in_use(profile_id);
            }

            for session_id in active_sessions {
                guard.sessions.remove(&session_id);
            }
            guard.containers.kill(&profile_id);
            let _ = guard.assignments.remove_profile(&profile_id);
            guard.profiles.remove(&profile_id).map_err(io_err)?;
            call.reply()
        })
    }

    fn assign_use_case(
        &self,
        call: &mut dyn Call_AssignUseCase,
        profile_id: String,
        use_case: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if guard.profiles.get(&profile_id).is_none() {
                return call.reply_profile_not_found(profile_id);
            }
            guard
                .assignments
                .assign(use_case, profile_id)
                .map_err(io_err)?;
            call.reply()
        })
    }
}

#[derive(serde::Deserialize)]
struct ModelManifest {
    profile_id: String,
    model_id: String,
    runtime_id: String,
    #[serde(default)]
    runtime_images: Vec<StoredRuntimeImage>,
    use_cases: Vec<String>,
    #[serde(default)]
    artifacts: Vec<ManifestArtifact>,
}

fn resolve_runtime_image<'a>(
    guard: &'a crate::state::Inner,
    profile: &'a Profile,
) -> Option<&'a str> {
    guard
        .runtimes
        .resolve(&profile.runtime_id, guard.variant)
        .or_else(|| profile.runtime_image_for(guard.variant))
}

fn validate_use_cases(use_cases: &[String]) -> anyhow::Result<()> {
    if use_cases.is_empty() {
        anyhow::bail!("at least one use-case is required");
    }
    for use_case in use_cases {
        if !USE_CASES.contains(&use_case.as_str()) {
            anyhow::bail!("unsupported use-case: {use_case}");
        }
    }
    Ok(())
}

fn filename_from_url(url: &str) -> anyhow::Result<String> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let filename = without_query.rsplit('/').next().unwrap_or("").trim();
    if filename.is_empty() {
        anyhow::bail!("model file URL must end with a filename");
    }
    Ok(filename.to_string())
}

fn generated_model_id(runtime_id: &str, filename: &str, sha256: &str) -> String {
    let stem = filename
        .rsplit_once('.')
        .map_or(filename, |(stem, _)| stem)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let prefix: String = sha256.chars().take(12).collect();
    format!("{runtime_id}-{stem}-{prefix}")
}

impl ModelManifest {
    fn into_profile(self, artifact_path: PathBuf) -> Profile {
        Profile {
            profile_id: self.profile_id,
            model_id: self.model_id,
            runtime_id: self.runtime_id,
            artifact_path,
            runtime_images: self.runtime_images,
            use_cases: self.use_cases,
            artifact_hashes: self
                .artifacts
                .into_iter()
                .map(|artifact| ArtifactHash {
                    filename: artifact.filename,
                    sha256: artifact.sha256,
                })
                .collect(),
            installed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

async fn install_manifest_path(
    state: &SharedState,
    path: PathBuf,
    requested_profile_id: Option<String>,
) -> anyhow::Result<(Vec<String>, Vec<UseCaseConflict>)> {
    let data = std::fs::read_to_string(&path)?;
    let manifest: ModelManifest = serde_json::from_str(&data)?;
    if let Some(requested) = requested_profile_id {
        if manifest.profile_id != requested {
            anyhow::bail!("manifest profile_id does not match requested profile");
        }
    }
    validate_use_cases(&manifest.use_cases)?;

    install_manifest_data(state, manifest).await
}

async fn install_manifest_data(
    state: &SharedState,
    manifest: ModelManifest,
) -> anyhow::Result<(Vec<String>, Vec<UseCaseConflict>)> {
    validate_use_cases(&manifest.use_cases)?;
    let artifact_dir = crate::profiles::model_dir(&manifest.model_id);
    install_artifacts(&artifact_dir, &manifest.artifacts)?;
    let profile = manifest.into_profile(artifact_dir);

    let runtime_image = {
        let guard = state.0.lock().await;
        resolve_runtime_image(&guard, &profile)
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "runtime {} does not support {}",
                    profile.runtime_id,
                    guard.variant.as_tag()
                )
            })?
    };

    pull_runtime_image(&runtime_image).await?;
    register_profile(state, profile).await
}

#[derive(serde::Deserialize)]
struct ManifestArtifact {
    url: String,
    filename: String,
    sha256: String,
}

async fn pull_runtime_image(image_ref: &str) -> anyhow::Result<()> {
    let exists = tokio::process::Command::new("podman")
        .args(["image", "exists", image_ref])
        .status()
        .await?;
    if exists.success() {
        return Ok(());
    }

    let status = tokio::process::Command::new("podman")
        .args(["pull", image_ref])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("podman pull failed for {image_ref}");
    }
    Ok(())
}

async fn register_profile(
    state: &SharedState,
    profile: Profile,
) -> anyhow::Result<(Vec<String>, Vec<UseCaseConflict>)> {
    let mut guard = state.0.lock().await;
    let profile_id = profile.profile_id.clone();
    let suggested = profile.use_cases.clone();
    guard.profiles.insert(profile)?;

    let mut auto_assigned = Vec::new();
    let mut conflicts = Vec::new();
    for use_case in suggested {
        validate_use_cases(std::slice::from_ref(&use_case))?;
        match guard.assignments.get(&use_case) {
            None => {
                guard
                    .assignments
                    .assign(use_case.clone(), profile_id.clone())?;
                auto_assigned.push(use_case);
            }
            Some(current) if current == profile_id => {}
            Some(current) => conflicts.push(UseCaseConflict {
                use_case,
                current_profile: current.to_string(),
                new_profile: profile_id.clone(),
            }),
        }
    }

    Ok((auto_assigned, conflicts))
}

fn install_artifacts(target_dir: &PathBuf, artifacts: &[ManifestArtifact]) -> anyhow::Result<()> {
    let temp_dir = target_dir.with_extension("tmp");
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    for artifact in artifacts {
        let dest = temp_dir.join(&artifact.filename);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = reqwest::blocking::get(&artifact.url)?
            .error_for_status()?
            .bytes()?;
        let actual = sha256_hex(&bytes);
        if actual != artifact.sha256.to_lowercase() {
            anyhow::bail!(
                "checksum mismatch for {}: expected {}, got {}",
                artifact.filename,
                artifact.sha256,
                actual
            );
        }
        std::fs::write(dest, bytes)?;
    }

    if target_dir.exists() {
        std::fs::remove_dir_all(target_dir)?;
    }
    std::fs::rename(temp_dir, target_dir)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
