/// Runtime and model manifest storage, validation, and lookup.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
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

#[derive(Debug, Clone, Deserialize)]
pub struct ModelManifest {
    pub profile_id: String,
    pub model_id: String,
    #[serde(default)]
    pub llmfit_model_id: String,
    pub runtime_id: String,
    #[serde(default)]
    pub runtime_options: HashMap<String, String>,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub disk_size_gb: f64,
    #[serde(default)]
    pub min_ram_gb: f64,
    #[serde(default)]
    pub runtime_images: Vec<RuntimeImage>,
    pub use_cases: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<ManifestArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestArtifact {
    #[serde(default)]
    pub role: String,
    pub url: String,
    pub filename: String,
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: u64,
}

impl ModelManifest {
    pub fn into_profile(self, artifact_path: PathBuf) -> crate::profiles::Profile {
        crate::profiles::Profile {
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
                .map(|artifact| crate::profiles::ArtifactHash {
                    role: artifact.role,
                    filename: artifact.filename,
                    sha256: artifact.sha256,
                })
                .collect(),
            installed_at: chrono::Utc::now().to_rfc3339(),
            source: "user".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeManifestInfo {
    pub runtime_id: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogProfileInfo {
    pub profile_id: String,
    pub model_id: String,
    pub llmfit_model_id: String,
    pub runtime_id: String,
    pub tier: String,
    pub disk_size_gb: f64,
    pub min_ram_gb: f64,
    pub use_cases: Vec<String>,
}

impl RuntimeManifestStore {
    pub fn load() -> Result<Self> {
        Self::load_from_dirs(manifest_dirs())
    }

    pub fn load_from_dirs(dirs: Vec<PathBuf>) -> Result<Self> {
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
                let runtime = parse_runtime_manifest_json(&data)
                    .with_context(|| format!("parse runtime manifest {}", path.display()))?;
                runtimes.insert(runtime.runtime_id.clone(), runtime);
            }
        }

        Ok(Self { runtimes })
    }

    pub fn resolve(&self, runtime_id: &str, detected: Variant) -> Option<&str> {
        let runtime = self.runtimes.get(runtime_id)?;
        detected
            .fallback_tags()
            .iter()
            .find_map(|variant| runtime.images.get(*variant).map(String::as_str))
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

pub fn list_catalog_profiles() -> Result<Vec<CatalogProfileInfo>> {
    let mut profiles = Vec::new();
    for dir in manifest_dirs() {
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
            let manifest = parse_model_manifest_json(&data)
                .with_context(|| format!("parse model manifest {}", path.display()))?;
            let disk_size_gb = manifest_disk_size_gb(&manifest);
            profiles.push(CatalogProfileInfo {
                profile_id: manifest.profile_id,
                model_id: manifest.model_id,
                llmfit_model_id: manifest.llmfit_model_id,
                runtime_id: manifest.runtime_id,
                tier: manifest.tier,
                disk_size_gb,
                min_ram_gb: manifest.min_ram_gb,
                use_cases: manifest.use_cases,
            });
        }
    }
    profiles.sort_by(|a, b| a.profile_id.cmp(&b.profile_id));
    profiles.dedup_by(|a, b| a.profile_id == b.profile_id);
    Ok(profiles)
}

fn manifest_disk_size_gb(manifest: &ModelManifest) -> f64 {
    let size_bytes: u64 = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.size_bytes)
        .sum();
    if size_bytes > 0 {
        size_bytes as f64 / 1024.0 / 1024.0 / 1024.0
    } else {
        manifest.disk_size_gb
    }
}

pub fn parse_model_manifest_json(data: &str) -> Result<ModelManifest> {
    let manifest: ModelManifest = serde_json::from_str(data)?;
    validate_non_empty("profile_id", &manifest.profile_id)?;
    validate_non_empty("model_id", &manifest.model_id)?;
    if !manifest.llmfit_model_id.is_empty() {
        validate_non_empty("llmfit_model_id", &manifest.llmfit_model_id)?;
    }
    validate_non_empty("runtime_id", &manifest.runtime_id)?;
    for (key, value) in &manifest.runtime_options {
        validate_runtime_option(key, value)?;
    }
    if manifest.disk_size_gb < 0.0 {
        bail!("disk_size_gb must not be negative");
    }
    if manifest.min_ram_gb < 0.0 {
        bail!("min_ram_gb must not be negative");
    }
    if !manifest.tier.is_empty() {
        validate_tier(&manifest.tier)?;
    }
    validate_use_cases(&manifest.use_cases)?;
    for image in &manifest.runtime_images {
        validate_variant(&image.variant)?;
        validate_non_empty("runtime_images[].image_ref", &image.image_ref)?;
    }
    let mut artifact_roles = HashSet::new();
    for artifact in &manifest.artifacts {
        validate_artifact(artifact)?;
        if !artifact.role.is_empty() && !artifact_roles.insert(artifact.role.as_str()) {
            bail!("duplicate artifact role: {}", artifact.role);
        }
    }
    Ok(manifest)
}

fn parse_runtime_manifest_json(data: &str) -> Result<RuntimeManifest> {
    let manifest: RuntimeManifest = serde_json::from_str(data)?;
    validate_non_empty("runtime_id", &manifest.runtime_id)?;
    if manifest.images.is_empty() {
        bail!("runtime manifest must define at least one image");
    }
    for (variant, image_ref) in &manifest.images {
        validate_variant(variant)?;
        validate_non_empty("images[].image_ref", image_ref)?;
    }
    Ok(manifest)
}

pub fn validate_use_cases(use_cases: &[String]) -> Result<()> {
    if use_cases.is_empty() {
        bail!("at least one use-case is required");
    }
    for use_case in use_cases {
        if !SUPPORTED_USE_CASES.contains(&use_case.as_str()) {
            bail!("unsupported use-case: {use_case}");
        }
    }
    Ok(())
}

pub const SUPPORTED_USE_CASES: &[&str] = &[
    "llm.summarize",
    "llm.translate",
    "llm.rephrase",
    "llm.classify",
    "llm.extract",
    "llm.analyze",
    "llm.chat",
    "llm.embed",
    "asr.transcribe",
    "asr.translate",
    "vision.describe",
    "vision.segment",
    "vision.ocr",
];

fn validate_artifact(artifact: &ManifestArtifact) -> Result<()> {
    if !artifact.role.is_empty() {
        validate_artifact_role(&artifact.role)?;
    }
    validate_non_empty("artifacts[].url", &artifact.url)?;
    validate_non_empty("artifacts[].filename", &artifact.filename)?;
    if artifact.filename.contains('/') || artifact.filename.contains('\\') {
        bail!(
            "artifact filename must not contain path separators: {}",
            artifact.filename
        );
    }
    validate_sha256(&artifact.sha256)
}

fn validate_runtime_option(key: &str, value: &str) -> Result<()> {
    validate_non_empty("runtime_options key", key)?;
    validate_non_empty("runtime_options value", value)?;
    if !key
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("runtime option keys must be uppercase ASCII, digits or '_': {key}");
    }
    if key.starts_with("AILERON_") {
        bail!("runtime option keys must not use reserved AILERON_ prefix: {key}");
    }
    if value.contains('\0') || value.contains('\n') {
        bail!("runtime option values must not contain NUL or newline characters: {key}");
    }
    Ok(())
}

fn validate_artifact_role(value: &str) -> Result<()> {
    validate_non_empty("artifacts[].role", value)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!("artifact role must use lowercase ASCII, digits, '_' or '-': {value}");
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("sha256 must be 64 hexadecimal characters");
    }
    Ok(())
}

fn validate_variant(value: &str) -> Result<()> {
    match value {
        "cpu" | "cuda" | "rocm" | "vulkan" => Ok(()),
        other => bail!("unsupported runtime variant: {other}"),
    }
}

fn validate_tier(value: &str) -> Result<()> {
    match value {
        "small" | "balanced" | "large" | "accelerated" => Ok(()),
        other => bail!("unsupported model tier: {other}"),
    }
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
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
    fn resolves_accelerator_variant_with_vulkan_fallback() {
        let runtime = RuntimeManifest {
            runtime_id: "asr-whisper-cpp".to_string(),
            images: HashMap::from([
                ("cpu".to_string(), "example/asr:cpu".to_string()),
                ("vulkan".to_string(), "example/asr:vulkan".to_string()),
            ]),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("asr-whisper-cpp".to_string(), runtime)]),
        };

