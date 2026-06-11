/// Runtime and model manifest storage and lookup.
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::hardware::Variant;
use crate::profiles::RuntimeImage;

#[derive(Debug, Clone, Default)]
pub struct RuntimeManifestStore {
    runtimes: HashMap<String, RuntimeManifest>,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeManifest {
    runtime_id: String,
    images: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeManifestInfo {
    pub runtime_id: String,
    pub variants: Vec<String>,
}

impl RuntimeManifestStore {
    pub fn load() -> Result<Self> {
        Self::load_from_dirs(manifest_dirs())
    }

    fn load_from_dirs(dirs: Vec<PathBuf>) -> Result<Self> {
        let mut runtimes = HashMap::new();
        for dir in dirs {
            let runtimes_dir = dir.join("runtimes");
            if !runtimes_dir.exists() {
                continue;
            }

            for entry in std::fs::read_dir(&runtimes_dir)
                .with_context(|| format!("read {}", runtimes_dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let data = std::fs::read_to_string(&path)
                    .with_context(|| format!("read runtime manifest {}", path.display()))?;
                let runtime: RuntimeManifest = serde_json::from_str(&data)
                    .with_context(|| format!("parse runtime manifest {}", path.display()))?;
                runtimes.insert(runtime.runtime_id.clone(), runtime);
            }
        }

        Ok(Self { runtimes })
    }

    pub fn resolve(&self, runtime_id: &str, detected: Variant) -> Option<&str> {
        let runtime = self.runtimes.get(runtime_id)?;
        let detected_tag = detected.as_tag();
        runtime
            .images
            .get(detected_tag)
            .or_else(|| {
                if detected == Variant::Cpu {
                    None
                } else {
                    runtime.images.get("cpu")
                }
            })
            .map(String::as_str)
    }

    pub fn images_for(&self, runtime_id: &str) -> Vec<RuntimeImage> {
        let runtime = match self.runtimes.get(runtime_id) {
            Some(runtime) => runtime,
            None => return Vec::new(),
        };
        let mut images: Vec<_> = runtime
            .images
            .iter()
            .map(|(variant, image_ref)| RuntimeImage {
                variant: variant.clone(),
                image_ref: image_ref.clone(),
            })
            .collect();
        images.sort_by(|a, b| a.variant.cmp(&b.variant));
        images
    }

    pub fn all(&self) -> Vec<RuntimeManifestInfo> {
        let mut runtimes: Vec<_> = self
            .runtimes
            .values()
            .map(|runtime| {
                let mut variants: Vec<_> = runtime.images.keys().cloned().collect();
                variants.sort();
                RuntimeManifestInfo {
                    runtime_id: runtime.runtime_id.clone(),
                    variants,
                }
            })
            .collect();
        runtimes.sort_by(|a, b| a.runtime_id.cmp(&b.runtime_id));
        runtimes
    }
}

pub fn manifest_dirs() -> Vec<PathBuf> {
    if let Ok(paths) = std::env::var("AILERON_MANIFEST_DIRS") {
        return std::env::split_paths(&paths).collect();
    }

    let mut dirs = Vec::new();
    if let Ok(data_home) = std::env::var("AILERON_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join("aileron").join("manifests"));
    } else if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join("aileron").join("manifests"));
    } else if let Ok(home) = std::env::var("HOME") {
        dirs.push(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("aileron")
                .join("manifests"),
        );
    }

    dirs.push(PathBuf::from("/etc/aileron/manifests"));
    dirs.push(PathBuf::from("/usr/share/aileron/manifests"));
    dirs.push(PathBuf::from("manifests"));
    dirs
}

pub fn find_model_manifest(profile_id: &str) -> Result<Option<PathBuf>> {
    let filename = format!("{profile_id}.json");
    for dir in manifest_dirs() {
        let path = dir.join("models").join(&filename);
        if path.exists() {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_detected_variant_with_cpu_fallback() {
        let runtime = RuntimeManifest {
            runtime_id: "llm-llama-cpp".to_string(),
            images: HashMap::from([
                ("cpu".to_string(), "example/llm:cpu".to_string()),
                ("cuda".to_string(), "example/llm:cuda".to_string()),
            ]),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("llm-llama-cpp".to_string(), runtime)]),
        };

        assert_eq!(
            manifests.resolve("llm-llama-cpp", Variant::Cuda),
            Some("example/llm:cuda")
        );
        assert_eq!(
            manifests.resolve("llm-llama-cpp", Variant::Vulkan),
            Some("example/llm:cpu")
        );
    }

    #[test]
    fn does_not_fallback_from_cpu() {
        let runtime = RuntimeManifest {
            runtime_id: "llm-llama-cpp".to_string(),
            images: HashMap::from([("cuda".to_string(), "example/llm:cuda".to_string())]),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("llm-llama-cpp".to_string(), runtime)]),
        };

        assert_eq!(manifests.resolve("llm-llama-cpp", Variant::Cpu), None);
    }
}
