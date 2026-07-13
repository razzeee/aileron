use aileron_daemon::container::{InputMessage, InputPart};
use aileron_daemon::manifests::{RuntimeManifestStore, parse_model_manifest_json};
use criterion::{Criterion, criterion_group, criterion_main};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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
        "use_cases": ["language.summarize", "language.extract"],
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

fn stream_response_fixture(token_count: usize) -> Vec<u8> {
    let mut input = String::new();
    for idx in 0..token_count {
        let done = idx + 1 == token_count;
        input.push_str(
            &serde_json::json!({
                "id": "request-1",
                "token": format!("token-{idx}"),
                "done": done,
            })
            .to_string(),
        );
        input.push('\n');
    }
    input.into_bytes()
}

fn bench_text_stream_response_parsing(c: &mut Criterion) {
    let input = stream_response_fixture(256);

    c.bench_function("parse 256 streamed token responses", |b| {
        b.iter(|| {
            aileron_daemon::container::benchmark_read_text_stream_response(&input, "request-1")
                .expect("parse text stream responses")
        });
    });
}

fn response_fixture(use_case: &str) -> Vec<u8> {
    let response = match use_case {
        "language.generate" | "speech.transcribe" | "vision.describe" | "vision.ocr" => {
            return stream_response_fixture(32);
        }
        "language.structured" | "language.tool" => serde_json::json!({
            "id": "request-1",
            "result": r#"{"answer":"ok"}"#,
            "done": true,
        }),
        "language.embed" => serde_json::json!({
            "id": "request-1",
            "embedding": [0.1, 0.2, 0.3, 0.4],
            "done": true,
        }),
        "vision.segment" => serde_json::json!({
            "id": "request-1",
            "result": r#"{"segments":[{"label":"object","confidence":0.9,"x":0.1,"y":0.2,"width":0.3,"height":0.4}]}"#,
            "done": true,
        }),
        other => panic!("unsupported benchmark use-case: {other}"),
    };
    format!("{response}\n").into_bytes()
}

fn model_use_cases() -> [&'static str; 8] {
    [
        "language.generate",
        "language.structured",
        "language.tool",
        "language.embed",
        "speech.transcribe",
        "vision.describe",
        "vision.ocr",
        "vision.segment",
    ]
}

fn bench_model_response_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse model response");
    for use_case in model_use_cases() {
        let input = response_fixture(use_case);
        group.bench_function(use_case, |b| {
            b.iter(|| {
                aileron_daemon::container::benchmark_read_response_for_use_case(use_case, &input)
                    .expect("parse model response")
            });
        });
    }
    group.finish();
}

fn request_input_fixture() -> Vec<InputMessage> {
    vec![
        InputMessage {
            role: "system".to_string(),
            content: vec![InputPart::InputText {
                text: "Keep the answer short and factual.".to_string(),
            }],
        },
        InputMessage {
            role: "user".to_string(),
            content: vec![
                InputPart::InputText {
                    text: "Summarize this article.".to_string(),
                },
                InputPart::InputText {
                    text:
                        "The model-call pipeline should avoid unnecessary daemon-side allocation."
                            .to_string(),
                },
            ],
        },
    ]
}

fn bench_model_request_serialization(c: &mut Criterion) {
    let input = request_input_fixture();
    let mut group = c.benchmark_group("write model request");
    for use_case in model_use_cases() {
        group.bench_function(use_case, |b| {
            b.iter(|| {
                aileron_daemon::container::benchmark_write_request_for_use_case(use_case, &input)
                    .expect("write model request")
            });
        });
    }
    group.finish();
}

fn runtime_stub_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("AILERON_RUNTIME_STUB_BIN") {
        return Some(PathBuf::from(path));
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target/release/aileron-runtime-stub");
    default.exists().then_some(default)
}