        assert_eq!(
            manifests.resolve("asr-whisper-cpp", Variant::Rocm),
            Some("example/asr:vulkan")
        );
        assert_eq!(
            manifests.resolve("asr-whisper-cpp", Variant::Cuda),
            Some("example/asr:vulkan")
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

    #[test]
    fn validates_model_manifest() {
        let data = r#"
        {
            "profile_id": "stub",
            "model_id": "stub",
            "runtime_id": "stub",
            "tier": "small",
            "disk_size_gb": 0.0,
            "min_ram_gb": 1.0,
            "use_cases": ["llm.summarize"],
            "artifacts": []
        }
        "#;

        parse_model_manifest_json(data).expect("valid model manifest");
    }

    #[test]
    fn accepts_runtime_options() {
        let data = r#"
        {
            "profile_id": "vision-profile",
            "model_id": "vision-model",
            "runtime_id": "vision-llama-cpp",
            "runtime_options": { "VISION_HANDLER": "gemma4" },
            "use_cases": ["vision.describe"],
            "artifacts": []
        }
        "#;

        let manifest = parse_model_manifest_json(data).expect("runtime options should be valid");
        assert_eq!(
            manifest
                .runtime_options
                .get("VISION_HANDLER")
                .map(String::as_str),
            Some("gemma4")
        );
    }

    #[test]
    fn rejects_invalid_runtime_option_key() {
        let data = r#"
        {
            "profile_id": "vision-profile",
            "model_id": "vision-model",
            "runtime_id": "vision-llama-cpp",
            "runtime_options": { "vision_handler": "gemma4" },
            "use_cases": ["vision.describe"],
            "artifacts": []
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("invalid key should fail");
        assert!(err.to_string().contains("runtime option keys"));
    }

    #[test]
    fn derives_disk_size_from_artifact_sizes() {
        let manifest = ModelManifest {
            profile_id: "sized".to_string(),
            model_id: "sized".to_string(),
            llmfit_model_id: String::new(),
            runtime_id: "stub".to_string(),
            runtime_options: Default::default(),
            tier: String::new(),
            disk_size_gb: 99.0,
            min_ram_gb: 0.0,
            runtime_images: Vec::new(),
            use_cases: vec!["llm.summarize".to_string()],
            artifacts: vec![ManifestArtifact {
                role: "model".to_string(),
                url: "https://example.invalid/model.gguf".to_string(),
                filename: "model.gguf".to_string(),
                sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                size_bytes: 1024 * 1024 * 1024,
            }],
        };

        assert_eq!(manifest_disk_size_gb(&manifest), 1.0);
    }

    #[test]
    fn validates_example_model_manifests() {
        for data in [
            include_str!("../../../manifests/examples/models/llama3.2-3b-instruct-q4-k-m.json"),
            include_str!("../../../manifests/examples/models/whisper-large-v3-turbo-q5-0.json"),
            include_str!("../../../manifests/examples/models/gemma-4-e4b-it-q4-k-xl.json"),
        ] {
            parse_model_manifest_json(data).expect("example model manifest should be valid");
        }
    }

    #[test]
    fn validates_profile_library_manifests() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests/models");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("read profile library model manifests") {
            let path = entry.expect("read profile library model entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let data = std::fs::read_to_string(&path).expect("read profile library model manifest");
            parse_model_manifest_json(&data)
                .expect("profile library model manifest should be valid");
            count += 1;
        }
        assert!(
            count >= 15,
            "expected at least 15 profile library manifests"
        );
    }

    #[test]
    fn profile_library_does_not_duplicate_same_runtime_artifacts() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests/models");
        let mut profiles_by_signature: HashMap<String, Vec<String>> = HashMap::new();
        for entry in std::fs::read_dir(&dir).expect("read profile library model manifests") {
            let path = entry.expect("read profile library model entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let data = std::fs::read_to_string(&path).expect("read profile library model manifest");
            let manifest = parse_model_manifest_json(&data)
                .expect("profile library model manifest should be valid");
            let mut artifacts = manifest
                .artifacts
                .iter()
                .map(|artifact| format!("{}:{}:{}", artifact.role, artifact.url, artifact.sha256))
                .collect::<Vec<_>>();
            artifacts.sort();
            let signature = format!("{}|{}", manifest.runtime_id, artifacts.join("|"));
            profiles_by_signature
                .entry(signature)
                .or_default()
                .push(manifest.profile_id);
        }

        let duplicates = profiles_by_signature
            .into_values()
            .filter(|profiles| profiles.len() > 1)
            .collect::<Vec<_>>();

        assert!(
            duplicates.is_empty(),
            "profiles with identical runtime/artifacts should be merged: {duplicates:?}"
        );
    }

    #[test]
    fn validates_profile_library_runtime_manifests() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manifests/runtimes");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("read profile library runtime manifests") {
            let path = entry.expect("read profile library runtime entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let data =
                std::fs::read_to_string(&path).expect("read profile library runtime manifest");
            parse_runtime_manifest_json(&data)
                .expect("profile library runtime manifest should be valid");
            count += 1;
        }
        assert!(
            count >= 3,
            "expected at least 3 profile library runtime manifests"
        );
    }

