use aileron_daemon::manifests::{RuntimeManifestStore, parse_model_manifest_json};
use criterion::{Criterion, criterion_group, criterion_main};
use std::fs;
use std::path::{Path, PathBuf};

fn model_manifest_json() -> String {
    serde_json::json!({
        "profile_id": "llama-test-q4",
        "model_id": "llama-test",
        "llmfit_model_id": "llama/test",
        "runtime_id": "llama-cpp",
        "tier": "small",
        "disk_size_gb": 2.5,
        "min_ram_gb": 8.0,
        "runtime_images": [
            { "variant": "cpu", "image_ref": "registry.example/aileron/llama-cpp:cpu" },
            { "variant": "vulkan", "image_ref": "registry.example/aileron/llama-cpp:vulkan" }
        ],
        "use_cases": ["language.chat", "language.summarize", "language.extract"],
        "artifacts": [
            {
                "role": "model",
                "url": "https://example.invalid/model.gguf",
                "filename": "model.gguf",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "size_bytes": 2684354560u64
            }
        ]
    })
    .to_string()
}

fn write_runtime_manifests(root: &Path, count: usize) -> Vec<PathBuf> {
    let manifest_dir = root.join("manifests");
    let runtimes_dir = manifest_dir.join("runtimes");
    fs::create_dir_all(&runtimes_dir).expect("create runtime manifest directory");

    for idx in 0..count {
        let data = serde_json::json!({
            "runtime_id": format!("runtime-{idx:04}"),
            "images": {
                "cpu": format!("registry.example/aileron/runtime-{idx:04}:cpu"),
                "vulkan": format!("registry.example/aileron/runtime-{idx:04}:vulkan"),
                "cuda": format!("registry.example/aileron/runtime-{idx:04}:cuda"),
                "rocm": format!("registry.example/aileron/runtime-{idx:04}:rocm")
            }
        });
        fs::write(
            runtimes_dir.join(format!("runtime-{idx:04}.json")),
            data.to_string(),
        )
        .expect("write runtime manifest");
    }

    vec![manifest_dir]
}

fn bench_manifest_parsing(c: &mut Criterion) {
    let data = model_manifest_json();

    c.bench_function("parse model manifest", |b| {
        b.iter(|| parse_model_manifest_json(&data).expect("parse model manifest"));
    });
}

fn bench_runtime_manifest_loading(c: &mut Criterion) {
    let temp = tempfile::tempdir().expect("create temp directory");
    let dirs = write_runtime_manifests(temp.path(), 1_000);

    c.bench_function("load 1000 runtime manifests", |b| {
        b.iter(|| RuntimeManifestStore::load_from_dirs(dirs.clone()).expect("load runtimes"));
    });
}

criterion_group!(
    benches,
    bench_manifest_parsing,
    bench_runtime_manifest_loading
);
criterion_main!(benches);