struct StubRuntimeProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for StubRuntimeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_stub_runtime(binary: &Path) -> StubRuntimeProcess {
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stub runtime");
    let stdin = child.stdin.take().expect("stub stdin");
    let stdout = BufReader::new(child.stdout.take().expect("stub stdout"));
    let stderr = child.stderr.take().expect("stub stderr");
    let mut stderr = BufReader::new(stderr);
    let mut ready = String::new();
    stderr.read_line(&mut ready).expect("read stub ready line");
    assert!(ready.contains("ready"));
    StubRuntimeProcess {
        child,
        stdin,
        stdout,
    }
}

fn stub_request(use_case: &str, id: &str) -> serde_json::Value {
    match use_case {
        "language.generate" => serde_json::json!({
            "id": id,
            "type": "generate",
            "prompt": "summarize this article quickly",
            "max_tokens": 32,
            "execution_mode": "interactive",
        }),
        "language.structured" => serde_json::json!({
            "id": id,
            "type": "generate_structured_stream",
            "prompt": "extract fields",
            "response_format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "required": ["answer"],
                    "properties": { "answer": { "type": "string" } }
                }
            },
            "execution_mode": "interactive",
        }),
        "language.tool" => serde_json::json!({
            "id": id,
            "type": "generate_structured",
            "prompt": "call lookup",
            "tools": [{
                "name": "lookup",
                "description": "Look up a fact",
                "schema": {"type":"object"}
            }],
            "response_format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "required": ["answer"],
                    "properties": { "answer": { "type": "string" } }
                }
            },
            "execution_mode": "interactive",
        }),
        "language.embed" => serde_json::json!({
            "id": id,
            "type": "embed",
            "prompt": "text to embed",
            "execution_mode": "interactive",
        }),
        "speech.transcribe" => serde_json::json!({
            "id": id,
            "type": "transcribe",
            "audio": "",
            "task": "transcribe",
            "language_hint": "en",
            "execution_mode": "interactive",
        }),
        "vision.describe" => serde_json::json!({
            "id": id,
            "type": "describe",
            "image": "",
            "prompt": "describe image",
            "execution_mode": "interactive",
        }),
        "vision.ocr" => serde_json::json!({
            "id": id,
            "type": "ocr",
            "image": "",
            "prompt": "extract text",
            "execution_mode": "interactive",
        }),
        "vision.segment" => serde_json::json!({
            "id": id,
            "type": "segment",
            "image": "",
            "prompt": "segment objects",
            "execution_mode": "interactive",
        }),
        other => panic!("unsupported benchmark use-case: {other}"),
    }
}

fn stub_roundtrip(runtime: &mut StubRuntimeProcess, use_case: &str, id: &str) -> usize {
    let request = stub_request(use_case, id);
    serde_json::to_writer(&mut runtime.stdin, &request).expect("write stub request");
    runtime
        .stdin
        .write_all(b"\n")
        .expect("write request newline");
    runtime.stdin.flush().expect("flush stub request");

    let mut lines = 0;
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = runtime
            .stdout
            .read_line(&mut buf)
            .expect("read stub response");
        assert!(n > 0, "stub runtime stdout closed");
        let response: serde_json::Value =
            serde_json::from_str(buf.trim()).expect("stub response json");
        if response.get("id").and_then(serde_json::Value::as_str) != Some(id) {
            continue;
        }
        lines += 1;
        if response
            .get("done")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return lines;
        }
    }
}

fn bench_stub_runtime_roundtrip(c: &mut Criterion) {
    let Some(binary) = runtime_stub_binary() else {
        eprintln!(
            "skipping stub runtime roundtrip benchmark; build target/release/aileron-runtime-stub or set AILERON_RUNTIME_STUB_BIN"
        );
        return;
    };
    let mut group = c.benchmark_group("stub runtime roundtrip");
    for use_case in model_use_cases() {
        let mut runtime = spawn_stub_runtime(&binary);
        let mut counter = 0_u64;
        group.bench_function(use_case, |b| {
            b.iter(|| {
                counter += 1;
                let id = format!("request-{counter}");
                stub_roundtrip(&mut runtime, use_case, &id)
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_manifest_parsing,
    bench_runtime_manifest_loading,
    bench_text_stream_response_parsing,
    bench_model_response_parsing,
    bench_model_request_serialization,
    bench_stub_runtime_roundtrip
);
criterion_main!(benches);
