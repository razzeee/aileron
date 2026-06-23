/// Runtime and model manifest storage, validation, and lookup.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use llmfit_core::{Capability, ModelFormat, UseCase};
use serde::Deserialize;

use crate::hardware::Variant;
use crate::profiles::{RuntimeCandidate, RuntimeImage};

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
    pub specializations: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<ManifestArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestArtifact {
    #[serde(default)]
    pub role: String,
    pub url: String,
    #[serde(default)]
    pub filename: String,
    pub sha256: String,
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawModelManifest {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    model_id: String,
    #[serde(default)]
    llmfit_model_id: String,
    #[serde(default)]
    runtime_id: String,
    #[serde(default)]
    runtime_options: HashMap<String, String>,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    disk_size_gb: f64,
    #[serde(default)]
    min_ram_gb: f64,
    #[serde(default)]
    runtime_images: Vec<RuntimeImage>,
    #[serde(default)]
    use_cases: Vec<String>,
    #[serde(default)]
    specializations: Vec<String>,
    #[serde(default)]
    artifact: Option<ManifestArtifact>,
    #[serde(default)]
    artifacts: Vec<ManifestArtifact>,
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
            specializations: self.specializations,
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
    pub specializations: Vec<String>,
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
        self.resolve_candidates(runtime_id, detected)
            .into_iter()
            .next()
    }

    pub fn resolve_candidates(&self, runtime_id: &str, detected: Variant) -> Vec<&str> {
        let Some(runtime) = self.runtimes.get(runtime_id) else {
            return Vec::new();
        };
        self.resolve_runtime_candidates(runtime_id, detected)
            .into_iter()
            .map(|candidate| {
                runtime
                    .images
                    .get(candidate.variant.as_tag())
                    .map(String::as_str)
                    .expect("runtime candidate came from manifest image")
            })
            .collect()
    }

    pub fn resolve_runtime_candidates(
        &self,
        runtime_id: &str,
        detected: Variant,
    ) -> Vec<RuntimeCandidate> {
        let Some(runtime) = self.runtimes.get(runtime_id) else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for variant_tag in detected.fallback_tags() {
            let Some(variant) = Variant::from_tag(variant_tag) else {
                continue;
            };
            if let Some(image_ref) = runtime.images.get(*variant_tag).cloned()
                && !candidates.iter().any(|candidate: &RuntimeCandidate| {
                    candidate.variant == variant && candidate.image_ref == image_ref
                })
            {
                candidates.push(RuntimeCandidate { variant, image_ref });
            }
        }
        candidates
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
    find_model_manifest_in_dirs(profile_id, manifest_dirs())
}

fn find_model_manifest_in_dirs(profile_id: &str, dirs: Vec<PathBuf>) -> Result<Option<PathBuf>> {
    let filename = format!("{profile_id}.json");
    for dir in &dirs {
        let path = dir.join("models").join(&filename);
        if path.exists() {
            let manifest = read_model_manifest_path(&path)?;
            if manifest.profile_id == profile_id {
                return Ok(Some(path));
            }
        }
    }
    for dir in dirs {
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
            let manifest = read_model_manifest_path(&path)?;
            if manifest.profile_id == profile_id {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

pub(crate) fn read_model_manifest_path(path: &std::path::Path) -> Result<ModelManifest> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("read model manifest {}", path.display()))?;
    let profile_id_hint = path.file_stem().and_then(|stem| stem.to_str());
    parse_model_manifest_json_with_profile_id_hint(&data, profile_id_hint)
        .with_context(|| format!("parse model manifest {}", path.display()))
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
            let manifest = read_model_manifest_path(&path)?;
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
                specializations: manifest.specializations,
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
    parse_model_manifest_json_with_profile_id_hint(data, None)
}

fn parse_model_manifest_json_with_profile_id_hint(
    data: &str,
    profile_id_hint: Option<&str>,
) -> Result<ModelManifest> {
    let raw: RawModelManifest = serde_json::from_str(data)?;
    let manifest = normalize_model_manifest(raw, profile_id_hint)?;
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

fn normalize_model_manifest(
    raw: RawModelManifest,
    profile_id_hint: Option<&str>,
) -> Result<ModelManifest> {
    if raw.artifact.is_some() && !raw.artifacts.is_empty() {
        bail!("model manifest must use either artifact or artifacts, not both");
    }
    let mut artifacts = raw.artifacts.clone();
    if let Some(artifact) = raw.artifact.clone() {
        artifacts.push(artifact);
    }
    normalize_artifacts(&mut artifacts)?;

    let metadata = (!raw.llmfit_model_id.is_empty())
        .then(|| crate::llmfit_metadata::find(&raw.llmfit_model_id))
        .flatten();
    let profile_id = derive_profile_id(&raw, profile_id_hint, &artifacts)?;
    let model_id = if raw.model_id.trim().is_empty() {
        profile_id.clone()
    } else {
        raw.model_id
    };
    let runtime_id = derive_runtime_id(&raw.runtime_id, &artifacts, &raw.use_cases, metadata)?;
    let use_cases = if raw.use_cases.is_empty() {
        derive_use_cases(&runtime_id, metadata)?
    } else {
        raw.use_cases
    };

    Ok(ModelManifest {
        profile_id,
        model_id,
        llmfit_model_id: raw.llmfit_model_id,
        runtime_id,
        runtime_options: raw.runtime_options,
        tier: raw.tier,
        disk_size_gb: raw.disk_size_gb,
        min_ram_gb: raw.min_ram_gb,
        runtime_images: raw.runtime_images,
        use_cases,
        specializations: raw.specializations,
        artifacts,
    })
}

fn derive_profile_id(
    raw: &RawModelManifest,
    profile_id_hint: Option<&str>,
    artifacts: &[ManifestArtifact],
) -> Result<String> {
    if !raw.profile_id.trim().is_empty() {
        return Ok(raw.profile_id.clone());
    }
    if !raw.model_id.trim().is_empty() {
        return Ok(raw.model_id.clone());
    }
    if let Some(profile_id_hint) = profile_id_hint.filter(|hint| !hint.trim().is_empty()) {
        return normalize_manifest_id(profile_id_hint);
    }
    if !raw.llmfit_model_id.trim().is_empty() {
        return normalize_manifest_id(&raw.llmfit_model_id);
    }
    derive_profile_id_from_artifact(artifacts)
}

fn derive_profile_id_from_artifact(artifacts: &[ManifestArtifact]) -> Result<String> {
    let artifact = artifacts
        .first()
        .ok_or_else(|| anyhow::anyhow!("profile_id or llmfit_model_id is required"))?;
    let filename = artifact_url_filename(&artifact.url)
        .or_else(|| (!artifact.filename.trim().is_empty()).then(|| artifact.filename.clone()))
        .ok_or_else(|| anyhow::anyhow!("profile_id or llmfit_model_id is required"))?;
    let stem = filename_stem(&filename);
    let id = normalize_manifest_id(stem)?;
    let sha_prefix: String = artifact.sha256.chars().take(12).collect();
    if sha_prefix.is_empty() {
        Ok(id)
    } else {
        Ok(format!("{id}_{sha_prefix}"))
    }
}

fn normalize_manifest_id(value: &str) -> Result<String> {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        bail!("derived profile_id must not be empty");
    }
    Ok(normalized)
}

fn derive_runtime_id(
    runtime_id: &str,
    artifacts: &[ManifestArtifact],
    use_cases: &[String],
    metadata: Option<&llmfit_core::LlmModel>,
) -> Result<String> {
    if !runtime_id.trim().is_empty() {
        return Ok(runtime_id.to_string());
    }
    if use_cases
        .iter()
        .all(|use_case| use_case.starts_with("speech."))
        && !use_cases.is_empty()
    {
        return Ok("asr-whisper-cpp".to_string());
    }
    if let Some(model) = metadata {
        match model.format {
            ModelFormat::Gguf if model_capabilities(model).contains(&Capability::Vision) => {
                bail!("runtime_id is required for vision GGUF models");
            }
            ModelFormat::Gguf => return Ok("llm-llama-cpp".to_string()),
            _ => bail!(
                "runtime_id is required for {} model format",
                model_format_label(model.format)
            ),
        }
    }
    if artifacts.len() > 1 || artifacts.iter().any(|artifact| artifact.role == "mmproj") {
        bail!("runtime_id is required for multi-artifact manifests");
    }
    if artifacts
        .iter()
        .any(|artifact| artifact_filename_extension(artifact).as_deref() == Some("gguf"))
    {
        return Ok("llm-llama-cpp".to_string());
    }
    bail!("runtime_id is required when it cannot be inferred")
}

fn derive_use_cases(
    runtime_id: &str,
    metadata: Option<&llmfit_core::LlmModel>,
) -> Result<Vec<String>> {
    if runtime_id == "asr-whisper-cpp" {
        return Ok(vec![
            "speech.transcribe".to_string(),
            "speech.translate".to_string(),
        ]);
    }
    if let Some(model) = metadata {
        let use_case = UseCase::from_model(model);
        let mut use_cases = match use_case {
            UseCase::Embedding => vec!["language.embed"],
            UseCase::Coding => vec!["language.extract", "language.analyze"],
            UseCase::General | UseCase::Reasoning | UseCase::Chat | UseCase::Multimodal => {
                default_language_use_cases()
            }
        };
        if use_case == UseCase::Multimodal
            || runtime_id.starts_with("vision-")
            || model_capabilities(model).contains(&Capability::Vision)
        {
            use_cases.extend(["vision.describe", "vision.ocr"]);
        }
        return Ok(dedup_use_cases(use_cases));
    }
    if runtime_id.starts_with("vision-") {
        let mut use_cases = default_language_use_cases();
        use_cases.extend(["vision.describe", "vision.ocr"]);
        return Ok(dedup_use_cases(use_cases));
    }
    if runtime_id.starts_with("llm-") {
        return Ok(default_language_use_cases()
            .into_iter()
            .map(str::to_string)
            .collect());
    }
    bail!("use_cases are required when they cannot be inferred")
}

fn model_format_label(format: ModelFormat) -> &'static str {
    match format {
        ModelFormat::Gguf => "gguf",
        ModelFormat::Awq => "awq",
        ModelFormat::Gptq => "gptq",
        ModelFormat::Autoround => "autoround",
        ModelFormat::Mlx => "mlx",
        ModelFormat::Safetensors => "safetensors",
    }
}

fn default_language_use_cases() -> Vec<&'static str> {
    vec![
        "language.summarize",
        "language.rephrase",
        "language.classify",
        "language.extract",
        "language.analyze",
    ]
}

