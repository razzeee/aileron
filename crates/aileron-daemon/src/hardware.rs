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
///   3. ROCm  — ROCm userspace reports a GPU
///   4. Vulkan — `vulkaninfo` reports a device
///   5. CPU   — no accelerator detected
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
            Variant::Cpu => "cpu",
            Variant::Cuda => "cuda",
            Variant::Rocm => "rocm",
            Variant::Vulkan => "vulkan",
        }
    }

    pub fn fallback_tags(&self) -> &'static [&'static str] {
        match self {
            Variant::Cpu => &["cpu"],
            Variant::Cuda => &["cuda", "vulkan", "cpu"],
            Variant::Rocm => &["rocm", "vulkan", "cpu"],
            Variant::Vulkan => &["vulkan", "cpu"],
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
            "cuda" => Variant::Cuda,
            "rocm" => Variant::Rocm,
            "vulkan" => Variant::Vulkan,
            _ => Variant::Cpu,
        };
        info!(
            "hardware variant: {} (AILERON_VARIANT override)",
            variant.as_tag()
        );
        return variant;
    }

    if has_cuda() {
        info!("hardware variant: cuda (nvidia-smi detected GPU)");
        return Variant::Cuda;
    }
    if has_rocm() {
        info!("hardware variant: rocm (ROCm detected)");
        return Variant::Rocm;
    }
    if has_vulkan() {
        info!("hardware variant: vulkan (vulkaninfo detected device)");
        return Variant::Vulkan;
    }

    info!("hardware variant: cpu (no GPU detected)");
    Variant::Cpu
}

pub fn total_memory_gb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb = rest.split_whitespace().next()?.parse::<f64>().ok()?;
                return Some(kb / 1024.0 / 1024.0);
            }
        }
        None
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
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
        .or_else(|| run("rocminfo", &[]))
        .map(|out| {
            let lower = out.to_lowercase();
            lower.contains("gpu") || lower.contains("radeon") || lower.contains("gfx")
        })
        .unwrap_or(false)
}

fn has_vulkan() -> bool {
    has_dri_render_node()
        || run("vulkaninfo", &["--summary"])
            .map(|out| out.contains("deviceName") || out.contains("deviceType"))
            .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn has_dri_render_node() -> bool {
    has_dri_render_node_in(std::path::Path::new("/dev/dri"))
}

#[cfg(not(target_os = "linux"))]
fn has_dri_render_node() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn has_dri_render_node_in(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("renderD"))
        })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn fallback_tags_always_include_detected_variant(tc: TestCase) {
        let variant = tc.draw(gs::sampled_from(vec![
            Variant::Cpu,
            Variant::Cuda,
            Variant::Rocm,
            Variant::Vulkan,
        ]));

        assert!(variant.fallback_tags().contains(&variant.as_tag()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detects_dri_render_node_for_vulkan() {
        let temp = test_dir("detects_dri_render_node_for_vulkan");
        std::fs::create_dir_all(&temp).expect("tempdir");
        std::fs::File::create(temp.join("renderD128")).expect("render node fixture");

        assert!(has_dri_render_node_in(&temp));

        std::fs::remove_dir_all(temp).expect("cleanup");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ignores_non_render_dri_entries() {
        let temp = test_dir("ignores_non_render_dri_entries");
        std::fs::create_dir_all(&temp).expect("tempdir");
        std::fs::File::create(temp.join("card0")).expect("card fixture");

        assert!(!has_dri_render_node_in(&temp));

        std::fs::remove_dir_all(temp).expect("cleanup");
    }

    #[cfg(target_os = "linux")]
    fn test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }
}
