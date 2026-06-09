/// Hardware accelerator detection.
///
/// Probes the host once at daemon startup and returns the best available
/// image variant suffix to append to a base image ref:
///
///   `aileron/llama3.2-3b` → `aileron/llama3.2-3b:cuda`
///
/// Detection order (highest priority first):
///   1. `AILERON_VARIANT` env var — explicit override
///   2. CUDA  — `nvidia-smi` reports a GPU
///   3. ROCm  — `rocm-smi` reports a GPU
///   4. Vulkan — `vulkaninfo` reports a device
///   5. CPU   — fallback

use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Cpu,
    Cuda,
    Rocm,
    Vulkan,
}

impl Variant {
    pub fn as_tag(&self) -> &'static str {
        match self {
            Variant::Cpu    => "cpu",
            Variant::Cuda   => "cuda",
            Variant::Rocm   => "rocm",
            Variant::Vulkan => "vulkan",
        }
    }
}

/// Run the hardware probe and return the best variant.
/// This is cheap to call but involves subprocess spawns — call it once and
/// store the result in [`crate::state::Inner`].
pub fn detect() -> Variant {
    // Explicit override.
    if let Ok(v) = std::env::var("AILERON_VARIANT") {
        let variant = match v.to_lowercase().as_str() {
            "cuda"   => Variant::Cuda,
            "rocm"   => Variant::Rocm,
            "vulkan" => Variant::Vulkan,
            _        => Variant::Cpu,
        };
        info!("hardware variant: {} (AILERON_VARIANT override)", variant.as_tag());
        return variant;
    }

    if has_cuda() {
        info!("hardware variant: cuda (nvidia-smi detected GPU)");
        return Variant::Cuda;
    }
    if has_rocm() {
        info!("hardware variant: rocm (rocm-smi detected GPU)");
        return Variant::Rocm;
    }
    if has_vulkan() {
        info!("hardware variant: vulkan (vulkaninfo detected device)");
        return Variant::Vulkan;
    }

    info!("hardware variant: cpu (no GPU detected)");
    Variant::Cpu
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    which(cmd)?;
    std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn which(cmd: &str) -> Option<()> {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
}

fn has_cuda() -> bool {
    run("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(false)
}

fn has_rocm() -> bool {
    run("rocm-smi", &["--showproductname"])
        .map(|out| {
            let lower = out.to_lowercase();
            lower.contains("gpu") || lower.contains("radeon") || lower.contains("gfx")
        })
        .unwrap_or(false)
}

fn has_vulkan() -> bool {
    run("vulkaninfo", &["--summary"])
        .map(|out| out.contains("deviceName") || out.contains("deviceType"))
        .unwrap_or(false)
}

/// Given a stored image ref, append the detected variant tag if the ref has
/// no explicit tag already.
///
/// Examples:
///   `aileron/llama3.2-3b`         + cuda  → `aileron/llama3.2-3b:cuda`
///   `aileron/llama3.2-3b:latest`  + cuda  → `aileron/llama3.2-3b:latest`  (unchanged)
///   `localhost/aileron/stub:latest`        → unchanged (has tag)
pub fn resolve(image_ref: &str, variant: Variant) -> String {
    // If the ref already has an explicit tag (contains ':' after the last '/')
    // leave it untouched.
    let after_slash = image_ref.rsplit('/').next().unwrap_or(image_ref);
    if after_slash.contains(':') {
        return image_ref.to_string();
    }
    format!("{}:{}", image_ref, variant.as_tag())
}
