/// Varlink handler for `aileron.Models`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::manifests::{self, ManifestArtifact, ModelManifest};
use crate::profiles::{ArtifactHash, Profile};
use crate::state::{InstallRecord, InstallSample, SharedState};
#[allow(unused_imports)]
use aileron_varlink::aileron_Models::{
    Call_AssignUseCase, Call_CancelInstall, Call_DeleteProfile, Call_InstallManifest,
    Call_InstallUrlProfile, Call_List, Call_ListCatalog, Call_ListInstalls, Call_ListRuntimeImages,
    Call_ListRuntimeManifests, Call_PruneUnusedRuntimeImages, Call_RemoveRuntimeImage,
    CatalogProfileInfo, InstallProgress, InstallStatus, OciRuntimeImage, ProfileInfo, RuntimeImage,
    RuntimeImageCleanupError, RuntimeManifestInfo, UseCaseConflict, UseCaseFitScore,
    VarlinkCallError, VarlinkInterface,
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
                        size_bytes: profile_artifact_size_bytes(&profile.artifact_path),
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

    fn list_runtime_images(&self, call: &mut dyn Call_ListRuntimeImages) -> varlink::Result<()> {
        let images = self.rt.block_on(async {
            let usage = runtime_image_usage(&self.state).await;
            list_aileron_runtime_images(&usage)
        });
        match images {
            Ok(images) => call.reply(images),
            Err(e) => Err(io_err(e)),
        }
    }

    fn remove_runtime_image(
        &self,
        call: &mut dyn Call_RemoveRuntimeImage,
        image_id: String,
    ) -> varlink::Result<()> {
        let result = self.rt.block_on(async {
            let usage = runtime_image_usage(&self.state).await;
            remove_aileron_runtime_image(&image_id, &usage).await
        });
        match result {
            Ok(()) => call.reply(),
            Err(e) => Err(io_err(e)),
        }
    }

    fn prune_unused_runtime_images(
        &self,
        call: &mut dyn Call_PruneUnusedRuntimeImages,
    ) -> varlink::Result<()> {
        let result = self.rt.block_on(async {
            let usage = runtime_image_usage(&self.state).await;
            let images = list_aileron_runtime_images(&usage)?;
            let mut removed = Vec::new();
            let mut errors = Vec::new();
            for image in images.into_iter().filter(|image| !image.in_use) {
                match remove_podman_image(&image.image_id).await {
                    Ok(()) => removed.push(image.image_ref),
                    Err(e) => errors.push(RuntimeImageCleanupError {
                        image_ref: image.image_ref,
                        reason: e.to_string(),
                    }),
                }
            }
            Ok::<_, anyhow::Error>((removed, errors))
        });
        match result {
            Ok((removed, errors)) => call.reply(removed, errors),
            Err(e) => Err(io_err(e)),
        }
    }

    fn list_installs(&self, call: &mut dyn Call_ListInstalls) -> varlink::Result<()> {
        let installs = self.rt.block_on(async {
            let guard = self.state.0.lock().await;
            guard
                .installing_profiles
                .iter()
                .chain(
                    guard
                        .recent_installs
                        .iter()
                        .map(|(profile_id, install)| (profile_id, install)),
                )
                .map(|(profile_id, install)| {
                    let bytes_per_second = install_bytes_per_second(install);
                    InstallStatus {
                        profile_id: profile_id.clone(),
                        bytes_pulled: install.bytes_pulled as i64,
                        total_bytes: install.total_bytes as i64,
                        bytes_per_second: bytes_per_second as i64,
                        eta_seconds: install_eta_seconds(install, bytes_per_second),
                        status: install.status.clone(),
                        cancel_requested: install.cancel_requested,
                    }
                })
                .collect()
        });
        call.reply(installs)
    }

    fn cancel_install(
        &self,
        call: &mut dyn Call_CancelInstall,
        profile_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if let Some(install) = guard.installing_profiles.get_mut(&profile_id) {
                install.cancel_requested = true;
                install.status = "Cancelling...".to_string();
            }
            call.reply()
        })
    }

    fn list_catalog(&self, call: &mut dyn Call_ListCatalog) -> varlink::Result<()> {
        let (variant, memory_gb, installing_profiles) = self.rt.block_on(async {
            let guard = self.state.0.lock().await;
            (
                guard.variant,
                crate::hardware::total_memory_gb(),
                guard
                    .installing_profiles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        let llmfit_system = crate::llmfit_metadata::detect_system();
        let profiles = manifests::list_catalog_profiles().unwrap_or_default();
        let profiles = profiles
            .into_iter()
            .map(|profile| {
                let metadata = if profile.llmfit_model_id.is_empty() {
                    None
                } else {
                    crate::llmfit_metadata::find(&profile.llmfit_model_id)
                };
                let tier = if profile.tier.is_empty() {
                    "balanced".to_string()
                } else {
                    profile.tier
                };
                let min_ram_gb = metadata
                    .map(|model| model.min_ram_gb)
                    .filter(|gb| *gb > 0.0)
                    .unwrap_or(profile.min_ram_gb);
                let recommended_ram_gb = metadata
                    .map(|model| model.recommended_ram_gb)
                    .filter(|gb| *gb > 0.0)
                    .unwrap_or(min_ram_gb);
                let has_fit_metadata = metadata.is_some() || min_ram_gb > 0.0;
                let has_enough_memory = memory_gb
                    .map(|gb| min_ram_gb <= 0.0 || gb >= min_ram_gb)
                    .unwrap_or(min_ram_gb <= 0.0);
                let has_recommended_memory = memory_gb
                    .map(|gb| recommended_ram_gb <= 0.0 || gb >= recommended_ram_gb)
                    .unwrap_or(recommended_ram_gb <= 0.0);
                let fit_level = fit_level(
                    has_enough_memory,
                    has_recommended_memory,
                    has_fit_metadata,
                    metadata.is_some(),
                );
                let recommended = fit_level == "recommended";
                let installing = installing_profiles.contains(&profile.profile_id);
                let use_case_fit_scores = metadata
                    .map(|model| use_case_fit_scores(model, &llmfit_system, &profile.use_cases))
                    .unwrap_or_default();
                let fit_score = use_case_fit_scores
                    .iter()
                    .map(|fit| fit.score)
                    .reduce(f64::max)
                    .unwrap_or_else(|| {
                        metadata
                            .map(|model| crate::llmfit_metadata::fit_score(model, &llmfit_system))
                            .unwrap_or(0.0)
                    });
                CatalogProfileInfo {
                    profile_id: profile.profile_id,
                    model_id: profile.model_id,
                    llmfit_model_id: profile.llmfit_model_id,
                    runtime_id: profile.runtime_id,
                    tier: tier.clone(),
                    disk_size_gb: profile.disk_size_gb,
                    min_ram_gb,
                    recommended_ram_gb,
                    min_vram_gb: metadata.and_then(|model| model.min_vram_gb).unwrap_or(0.0),
                    fit_score,
                    use_case_fit_scores,
                    fit_level: fit_level.to_string(),
                    recommended,
                    installing,
                    recommendation_reason: recommendation_reason(CatalogFit {
                        has_enough_memory,
                        has_recommended_memory,
                        has_fit_metadata,
                        has_llmfit_metadata: metadata.is_some(),
                        tier: &tier,
                        memory_gb,
                        min_ram_gb,
                        recommended_ram_gb,
                        min_vram_gb: metadata.and_then(|model| model.min_vram_gb),
                        variant,
                    }),
                    use_cases: profile.use_cases,
                }
            })
            .collect();
        call.reply(profiles)
    }

    fn install_url_profile(
        &self,
        call: &mut dyn Call_InstallUrlProfile,
        runtime_id: String,
        url: String,
        sha256: String,
        mmproj_url: String,
        mmproj_sha256: String,
        use_cases: Vec<String>,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let filename = match filename_from_url(&url) {
                Ok(filename) => filename,
                Err(e) => return call.reply_install_failed(url, e.to_string()),
            };
            let mut artifacts = vec![ManifestArtifact {
                role: "model".to_string(),
                url,
                filename: filename.clone(),
                sha256: sha256.clone(),
                size_bytes: 0,
            }];
            if !mmproj_url.is_empty() || !mmproj_sha256.is_empty() {
                if mmproj_url.is_empty() || mmproj_sha256.is_empty() {
                    return call.reply_install_failed(
                        filename,
                        "mmproj URL and SHA-256 must be provided together".to_string(),
                    );
                }
                let mmproj_filename = match filename_from_url(&mmproj_url) {
                    Ok(filename) => filename,
                    Err(e) => return call.reply_install_failed(mmproj_url, e.to_string()),
                };
                artifacts.push(ManifestArtifact {
                    role: "mmproj".to_string(),
                    url: mmproj_url,
                    filename: mmproj_filename,
                    sha256: mmproj_sha256,
                    size_bytes: 0,
                });
            }
            let model_id = generated_model_id(&runtime_id, &filename, &sha256);
            let profile_id = model_id.clone();
            let manifest = ModelManifest {
                profile_id: profile_id.clone(),
                model_id,
                llmfit_model_id: String::new(),
                runtime_id,
                runtime_options: Default::default(),
                tier: String::new(),
                disk_size_gb: 0.0,
                min_ram_gb: 0.0,
                runtime_images: Vec::new(),
                use_cases,
                artifacts,
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

fn fit_level(
    has_enough_memory: bool,
    has_recommended_memory: bool,
    has_fit_metadata: bool,
    has_llmfit_metadata: bool,
) -> &'static str {
    if !has_fit_metadata {
        "unknown"
    } else if !has_enough_memory {
        "too_large"
    } else if has_recommended_memory && has_llmfit_metadata {
        "recommended"
    } else if has_enough_memory {
        "fits_minimum"
    } else {
        "unknown"
    }
}

struct CatalogFit<'a> {
    has_enough_memory: bool,
    has_recommended_memory: bool,
    has_fit_metadata: bool,
    has_llmfit_metadata: bool,
    tier: &'a str,
    memory_gb: Option<f64>,
    min_ram_gb: f64,
    recommended_ram_gb: f64,
    min_vram_gb: Option<f64>,
    variant: crate::hardware::Variant,
}

fn recommendation_reason(fit: CatalogFit<'_>) -> String {
    let memory = fit
        .memory_gb
        .map(|gb| format!("{gb:.1} GB RAM detected"))
        .unwrap_or_else(|| "RAM amount could not be detected".to_string());
    let source = if fit.has_llmfit_metadata {
        "model metadata"
    } else {
        "manifest metadata"
    };
    let vram = fit
        .min_vram_gb
        .filter(|gb| *gb > 0.0)
        .map(|gb| {
            if fit.variant == crate::hardware::Variant::Cpu {
                format!("; published VRAM target is {gb:.1} GB")
            } else {
                format!("; published VRAM target is {gb:.1} GB for accelerated runs")
            }
        })
        .unwrap_or_default();
    if !fit.has_fit_metadata {
        return "No fit metadata is available for this profile.".to_string();
    }
    if !fit.has_enough_memory {
        return format!(
            "Requires at least {:.1} GB RAM from {source}; {memory}{vram}.",
            fit.min_ram_gb
        );
    }
    if fit.has_recommended_memory {
        return format!(
            "Meets recommended {:.1} GB RAM from {source}; {memory}{vram}.",
            fit.recommended_ram_gb
        );
    }
    format!(
        "Meets minimum {:.1} GB RAM but not recommended {:.1} GB from {source}; catalog tier is {}; {memory}{vram}.",
        fit.min_ram_gb, fit.recommended_ram_gb, fit.tier
    )
}

fn use_case_fit_scores(
    model: &llmfit_core::LlmModel,
    system: &llmfit_core::SystemSpecs,
    use_cases: &[String],
) -> Vec<UseCaseFitScore> {
    use_cases
        .iter()
        .filter_map(|use_case| {
            fit_category(use_case).map(|category| UseCaseFitScore {
                use_case: use_case.clone(),
                score: crate::llmfit_metadata::fit_score_for_category(model, system, category),
            })
        })
        .collect()
}

fn fit_category(use_case: &str) -> Option<&'static str> {
    match use_case {
        "llm.summarize" | "llm.translate" | "llm.rephrase" | "llm.chat" => Some("Chat"),
        "llm.analyze" => Some("Reasoning"),
        "llm.classify" | "llm.extract" => Some("General"),
        "vision.describe" | "vision.segment" => Some("Multimodal"),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct RuntimeImageUsage {
    profiles_by_ref: HashMap<String, Vec<String>>,
}

impl RuntimeImageUsage {
    fn used_by(&self, image: &PodmanRuntimeImage) -> Vec<String> {
        let mut profiles = Vec::new();
        for image_ref in image.match_refs() {
            if let Some(used_by) = self.profiles_by_ref.get(&image_ref) {
                profiles.extend(used_by.iter().cloned());
            }
        }
        profiles.sort();
        profiles.dedup();
        profiles
    }
}

#[derive(Debug)]
struct PodmanRuntimeImage {
    image_id: String,
    image_ref: String,
    names: Vec<String>,
    runtime_id: String,
    variant: String,
    size_bytes: i64,
}

impl PodmanRuntimeImage {
    fn match_refs(&self) -> Vec<String> {
        let mut refs = self.names.clone();
        refs.push(self.image_ref.clone());
        refs.retain(|value| !value.is_empty() && value != "<none>:<none>");
        refs.sort();
        refs.dedup();
        refs
    }
}

async fn runtime_image_usage(state: &SharedState) -> RuntimeImageUsage {
    let guard = state.0.lock().await;
    let mut profiles_by_ref: HashMap<String, Vec<String>> = HashMap::new();
    for profile in guard.profiles.all() {
        if let Some(image_ref) = resolve_runtime_image(&guard, profile) {
            profiles_by_ref
                .entry(image_ref.to_string())
                .or_default()
                .push(profile.profile_id.clone());
        }
    }
    RuntimeImageUsage { profiles_by_ref }
}

fn list_aileron_runtime_images(usage: &RuntimeImageUsage) -> anyhow::Result<Vec<OciRuntimeImage>> {
    let values = podman_images_with_label("org.aileron.runtime=true")?;
    let mut images = values
        .iter()
        .filter_map(parse_podman_runtime_image)
        .map(|image| {
            let used_by_profiles = usage.used_by(&image);
            OciRuntimeImage {
                image_id: image.image_id,
                image_ref: image.image_ref,
                runtime_id: image.runtime_id,
                variant: image.variant,
                size_bytes: image.size_bytes,
                in_use: !used_by_profiles.is_empty(),
                used_by_profiles,
            }
        })
        .collect::<Vec<_>>();
    images.sort_by(|a, b| {
        a.runtime_id
            .cmp(&b.runtime_id)
            .then(a.variant.cmp(&b.variant))
            .then(a.image_ref.cmp(&b.image_ref))
    });
    Ok(images)
}

fn podman_images_with_label(label: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let output = std::process::Command::new("podman")
        .args([
            "image",
            "ls",
            "--filter",
            &format!("label={label}"),
            "--format",
            "json",
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("podman image ls failed for label {label}");
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn parse_podman_runtime_image(value: &serde_json::Value) -> Option<PodmanRuntimeImage> {
    let labels = value.get("Labels")?;
    if label_value(labels, "org.aileron.runtime").as_deref() != Some("true") {
        return None;
    }
    let image_id = string_field(value, &["Id", "ID"])?;
    let runtime_id = label_value(labels, "org.aileron.runtime_id").unwrap_or_default();
    let names = image_names(value);
    let variant = label_value(labels, "org.aileron.variant").unwrap_or_default();
    let image_ref = label_value(labels, "org.aileron.image_ref")
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| image_id.clone());
    Some(PodmanRuntimeImage {
        image_id,
        image_ref,
        names,
        runtime_id,
        variant,
        size_bytes: image_size_bytes(value),
    })
}

fn string_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(str::to_string))
}

fn label_value(labels: &serde_json::Value, key: &str) -> Option<String> {
    labels
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn image_names(value: &serde_json::Value) -> Vec<String> {
    if let Some(names) = value.get("Names").and_then(|names| names.as_array()) {
        return names
            .iter()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect();
    }
    let repository = string_field(value, &["Repository"]);
    let tag = string_field(value, &["Tag"]);
    match (repository, tag) {
        (Some(repository), Some(tag)) => vec![format!("{repository}:{tag}")],
        _ => Vec::new(),
    }
}

fn image_size_bytes(value: &serde_json::Value) -> i64 {
    value
        .get("Size")
        .and_then(|size| {
            size.as_i64()
                .or_else(|| size.as_u64().map(|size| size as i64))
        })
        .unwrap_or(0)
}

fn profile_artifact_size_bytes(path: &Path) -> i64 {
    directory_size_bytes(path).unwrap_or(0).min(i64::MAX as u64) as i64
}

fn directory_size_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        total = total.saturating_add(directory_size_bytes(&entry.path()).unwrap_or(0));
    }
    Ok(total)
}

async fn remove_aileron_runtime_image(
    image_id: &str,
    usage: &RuntimeImageUsage,
) -> anyhow::Result<()> {
    let image = list_aileron_runtime_images(usage)?
        .into_iter()
        .find(|image| image.image_id == image_id || image.image_ref == image_id)
        .ok_or_else(|| anyhow::anyhow!("Aileron runtime image not found: {image_id}"))?;
    if image.in_use {
        anyhow::bail!(
            "runtime image is used by {}",
            image.used_by_profiles.join(", ")
        );
    }
    remove_podman_image(&image.image_id).await
}

async fn remove_podman_image(image_id: &str) -> anyhow::Result<()> {
    let status = tokio::process::Command::new("podman")
        .args(["image", "rm", image_id])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("podman image rm failed for {image_id}");
    }
    Ok(())
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
            runtime_options: self.runtime_options,
            artifact_path,
            runtime_images: self.runtime_images,
            use_cases: self.use_cases,
            artifact_hashes: self
                .artifacts
                .into_iter()
                .map(|artifact| ArtifactHash {
                    role: artifact.role,
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
    let manifest = manifests::parse_model_manifest_json(&data)?;
    if let Some(requested) = requested_profile_id
        && manifest.profile_id != requested
    {
        anyhow::bail!("manifest profile_id does not match requested profile");
    }

    install_manifest_data(state, manifest).await
}

async fn install_manifest_data(
    state: &SharedState,
    manifest: ModelManifest,
) -> anyhow::Result<(Vec<String>, Vec<UseCaseConflict>)> {
    let profile_id = manifest.profile_id.clone();
    let total_bytes = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.size_bytes)
        .sum();
    begin_install(state, &profile_id, total_bytes).await?;
    let result = install_manifest_data_inner(state, manifest).await;
    finish_install(
        state,
        &profile_id,
        result.as_ref().err().map(|error| error.to_string()),
    )
    .await;
    result
}

async fn install_manifest_data_inner(
    state: &SharedState,
    manifest: ModelManifest,
) -> anyhow::Result<(Vec<String>, Vec<UseCaseConflict>)> {
    manifests::validate_use_cases(&manifest.use_cases)?;
    let artifact_dir = crate::profiles::model_dir(&manifest.model_id);
    let profile = manifest.clone().into_profile(artifact_dir.clone());

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

    update_install_status(state, &profile.profile_id, "Preparing runtime image...").await;
    pull_runtime_image(&runtime_image).await?;

    install_artifacts(
        state,
        &manifest.profile_id,
        &artifact_dir,
        &manifest.artifacts,
    )
    .await?;
    register_profile(state, profile).await
}

async fn begin_install(
    state: &SharedState,
    profile_id: &str,
    total_bytes: u64,
) -> anyhow::Result<()> {
    let mut guard = state.0.lock().await;
    if guard.installing_profiles.contains_key(profile_id) {
        anyhow::bail!("install already running for {profile_id}");
    }
    guard
        .recent_installs
        .retain(|(recent_profile_id, _)| recent_profile_id != profile_id);
    guard.installing_profiles.insert(
        profile_id.to_string(),
        InstallRecord {
            bytes_pulled: 0,
            total_bytes,
            status: "Starting...".to_string(),
            cancel_requested: false,
            samples: std::collections::VecDeque::from([InstallSample {
                at: chrono::Utc::now(),
                bytes_pulled: 0,
            }]),
        },
    );
    Ok(())
}

async fn finish_install(state: &SharedState, profile_id: &str, error: Option<String>) {
    let mut guard = state.0.lock().await;
    let Some(mut install) = guard.installing_profiles.remove(profile_id) else {
        return;
    };
    if let Some(error) = error {
        install.status = format!("Failed: {error}");
        install.cancel_requested = true;
        guard
            .recent_installs
            .push_front((profile_id.to_string(), install));
        while guard.recent_installs.len() > 10 {
            guard.recent_installs.pop_back();
        }
    }
}

async fn update_install_status(state: &SharedState, profile_id: &str, status: &str) {
    let mut guard = state.0.lock().await;
    if let Some(install) = guard.installing_profiles.get_mut(profile_id) {
        install.status = status.to_string();
    }
}

async fn add_install_bytes(
    state: &SharedState,
    profile_id: &str,
    bytes: u64,
) -> anyhow::Result<()> {
    let mut guard = state.0.lock().await;
    let Some(install) = guard.installing_profiles.get_mut(profile_id) else {
        anyhow::bail!("install no longer active for {profile_id}");
    };
    if install.cancel_requested {
        anyhow::bail!("install cancelled for {profile_id}");
    }
    install.bytes_pulled = install.bytes_pulled.saturating_add(bytes);
    let now = chrono::Utc::now();
    install.samples.push_back(InstallSample {
        at: now,
        bytes_pulled: install.bytes_pulled,
    });
    while install.samples.len() > 2
        && install
            .samples
            .front()
            .map(|sample| (now - sample.at).num_seconds() > 10)
            .unwrap_or(false)
    {
        install.samples.pop_front();
    }
    while install.samples.len() > 20 {
        install.samples.pop_front();
    }
    Ok(())
}

async fn set_install_bytes(
    state: &SharedState,
    profile_id: &str,
    bytes: u64,
) -> anyhow::Result<()> {
    let mut guard = state.0.lock().await;
    let Some(install) = guard.installing_profiles.get_mut(profile_id) else {
        anyhow::bail!("install no longer active for {profile_id}");
    };
    if install.cancel_requested {
        anyhow::bail!("install cancelled for {profile_id}");
    }
    install.bytes_pulled = bytes;
    install.samples.push_back(InstallSample {
        at: chrono::Utc::now(),
        bytes_pulled: bytes,
    });
    Ok(())
}

fn install_bytes_per_second(install: &InstallRecord) -> u64 {
    let Some(first) = install.samples.front() else {
        return 0;
    };
    let Some(last) = install.samples.back() else {
        return 0;
    };
    let elapsed_ms = (last.at - first.at).num_milliseconds();
    if elapsed_ms <= 0 || last.bytes_pulled <= first.bytes_pulled {
        return 0;
    }
    ((last.bytes_pulled - first.bytes_pulled) as f64 / (elapsed_ms as f64 / 1000.0)) as u64
}

fn install_eta_seconds(install: &InstallRecord, bytes_per_second: u64) -> i64 {
    if install.total_bytes == 0
        || install.bytes_pulled >= install.total_bytes
        || bytes_per_second == 0
    {
        return -1;
    }
    let remaining = install.total_bytes - install.bytes_pulled;
    remaining.div_ceil(bytes_per_second) as i64
}

async fn ensure_install_not_cancelled(state: &SharedState, profile_id: &str) -> anyhow::Result<()> {
    let guard = state.0.lock().await;
    if guard
        .installing_profiles
        .get(profile_id)
        .map(|install| install.cancel_requested)
        .unwrap_or(false)
    {
        anyhow::bail!("install cancelled for {profile_id}");
    }
    Ok(())
}

async fn pull_runtime_image(image_ref: &str) -> anyhow::Result<()> {
    let exists = tokio::process::Command::new("podman")
        .args(["image", "exists", image_ref])
        .status()
        .await?;
    if exists.success() {
        return Ok(());
    }

    if image_ref.starts_with("localhost/") {
        anyhow::bail!("local runtime image is not built: {image_ref}");
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
        manifests::validate_use_cases(std::slice::from_ref(&use_case))?;
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

async fn install_artifacts(
    state: &SharedState,
    profile_id: &str,
    target_dir: &PathBuf,
    artifacts: &[ManifestArtifact],
) -> anyhow::Result<()> {
    if target_dir.exists() {
        update_install_status(state, profile_id, "Verifying existing artifacts...").await;
        if artifacts_match(target_dir, artifacts).await? {
            let existing_bytes: u64 = artifacts.iter().map(|artifact| artifact.size_bytes).sum();
            if existing_bytes > 0 {
                set_install_bytes(state, profile_id, existing_bytes).await?;
            }
            return Ok(());
        }
    }

    let temp_dir = target_dir.with_extension("tmp");
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let result = download_artifacts_to_temp(state, profile_id, &temp_dir, artifacts).await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    result?;

    if target_dir.exists() {
        std::fs::remove_dir_all(target_dir)?;
    }
    std::fs::rename(temp_dir, target_dir)?;
    Ok(())
}

async fn artifacts_match(
    target_dir: &std::path::Path,
    artifacts: &[ManifestArtifact],
) -> anyhow::Result<bool> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    for artifact in artifacts {
        let path = target_dir.join(&artifact.filename);
        if !path.exists() {
            return Ok(false);
        }

        let mut file = tokio::fs::File::open(&path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != artifact.sha256.to_lowercase() {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn download_artifacts_to_temp(
    state: &SharedState,
    profile_id: &str,
    temp_dir: &std::path::Path,
    artifacts: &[ManifestArtifact],
) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;

    for artifact in artifacts {
        ensure_install_not_cancelled(state, profile_id).await?;
        update_install_status(
            state,
            profile_id,
            &format!("Downloading {}...", artifact.filename),
        )
        .await;
        let dest = temp_dir.join(&artifact.filename);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut response = reqwest::get(&artifact.url).await?.error_for_status()?;
        let mut file = tokio::fs::File::create(&dest).await?;
        let mut hasher = Sha256::new();

        while let Some(chunk) = response.chunk().await? {
            ensure_install_not_cancelled(state, profile_id).await?;
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            add_install_bytes(state, profile_id, chunk.len() as u64).await?;
        }

        file.flush().await?;
        let actual = format!("{:x}", hasher.finalize());
        if actual != artifact.sha256.to_lowercase() {
            anyhow::bail!(
                "checksum mismatch for {}: expected {}, got {}",
                artifact.filename,
                artifact.sha256,
                actual
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owned_runtime_image_labels() {
        let image = serde_json::json!({
            "Id": "def456",
            "Names": ["registry.example/aileron-runtime-asr:cuda"],
            "Size": 5678,
            "Labels": {
                "org.aileron.runtime": "true",
                "org.aileron.runtime_id": "asr-whisper-cpp",
                "org.aileron.variant": "cuda",
                "org.aileron.image_ref": "registry.example/aileron-runtime-asr:cuda"
            }
        });

        let parsed = parse_podman_runtime_image(&image).expect("owned runtime image parses");

        assert_eq!(parsed.image_id, "def456");
        assert_eq!(parsed.runtime_id, "asr-whisper-cpp");
        assert_eq!(parsed.variant, "cuda");
        assert_eq!(
            parsed.image_ref,
            "registry.example/aileron-runtime-asr:cuda"
        );
        assert_eq!(parsed.size_bytes, 5678);
    }

    #[test]
    fn sums_profile_artifact_directory_size() {
        let root =
            std::env::temp_dir().join(format!("aileron-profile-size-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).expect("temp dir");
        let nested = root.join("nested");
        std::fs::create_dir(&nested).expect("nested dir");
        std::fs::write(root.join("model.gguf"), vec![0; 7]).expect("model file");
        std::fs::write(nested.join("projector.gguf"), vec![0; 5]).expect("projector file");

        assert_eq!(profile_artifact_size_bytes(&root), 12);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn usage_marks_only_resolved_runtime_image() {
        let usage = RuntimeImageUsage {
            profiles_by_ref: HashMap::from([(
                "example/asr:vulkan".to_string(),
                vec!["whisper".to_string()],
            )]),
        };
        let cpu = PodmanRuntimeImage {
            image_id: "cpu".to_string(),
            image_ref: "example/asr:cpu".to_string(),
            names: vec!["example/asr:cpu".to_string()],
            runtime_id: "asr-whisper-cpp".to_string(),
            variant: "cpu".to_string(),
            size_bytes: 1,
        };
        let vulkan = PodmanRuntimeImage {
            image_id: "vulkan".to_string(),
            image_ref: "example/asr:vulkan".to_string(),
            names: vec!["example/asr:vulkan".to_string()],
            runtime_id: "asr-whisper-cpp".to_string(),
            variant: "vulkan".to_string(),
            size_bytes: 1,
        };

        assert!(usage.used_by(&cpu).is_empty());
        assert_eq!(usage.used_by(&vulkan), vec!["whisper".to_string()]);
    }

    #[test]
    fn ignores_legacy_labeled_runtime_image() {
        let image = serde_json::json!({
            "Id": "abc123",
            "Names": ["localhost/aileron-runtime-llm-llama-cpp:cpu"],
            "Size": 1234,
            "Labels": {
                "aileron.runtime_id": "llm-llama-cpp"
            }
        });

        assert!(parse_podman_runtime_image(&image).is_none());
    }
}