fn dedup_use_cases(use_cases: Vec<&'static str>) -> Vec<String> {
    let mut seen = HashSet::new();
    use_cases
        .into_iter()
        .filter(|use_case| seen.insert(*use_case))
        .map(str::to_string)
        .collect()
}

fn model_capabilities(model: &llmfit_core::LlmModel) -> Vec<Capability> {
    Capability::infer(model)
}

fn normalize_artifacts(artifacts: &mut [ManifestArtifact]) -> Result<()> {
    let artifact_count = artifacts.len();
    for artifact in artifacts {
        if artifact.role.trim().is_empty() {
            if artifact_count <= 1 {
                artifact.role = "model".to_string();
            } else {
                bail!("multi-artifact manifests must define artifact roles");
            }
        }
        if artifact.filename.trim().is_empty() {
            artifact.filename = default_artifact_filename(artifact, artifact_count)?;
        }
    }
    Ok(())
}

fn default_artifact_filename(artifact: &ManifestArtifact, artifact_count: usize) -> Result<String> {
    match artifact.role.as_str() {
        "model" => match artifact_filename_extension(artifact).as_deref() {
            Some("bin") => Ok("model.bin".to_string()),
            Some("gguf") => Ok("model.gguf".to_string()),
            _ if artifact_count == 1 => artifact_url_filename(&artifact.url)
                .ok_or_else(|| anyhow::anyhow!("artifacts[].filename is required")),
            _ => bail!("artifacts[].filename is required for model artifact"),
        },
        "mmproj" => Ok("mmproj.gguf".to_string()),
        _ => artifact_url_filename(&artifact.url)
            .ok_or_else(|| anyhow::anyhow!("artifacts[].filename is required")),
    }
}

