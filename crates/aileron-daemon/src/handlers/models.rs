/// Varlink handler for `aileron.Models`.
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use crate::manifests::{self, ManifestArtifact, ModelManifest};
use crate::profiles::Profile;
use crate::state::{InstallRecord, InstallSample, SharedState};
#[allow(unused_imports)]
use aileron_varlink::aileron_Models::{
    Call_AssignUseCase, Call_CancelInstall, Call_DeleteProfile, Call_InstallManifest,
    Call_InstallUrlProfile, Call_List, Call_ListCatalog, Call_ListInstalls, Call_ListRuntimeImages,
    Call_ListRuntimeManifests, Call_PruneUnusedRuntimeImages, Call_RemoveRuntimeImage,
    Call_UpdateRuntimeImage, CatalogProfileInfo, InstallProgress, InstallStatus, OciRuntimeImage,
    ProfileInfo, RuntimeImage, RuntimeImageCleanupError, RuntimeManifestInfo, UseCaseConflict,
    UseCaseFitScore, VarlinkCallError, VarlinkInterface,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
                        source: profile.source.clone(),
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
            let store = oci_store_for_state(&self.state).await;
            list_aileron_runtime_images(&store, &usage)
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
            let store = oci_store_for_state(&self.state).await;
            remove_aileron_runtime_image(&store, &image_id, &usage).await
        });
        match result {
            Ok(()) => call.reply(),
            Err(e) => Err(io_err(e)),
        }
    }

    fn update_runtime_image(
        &self,
        call: &mut dyn Call_UpdateRuntimeImage,
        image_ref: String,
    ) -> varlink::Result<()> {
        let result = self
            .rt
            .block_on(async { start_runtime_image_update(&self.state, &image_ref).await });
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
            let store = oci_store_for_state(&self.state).await;
            let images = list_aileron_runtime_images(&store, &usage)?;
            let mut removed = Vec::new();
            let mut errors = Vec::new();
            for image in images.into_iter().filter(|image| !image.in_use) {
                match remove_oci_runtime_rootfs(&store, &image.image_id).await {
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
                .chain(guard.runtime_downloads.iter())
                .chain(
                    guard
                        .recent_runtime_downloads
                        .iter()
                        .map(|(image_ref, install)| (image_ref, install)),
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
            request_cancel_install(&mut guard, &profile_id);
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
        "llm.embed" => Some("Embeddings"),
        "vision.describe" | "vision.segment" | "vision.ocr" => Some("Multimodal"),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct RuntimeImageUsage {
    profiles_by_ref: HashMap<String, Vec<String>>,
}

impl RuntimeImageUsage {
    fn used_by(&self, image: &StoredRuntimeImage) -> Vec<String> {
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
struct StoredRuntimeImage {
    image_id: String,
    image_ref: String,
    names: Vec<String>,
    runtime_id: String,
    variant: String,
    digest: Option<String>,
    size_bytes: i64,
    source: String,
}

impl StoredRuntimeImage {
    fn match_refs(&self) -> Vec<String> {
        let mut refs = self.names.clone();
        refs.push(self.image_ref.clone());
        refs.retain(|value| !value.is_empty() && value != "<none>:<none>");
        refs.sort();
        refs.dedup();
        refs
    }
}

#[derive(Debug, Clone)]
struct RuntimeImageUpdateCheck {
    available: bool,
    status: String,
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

async fn oci_store_for_state(state: &SharedState) -> PathBuf {
    let guard = state.0.lock().await;
    guard.containers.oci_store.clone()
}

fn list_aileron_runtime_images(
    store: &Path,
    usage: &RuntimeImageUsage,
) -> anyhow::Result<Vec<OciRuntimeImage>> {
    let mut images = stored_runtime_images(store, "user")?;
    images.extend(stored_runtime_images(
        &crate::container::default_system_oci_store(),
        "system",
    )?);
    dedupe_runtime_images(&mut images);
    let mut images = images
        .into_iter()
        .map(|image| {
            let used_by_profiles = usage.used_by(&image);
            let update = runtime_image_local_status(&image);
            OciRuntimeImage {
                image_id: image.image_id,
                image_ref: image.image_ref,
                runtime_id: image.runtime_id,
                variant: image.variant,
                size_bytes: image.size_bytes,
                in_use: !used_by_profiles.is_empty(),
                used_by_profiles,
                update_available: update.available,
                update_status: update.status,
                source: image.source,
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

fn dedupe_runtime_images(images: &mut Vec<StoredRuntimeImage>) {
    let mut seen_ids = HashSet::new();
    let mut seen_refs = HashSet::new();
    images.retain(|image| {
        let id_key = (
            image.image_id.clone(),
            image.runtime_id.clone(),
            image.variant.clone(),
        );
        let ref_key = (
            image.image_ref.clone(),
            image.runtime_id.clone(),
            image.variant.clone(),
        );
        seen_ids.insert(id_key) && seen_refs.insert(ref_key)
    });
}

fn runtime_image_local_status(image: &StoredRuntimeImage) -> RuntimeImageUpdateCheck {
    if image.source == "system" {
        return RuntimeImageUpdateCheck {
            available: false,
            status: "system package".to_string(),
        };
    }

    if remote_tag_is_checkable(&image.image_ref) {
        // TODO: Compare the remote tag digest and report updates when it changes.
        return RuntimeImageUpdateCheck {
            available: false,
            status: if image.digest.is_some() {
                "installed: update not checked".to_string()
            } else {
                "installed: digest unavailable".to_string()
            },
        };
    }

    RuntimeImageUpdateCheck {
        available: false,
        status: "not checkable".to_string(),
    }
}

fn remote_tag_is_checkable(image_ref: &str) -> bool {
    !image_ref.is_empty()
        && !image_ref.starts_with("localhost/")
        && !image_ref.contains('@')
        && image_ref
            .rsplit_once('/')
            .map_or(image_ref, |(_, after_slash)| after_slash)
            .contains(':')
}

fn oci_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        "powerpc64le" => "ppc64le",
        "s390x" => "s390x",
        "x86" | "i386" | "i586" | "i686" => "386",
        other => other,
    }
}

fn stored_runtime_images(store: &Path, source: &str) -> anyhow::Result<Vec<StoredRuntimeImage>> {
    let rootfs_dir = store.join("rootfs");
    if !rootfs_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut images = Vec::new();
    for entry in std::fs::read_dir(&rootfs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let image_id = entry.file_name().to_string_lossy().to_string();
        let metadata =
            read_runtime_rootfs_metadata(store, &image_id, &entry.path()).unwrap_or_default();
        let image_ref = metadata
            .image_ref
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| image_id.clone());
        let variant = metadata
            .variant
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| image_ref_variant(&image_ref).unwrap_or_default());
        let runtime_id = metadata.runtime_id.unwrap_or_default();
        let digest = metadata.digest;
        let size_bytes = directory_size_bytes(&entry.path())
            .unwrap_or(0)
            .min(i64::MAX as u64) as i64;
        images.push(StoredRuntimeImage {
            image_id,
            image_ref: image_ref.clone(),
            names: vec![image_ref],
            runtime_id,
            variant,
            digest,
            size_bytes,
            source: source.to_string(),
        });
    }
    Ok(images)
}

#[derive(Default, Deserialize, Serialize)]
struct RuntimeRootfsMetadata {
    image_ref: Option<String>,
    runtime_id: Option<String>,
    variant: Option<String>,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct OciIndex {
    manifests: Vec<OciDescriptor>,
}

#[derive(Clone, Deserialize)]
struct OciDescriptor {
    digest: String,
    platform: Option<OciPlatform>,
}

#[derive(Clone, Deserialize)]
struct OciPlatform {
    os: Option<String>,
    architecture: Option<String>,
}

#[derive(Deserialize)]
struct OciManifest {
    #[serde(skip)]
    digest: Option<String>,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Default, Deserialize)]
struct OciImageConfig {
    config: Option<OciImageConfigFields>,
}

#[derive(Default, Deserialize)]
struct OciImageConfigFields {
    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,
}

fn read_runtime_rootfs_metadata(
    store: &Path,
    image_id: &str,
    rootfs: &Path,
) -> anyhow::Result<RuntimeRootfsMetadata> {
    let metadata_path = store.join("metadata").join(format!("{image_id}.json"));
    let path = if metadata_path.is_file() {
        metadata_path
    } else {
        rootfs.join("metadata.json")
    };
    let data = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

fn image_ref_variant(image_ref: &str) -> Option<String> {
    image_ref
        .rsplit_once('/')
        .map_or(image_ref, |(_, after_slash)| after_slash)
        .rsplit_once(':')
        .map(|(_, tag)| tag.to_string())
}

fn profile_artifact_size_bytes(path: &Path) -> i64 {
    directory_size_bytes(path).unwrap_or(0).min(i64::MAX as u64) as i64
}

fn directory_size_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
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
    store: &Path,
    image_id: &str,
    usage: &RuntimeImageUsage,
) -> anyhow::Result<()> {
    let image = list_aileron_runtime_images(store, usage)?
        .into_iter()
        .find(|image| image.image_id == image_id || image.image_ref == image_id)
        .ok_or_else(|| anyhow::anyhow!("Aileron runtime image not found: {image_id}"))?;
    if image.source != "user" {
        anyhow::bail!(
            "system runtime image cannot be removed: {}",
            image.image_ref
        );
    }
    if image.in_use {
        anyhow::bail!(
            "runtime image is used by {}",
            image.used_by_profiles.join(", ")
        );
    }
    remove_oci_runtime_rootfs(store, &image.image_id).await
}

async fn start_runtime_image_update(state: &SharedState, image_ref: &str) -> anyhow::Result<()> {
    if !remote_tag_is_checkable(image_ref) {
        anyhow::bail!("runtime image is not a remote tag: {image_ref}");
    }
    let usage = RuntimeImageUsage::default();
    let store = oci_store_for_state(state).await;
    let known = list_aileron_runtime_images(&store, &usage)?
        .into_iter()
        .any(|image| image.image_ref == image_ref || image.image_id == image_ref);
    if !known {
        anyhow::bail!("Aileron runtime image not found: {image_ref}");
    }
    begin_runtime_download(state, image_ref, None).await?;

    let state = state.clone();
    let image_ref = image_ref.to_string();
    tokio::spawn(async move {
        let result =
            pull_runtime_image_unconditional(&store, &image_ref, Some(state.clone()), None).await;
        finish_runtime_download(&state, &image_ref, result.err().map(|e| e.to_string())).await;
    });
    Ok(())
}

async fn remove_oci_runtime_rootfs(store: &Path, image_id: &str) -> anyhow::Result<()> {
    let path = store.join("rootfs").join(image_id);
    tokio::fs::remove_dir_all(&path).await?;
    let _ = tokio::fs::remove_file(store.join("metadata").join(format!("{image_id}.json"))).await;
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
    pull_runtime_image(state, &runtime_image, Some(&profile.profile_id)).await?;

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

fn runtime_download_key(image_ref: &str) -> String {
    format!("runtime:{image_ref}")
}

fn request_cancel_install(guard: &mut crate::state::Inner, profile_id: &str) {
    request_cancel_records(
        &mut guard.installing_profiles,
        &mut guard.runtime_downloads,
        &guard.runtime_download_owners,
        profile_id,
    );
}

fn request_cancel_records(
    installing_profiles: &mut HashMap<String, InstallRecord>,
    runtime_downloads: &mut HashMap<String, InstallRecord>,
    runtime_download_owners: &HashMap<String, String>,
    profile_id: &str,
) {
    if let Some(install) = installing_profiles.get_mut(profile_id) {
        install.cancel_requested = true;
        install.status = "Cancelling...".to_string();
        let runtime_keys = runtime_download_owners
            .iter()
            .filter_map(|(runtime_key, owner)| (owner == profile_id).then_some(runtime_key.clone()))
            .collect::<Vec<_>>();
        for runtime_key in runtime_keys {
            if let Some(download) = runtime_downloads.get_mut(&runtime_key) {
                download.cancel_requested = true;
                download.status = "Cancelling runtime setup...".to_string();
            }
        }
        return;
    }

    if let Some(download) = runtime_downloads.get_mut(profile_id) {
        download.cancel_requested = true;
        download.status = "Cancelling runtime setup...".to_string();
    }
}

async fn begin_runtime_download(
    state: &SharedState,
    image_ref: &str,
    owner_profile_id: Option<&str>,
) -> anyhow::Result<()> {
    let mut guard = state.0.lock().await;
    let key = runtime_download_key(image_ref);
    if guard.runtime_downloads.contains_key(&key) {
        anyhow::bail!("runtime image download already running for {image_ref}");
    }
    guard
        .recent_runtime_downloads
        .retain(|(recent_image_ref, _)| recent_image_ref != &key);
    guard.runtime_downloads.insert(
        key.clone(),
        InstallRecord {
            bytes_pulled: 0,
            total_bytes: 0,
            status: "Pulling runtime image...".to_string(),
            cancel_requested: false,
            samples: std::collections::VecDeque::from([InstallSample {
                at: chrono::Utc::now(),
                bytes_pulled: 0,
            }]),
        },
    );
    if let Some(owner_profile_id) = owner_profile_id {
        guard
            .runtime_download_owners
            .insert(key, owner_profile_id.to_string());
    }
    Ok(())
}

async fn finish_runtime_download(state: &SharedState, image_ref: &str, error: Option<String>) {
    let mut guard = state.0.lock().await;
    let key = runtime_download_key(image_ref);
    let Some(mut download) = guard.runtime_downloads.remove(&key) else {
        return;
    };
    guard.runtime_download_owners.remove(&key);
    if let Some(error) = error {
        download.status = format!("Failed: {error}");
        guard.recent_runtime_downloads.push_front((key, download));
        while guard.recent_runtime_downloads.len() > 10 {
            guard.recent_runtime_downloads.pop_back();
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

async fn pull_runtime_image(
    state: &SharedState,
    image_ref: &str,
    owner_profile_id: Option<&str>,
) -> anyhow::Result<()> {
    let store = oci_store_for_state(state).await;
    if crate::container::runtime_rootfs_path(&store, image_ref).is_some() {
        return Ok(());
    }

    if image_ref.starts_with("localhost/") {
        anyhow::bail!("local runtime rootfs is not installed: {image_ref}");
    }

    begin_runtime_download(state, image_ref, owner_profile_id).await?;
    let result = pull_runtime_image_unconditional(
        &store,
        image_ref,
        Some(state.clone()),
        owner_profile_id.map(str::to_string),
    )
    .await;
    finish_runtime_download(
        state,
        image_ref,
        result.as_ref().err().map(|e| e.to_string()),
    )
    .await;
    result
}

async fn pull_runtime_image_unconditional(
    store: &Path,
    image_ref: &str,
    state: Option<SharedState>,
    owner_profile_id: Option<String>,
) -> anyhow::Result<()> {
    let store = store.to_path_buf();
    let image_ref = image_ref.to_string();
    tokio::task::spawn_blocking(move || {
        pull_runtime_image_blocking(&store, &image_ref, state, owner_profile_id)
    })
    .await
    .context("runtime image pull task failed")?
}

fn pull_runtime_image_blocking(
    store: &Path,
    image_ref: &str,
    state: Option<SharedState>,
    owner_profile_id: Option<String>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(store.join("rootfs"))?;
    std::fs::create_dir_all(store.join("metadata"))?;
    std::fs::create_dir_all(store.join("tmp"))?;

    let key = crate::container::store_key(image_ref);
    let oci_layout = store
        .join("tmp")
        .join(format!("oci-layout-{}", Uuid::new_v4()));
    let rootfs_tmp = store.join("tmp").join(format!("rootfs-{}", Uuid::new_v4()));

    let result = (|| {
        ensure_runtime_pull_not_cancelled(state.as_ref(), image_ref, owner_profile_id.as_deref())?;
        let copy_steps = remote_runtime_copy_steps(image_ref).ok().flatten();
        let cancel_check =
            runtime_pull_cancel_check(state.clone(), image_ref, owner_profile_id.clone());
        copy_image_to_oci_layout(
            image_ref,
            &oci_layout,
            |progress| {
                if let Some(state) = state.as_ref() {
                    update_runtime_download_sync(state, image_ref, progress);
                }
            },
            copy_steps,
            cancel_check,
        )?;
        ensure_runtime_pull_not_cancelled(state.as_ref(), image_ref, owner_profile_id.as_deref())?;
        let manifest = read_selected_manifest(&oci_layout)?;
        if let Some(state) = state.as_ref() {
            update_runtime_download_sync(
                state,
                image_ref,
                RuntimePullProgress::Status("Unpacking runtime image...".to_string()),
            );
        }
        std::fs::create_dir_all(&rootfs_tmp)?;
        ensure_runtime_pull_not_cancelled(state.as_ref(), image_ref, owner_profile_id.as_deref())?;
        render_oci_layout_dir(&oci_layout, &rootfs_tmp)?;
        ensure_runtime_pull_not_cancelled(state.as_ref(), image_ref, owner_profile_id.as_deref())?;
        let labels = read_image_config_labels(&oci_layout, &manifest).unwrap_or_default();

        replace_runtime_rootfs(store, &key, &rootfs_tmp, || {
            write_runtime_metadata(image_ref, &manifest, &labels, store, &key)
        })?;
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&oci_layout);
    let _ = std::fs::remove_dir_all(&rootfs_tmp);
    result
}

fn runtime_pull_cancel_check(
    state: Option<SharedState>,
    image_ref: &str,
    owner_profile_id: Option<String>,
) -> Option<Arc<dyn Fn() -> bool + Send + Sync>> {
    let state = state?;
    let image_ref = image_ref.to_string();
    Some(Arc::new(move || {
        runtime_pull_is_cancelled(&state, &image_ref, owner_profile_id.as_deref())
    }))
}

fn ensure_runtime_pull_not_cancelled(
    state: Option<&SharedState>,
    image_ref: &str,
    owner_profile_id: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(state) = state
        && runtime_pull_is_cancelled(state, image_ref, owner_profile_id)
    {
        anyhow::bail!("runtime image pull cancelled for {image_ref}");
    }
    Ok(())
}

fn runtime_pull_is_cancelled(
    state: &SharedState,
    image_ref: &str,
    owner_profile_id: Option<&str>,
) -> bool {
    let mut guard = state.0.blocking_lock();
    let key = runtime_download_key(image_ref);
    let runtime_cancelled = guard
        .runtime_downloads
        .get(&key)
        .map(|download| download.cancel_requested)
        .unwrap_or(false);
    let profile_cancelled = owner_profile_id
        .and_then(|profile_id| guard.installing_profiles.get(profile_id))
        .map(|install| install.cancel_requested)
        .unwrap_or(false);
    if profile_cancelled && let Some(download) = guard.runtime_downloads.get_mut(&key) {
        download.cancel_requested = true;
        download.status = "Cancelling runtime setup...".to_string();
    }
    runtime_cancelled || profile_cancelled
}

fn replace_runtime_rootfs(
    store: &Path,
    key: &str,
    rootfs_tmp: &Path,
    write_metadata: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let rootfs_final = store.join("rootfs").join(key);
    let old_rootfs = store
        .join("tmp")
        .join(format!("old-rootfs-{}", Uuid::new_v4()));
    let had_old = rootfs_final.exists();

    if had_old {
        std::fs::rename(&rootfs_final, &old_rootfs).with_context(|| {
            format!(
                "failed to move existing runtime rootfs {}",
                rootfs_final.display()
            )
        })?;
    }

    if let Err(error) = std::fs::rename(rootfs_tmp, &rootfs_final).with_context(|| {
        format!(
            "failed to install runtime rootfs at {}",
            rootfs_final.display()
        )
    }) {
        rollback_runtime_rootfs(&rootfs_final, &old_rootfs, had_old);
        return Err(error);
    }

    if let Err(error) = write_metadata() {
        rollback_runtime_rootfs(&rootfs_final, &old_rootfs, had_old);
        return Err(error);
    }

    if had_old {
        let _ = std::fs::remove_dir_all(old_rootfs);
    }
    Ok(())
}

fn rollback_runtime_rootfs(rootfs_final: &Path, old_rootfs: &Path, had_old: bool) {
    let _ = std::fs::remove_dir_all(rootfs_final);
    if had_old && old_rootfs.exists() {
        let _ = std::fs::rename(old_rootfs, rootfs_final);
    }
}

enum RuntimePullProgress {
    Status(String),
    Percent(u64),
}

fn update_runtime_download_sync(
    state: &SharedState,
    image_ref: &str,
    progress: RuntimePullProgress,
) {
    let mut guard = state.0.blocking_lock();
    let key = runtime_download_key(image_ref);
    let Some(download) = guard.runtime_downloads.get_mut(&key) else {
        return;
    };
    match progress {
        RuntimePullProgress::Status(status) => {
            if status.contains("Unpacking") {
                download.bytes_pulled = 0;
                download.total_bytes = 0;
                download.samples.clear();
            }
            download.status = status;
        }
        RuntimePullProgress::Percent(percent) => {
            download.status = "Pulling runtime image...".to_string();
            download.total_bytes = 100;
            download.bytes_pulled = percent.min(100);
            download.samples.push_back(InstallSample {
                at: chrono::Utc::now(),
                bytes_pulled: download.bytes_pulled,
            });
            while download.samples.len() > 20 {
                download.samples.pop_front();
            }
        }
    }
}

fn copy_image_to_oci_layout(
    image_ref: &str,
    oci_layout: &Path,
    mut on_progress: impl FnMut(RuntimePullProgress),
    copy_steps: Option<usize>,
    cancel_check: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> anyhow::Result<()> {
    let source = if image_ref.contains("://") {
        image_ref.to_string()
    } else {
        format!("docker://{image_ref}")
    };
    let destination = format!("oci:{}:image", oci_layout.display());
    let mut child = std::process::Command::new("skopeo")
        .args([
            "copy",
            "--override-os",
            "linux",
            "--override-arch",
            oci_arch(std::env::consts::ARCH),
            &source,
            &destination,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run skopeo for {image_ref}"))?;
    let child_done = Arc::new(AtomicBool::new(false));
    let child_cancelled = Arc::new(AtomicBool::new(false));
    let cancel_reader = cancel_check.map(|cancel_check| {
        let child_done = child_done.clone();
        let child_cancelled = child_cancelled.clone();
        let pid = child.id().to_string();
        thread::spawn(move || {
            while !child_done.load(Ordering::Relaxed) {
                if cancel_check() {
                    child_cancelled.store(true, Ordering::Relaxed);
                    let _ = std::process::Command::new("kill")
                        .args(["-TERM", &pid])
                        .status();
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        })
    });
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture skopeo progress"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture skopeo errors"))?;
    let stderr_reader = thread::spawn(move || read_stream_to_string(stderr));
    let mut progress_log = String::new();
    let progress_result =
        read_skopeo_progress(stdout, &mut progress_log, &mut on_progress, copy_steps);
    let status = child.wait()?;
    child_done.store(true, Ordering::Relaxed);
    if let Some(cancel_reader) = cancel_reader {
        let _ = cancel_reader.join();
    }
    progress_result?;
    let error_log = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("skopeo error reader panicked"))??;
    if child_cancelled.load(Ordering::Relaxed) {
        anyhow::bail!("runtime image pull cancelled for {image_ref}");
    }
    if !status.success() {
        anyhow::bail!(
            "skopeo copy failed for {image_ref}: {}",
            [progress_log.as_str(), error_log.as_str()]
                .into_iter()
                .filter(|log| !log.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    on_progress(RuntimePullProgress::Percent(100));
    Ok(())
}

fn read_stream_to_string(mut reader: impl Read) -> anyhow::Result<String> {
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn read_skopeo_progress(
    mut reader: impl Read,
    progress_log: &mut String,
    on_progress: &mut impl FnMut(RuntimePullProgress),
    copy_steps: Option<usize>,
) -> anyhow::Result<()> {
    let mut line = Vec::new();
    let mut byte = [0];
    let mut completed_steps = 0;
    loop {
        match reader.read(&mut byte)? {
            0 => {
                process_skopeo_progress_line(
                    &line,
                    progress_log,
                    on_progress,
                    copy_steps,
                    &mut completed_steps,
                );
                break;
            }
            _ if byte[0] == b'\n' || byte[0] == b'\r' => {
                process_skopeo_progress_line(
                    &line,
                    progress_log,
                    on_progress,
                    copy_steps,
                    &mut completed_steps,
                );
                line.clear();
            }
            _ => line.push(byte[0]),
        }
    }
    Ok(())
}

fn process_skopeo_progress_line(
    line: &[u8],
    progress_log: &mut String,
    on_progress: &mut impl FnMut(RuntimePullProgress),
    copy_steps: Option<usize>,
    completed_steps: &mut usize,
) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    progress_log.push_str(line);
    progress_log.push('\n');
    if let Some(percent) = skopeo_progress_percent(line) {
        on_progress(RuntimePullProgress::Percent(percent));
    } else if let Some(status) = skopeo_progress_status(line) {
        on_progress(RuntimePullProgress::Status(status));
        if let Some(copy_steps) = copy_steps
            && skopeo_progress_is_copy_step(line)
        {
            *completed_steps = completed_steps.saturating_add(1);
            let percent = ((*completed_steps as f64 / copy_steps.max(1) as f64) * 100.0)
                .round()
                .clamp(1.0, 99.0) as u64;
            on_progress(RuntimePullProgress::Percent(percent));
        }
    }
}

fn remote_runtime_copy_steps(image_ref: &str) -> anyhow::Result<Option<usize>> {
    let raw = skopeo_raw_manifest(image_ref)?;
    let value: serde_json::Value = serde_json::from_slice(&raw)?;
    let manifest = if value.get("layers").is_some() {
        value
    } else {
        let index: OciIndex = serde_json::from_value(value)?;
        let Some(descriptor) = index
            .manifests
            .iter()
            .find(|descriptor| descriptor_matches_host(descriptor))
            .or_else(|| index.manifests.first())
        else {
            return Ok(None);
        };
        let Some(digest_ref) = image_ref_with_digest(image_ref, &descriptor.digest) else {
            return Ok(None);
        };
        serde_json::from_slice(&skopeo_raw_manifest(&digest_ref)?)?
    };
    let manifest: OciManifest = serde_json::from_value(manifest)?;
    Ok(Some(manifest.layers.len() + 2))
}

fn skopeo_raw_manifest(image_ref: &str) -> anyhow::Result<Vec<u8>> {
    let output = std::process::Command::new("skopeo")
        .args(["inspect", "--raw", &transport_ref(image_ref)])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("skopeo raw inspect failed for {image_ref}");
    }
    Ok(output.stdout)
}

fn image_ref_with_digest(image_ref: &str, digest: &str) -> Option<String> {
    if image_ref.contains('@') {
        return Some(image_ref.to_string());
    }
    let (prefix, image) = image_ref.rsplit_once('/').unwrap_or(("", image_ref));
    let image = image.rsplit_once(':').map_or(image, |(name, _)| name);
    if prefix.is_empty() {
        Some(format!("{image}@{digest}"))
    } else {
        Some(format!("{prefix}/{image}@{digest}"))
    }
}

fn skopeo_progress_percent(line: &str) -> Option<u64> {
    let percent_index = line.find('%')?;
    let before_percent = &line[..percent_index];
    let start = before_percent
        .rfind(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .map(|index| index + 1)
        .unwrap_or(0);
    before_percent[start..]
        .parse::<f64>()
        .ok()
        .map(|percent| percent.round().clamp(0.0, 100.0) as u64)
}

fn skopeo_progress_status(line: &str) -> Option<String> {
    if line.starts_with("Copying blob ") {
        Some("Pulling runtime image layer...".to_string())
    } else if line.starts_with("Copying config ") {
        Some("Pulling runtime image config...".to_string())
    } else if line.starts_with("Writing manifest") {
        Some("Writing runtime image manifest...".to_string())
    } else if line.starts_with("Storing signatures") {
        Some("Storing runtime image signatures...".to_string())
    } else {
        None
    }
}

fn skopeo_progress_is_copy_step(line: &str) -> bool {
    line.starts_with("Copying blob ")
        || line.starts_with("Copying config ")
        || line.starts_with("Writing manifest")
}

fn transport_ref(image_ref: &str) -> String {
    if image_ref.contains("://") {
        image_ref.to_string()
    } else {
        format!("docker://{image_ref}")
    }
}

fn read_selected_manifest(oci_layout: &Path) -> anyhow::Result<OciManifest> {
    let index_path = oci_layout.join("index.json");
    let index: OciIndex = serde_json::from_slice(&std::fs::read(&index_path)?)
        .with_context(|| format!("failed to parse {}", index_path.display()))?;
    let descriptor = index
        .manifests
        .iter()
        .find(|descriptor| descriptor_matches_host(descriptor))
        .or_else(|| index.manifests.first())
        .ok_or_else(|| anyhow::anyhow!("OCI layout has no manifests"))?;
    let manifest_path = blob_path(oci_layout, &descriptor.digest)?;
    let mut manifest: OciManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    manifest.digest = Some(descriptor.digest.clone());
    Ok(manifest)
}

fn descriptor_matches_host(descriptor: &OciDescriptor) -> bool {
    let Some(platform) = descriptor.platform.as_ref() else {
        return false;
    };
    platform.os.as_deref() == Some("linux")
        && platform.architecture.as_deref() == Some(oci_arch(std::env::consts::ARCH))
}

fn render_oci_layout_dir(oci_layout: &Path, rootfs: &Path) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(ocirender::convert_dir(oci_layout, rootfs))
        .with_context(|| format!("failed to render OCI layout {}", oci_layout.display()))
}

fn read_image_config_labels(
    oci_layout: &Path,
    manifest: &OciManifest,
) -> anyhow::Result<HashMap<String, String>> {
    let config_path = blob_path(oci_layout, &manifest.config.digest)?;
    let config: OciImageConfig = serde_json::from_slice(&std::fs::read(&config_path)?)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    Ok(config
        .config
        .and_then(|config| config.labels)
        .unwrap_or_default())
}

fn write_runtime_metadata(
    image_ref: &str,
    manifest: &OciManifest,
    labels: &HashMap<String, String>,
    store: &Path,
    key: &str,
) -> anyhow::Result<()> {
    let metadata = RuntimeRootfsMetadata {
        image_ref: Some(image_ref.to_string()),
        runtime_id: labels.get("org.aileron.runtime_id").cloned(),
        variant: labels
            .get("org.aileron.variant")
            .cloned()
            .or_else(|| image_ref_variant(image_ref)),
        digest: manifest.digest.clone(),
    };
    let metadata_dir = store.join("metadata");
    std::fs::create_dir_all(&metadata_dir)?;
    let path = metadata_dir.join(format!("{key}.json"));
    std::fs::write(path, serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}

fn blob_path(oci_layout: &Path, digest: &str) -> anyhow::Result<PathBuf> {
    let (algorithm, value) = digest
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid OCI digest {digest}"))?;
    if algorithm != "sha256"
        || !value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-' || ch == '_')
    {
        anyhow::bail!("unsupported OCI digest {digest}");
    }
    Ok(oci_layout.join("blobs").join(algorithm).join(value))
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
        let actual = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
        let actual = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

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
        let cpu = StoredRuntimeImage {
            image_id: "cpu".to_string(),
            image_ref: "example/asr:cpu".to_string(),
            names: vec!["example/asr:cpu".to_string()],
            runtime_id: "asr-whisper-cpp".to_string(),
            variant: "cpu".to_string(),
            digest: None,
            size_bytes: 1,
            source: "user".to_string(),
        };
        let vulkan = StoredRuntimeImage {
            image_id: "vulkan".to_string(),
            image_ref: "example/asr:vulkan".to_string(),
            names: vec!["example/asr:vulkan".to_string()],
            runtime_id: "asr-whisper-cpp".to_string(),
            variant: "vulkan".to_string(),
            digest: None,
            size_bytes: 1,
            source: "user".to_string(),
        };

        assert!(usage.used_by(&cpu).is_empty());
        assert_eq!(usage.used_by(&vulkan), vec!["whisper".to_string()]);
    }

    #[test]
    fn dedupes_duplicate_runtime_image_rows() {
        let mut images = vec![
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "example/vision:rocm".to_string(),
                names: vec!["example/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "example/vision:rocm".to_string(),
                names: vec!["example/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
        ];

        dedupe_runtime_images(&mut images);

        assert_eq!(images.len(), 1);
    }

    #[test]
    fn dedupes_same_runtime_image_with_multiple_refs() {
        let mut images = vec![
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "ghcr.io/example/vision:rocm".to_string(),
                names: vec!["ghcr.io/example/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "localhost/vision:rocm".to_string(),
                names: vec!["localhost/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
        ];

        dedupe_runtime_images(&mut images);

        assert_eq!(images.len(), 1);
    }

    #[test]
    fn dedupes_same_runtime_ref_with_multiple_image_ids() {
        let mut images = vec![
            StoredRuntimeImage {
                image_id: "old-image".to_string(),
                image_ref: "ghcr.io/example/vision:rocm".to_string(),
                names: vec!["ghcr.io/example/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
            StoredRuntimeImage {
                image_id: "new-image".to_string(),
                image_ref: "ghcr.io/example/vision:rocm".to_string(),
                names: vec!["ghcr.io/example/vision:rocm".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "rocm".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
        ];

        dedupe_runtime_images(&mut images);

        assert_eq!(images.len(), 1);
    }

    #[test]
    fn dedupe_prefers_user_runtime_before_system_runtime() {
        let mut images = vec![
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "ghcr.io/example/vision:cpu".to_string(),
                names: vec!["ghcr.io/example/vision:cpu".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "cpu".to_string(),
                digest: None,
                size_bytes: 1,
                source: "user".to_string(),
            },
            StoredRuntimeImage {
                image_id: "same-image".to_string(),
                image_ref: "ghcr.io/example/vision:cpu".to_string(),
                names: vec!["ghcr.io/example/vision:cpu".to_string()],
                runtime_id: "vision-llama-cpp-gemma4".to_string(),
                variant: "cpu".to_string(),
                digest: None,
                size_bytes: 1,
                source: "system".to_string(),
            },
        ];

        dedupe_runtime_images(&mut images);

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].source, "user");
    }

    #[test]
    fn remote_tag_check_rejects_local_digest_and_untagged_refs() {
        assert!(remote_tag_is_checkable("ghcr.io/example/runtime:cpu"));
        assert!(remote_tag_is_checkable("registry.example:5000/runtime:cpu"));
        assert!(!remote_tag_is_checkable("localhost/runtime:cpu"));
        assert!(!remote_tag_is_checkable(
            "ghcr.io/example/runtime@sha256:1234"
        ));
        assert!(!remote_tag_is_checkable("ghcr.io/example/runtime"));
    }

    #[test]
    fn remote_tag_runtime_status_does_not_report_update_without_remote_check() {
        let image = StoredRuntimeImage {
            image_id: "runtime".to_string(),
            image_ref: "ghcr.io/example/runtime:cpu".to_string(),
            names: vec!["ghcr.io/example/runtime:cpu".to_string()],
            runtime_id: "llm-llama-cpp".to_string(),
            variant: "cpu".to_string(),
            digest: Some("sha256:abc".to_string()),
            size_bytes: 1,
            source: "user".to_string(),
        };

        let status = runtime_image_local_status(&image);

        assert!(!status.available);
        assert_eq!(status.status, "installed: update not checked");
    }

    #[test]
    fn digest_pinned_runtime_status_is_not_updateable() {
        let image = StoredRuntimeImage {
            image_id: "runtime".to_string(),
            image_ref: "ghcr.io/example/runtime@sha256:abc".to_string(),
            names: vec!["ghcr.io/example/runtime@sha256:abc".to_string()],
            runtime_id: "llm-llama-cpp".to_string(),
            variant: "cpu".to_string(),
            digest: Some("sha256:abc".to_string()),
            size_bytes: 1,
            source: "user".to_string(),
        };

        let status = runtime_image_local_status(&image);

        assert!(!status.available);
        assert_eq!(status.status, "not checkable");
    }

    #[test]
    fn skopeo_progress_percent_parses_percentage_text() {
        assert_eq!(skopeo_progress_percent("Copying blob abc 42%"), Some(42));
        assert_eq!(skopeo_progress_percent("Copying blob abc 42.6%"), Some(43));
        assert_eq!(skopeo_progress_percent("Copying blob abc"), None);
    }

    #[test]
    fn skopeo_progress_status_maps_copy_phases() {
        assert_eq!(
            skopeo_progress_status("Copying blob sha256:abc").as_deref(),
            Some("Pulling runtime image layer...")
        );
        assert_eq!(
            skopeo_progress_status("Writing manifest to image destination").as_deref(),
            Some("Writing runtime image manifest...")
        );
        assert_eq!(
            skopeo_progress_status("Getting image source signatures"),
            None
        );
    }

    #[test]
    fn skopeo_progress_reader_derives_progress_from_stdout_lines() {
        let output = b"Getting image source signatures\n\
Copying blob sha256:one\n\
Copying blob sha256:two\n\
Copying config sha256:config\n\
Writing manifest to image destination\n";
        let mut events = Vec::new();
        let mut log = String::new();

        read_skopeo_progress(
            &output[..],
            &mut log,
            &mut |progress| match progress {
                RuntimePullProgress::Status(status) => events.push(format!("status:{status}")),
                RuntimePullProgress::Percent(percent) => events.push(format!("percent:{percent}")),
            },
            Some(4),
        )
        .expect("read skopeo progress");

        assert!(log.contains("Copying blob sha256:one"));
        assert_eq!(
            events,
            vec![
                "status:Pulling runtime image layer...",
                "percent:25",
                "status:Pulling runtime image layer...",
                "percent:50",
                "status:Pulling runtime image config...",
                "percent:75",
                "status:Writing runtime image manifest...",
                "percent:99",
            ]
        );
    }

    #[test]
    fn skopeo_copy_step_detection_ignores_non_copy_lines() {
        assert!(skopeo_progress_is_copy_step("Copying blob sha256:abc"));
        assert!(skopeo_progress_is_copy_step("Copying config sha256:abc"));
        assert!(skopeo_progress_is_copy_step(
            "Writing manifest to image destination"
        ));
        assert!(!skopeo_progress_is_copy_step(
            "Getting image source signatures"
        ));
    }

    #[test]
    fn cancelling_profile_install_marks_owned_runtime_download_only() {
        let mut installing_profiles = HashMap::from([("profile-a".to_string(), install_record())]);
        let mut runtime_downloads = HashMap::from([
            (
                "runtime:ghcr.io/example/runtime-a:cpu".to_string(),
                install_record(),
            ),
            (
                "runtime:ghcr.io/example/runtime-b:cpu".to_string(),
                install_record(),
            ),
        ]);
        let runtime_download_owners = HashMap::from([(
            "runtime:ghcr.io/example/runtime-a:cpu".to_string(),
            "profile-a".to_string(),
        )]);

        request_cancel_records(
            &mut installing_profiles,
            &mut runtime_downloads,
            &runtime_download_owners,
            "profile-a",
        );

        assert!(installing_profiles["profile-a"].cancel_requested);
        assert_eq!(installing_profiles["profile-a"].status, "Cancelling...");
        assert!(runtime_downloads["runtime:ghcr.io/example/runtime-a:cpu"].cancel_requested);
        assert_eq!(
            runtime_downloads["runtime:ghcr.io/example/runtime-a:cpu"].status,
            "Cancelling runtime setup..."
        );
        assert!(!runtime_downloads["runtime:ghcr.io/example/runtime-b:cpu"].cancel_requested);
    }

    #[test]
    fn cancelling_runtime_download_id_marks_runtime_download() {
        let mut installing_profiles = HashMap::new();
        let mut runtime_downloads = HashMap::from([(
            "runtime:ghcr.io/example/runtime-a:cpu".to_string(),
            install_record(),
        )]);
        let runtime_download_owners = HashMap::new();

        request_cancel_records(
            &mut installing_profiles,
            &mut runtime_downloads,
            &runtime_download_owners,
            "runtime:ghcr.io/example/runtime-a:cpu",
        );

        assert!(runtime_downloads["runtime:ghcr.io/example/runtime-a:cpu"].cancel_requested);
        assert_eq!(
            runtime_downloads["runtime:ghcr.io/example/runtime-a:cpu"].status,
            "Cancelling runtime setup..."
        );
    }

    #[test]
    fn image_ref_with_digest_replaces_tag_after_last_slash() {
        assert_eq!(
            image_ref_with_digest("registry.example:5000/ns/runtime:cuda", "sha256:abc"),
            Some("registry.example:5000/ns/runtime@sha256:abc".to_string())
        );
        assert_eq!(
            image_ref_with_digest("runtime:cpu", "sha256:def"),
            Some("runtime@sha256:def".to_string())
        );
    }

    #[test]
    fn render_oci_layout_applies_whiteout_files() {
        let oci_layout = std::env::temp_dir().join(format!("aileron-oci-test-{}", Uuid::new_v4()));
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));
        write_test_oci_layout(
            &oci_layout,
            vec![
                tar_with_file("etc/keep", b"old"),
                tar_with_file("etc/.wh.keep", b""),
            ],
        );

        render_oci_layout_dir(&oci_layout, &rootfs).expect("render OCI layout");
        assert!(!rootfs.join("etc/keep").exists());

        let _ = std::fs::remove_dir_all(oci_layout);
        let _ = std::fs::remove_dir_all(rootfs);
    }

    #[test]
    fn render_oci_layout_applies_opaque_whiteout() {
        let oci_layout = std::env::temp_dir().join(format!("aileron-oci-test-{}", Uuid::new_v4()));
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));
        write_test_oci_layout(
            &oci_layout,
            vec![
                tar_with_file("var/cache/old", b"old"),
                tar_with_file("var/cache/.wh..wh..opq", b""),
            ],
        );

        render_oci_layout_dir(&oci_layout, &rootfs).expect("render OCI layout");
        assert!(!rootfs.join("var/cache/old").exists());

        let _ = std::fs::remove_dir_all(oci_layout);
        let _ = std::fs::remove_dir_all(rootfs);
    }

    #[cfg(unix)]
    #[test]
    fn render_oci_layout_preserves_absolute_symlinks() {
        let oci_layout = std::env::temp_dir().join(format!("aileron-oci-test-{}", Uuid::new_v4()));
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));
        write_test_oci_layout(
            &oci_layout,
            vec![tar_with_symlink("etc/alternatives/awk", "/usr/bin/mawk")],
        );

        render_oci_layout_dir(&oci_layout, &rootfs).expect("render OCI layout");
        assert_eq!(
            std::fs::read_link(rootfs.join("etc/alternatives/awk")).unwrap(),
            PathBuf::from("/usr/bin/mawk")
        );

        let _ = std::fs::remove_dir_all(oci_layout);
        let _ = std::fs::remove_dir_all(rootfs);
    }

    #[test]
    fn render_oci_layout_resolves_hardlinks_from_archive_root() {
        let oci_layout = std::env::temp_dir().join(format!("aileron-oci-test-{}", Uuid::new_v4()));
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));
        write_test_oci_layout(
            &oci_layout,
            vec![
                tar_with_file("usr/bin/perl", b"perl"),
                tar_with_hardlink("usr/bin/perl5.40.1", "usr/bin/perl"),
            ],
        );

        render_oci_layout_dir(&oci_layout, &rootfs).expect("render OCI layout");
        assert_eq!(
            std::fs::read_to_string(rootfs.join("usr/bin/perl5.40.1")).unwrap(),
            "perl"
        );

        let _ = std::fs::remove_dir_all(oci_layout);
        let _ = std::fs::remove_dir_all(rootfs);
    }

    #[cfg(unix)]
    #[test]
    fn directory_size_does_not_follow_rootfs_symlinks() {
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));
        let target = std::env::temp_dir().join(format!("aileron-rootfs-target-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&rootfs).expect("create rootfs");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("outside"), vec![0u8; 4096]).expect("write outside file");
        symlink(&target, rootfs.join("var-run")).expect("create symlink");

        assert_eq!(directory_size_bytes(&rootfs).unwrap(), 0);

        let _ = std::fs::remove_dir_all(rootfs);
        let _ = std::fs::remove_dir_all(target);
    }

    #[test]
    #[ignore = "requires AILERON_TEST_OCI_LAYOUT pointing at a real OCI layout"]
    fn render_real_oci_layout() {
        let oci_layout = std::env::var("AILERON_TEST_OCI_LAYOUT").expect("AILERON_TEST_OCI_LAYOUT");
        let rootfs = std::env::temp_dir().join(format!("aileron-rootfs-test-{}", Uuid::new_v4()));

        render_oci_layout_dir(Path::new(&oci_layout), &rootfs).expect("render real OCI layout");

        let _ = std::fs::remove_dir_all(rootfs);
    }

    #[test]
    fn replace_runtime_rootfs_rolls_back_when_metadata_write_fails() {
        let store = std::env::temp_dir().join(format!("aileron-store-test-{}", Uuid::new_v4()));
        let rootfs_dir = store.join("rootfs");
        let tmp_dir = store.join("tmp");
        let rootfs_final = rootfs_dir.join("runtime");
        let rootfs_tmp = tmp_dir.join("new-rootfs");
        std::fs::create_dir_all(&rootfs_final).expect("create old rootfs");
        std::fs::create_dir_all(&rootfs_tmp).expect("create new rootfs");
        std::fs::write(rootfs_final.join("entrypoint.py"), "old").expect("write old file");
        std::fs::write(rootfs_tmp.join("entrypoint.py"), "new").expect("write new file");

        let error = replace_runtime_rootfs(&store, "runtime", &rootfs_tmp, || {
            anyhow::bail!("metadata failed")
        })
        .expect_err("metadata failure should fail replacement");

        assert!(error.to_string().contains("metadata failed"));
        assert_eq!(
            std::fs::read_to_string(rootfs_final.join("entrypoint.py")).unwrap(),
            "old"
        );
        assert!(!rootfs_tmp.exists());

        let _ = std::fs::remove_dir_all(store);
    }

    fn write_test_oci_layout(root: &Path, layers: Vec<Vec<u8>>) {
        use sha2::{Digest, Sha256};

        let blobs = root.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs).expect("create OCI blob dir");
        std::fs::write(root.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#)
            .expect("write oci-layout");

        let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\",\"config\":{}}";
        let config_digest = write_blob(&blobs, config);
        let layer_descriptors = layers
            .into_iter()
            .map(|layer| {
                let digest = write_blob(&blobs, &layer);
                serde_json::json!({
                    "mediaType": "application/vnd.oci.image.layer.v1.tar",
                    "digest": digest,
                    "size": layer.len(),
                })
            })
            .collect::<Vec<_>>();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": layer_descriptors,
        });
        let manifest_data = serde_json::to_vec(&manifest).expect("encode manifest");
        let manifest_digest = write_blob(&blobs, &manifest_data);
        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": manifest_digest,
                "size": manifest_data.len(),
                "platform": {"architecture": "amd64", "os": "linux"},
            }],
        });
        std::fs::write(root.join("index.json"), serde_json::to_vec(&index).unwrap())
            .expect("write index");

        fn write_blob(blobs: &Path, data: &[u8]) -> String {
            let digest = Sha256::digest(data)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            std::fs::write(blobs.join(&digest), data).expect("write blob");
            format!("sha256:{digest}")
        }
    }

    fn install_record() -> InstallRecord {
        InstallRecord {
            bytes_pulled: 0,
            total_bytes: 0,
            status: "Pulling runtime image...".to_string(),
            cancel_requested: false,
            samples: std::collections::VecDeque::new(),
        }
    }

    fn tar_with_file(path: &str, contents: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut data);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, contents)
                .expect("append tar entry");
            builder.finish().expect("finish tar");
        }
        data
    }

    fn tar_with_symlink(path: &str, target: &str) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut data);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name(target).expect("set link target");
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::empty())
                .expect("append symlink entry");
            builder.finish().expect("finish tar");
        }
        data
    }

    fn tar_with_hardlink(path: &str, target: &str) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut data);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Link);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_link_name(target).expect("set link target");
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::empty())
                .expect("append hardlink entry");
            builder.finish().expect("finish tar");
        }
        data
    }
}