    #[test]
    fn rejects_unknown_use_case() {
        let data = r#"
        {
            "profile_id": "stub",
            "model_id": "stub",
            "runtime_id": "stub",
            "use_cases": ["llm.magic"],
            "artifacts": []
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("unknown use-case should fail");

        assert!(err.to_string().contains("unsupported use-case"));
    }

    #[test]
    fn rejects_artifact_path_traversal() {
        let data = r#"
        {
            "profile_id": "bad",
            "model_id": "bad",
            "runtime_id": "stub",
            "use_cases": ["llm.summarize"],
            "artifacts": [{
                "role": "model",
                "url": "https://example.invalid/model.gguf",
                "filename": "../model.gguf",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "size_bytes": 123
            }]
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("path traversal should fail");

        assert!(err.to_string().contains("path separators"));
    }

    #[test]
    fn rejects_duplicate_artifact_roles() {
        let data = r#"
        {
            "profile_id": "bad",
            "model_id": "bad",
            "runtime_id": "stub",
            "use_cases": ["vision.describe"],
            "artifacts": [
                {
                    "role": "model",
                    "url": "https://example.invalid/model.gguf",
                    "filename": "model.gguf",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "role": "model",
                    "url": "https://example.invalid/mmproj.gguf",
                    "filename": "mmproj.gguf",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            ]
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("duplicate role should fail");

        assert!(err.to_string().contains("duplicate artifact role"));
    }

    #[test]
    fn validates_runtime_manifest() {
        let data = r#"
        {
            "runtime_id": "stub",
            "images": {
                "cpu": "localhost/aileron/stub:cpu"
            }
        }
        "#;

        parse_runtime_manifest_json(data).expect("valid runtime manifest");
    }

    #[test]
    fn rejects_unknown_runtime_variant() {
        let data = r#"
        {
            "runtime_id": "stub",
            "images": {
                "quantum": "localhost/aileron/stub:quantum"
            }
        }
        "#;

        let err = parse_runtime_manifest_json(data).expect_err("unknown variant should fail");

        assert!(err.to_string().contains("unsupported runtime variant"));
    }

    #[test]
    fn rejects_unknown_model_tier() {
        let data = r#"
        {
            "profile_id": "stub",
            "model_id": "stub",
            "runtime_id": "stub",
            "tier": "tiny-ish",
            "use_cases": ["llm.summarize"],
            "artifacts": []
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("unknown tier should fail");

        assert!(err.to_string().contains("unsupported model tier"));
    }
}