fn artifact_filename_extension(artifact: &ManifestArtifact) -> Option<String> {
    let filename = if artifact.filename.trim().is_empty() {
        artifact_url_filename(&artifact.url)?
    } else {
        artifact.filename.clone()
    };
    filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
}

fn artifact_url_filename(url: &str) -> Option<String> {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let filename = without_query.rsplit('/').next()?.trim();
    if filename.is_empty() || filename == "." || filename == ".." {
        None
    } else {
        Some(filename.to_string())
    }
}

fn filename_stem(filename: &str) -> &str {
    filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename)
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
    "language.summarize",
    "language.translate",
    "language.rephrase",
    "language.classify",
    "language.extract",
    "language.analyze",
    "language.embed",
    "speech.transcribe",
    "speech.translate",
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
    use hegel::TestCase;
    use hegel::generators as gs;

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
    fn resolves_accelerator_variant_with_cpu_fallback() {
        let runtime = RuntimeManifest {
            runtime_id: "asr-whisper-cpp".to_string(),
            images: HashMap::from([("cpu".to_string(), "example/asr:cpu".to_string())]),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("asr-whisper-cpp".to_string(), runtime)]),
        };

        assert_eq!(
            manifests.resolve("asr-whisper-cpp", Variant::Rocm),
            Some("example/asr:cpu")
        );
        assert_eq!(
            manifests.resolve_candidates("asr-whisper-cpp", Variant::Rocm),
            vec!["example/asr:cpu"]
        );
    }

    #[test]
    fn runtime_candidates_keep_same_ref_for_distinct_variants() {
        let shared_ref = "example/runtime@sha256:abcdef";
        let runtime = RuntimeManifest {
            runtime_id: "runtime".to_string(),
            images: HashMap::from([
                ("cpu".to_string(), shared_ref.to_string()),
                ("vulkan".to_string(), shared_ref.to_string()),
            ]),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("runtime".to_string(), runtime)]),
        };

        let candidates = manifests.resolve_runtime_candidates("runtime", Variant::Vulkan);

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

    #[test]
    fn load_from_dirs_reads_runtime_manifests_and_sorts_results() {
        let dir = test_dir("runtime-manifest-load");
        let runtimes_dir = dir.join("runtimes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&runtimes_dir).expect("create runtimes dir");
        std::fs::write(
            runtimes_dir.join("z-runtime.json"),
            r#"{"runtime_id":"z-runtime","images":{"vulkan":"example/z:vulkan","cpu":"example/z:cpu"}}"#,
        )
        .expect("write runtime manifest");
        std::fs::write(
            runtimes_dir.join("a-runtime.json"),
            r#"{"runtime_id":"a-runtime","images":{"cpu":"example/a:cpu"}}"#,
        )
        .expect("write runtime manifest");
        std::fs::write(runtimes_dir.join("ignored.txt"), "not json").expect("write ignored file");

        let store = RuntimeManifestStore::load_from_dirs(vec![dir.clone()]).expect("load runtimes");
        let all = store.all();

        assert_eq!(
            all.iter()
                .map(|runtime| runtime.runtime_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-runtime", "z-runtime"]
        );
        assert_eq!(
            store
                .images_for("z-runtime")
                .into_iter()
                .map(|image| image.variant)
                .collect::<Vec<_>>(),
            vec!["cpu", "vulkan"]
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn find_model_manifest_skips_filename_match_with_different_profile_id() {
        let dir = test_dir("model-manifest-profile-id-lookup");
        let models_dir = dir.join("models");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&models_dir).expect("create models dir");
        std::fs::write(
            models_dir.join("foo.json"),
            r#"{"profile_id":"bar","runtime_id":"stub","use_cases":["language.summarize"],"artifacts":[]}"#,
        )
        .expect("write mismatched manifest");
        let expected = models_dir.join("actual.json");
        std::fs::write(
            &expected,
            r#"{"profile_id":"foo","runtime_id":"stub","use_cases":["language.summarize"],"artifacts":[]}"#,
        )
        .expect("write matching manifest");

        let found = find_model_manifest_in_dirs("foo", vec![dir.clone()])
            .expect("find manifest")
            .expect("matching manifest should exist");

        assert_eq!(found, expected);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_model_manifest_path_uses_filename_as_profile_id_hint() {
        let dir = test_dir("model-manifest-profile-id-hint");
        let models_dir = dir.join("models");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&models_dir).expect("create models dir");
        let path = models_dir.join("file-stem-profile.json");
        std::fs::write(
            &path,
            r#"{"artifact":{"url":"https://example.invalid/model.gguf","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
        )
        .expect("write compact manifest");

        let manifest = read_model_manifest_path(&path).expect("read compact manifest");

        assert_eq!(manifest.profile_id, "file-stem-profile");
        assert_eq!(manifest.model_id, "file-stem-profile");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn model_manifest_into_profile_preserves_install_fields() {
        let manifest = ModelManifest {
            profile_id: "profile".to_string(),
            model_id: "model".to_string(),
            llmfit_model_id: "llmfit".to_string(),
            runtime_id: "runtime".to_string(),
            runtime_options: HashMap::from([("VISION_HANDLER".to_string(), "gemma4".to_string())]),
            tier: "balanced".to_string(),
            disk_size_gb: 1.0,
            min_ram_gb: 2.0,
            runtime_images: vec![RuntimeImage {
                variant: "cpu".to_string(),
                image_ref: "example/runtime:cpu".to_string(),
            }],
            use_cases: vec!["vision.describe".to_string()],
            specializations: vec!["ocr".to_string()],
            artifacts: vec![ManifestArtifact {
                role: "model".to_string(),
                url: "https://example.invalid/model.gguf".to_string(),
                filename: "model.gguf".to_string(),
                sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
                size_bytes: 123,
            }],
        };
        let artifact_path = PathBuf::from("/tmp/model");

        let profile = manifest.into_profile(artifact_path.clone());

        assert_eq!(profile.profile_id, "profile");
        assert_eq!(profile.artifact_path, artifact_path);
        assert_eq!(profile.runtime_options["VISION_HANDLER"], "gemma4");
        assert_eq!(profile.artifact_hashes[0].filename, "model.gguf");
        assert_eq!(profile.source, "user");
        assert!(!profile.installed_at.is_empty());
    }

    #[hegel::test]
    fn resolve_uses_first_available_variant_fallback(tc: TestCase) {
        let requested = tc.draw(gs::sampled_from(vec![
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
        let runtime = RuntimeManifest {
            runtime_id: "runtime".to_string(),
            images: available
                .iter()
                .map(|tag| (tag.clone(), format!("example/runtime:{tag}")))
                .collect(),
        };
        let manifests = RuntimeManifestStore {
            runtimes: HashMap::from([("runtime".to_string(), runtime)]),
        };
        let expected = requested
            .fallback_tags()
            .iter()
            .find(|tag| available.iter().any(|available| available == *tag))
            .map(|tag| format!("example/runtime:{tag}"));

        assert_eq!(manifests.resolve("runtime", requested), expected.as_deref());
    }

    #[hegel::test]
    fn accepts_64_character_hex_sha256_values(tc: TestCase) {
        let value = tc.draw(gs::integers::<u64>());
        let sha256 = format!("{value:064x}");

        validate_sha256(&sha256).expect("generated sha256 should be valid");
    }

    #[hegel::test]
    fn rejects_wrong_length_sha256_values(tc: TestCase) {
        let len = tc.draw(gs::integers::<usize>().max_value(63));
        let sha256 = "a".repeat(len);

        assert!(validate_sha256(&sha256).is_err());
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
            "use_cases": ["language.summarize"],
            "artifacts": []
        }
        "#;

        parse_model_manifest_json(data).expect("valid model manifest");
    }

    #[test]
    fn derives_compact_llm_manifest_fields_from_llmfit_id() {
        let data = r#"
        {
            "llmfit_model_id": "meta-llama/Llama-3.2-3B-Instruct",
            "artifact": {
                "url": "https://example.invalid/Llama-3.2-3B-Instruct-Q4_K_M.gguf?download=1",
                "sha256": "42e64ea673cdfe2512e48df7e7e616ff61a52164dc41e18a9945ca200825a83c"
            }
        }
        "#;

        let manifest = parse_model_manifest_json(data).expect("compact manifest should parse");

        assert_eq!(manifest.profile_id, "meta-llama_llama-3.2-3b-instruct");
        assert_eq!(manifest.model_id, manifest.profile_id);
        assert_eq!(manifest.runtime_id, "llm-llama-cpp");
        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].role, "model");
        assert_eq!(manifest.artifacts[0].filename, "model.gguf");
        assert!(
            manifest
                .use_cases
                .contains(&"language.summarize".to_string())
        );
        assert!(manifest.use_cases.contains(&"language.analyze".to_string()));
        assert!(
            !manifest
                .use_cases
                .contains(&"language.translate".to_string())
        );
    }

    #[test]
    fn compact_manifest_normalization_does_not_collapse_underscores() {
        assert_eq!(
            normalize_manifest_id("Org/Foo::Bar").expect("normalize id"),
            "org_foo__bar"
        );
    }

    #[test]
    fn derives_fallback_profile_id_from_artifact_when_no_llmfit_id_exists() {
        let data = r#"
        {
            "artifact": {
                "url": "https://example.invalid/models/stories260K.gguf",
                "sha256": "270cba1bd5109f42d03350f60406024560464db173c0e387d91f0426d3bd256d"
            }
        }
        "#;

        let manifest = parse_model_manifest_json(data).expect("compact manifest should parse");

        assert_eq!(manifest.profile_id, "stories260k_270cba1bd510");
        assert_eq!(manifest.model_id, manifest.profile_id);
        assert_eq!(manifest.runtime_id, "llm-llama-cpp");
        assert_eq!(manifest.artifacts[0].filename, "model.gguf");
    }

    #[test]
    fn compact_multi_artifact_manifest_requires_roles() {
        let data = r#"
        {
            "runtime_id": "vision-llama-cpp-gemma4",
            "use_cases": ["vision.describe"],
            "artifacts": [
                {
                    "url": "https://example.invalid/model.gguf",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "url": "https://example.invalid/mmproj.gguf",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            ]
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("roles should be required");

        assert!(err.to_string().contains("artifact roles"));
    }

    #[test]
    fn compact_multi_artifact_manifest_requires_runtime_id() {
        let data = r#"
        {
            "use_cases": ["vision.describe"],
            "artifacts": [
                {
                    "role": "model",
                    "url": "https://example.invalid/model.gguf",
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "role": "mmproj",
                    "url": "https://example.invalid/mmproj.gguf",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            ]
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("runtime_id should be required");

        assert!(err.to_string().contains("multi-artifact"));
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
            use_cases: vec!["language.summarize".to_string()],
            specializations: Vec::new(),
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
        for path in [
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../manifests/examples/models/llama3.2-3b-instruct-q4-k-m.json"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../manifests/examples/models/whisper-large-v3-turbo-q5-0.json"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../manifests/examples/models/gemma-4-e4b-it-q4-k-xl.json"),
        ] {
            read_model_manifest_path(&path).expect("example model manifest should be valid");
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
            read_model_manifest_path(&path)
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
            let manifest = read_model_manifest_path(&path)
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
            "use_cases": ["language.magic"],
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
            "use_cases": ["language.summarize"],
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
            "use_cases": ["language.summarize"],
            "artifacts": []
        }
        "#;

        let err = parse_model_manifest_json(data).expect_err("unknown tier should fail");

        assert!(err.to_string().contains("unsupported model tier"));
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }
}
