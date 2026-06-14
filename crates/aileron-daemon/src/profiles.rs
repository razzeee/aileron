/// Installed model profile storage.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::hardware::Variant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeImage {
    pub variant: String,
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
    pub artifact_hashes: Vec<ArtifactHash>,
    pub installed_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileStore {
    profiles: HashMap<String, Profile>,
}

impl ProfileStore {
    pub fn load() -> Result<Self> {
        let dir = profiles_dir();
        if !dir.exists() {
            return Ok(Self::default());
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

        Ok(Self { profiles })
    }

    pub fn all(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.values()
    }

    pub fn get(&self, profile_id: &str) -> Option<&Profile> {
        self.profiles.get(profile_id)
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
        let removed = self.profiles.remove(profile_id);
        let path = profile_path(profile_id);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(removed)
    }
}

impl Profile {
    pub fn runtime_image_for(&self, detected: Variant) -> Option<&str> {
        detected.fallback_tags().iter().find_map(|variant| {
            self.runtime_images
                .iter()
                .find(|img| img.variant == *variant)
                .map(|img| img.image_ref.as_str())
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_with_runtime_images(runtime_images: Vec<RuntimeImage>) -> Profile {
        Profile {
            profile_id: "profile".to_string(),
            model_id: "model".to_string(),
            runtime_id: "runtime".to_string(),
            runtime_options: HashMap::new(),
            artifact_path: PathBuf::from("/tmp/model"),
            runtime_images,
            use_cases: vec!["asr.transcribe".to_string()],
            artifact_hashes: Vec::new(),
            installed_at: "2026-06-14T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn runtime_image_for_prefers_vulkan_before_cpu_for_accelerators() {
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
}
