/// Installed model profile storage.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hardware::Variant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeImage {
    pub variant: String,
    pub image_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCandidate {
    pub variant: Variant,
    pub image_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactHash {
    #[serde(default)]
    pub role: String,
    pub filename: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub profile_id: String,
    pub model_id: String,
    pub runtime_id: String,
    #[serde(default)]
    pub runtime_options: HashMap<String, String>,
    pub artifact_path: PathBuf,
    #[serde(default)]
    pub runtime_images: Vec<RuntimeImage>,
    pub use_cases: Vec<String>,
    #[serde(default)]
    pub specializations: Vec<String>,
    #[serde(default)]
    pub artifact_hashes: Vec<ArtifactHash>,
    pub installed_at: String,
    #[serde(default = "default_source")]
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileStore {
    profiles: HashMap<String, Profile>,
    system_profiles: HashMap<String, Profile>,
}

impl ProfileStore {
    pub fn load() -> Result<Self> {
        let dir = profiles_dir();
        let system_profiles = load_system_profiles()?;
        if !dir.exists() {
            return Ok(Self {
                profiles: HashMap::new(),
                system_profiles,
            });
        }

        let mut profiles = HashMap::new();
        for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let data = std::fs::read_to_string(&path)
                .with_context(|| format!("read profile {}", path.display()))?;
            let profile: Profile = serde_json::from_str(&data)
                .with_context(|| format!("parse profile {}", path.display()))?;
            profiles.insert(profile.profile_id.clone(), profile);
        }

        Ok(Self {
            profiles,
            system_profiles,
        })
    }

    pub fn all(&self) -> impl Iterator<Item = &Profile> {
        self.profiles
            .values()
            .chain(
                self.system_profiles
                    .iter()
                    .filter_map(|(profile_id, profile)| {
                        (!self.profiles.contains_key(profile_id)).then_some(profile)
                    }),
            )
    }

    pub fn get(&self, profile_id: &str) -> Option<&Profile> {
        self.profiles
            .get(profile_id)
            .or_else(|| self.system_profiles.get(profile_id))
    }

    pub fn insert(&mut self, profile: Profile) -> Result<()> {
        validate_profile(&profile)?;
        std::fs::create_dir_all(profiles_dir())?;
        let path = profile_path(&profile.profile_id);
        std::fs::write(&path, serde_json::to_string_pretty(&profile)?)
            .with_context(|| format!("write profile {}", path.display()))?;
        self.profiles.insert(profile.profile_id.clone(), profile);
        Ok(())
    }

    pub fn remove(&mut self, profile_id: &str) -> Result<Option<Profile>> {
        if !self.profiles.contains_key(profile_id) && self.system_profiles.contains_key(profile_id)
        {
            bail!("system-backed profile cannot be deleted: {profile_id}");
        }
        let removed = self.profiles.remove(profile_id);
        let path = profile_path(profile_id);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(removed)
    }
}

impl Profile {
    pub fn effective_use_cases(&self) -> Vec<String> {
        let mut use_cases = self.use_cases.clone();
        if self.implies_use_case("speech.translate")
            && !use_cases
                .iter()
                .any(|use_case| use_case == "speech.translate")
        {
            use_cases.push("speech.translate".to_string());
        }
        use_cases
    }

    pub fn supports_use_case(&self, use_case: &str) -> bool {
        self.use_cases.iter().any(|supported| supported == use_case)
            || self.implies_use_case(use_case)
    }

    pub fn runtime_image_for(&self, detected: Variant) -> Option<&str> {
        self.runtime_image_candidates(detected).into_iter().next()
    }

    pub fn runtime_image_candidates(&self, detected: Variant) -> Vec<&str> {
        self.runtime_candidates(detected)
            .into_iter()
            .map(|candidate| {
                self.runtime_images
                    .iter()
                    .find(|image| image.image_ref == candidate.image_ref)
                    .map(|image| image.image_ref.as_str())
                    .expect("runtime candidate came from profile image")
            })
            .collect()
    }

    pub fn runtime_candidates(&self, detected: Variant) -> Vec<RuntimeCandidate> {
        let mut candidates = Vec::new();
        for variant_tag in detected.fallback_tags() {
            let Some(variant) = Variant::from_tag(variant_tag) else {
                continue;
            };
            if let Some(image_ref) = self
                .runtime_images
                .iter()
                .find(|img| img.variant == *variant_tag)
                .map(|img| img.image_ref.clone())
                && !candidates.iter().any(|candidate: &RuntimeCandidate| {
                    candidate.variant == variant && candidate.image_ref == image_ref
                })
            {
                candidates.push(RuntimeCandidate { variant, image_ref });
            }
        }
        candidates
    }

    fn implies_use_case(&self, use_case: &str) -> bool {
        use_case == "speech.translate"
            && self.runtime_id == "asr-whisper-cpp"
            && self
                .use_cases
                .iter()
                .any(|supported| supported == "speech.transcribe")
    }
}

pub fn data_dir() -> PathBuf {
    let data_home = std::env::var("AILERON_DATA_HOME")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{}/.local/share", home)
        });
    PathBuf::from(data_home).join("aileron")
}

pub fn model_dir(model_id: &str) -> PathBuf {
    data_dir().join("models").join(model_id)
}

pub fn system_model_dir(model_id: &str) -> PathBuf {
    system_data_dir().join("models").join(model_id)
}

pub fn system_data_dir() -> PathBuf {
    std::env::var("AILERON_SYSTEM_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/lib/aileron"))
}

fn profiles_dir() -> PathBuf {
    data_dir().join("profiles")
}

fn profile_path(profile_id: &str) -> PathBuf {
    profiles_dir().join(format!("{profile_id}.json"))
}

fn validate_profile(profile: &Profile) -> Result<()> {
    if profile.profile_id.trim().is_empty() {
        bail!("profile_id must not be empty");
    }
    if profile.model_id.trim().is_empty() {
        bail!("model_id must not be empty");
    }
    if profile.runtime_id.trim().is_empty() {
        bail!("runtime_id must not be empty");
    }
    if !Path::new(&profile.artifact_path).exists() {
        bail!(
            "artifact path does not exist: {}",
            profile.artifact_path.display()
        );
    }
    Ok(())
}

fn default_source() -> String {
    "user".to_string()
}

fn load_system_profiles() -> Result<HashMap<String, Profile>> {
    let mut profiles = HashMap::new();
    for dir in crate::manifests::manifest_dirs() {
        let models_dir = dir.join("models");
        if !models_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&models_dir)
            .with_context(|| format!("read {}", models_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let data = std::fs::read_to_string(&path)
                .with_context(|| format!("read model manifest {}", path.display()))?;
            let manifest = crate::manifests::parse_model_manifest_json(&data)
                .with_context(|| format!("parse model manifest {}", path.display()))?;
            let artifact_dir = system_model_dir(&manifest.model_id);
            if !artifact_dir.is_dir()
                || !system_artifacts_match(&artifact_dir, &manifest.artifacts)?
            {
                continue;
            }

            let mut profile = manifest.into_profile(artifact_dir);
            profile.installed_at = "system".to_string();
            profile.source = "system".to_string();
            profiles
                .entry(profile.profile_id.clone())
                .or_insert(profile);
        }
    }
    Ok(profiles)
}

fn system_artifacts_match(
    target_dir: &Path,
    artifacts: &[crate::manifests::ManifestArtifact],
) -> Result<bool> {
    for artifact in artifacts {
        let path = target_dir.join(&artifact.filename);
        if !path.is_file() {
            return Ok(false);
        }
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("open system model artifact {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    fn profile_with_runtime_images(runtime_images: Vec<RuntimeImage>) -> Profile {
        Profile {
            profile_id: "profile".to_string(),
            model_id: "model".to_string(),
            runtime_id: "runtime".to_string(),
            runtime_options: HashMap::new(),
            artifact_path: PathBuf::from("/tmp/model"),
            runtime_images,
            use_cases: vec!["speech.transcribe".to_string()],
            specializations: Vec::new(),
            artifact_hashes: Vec::new(),
            installed_at: "2026-06-14T00:00:00Z".to_string(),
            source: "user".to_string(),
        }
    }

    #[test]
    fn runtime_image_for_uses_vulkan_fallback_for_accelerators() {
        let profile = profile_with_runtime_images(vec![
            RuntimeImage {
                variant: "cpu".to_string(),
                image_ref: "example/asr:cpu".to_string(),
            },
            RuntimeImage {
                variant: "vulkan".to_string(),
                image_ref: "example/asr:vulkan".to_string(),
            },
        ]);

        assert_eq!(
            profile.runtime_image_for(Variant::Rocm),
            Some("example/asr:vulkan")
        );
        assert_eq!(
            profile.runtime_image_for(Variant::Cuda),
            Some("example/asr:vulkan")
        );
    }

    #[test]
    fn runtime_image_for_uses_cpu_as_final_fallback() {
        let profile = profile_with_runtime_images(vec![RuntimeImage {
            variant: "cpu".to_string(),
            image_ref: "example/asr:cpu".to_string(),
        }]);

        assert_eq!(
            profile.runtime_image_for(Variant::Rocm),
            Some("example/asr:cpu")
        );
        assert_eq!(
            profile.runtime_image_candidates(Variant::Rocm),
            vec!["example/asr:cpu"]
        );
    }

    #[test]
    fn runtime_candidates_keep_same_ref_for_distinct_variants() {
        let shared_ref = "example/runtime@sha256:abcdef";
        let profile = profile_with_runtime_images(vec![
            RuntimeImage {
                variant: "cpu".to_string(),
                image_ref: shared_ref.to_string(),
            },
            RuntimeImage {
                variant: "vulkan".to_string(),
                image_ref: shared_ref.to_string(),
            },
        ]);

        let candidates = profile.runtime_candidates(Variant::Vulkan);

        assert_eq!(
            candidates,
            vec![
                RuntimeCandidate {
                    variant: Variant::Vulkan,
                    image_ref: shared_ref.to_string(),
                },
                RuntimeCandidate {
                    variant: Variant::Cpu,
                    image_ref: shared_ref.to_string(),
                },
            ]
        );
    }

    #[hegel::test]
    fn runtime_image_for_uses_first_available_fallback_tag(tc: TestCase) {
        let variant = tc.draw(gs::sampled_from(vec![
            Variant::Cpu,
            Variant::Cuda,
            Variant::Rocm,
            Variant::Vulkan,
        ]));
        let available = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                "cpu".to_string(),
                "cuda".to_string(),
                "rocm".to_string(),
                "vulkan".to_string(),
            ]))
            .max_size(4),
        );
        let profile = profile_with_runtime_images(
            available
                .iter()
                .map(|tag| RuntimeImage {
                    variant: tag.clone(),
                    image_ref: format!("example/runtime:{tag}"),
                })
                .collect(),
        );

        let expected = variant
            .fallback_tags()
            .iter()
            .find(|tag| available.iter().any(|available| available == *tag))
            .map(|tag| format!("example/runtime:{tag}"));

        assert_eq!(profile.runtime_image_for(variant), expected.as_deref());
    }

    #[test]
    fn validate_profile_requires_existing_artifact_path() {
        let mut profile = profile_with_runtime_images(Vec::new());
        profile.artifact_path = test_dir("missing-artifact-path");

        let err = validate_profile(&profile).expect_err("missing artifact path should fail");

        assert!(err.to_string().contains("artifact path does not exist"));
    }

    #[test]
    fn validate_profile_accepts_existing_artifact_path() {
        let dir = test_dir("existing-artifact-path");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create artifact path");
        let mut profile = profile_with_runtime_images(Vec::new());
        profile.artifact_path = dir.clone();

        validate_profile(&profile).expect("existing artifact path should be valid");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn system_artifacts_match_hashes_declared_artifacts() {
        let dir = test_dir("system-artifacts-match");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("model.gguf"), b"model").expect("write artifact");
        let sha256 = Sha256::digest(b"model")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let artifacts = vec![crate::manifests::ManifestArtifact {
            role: "model".to_string(),
            url: "https://example.invalid/model.gguf".to_string(),
            filename: "model.gguf".to_string(),
            sha256,
            size_bytes: 5,
        }];

        assert!(system_artifacts_match(&dir, &artifacts).expect("hash artifacts"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn system_artifacts_reject_hash_mismatch() {
        let dir = test_dir("system-artifacts-mismatch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create artifact dir");
        std::fs::write(dir.join("model.gguf"), b"model").expect("write artifact");
        let artifacts = vec![crate::manifests::ManifestArtifact {
            role: "model".to_string(),
            url: "https://example.invalid/model.gguf".to_string(),
            filename: "model.gguf".to_string(),
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            size_bytes: 5,
        }];

        assert!(!system_artifacts_match(&dir, &artifacts).expect("hash artifacts"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_rejects_system_backed_profile_without_user_shadow() {
        let mut store = ProfileStore {
            profiles: HashMap::new(),
            system_profiles: HashMap::from([(
                "profile".to_string(),
                Profile {
                    source: "system".to_string(),
                    ..profile_with_runtime_images(Vec::new())
                },
            )]),
        };

        let err = store
            .remove("profile")
            .expect_err("system profile deletion should fail");

        assert!(err.to_string().contains("system-backed profile"));
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }
}
