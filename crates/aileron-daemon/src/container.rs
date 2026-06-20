/// Runtime lifecycle management.
///
/// One OCI runtime process is maintained per use-case. The process receives
/// newline-delimited JSON requests on stdin and emits newline-delimited JSON
/// response chunks on stdout.
///
/// ## Protocol
///
/// ### Streaming text generation
/// Request:
///   {"id":"<uuid>","type":"generate","prompt":"...","max_tokens":512}
/// Response (one line per token, final line has done:true):
///   {"id":"<uuid>","token":"Hello"}
///   {"id":"<uuid>","token":" world","done":true}
///
/// ### Structured output (JSON Schema constrained)
/// Request:
///   {"id":"<uuid>","type":"generate_structured","prompt":"...",
///    "max_tokens":1024,
///    "response_format":{"type":"json_schema","schema":{...}}}
/// Response (single line, no streaming):
///   {"id":"<uuid>","result":"{\"name\":\"Alice\",\"age\":30}","done":true}
///
/// ### Embeddings
/// Request:
///   {"id":"<uuid>","type":"embed","prompt":"text to embed"}
/// Response (single line, no streaming):
///   {"id":"<uuid>","embedding":[0.1,0.2,...],"done":true}
///
/// ### Audio transcription / translation
/// Request:
///   {"id":"<uuid>","type":"transcribe","audio":"<base64 PCM>","task":"transcribe","language_hint":"en"}
/// `task` is "transcribe" (verbatim, source language) or "translate"
/// (translate speech to English).
/// Response (streamed tokens, same as generate):
///   {"id":"<uuid>","token":"Hello world","done":true}
///
/// ### Image description
/// Request:
///   {"id":"<uuid>","type":"describe","image":"<base64 PNG/JPEG>"}
/// Response (same as generate):
///   {"id":"<uuid>","token":"A cat sitting...","done":true}
///
/// ### Image OCR (text extraction)
/// Request:
///   {"id":"<uuid>","type":"ocr","image":"<base64 PNG/JPEG>"}
/// Response (same as generate):
///   {"id":"<uuid>","token":"extracted text...","done":true}
///
/// ### Image segmentation
/// Request:
///   {"id":"<uuid>","type":"segment","image":"<base64 PNG/JPEG>"}
/// Response (single line, no streaming):
///   {"id":"<uuid>","result":"{\"segments\":[...]}","done":true}
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::hardware::Variant;

/// A running container for a single use-case.
pub struct Container {
    #[allow(dead_code)]
    pub image_ref: String,
    #[allow(dead_code)]
    pub artifact_path: PathBuf,
    #[allow(dead_code)]
    runtime_options: HashMap<String, String>,
    /// Kept alive to prevent the container process from being killed on drop.
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub last_used: std::time::Instant,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VisionSegment {
    pub label: String,
    pub confidence: f64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub schema_json: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub content_json: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GuidedToolResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

impl Container {
    /// Spawn a hardened OCI runtime bundle for the given image and block until the
    /// entrypoint signals it is ready by writing `ready` to stderr.
    ///
    /// The `on_status` callback is called with human-readable progress lines
    /// from the container's stderr while we wait (e.g. model loading messages).
    /// It may be called from a background thread.
    ///
    /// Isolation is encoded in the generated OCI `config.json`: no network
    /// namespace, read-only rootfs, tmpfs `/tmp`, no capabilities, no new
    /// privileges, PID limit, memory limit, and `/model` mounted read-only.
    pub fn spawn(
        image_ref: &str,
        detected_variant: Variant,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        memory_limit: &str,
        oci_store: &Path,
        system_oci_store: &Path,
        mut on_status: impl FnMut(String) + Send + 'static,
    ) -> Result<Self> {
        info!("spawning OCI runtime for {}", image_ref);
        let bundle = OciRuntimeManager::new(oci_store, system_oci_store).prepare_bundle(
            image_ref,
            detected_variant,
            artifact_path,
            runtime_options,
            memory_limit,
        )?;
        let container_id = format!("aileron-{}", Uuid::new_v4());
        let mut child = std::process::Command::new("crun")
            .args(["run", "--bundle"])
            .arg(&bundle.bundle_dir)
            .arg(&container_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn crun for {}", image_ref))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let stderr = BufReader::new(child.stderr.take().expect("piped stderr"));

        // Read stderr lines in a background thread, forwarding them to
        // `on_status` and watching for the "ready" sentinel.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        thread::spawn(move || {
            let mut signalled = false;
            let mut recent = std::collections::VecDeque::with_capacity(8);
            for line in stderr.lines() {
                match line {
                    Ok(l) => {
                        info!("[container stderr] {}", l);
                        if recent.len() == 8 {
                            recent.pop_front();
                        }
                        recent.push_back(l.clone());
                        let lower = l.to_lowercase();
                        if !signalled && lower.contains("ready") {
                            let _ = ready_tx.send(Ok(()));
                            signalled = true;
                        } else if !signalled {
                            on_status(l);
                        }
                    }
                    Err(e) => {
                        if !signalled {
                            let _ = ready_tx.send(Err(e.to_string()));
                            signalled = true;
                        }
                        break;
                    }
                }
            }
            // If stderr closed without a ready line, signal an error.
            if !signalled {
                let detail = recent.into_iter().collect::<Vec<_>>().join(" | ");
                let reason = if detail.is_empty() {
                    "container exited before ready".to_string()
                } else {
                    format!("container exited before ready: {detail}")
                };
                let _ = ready_tx.send(Err(reason));
            }
        });

        // Block until the container is ready or fails.
        match ready_rx.recv() {
            Ok(Ok(())) => info!("container ready: {}", image_ref),
            Ok(Err(e)) => bail!("container failed to start: {}", e),
            Err(_) => bail!("container stderr thread dropped before ready"),
        }

        Ok(Self {
            image_ref: image_ref.to_string(),
            artifact_path: artifact_path.to_path_buf(),
            runtime_options: runtime_options.clone(),
            child,
            stdin,
            stdout,
            last_used: std::time::Instant::now(),
        })
    }

    /// Send a generate request and collect streamed token responses.
    /// `on_token` is called once per token as it arrives.
    pub fn generate(
        &mut self,
        system: Option<&str>,
        prompt: &str,
        max_tokens: u32,
        mut on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate");
        req.system = system.map(str::to_string);
        req.prompt = Some(prompt.to_string());
        req.max_tokens = Some(max_tokens);
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if let Some(token) = resp.token {
                on_token(token);
            }
            if resp.done.unwrap_or(false) {
                break;
            }
        }
        Ok(())
    }

    /// Send a raw inline prediction request and return runtime-cleaned completions.
    pub fn predict_next(
        &mut self,
        prefix: &str,
        count: u32,
        max_tokens: u32,
        temperature: f64,
    ) -> Result<Vec<String>> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "predict_next");
        req.prompt = Some(prefix.to_string());
        req.max_tokens = Some(max_tokens);
        req.choices = Some(count);
        req.temperature = Some(temperature);
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if resp.done.unwrap_or(false) {
                return Ok(limit_completions(
                    resp.completions
                        .or_else(|| resp.completion.map(|c| vec![c]))
                        .unwrap_or_default(),
                    count,
                ));
            }
            if let Some(completions) = resp.completions {
                return Ok(limit_completions(completions, count));
            }
            if let Some(completion) = resp.completion {
                return Ok(limit_completions(vec![completion], count));
            }
        }
    }

    /// Send a structured-output request.
    ///
    /// `schema` must be a valid JSON Schema object (as a `serde_json::Value`).
    /// The container must reply with a single `result` field containing a JSON
    /// string.  The daemon validates that string against the schema before
    /// returning it to the caller.
    ///
    /// Returns the validated JSON string.
    pub fn generate_structured(
        &mut self,
        system: Option<&str>,
        prompt: &str,
        max_tokens: u32,
        schema: &Value,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate_structured");
        req.system = system.map(str::to_string);
        req.prompt = Some(prompt.to_string());
        req.max_tokens = Some(max_tokens);
        req.response_format = Some(ResponseFormat {
            r#type: "json_schema".to_string(),
            schema: schema.clone(),
        });
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        // Structured responses arrive as a single line with `result`.
        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(result) = structured_response_result(resp, schema)? {
                return Ok(result);
            }
        }
    }

    pub fn generate_structured_with_tools(
        &mut self,
        system: Option<&str>,
        prompt: Option<&str>,
        max_tokens: u32,
        schema: &Value,
        tools: Vec<ToolDefinition>,
        tool_results: Vec<ToolResult>,
    ) -> Result<GuidedToolResponse> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate_structured");
        req.system = system.map(str::to_string);
        req.prompt = prompt.map(str::to_string);
        req.max_tokens = Some(max_tokens);
        req.response_format = Some(ResponseFormat {
            r#type: "json_schema".to_string(),
            schema: schema.clone(),
        });
        req.tools = if tools.is_empty() { None } else { Some(tools) };
        req.tool_results = if tool_results.is_empty() {
            None
        } else {
            Some(tool_results)
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if let Some(tool_calls) = resp.tool_calls {
                return Ok(GuidedToolResponse {
                    content: resp.result.unwrap_or_default(),
                    tool_calls,
                });
            }
            if let Some(result) = resp.result {
                validate_json_schema(&result, schema)?;
                return Ok(GuidedToolResponse {
                    content: result,
                    tool_calls: Vec::new(),
                });
            }
            if resp.done.unwrap_or(false) {
                bail!("container sent done without a result or tool_calls field");
            }
        }
    }

    /// Send a structured-output request and stream schema-valid snapshots.
    pub fn stream_structured(
        &mut self,
        system: Option<&str>,
        prompt: &str,
        max_tokens: u32,
        schema: &Value,
        mut on_snapshot: impl FnMut(String, bool),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate_structured_stream");
        req.system = system.map(str::to_string);
        req.prompt = Some(prompt.to_string());
        req.max_tokens = Some(max_tokens);
        req.response_format = Some(ResponseFormat {
            r#type: "json_schema".to_string(),
            schema: schema.clone(),
        });
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if let Some(snapshot) = resp.snapshot.or(resp.result) {
                validate_json_schema(&snapshot, schema)?;
                on_snapshot(snapshot, resp.done.unwrap_or(false));
            }
            if resp.done.unwrap_or(false) {
                break;
            }
        }
        Ok(())
    }

    /// Send a transcribe request and return the full transcript.
    ///
    /// `task` is "transcribe" (verbatim, source language) or "translate"
    /// (translate speech to English).
    pub fn transcribe(
        &mut self,
        audio: Vec<u8>,
        language_hint: Option<&str>,
        task: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "transcribe");
        req.audio = Some(base64_encode(&audio));
        req.task = Some(task.to_string());
        req.language_hint = language_hint
            .filter(|hint| !hint.is_empty())
            .map(str::to_string);
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    /// Send a vision describe request and return the full description.
    pub fn describe(&mut self, image: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "describe");
        req.image = Some(base64_encode(&image));
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    /// Send a vision OCR request and return the extracted text.
    pub fn ocr(&mut self, image: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "ocr");
        req.image = Some(base64_encode(&image));
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    /// Send a vision segment request and return normalized object boxes.
    pub fn segment(&mut self, image: Vec<u8>) -> Result<Vec<VisionSegment>> {
        let id = Uuid::new_v4().to_string();
        let schema = vision_segment_schema();
        let mut req = ContainerRequest::new(id.clone(), "segment");
        req.image = Some(base64_encode(&image));
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(result) = structured_response_result(resp, &schema)? {
                let value: VisionSegmentResult = serde_json::from_str(&result)?;
                return Ok(value.segments);
            }
        }
    }

    /// Send an embedding request and return the embedding vector.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "embed");
        req.prompt = Some(text.to_string());
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if let Some(embedding) = resp.embedding {
                return Ok(embedding);
            }
            if resp.done.unwrap_or(false) {
                bail!("container completed embedding request without an embedding");
            }
        }
    }

    fn read_text_response(&mut self, id: &str) -> Result<String> {
        let mut result = String::new();
        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(error) = resp.error {
                let reason = resp.reason.unwrap_or_else(|| error.clone());
                bail!("container returned error {error}: {reason}");
            }
            if let Some(token) = resp.token {
                result.push_str(&token);
            }
            if resp.done.unwrap_or(false) {
                break;
            }
        }
        Ok(result)
    }
}

fn structured_response_result(resp: ContainerResponse, schema: &Value) -> Result<Option<String>> {
    if let Some(error) = resp.error {
        let reason = resp.reason.unwrap_or_else(|| error.clone());
        bail!("container returned error {error}: {reason}");
    }
    if let Some(result) = resp.result {
        validate_json_schema(&result, schema)?;
        return Ok(Some(result));
    }
    if resp.done.unwrap_or(false) {
        bail!("container sent done without a result field");
    }
    Ok(None)
}

fn limit_completions(mut completions: Vec<String>, count: u32) -> Vec<String> {
    completions.truncate(count as usize);
    completions
}

#[derive(Deserialize)]
struct VisionSegmentResult {
    segments: Vec<VisionSegment>,
}

fn vision_segment_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["segments"],
        "additionalProperties": false,
        "properties": {
            "segments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label", "confidence", "x", "y", "width", "height"],
                    "additionalProperties": false,
                    "properties": {
                        "label": { "type": "string" },
                        "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                        "x": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                        "y": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                        "width": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                        "height": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
                    }
                }
            }
        }
    })
}

struct PreparedBundle {
    bundle_dir: PathBuf,
}

struct OciRuntimeManager {
    store: PathBuf,
    system_store: PathBuf,
}

impl OciRuntimeManager {
    fn new(store: &Path, system_store: &Path) -> Self {
        Self {
            store: store.to_path_buf(),
            system_store: system_store.to_path_buf(),
        }
    }

    fn prepare_bundle(
        &self,
        image_ref: &str,
        detected_variant: Variant,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        memory_limit: &str,
    ) -> Result<PreparedBundle> {
        let Some(rootfs) =
            runtime_rootfs_path_from_stores(&self.store, &self.system_store, image_ref)
        else {
            bail!(
                "OCI rootfs for {image_ref} is not installed under {} or {}; image transport/unpack is not implemented yet. Install skopeo/crun and populate the Aileron OCI store before starting this runtime.",
                self.store
                    .join("rootfs")
                    .join(store_key(image_ref))
                    .display(),
                self.system_store
                    .join("rootfs")
                    .join(store_key(image_ref))
                    .display()
            );
        };

        let bundle_dir = self.store.join("bundles").join(Uuid::new_v4().to_string());
        fs::create_dir_all(&bundle_dir)
            .with_context(|| format!("failed to create OCI bundle at {}", bundle_dir.display()))?;
        let config = runtime_config_json(
            image_ref,
            detected_variant,
            &rootfs,
            artifact_path,
            runtime_options,
            memory_limit,
        )?;
        let config_path = bundle_dir.join("config.json");
        fs::write(&config_path, serde_json::to_vec_pretty(&config)?)
            .with_context(|| format!("failed to write {}", config_path.display()))?;

        Ok(PreparedBundle { bundle_dir })
    }
}

pub fn default_oci_store() -> PathBuf {
    let data_home = std::env::var("AILERON_DATA_HOME")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local").join("share")
        });
    data_home.join("aileron").join("oci")
}

pub fn default_system_oci_store() -> PathBuf {
    crate::profiles::system_data_dir().join("oci")
}

pub fn runtime_rootfs_path(user_store: &Path, image_ref: &str) -> Option<PathBuf> {
    runtime_rootfs_path_from_stores(user_store, &default_system_oci_store(), image_ref)
}

fn runtime_rootfs_path_from_stores(
    user_store: &Path,
    system_store: &Path,
    image_ref: &str,
) -> Option<PathBuf> {
    let key = store_key(image_ref);
    [user_store, system_store]
        .into_iter()
        .map(|store| store.join("rootfs").join(&key))
        .find(|path| path.is_dir())
}

fn runtime_config_json(
    image_ref: &str,
    detected_variant: Variant,
    rootfs: &Path,
    artifact_path: &Path,
    runtime_options: &HashMap<String, String>,
    memory_limit: &str,
) -> Result<Value> {
    let mut env = vec![
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        "PYTHONUNBUFFERED=1".to_string(),
    ];
    let mut mounts = vec![
        serde_json::json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        serde_json::json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["rw", "nosuid", "noexec", "nodev", "size=256m", "mode=1777"]
        }),
        serde_json::json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["rw", "nosuid", "noexec", "nodev", "size=256m", "mode=1777"]
        }),
        serde_json::json!({
            "destination": "/model",
            "type": "bind",
            "source": artifact_path.display().to_string(),
            "options": ["rbind", "ro", "nosuid", "nodev"]
        }),
    ];

    let generic_gpu = image_ref_uses_tag(image_ref, "gpu");
    if image_ref_uses_tag(image_ref, "cuda") || generic_gpu && detected_variant == Variant::Cuda {
        expose_cuda(&mut mounts, &mut env);
    } else if image_ref_uses_tag(image_ref, "rocm")
        || generic_gpu && detected_variant == Variant::Rocm
    {
        expose_rocm(&mut mounts, &mut env);
    } else if image_ref_uses_tag(image_ref, "vulkan")
        || generic_gpu && detected_variant == Variant::Vulkan
    {
        expose_vulkan(&mut mounts, &mut env);
    }

    let mut runtime_options: Vec<_> = runtime_options.iter().collect();
    runtime_options.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in runtime_options {
        env.push(format!("{key}={value}"));
    }

    Ok(serde_json::json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0 },
            "args": ["python", "/entrypoint.py"],
            "env": env,
            "cwd": "/",
            "noNewPrivileges": true,
            "capabilities": {
                "bounding": [],
                "effective": [],
                "inheritable": [],
                "permitted": [],
                "ambient": []
            }
        },
        "root": {
            "path": rootfs.display().to_string(),
            "readonly": true
        },
        "hostname": "aileron-runtime",
        "mounts": mounts,
        "linux": {
            "namespaces": [
                { "type": "pid" },
                { "type": "ipc" },
                { "type": "uts" },
                { "type": "mount" },
                { "type": "network" },
                { "type": "cgroup" }
            ],
            "maskedPaths": [
                "/proc/acpi",
                "/proc/asound",
                "/proc/kcore",
                "/proc/keys",
                "/proc/latency_stats",
                "/proc/timer_list",
                "/proc/timer_stats",
                "/proc/sched_debug",
                "/sys/firmware"
            ],
            "readonlyPaths": [
                "/proc/bus",
                "/proc/fs",
                "/proc/irq",
                "/proc/sys",
                "/proc/sysrq-trigger"
            ],
            "devices": [],
            "resources": {
                "pids": { "limit": 256 },
                "memory": { "limit": parse_memory_limit(memory_limit)? }
            }
        }
    }))
}

fn expose_cuda(mounts: &mut Vec<Value>, env: &mut Vec<String>) {
    add_cuda_mounts(mounts);
    add_readonly_mount(mounts, "/sys");
    env.push("N_GPU_LAYERS=-1".to_string());
    env.push("AILERON_DEVICE=cuda".to_string());
}

fn expose_rocm(mounts: &mut Vec<Value>, env: &mut Vec<String>) {
    add_device_mount(mounts, "/dev/kfd");
    add_device_mount(mounts, "/dev/dri");
    add_readonly_mount(mounts, "/sys");
    env.push("N_GPU_LAYERS=-1".to_string());
    env.push("AILERON_DEVICE=rocm".to_string());
}

fn expose_vulkan(mounts: &mut Vec<Value>, env: &mut Vec<String>) {
    add_device_mount(mounts, "/dev/dri");
    add_readonly_mount(mounts, "/sys");
    env.push("N_GPU_LAYERS=-1".to_string());
    env.push("AILERON_DEVICE=vulkan".to_string());
}

fn add_device_mount(mounts: &mut Vec<Value>, path: &str) {
    mounts.push(serde_json::json!({
        "destination": path,
        "type": "bind",
        "source": path,
        "options": ["rbind", "rw", "nosuid"]
    }));
}

fn add_cuda_mounts(mounts: &mut Vec<Value>) {
    add_existing_device_mounts(
        mounts,
        [
            "/dev/nvidia0",
            "/dev/nvidia1",
            "/dev/nvidiactl",
            "/dev/nvidia-modeset",
            "/dev/nvidia-uvm",
            "/dev/nvidia-uvm-tools",
            "/dev/nvidia-caps",
        ],
    );
    if Path::new("/proc/driver/nvidia").exists() {
        add_readonly_mount(mounts, "/proc/driver/nvidia");
    }
}

fn add_existing_device_mounts<const N: usize>(mounts: &mut Vec<Value>, paths: [&str; N]) {
    for path in paths {
        if Path::new(path).exists() {
            add_device_mount(mounts, path);
        }
    }
}

fn add_readonly_mount(mounts: &mut Vec<Value>, path: &str) {
    mounts.push(serde_json::json!({
        "destination": path,
        "type": "bind",
        "source": path,
        "options": ["rbind", "ro", "nosuid", "nodev", "noexec"]
    }));
}

fn parse_memory_limit(limit: &str) -> Result<i64> {
    let limit = limit.trim();
    let split = limit
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(limit.len());
    let (digits, suffix) = limit.split_at(split);
    let value: i64 = digits
        .parse()
        .with_context(|| format!("invalid memory limit '{limit}'"))?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        other => bail!("unsupported memory limit suffix '{other}' in '{limit}'"),
    };
    value
        .checked_mul(multiplier)
        .context("memory limit is too large")
}

pub(crate) fn store_key(image_ref: &str) -> String {
    image_ref
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn image_ref_uses_tag(image_ref: &str, tag: &str) -> bool {
    image_ref
        .rsplit_once('/')
        .map_or(image_ref, |(_, after_slash)| after_slash)
        .rsplit_once(':')
        .is_some_and(|(_, image_tag)| image_tag == tag)
}

// ── Schema validation ─────────────────────────────────────────────────────────

/// Validate `json_str` against a JSON Schema `schema`.
///
/// This is a structural validator covering the subset of JSON Schema most
/// useful for structured output: type, required, properties, items,
/// minLength/maxLength, minimum/maximum, enum.  It does not implement the full
/// JSON Schema specification.
fn validate_json_schema(json_str: &str, schema: &Value) -> Result<()> {
    let value: Value = serde_json::from_str(json_str).context("model output is not valid JSON")?;
    validate_value(&value, schema, "$")
}

fn validate_value(value: &Value, schema: &Value, path: &str) -> Result<()> {
    // $ref, allOf, anyOf, oneOf are not supported — reject them explicitly so
    // callers know they're outside this validator's scope.
    if schema.get("$ref").is_some() {
        bail!("{path}: $ref is not supported in structured output schemas");
    }

    let schema_type = schema.get("type").and_then(|v| v.as_str());

    // type check
    match schema_type {
        Some("object") => {
            let obj = value.as_object().with_context(|| {
                format!("{path}: expected object, got {}", value_type_name(value))
            })?;

            // required fields
            if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
                for req in required {
                    let key = req.as_str().unwrap_or("");
                    if !obj.contains_key(key) {
                        bail!("{path}: missing required field '{key}'");
                    }
                }
            }

            // property schemas
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                for (key, prop_schema) in props {
                    if let Some(field_val) = obj.get(key) {
                        validate_value(field_val, prop_schema, &format!("{path}.{key}"))?;
                    }
                }
            }

            // additionalProperties: false
            if schema.get("additionalProperties").and_then(|v| v.as_bool()) == Some(false)
                && let Some(props) = schema.get("properties").and_then(|v| v.as_object())
            {
                for key in obj.keys() {
                    if !props.contains_key(key) {
                        bail!("{path}: unexpected additional property '{key}'");
                    }
                }
            }
        }
        Some("array") => {
            let arr = value.as_array().with_context(|| {
                format!("{path}: expected array, got {}", value_type_name(value))
            })?;
            if let Some(items_schema) = schema.get("items") {
                for (i, item) in arr.iter().enumerate() {
                    validate_value(item, items_schema, &format!("{path}[{i}]"))?;
                }
            }
            if let Some(min) = schema.get("minItems").and_then(|v| v.as_u64())
                && arr.len() < min as usize
            {
                bail!("{path}: array length {} < minItems {min}", arr.len());
            }
            if let Some(max) = schema.get("maxItems").and_then(|v| v.as_u64())
                && arr.len() > max as usize
            {
                bail!("{path}: array length {} > maxItems {max}", arr.len());
            }
        }
        Some("string") => {
            let s = value.as_str().with_context(|| {
                format!("{path}: expected string, got {}", value_type_name(value))
            })?;
            if let Some(min) = schema.get("minLength").and_then(|v| v.as_u64())
                && s.len() < min as usize
            {
                bail!("{path}: string length {} < minLength {min}", s.len());
            }
            if let Some(max) = schema.get("maxLength").and_then(|v| v.as_u64())
                && s.len() > max as usize
            {
                bail!("{path}: string length {} > maxLength {max}", s.len());
            }
            check_enum(value, schema, path)?;
        }
        Some("number") | Some("integer") => {
            let n = value.as_f64().with_context(|| {
                format!("{path}: expected number, got {}", value_type_name(value))
            })?;
            if schema_type == Some("integer") && value.as_i64().is_none() {
                bail!("{path}: expected integer, got non-integer number {n}");
            }
            if let Some(min) = schema.get("minimum").and_then(|v| v.as_f64())
                && n < min
            {
                bail!("{path}: {n} < minimum {min}");
            }
            if let Some(max) = schema.get("maximum").and_then(|v| v.as_f64())
                && n > max
            {
                bail!("{path}: {n} > maximum {max}");
            }
            check_enum(value, schema, path)?;
        }
        Some("boolean") => {
            if !value.is_boolean() {
                bail!("{path}: expected boolean, got {}", value_type_name(value));
            }
        }
        Some("null") => {
            if !value.is_null() {
                bail!("{path}: expected null, got {}", value_type_name(value));
            }
        }
        Some(other) => bail!("{path}: unsupported schema type '{other}'"),
        None => {
            // No type constraint — just check enum if present.
            check_enum(value, schema, path)?;
        }
    }

    Ok(())
}

fn check_enum(value: &Value, schema: &Value, path: &str) -> Result<()> {
    if let Some(variants) = schema.get("enum").and_then(|v| v.as_array())
        && !variants.contains(value)
    {
        bail!("{path}: value {:?} is not in enum {:?}", value, variants);
    }
    Ok(())
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

// ── Pool ──────────────────────────────────────────────────────────────────────

/// Pool of running containers, keyed by profile ID.
pub struct ContainerPool {
    containers: HashMap<String, Container>,
    /// Idle timeout in seconds (default 300 = 5 min).
    pub idle_timeout_secs: u64,
    /// OCI memory limit applied to each model runtime.
    pub memory_limit: String,
    /// Local Aileron-owned OCI runtime store.
    pub oci_store: PathBuf,
    /// Read-only distro-managed OCI runtime store.
    pub system_oci_store: PathBuf,
}

impl Default for ContainerPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerPool {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
            idle_timeout_secs: 300,
            memory_limit: "8g".to_string(),
            oci_store: default_oci_store(),
            system_oci_store: default_system_oci_store(),
        }
    }

    /// Get or spawn a container for a profile + runtime image + artifact path.
    /// `on_status` receives human-readable loading messages while the container
    /// starts up (only called on a cold start, not for warm containers).
    pub fn get_or_spawn(
        &mut self,
        profile_id: &str,
        image_ref: &str,
        detected_variant: Variant,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        on_status: impl FnMut(String) + Send + 'static,
    ) -> Result<&mut Container> {
        if self.containers.get(profile_id).is_some_and(|container| {
            container.image_ref != image_ref
                || container.artifact_path != artifact_path
                || container.runtime_options != *runtime_options
        }) {
            info!(
                "replacing container for profile {} with runtime image {}",
                profile_id, image_ref
            );
            self.containers.remove(profile_id);
        }

        if !self.containers.contains_key(profile_id) {
            let c = Container::spawn(
                image_ref,
                detected_variant,
                artifact_path,
                runtime_options,
                &self.memory_limit,
                &self.oci_store,
                &self.system_oci_store,
                on_status,
            )?;
            self.containers.insert(profile_id.to_string(), c);
        }
        Ok(self.containers.get_mut(profile_id).unwrap())
    }

    /// Kill and remove the container for a profile.
    pub fn kill(&mut self, profile_id: &str) {
        if self.containers.remove(profile_id).is_some() {
            info!("terminated container for profile {}", profile_id);
        }
    }

    /// Kill all containers.
    #[allow(dead_code)]
    pub fn kill_all(&mut self) {
        let keys: Vec<_> = self.containers.keys().cloned().collect();
        for k in keys {
            self.kill(&k);
        }
    }

    /// Evict containers that have been idle longer than `idle_timeout_secs`.
    pub fn evict_idle(&mut self) {
        let timeout = std::time::Duration::from_secs(self.idle_timeout_secs);
        let now = std::time::Instant::now();
        let idle: Vec<_> = self
            .containers
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_used) > timeout)
            .map(|(k, _)| k.clone())
            .collect();
        for k in idle {
            warn!("evicting idle container for profile {}", k);
            self.containers.remove(&k);
        }
    }
}

// ── Internal protocol types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct ContainerRequest {
    id: String,
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    choices: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<String>,
    /// Present only for `transcribe` requests: "transcribe" (verbatim) or
    /// "translate" (translate speech to English).
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_hint: Option<String>,
    /// Present only for `generate_structured` requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_results: Option<Vec<ToolResult>>,
}

impl ContainerRequest {
    fn new(id: String, request_type: &str) -> Self {
        Self {
            id,
            r#type: request_type.to_string(),
            system: None,
            prompt: None,
            max_tokens: None,
            choices: None,
            temperature: None,
            audio: None,
            task: None,
            image: None,
            language_hint: None,
            response_format: None,
            tools: None,
            tool_results: None,
        }
    }
}

/// Instructs the container to constrain output to a JSON Schema.
#[derive(Serialize)]
struct ResponseFormat {
    r#type: String, // always "json_schema"
    schema: Value,
}

#[derive(Deserialize)]
struct ContainerResponse {
    id: String,
    /// Present in streaming token responses.
    token: Option<String>,
    /// Present in inline prediction responses.
    completion: Option<String>,
    /// Present in inline prediction responses with multiple choices.
    completions: Option<Vec<String>>,
    /// Present in structured output responses (the full JSON string).
    result: Option<String>,
    /// Present in structured streaming responses.
    snapshot: Option<String>,
    /// Present when the model requests app-mediated tool execution.
    tool_calls: Option<Vec<ToolCall>>,
    /// Present in embedding responses.
    embedding: Option<Vec<f32>>,
    error: Option<String>,
    reason: Option<String>,
    done: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn oci_config_preserves_core_sandbox_settings() {
        let config = runtime_config_json(
            "localhost/aileron/summarize:cpu",
            Variant::Cpu,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert_eq!(config["root"]["readonly"], true);
        assert_eq!(config["root"]["path"], "/store/rootfs/runtime");
        assert_eq!(config["process"]["noNewPrivileges"], true);
        assert_eq!(
            config["process"]["capabilities"]["bounding"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(config["linux"]["resources"]["pids"]["limit"], 256);
        assert_eq!(
            config["linux"]["resources"]["memory"]["limit"],
            8 * 1024 * 1024 * 1024i64
        );
        assert!(mounts(&config).iter().any(|mount| {
            mount["destination"] == "/tmp"
                && mount["type"] == "tmpfs"
                && array_contains(&mount["options"], "noexec")
                && array_contains(&mount["options"], "size=256m")
                && array_contains(&mount["options"], "mode=1777")
        }));
        assert!(mounts(&config).iter().any(|mount| {
            mount["destination"] == "/dev/shm"
                && mount["type"] == "tmpfs"
                && array_contains(&mount["options"], "mode=1777")
        }));
        assert!(mounts(&config).iter().any(|mount| {
            mount["destination"] == "/model"
                && mount["source"] == "/models/foo"
                && array_contains(&mount["options"], "ro")
        }));
        assert!(namespaces(&config).contains(&"network"));
    }

    #[test]
    fn oci_config_exposes_rocm_devices_for_rocm_tag() {
        let config = runtime_config_json(
            "localhost/aileron/summarize:rocm",
            Variant::Rocm,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(device_mounts(&config).contains(&"/dev/kfd"));
        assert!(device_mounts(&config).contains(&"/dev/dri"));
        assert_device_mount_has_no_nodev(&config, "/dev/kfd");
        assert_device_mount_has_no_nodev(&config, "/dev/dri");
        assert!(mount_destinations(&config).contains(&"/sys"));
        assert!(
            !env(&config)
                .iter()
                .any(|item| item.starts_with("HSA_OVERRIDE_GFX_VERSION="))
        );
        assert!(env(&config).contains(&"N_GPU_LAYERS=-1"));
        assert!(env(&config).contains(&"AILERON_DEVICE=rocm"));
    }

    #[test]
    fn oci_config_exposes_cuda_env_for_cuda_tag() {
        let config = runtime_config_json(
            "localhost/aileron/summarize:cuda",
            Variant::Cuda,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(!device_mounts(&config).contains(&"/dev/kfd"));
        assert!(!device_mounts(&config).contains(&"/dev/dri"));
        assert!(mount_destinations(&config).contains(&"/sys"));
        assert!(!env(&config).contains(&"HSA_OVERRIDE_GFX_VERSION=10.3.0"));
        assert!(env(&config).contains(&"N_GPU_LAYERS=-1"));
        assert!(env(&config).contains(&"AILERON_DEVICE=cuda"));
    }

    #[test]
    fn oci_config_exposes_vulkan_device_for_vulkan_tag() {
        let config = runtime_config_json(
            "localhost/aileron/summarize:vulkan",
            Variant::Vulkan,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(device_mounts(&config).contains(&"/dev/dri"));
        assert!(!device_mounts(&config).contains(&"/dev/kfd"));
        assert!(mount_destinations(&config).contains(&"/sys"));
        assert!(!env(&config).contains(&"HSA_OVERRIDE_GFX_VERSION=10.3.0"));
        assert!(env(&config).contains(&"N_GPU_LAYERS=-1"));
        assert!(env(&config).contains(&"AILERON_DEVICE=vulkan"));
    }

    #[test]
    fn oci_config_exposes_detected_accelerator_for_generic_gpu_tag() {
        let cuda = runtime_config_json(
            "localhost/aileron/summarize:gpu",
            Variant::Cuda,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build CUDA generic GPU config");
        assert!(mount_destinations(&cuda).contains(&"/sys"));
        assert!(env(&cuda).contains(&"AILERON_DEVICE=cuda"));

        let rocm = runtime_config_json(
            "localhost/aileron/summarize:gpu",
            Variant::Rocm,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build ROCm generic GPU config");
        assert!(device_mounts(&rocm).contains(&"/dev/kfd"));
        assert!(device_mounts(&rocm).contains(&"/dev/dri"));
        assert!(env(&rocm).contains(&"AILERON_DEVICE=rocm"));

        let vulkan = runtime_config_json(
            "localhost/aileron/summarize:gpu",
            Variant::Vulkan,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build Vulkan generic GPU config");
        assert!(device_mounts(&vulkan).contains(&"/dev/dri"));
        assert!(!device_mounts(&vulkan).contains(&"/dev/kfd"));
        assert!(env(&vulkan).contains(&"AILERON_DEVICE=vulkan"));
    }

    #[test]
    fn oci_config_does_not_expose_gpu_devices_for_cpu_tag() {
        let config = runtime_config_json(
            "localhost/aileron/summarize:cpu",
            Variant::Cpu,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(!device_mounts(&config).contains(&"/dev/kfd"));
        assert!(!device_mounts(&config).contains(&"/dev/dri"));
        assert!(!mount_destinations(&config).contains(&"/sys"));
        assert!(!env(&config).contains(&"N_GPU_LAYERS=-1"));
        assert!(
            !env(&config)
                .iter()
                .any(|entry| entry.starts_with("AILERON_DEVICE="))
        );
    }

    #[test]
    fn all_declared_runtime_refs_get_expected_accelerator_mounts() {
        let refs = [
            "ghcr.io/razzeee/aileron-runtime-asr-whisper-cpp:cpu",
            "ghcr.io/razzeee/aileron-runtime-asr-whisper-cpp:cuda",
            "ghcr.io/razzeee/aileron-runtime-asr-whisper-cpp:vulkan",
            "ghcr.io/razzeee/aileron-runtime-llm-llama-cpp:cpu",
            "ghcr.io/razzeee/aileron-runtime-llm-llama-cpp:cuda",
            "ghcr.io/razzeee/aileron-runtime-llm-llama-cpp:rocm",
            "ghcr.io/razzeee/aileron-runtime-llm-llama-cpp:vulkan",
            "ghcr.io/razzeee/aileron-runtime-vision-llama-cpp-gemma4:cpu",
            "ghcr.io/razzeee/aileron-runtime-vision-llama-cpp-gemma4:cuda",
            "ghcr.io/razzeee/aileron-runtime-vision-llama-cpp-gemma4:rocm",
            "ghcr.io/razzeee/aileron-runtime-vision-llama-cpp-gemma4:vulkan",
        ];

        for image_ref in refs {
            let config = runtime_config_json(
                image_ref,
                variant_for_image_ref(image_ref),
                Path::new("/store/rootfs/runtime"),
                Path::new("/models/foo"),
                &HashMap::new(),
                "8g",
            )
            .unwrap_or_else(|error| panic!("build OCI config for {image_ref}: {error}"));

            assert!(
                mount_destinations(&config).contains(&"/dev/shm"),
                "{image_ref} should mount /dev/shm"
            );
            if image_ref_uses_tag(image_ref, "cpu") {
                assert_cpu_runtime_mounts(image_ref, &config);
            } else if image_ref_uses_tag(image_ref, "cuda") {
                assert_accelerator_common_mounts(image_ref, &config);
                assert!(env(&config).contains(&"AILERON_DEVICE=cuda"));
                assert_optional_device_mounts_have_no_nodev(&config, "/dev/nvidia");
            } else if image_ref_uses_tag(image_ref, "rocm") {
                assert_accelerator_common_mounts(image_ref, &config);
                assert!(env(&config).contains(&"AILERON_DEVICE=rocm"));
                assert_device_mount_has_no_nodev(&config, "/dev/kfd");
                assert_device_mount_has_no_nodev(&config, "/dev/dri");
            } else if image_ref_uses_tag(image_ref, "vulkan") {
                assert_accelerator_common_mounts(image_ref, &config);
                assert!(env(&config).contains(&"AILERON_DEVICE=vulkan"));
                assert_device_mount_has_no_nodev(&config, "/dev/dri");
            } else {
                panic!("unhandled runtime variant in {image_ref}");
            }
        }
    }

    #[test]
    fn oci_config_includes_runtime_options_as_env() {
        let mut runtime_options = HashMap::new();
        runtime_options.insert("VISION_HANDLER".to_string(), "gemma4".to_string());

        let config = runtime_config_json(
            "localhost/aileron/vision:cpu",
            Variant::Cpu,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &runtime_options,
            "8g",
        )
        .expect("build OCI config");

        assert!(env(&config).contains(&"VISION_HANDLER=gemma4"));
    }

    #[test]
    fn runtime_options_can_override_accelerator_defaults() {
        let mut runtime_options = HashMap::new();
        runtime_options.insert("N_GPU_LAYERS".to_string(), "16".to_string());
        runtime_options.insert("AILERON_DEVICE".to_string(), "cpu".to_string());

        let config = runtime_config_json(
            "localhost/aileron/llm:cuda",
            Variant::Cuda,
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &runtime_options,
            "8g",
        )
        .expect("build OCI config");

        let values = env(&config);
        let n_gpu_layers: Vec<_> = values
            .iter()
            .copied()
            .filter(|arg| arg.starts_with("N_GPU_LAYERS="))
            .collect();
        let devices: Vec<_> = values
            .iter()
            .copied()
            .filter(|arg| arg.starts_with("AILERON_DEVICE="))
            .collect();

        assert_eq!(n_gpu_layers, ["N_GPU_LAYERS=-1", "N_GPU_LAYERS=16"]);
        assert_eq!(devices, ["AILERON_DEVICE=cuda", "AILERON_DEVICE=cpu"]);
    }

    #[test]
    fn existing_device_mounts_only_include_present_paths() {
        let root = std::env::temp_dir().join(format!("aileron-device-test-{}", Uuid::new_v4()));
        let present = root.join("present");
        let missing = root.join("missing");
        std::fs::create_dir_all(&root).expect("create temp dir");
        std::fs::write(&present, "device placeholder").expect("write present path");

        let mut mounts = Vec::new();
        add_existing_device_mounts(
            &mut mounts,
            [present.to_str().unwrap(), missing.to_str().unwrap()],
        );

        let destinations = mounts
            .iter()
            .filter_map(|mount| mount["destination"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(destinations, vec![present.to_str().unwrap()]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[hegel::test]
    fn store_key_outputs_safe_path_component_for_generated_refs(tc: TestCase) {
        let chars = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                'a', 'Z', '0', '.', '-', '_', '/', ':', '@', '+', '=', '#', ' ',
            ]))
            .max_size(64),
        );
        let image_ref = chars.into_iter().collect::<String>();
        let key = store_key(&image_ref);

        assert_eq!(key.len(), image_ref.len());
        assert!(
            key.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_')
        );
    }

    #[hegel::test]
    fn parse_memory_limit_accepts_generated_supported_units(tc: TestCase) {
        let value = tc.draw(gs::integers::<i64>().min_value(0).max_value(1024));
        let (suffix, multiplier) = tc.draw(gs::sampled_from(vec![
            ("", 1_i64),
            ("b", 1_i64),
            ("k", 1024_i64),
            ("kb", 1024_i64),
            ("m", 1024_i64 * 1024),
            ("mb", 1024_i64 * 1024),
            ("g", 1024_i64 * 1024 * 1024),
            ("gb", 1024_i64 * 1024 * 1024),
        ]));
        let limit = format!("{value}{suffix}");

        assert_eq!(parse_memory_limit(&limit).unwrap(), value * multiplier);
    }

    #[hegel::test]
    fn image_ref_uses_tag_matches_generated_last_path_tag(tc: TestCase) {
        let tag = tc.draw(gs::sampled_from(vec![
            "cpu".to_string(),
            "cuda".to_string(),
            "rocm".to_string(),
            "vulkan".to_string(),
        ]));
        let other_tag = tc.draw(gs::sampled_from(vec![
            "debug".to_string(),
            "latest".to_string(),
            "test".to_string(),
        ]));
        let image_ref = format!("registry.example:5000/ns/runtime:{tag}");

        assert!(image_ref_uses_tag(&image_ref, &tag));
        assert!(!image_ref_uses_tag(&image_ref, &other_tag));
    }

    #[hegel::test]
    fn base64_encoding_uses_expected_length_alphabet_and_padding(tc: TestCase) {
        let data = tc.draw(gs::binary().max_size(128));
        let encoded = base64_encode(&data);

        assert_eq!(encoded.len(), data.len().div_ceil(3) * 4);
        assert!(
            encoded
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' || ch == '=')
        );
        match data.len() % 3 {
            0 => assert!(!encoded.ends_with('=')),
            1 => assert!(encoded.ends_with("==")),
            2 => assert!(encoded.ends_with('=')),
            _ => unreachable!(),
        }
    }

    fn env(config: &Value) -> Vec<&str> {
        config["process"]["env"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect()
    }

    fn mounts(config: &Value) -> Vec<&Value> {
        config["mounts"].as_array().unwrap().iter().collect()
    }

    fn device_mounts(config: &Value) -> Vec<&str> {
        mounts(config)
            .into_iter()
            .filter(|mount| {
                mount["destination"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("/dev/")
            })
            .filter_map(|mount| mount["destination"].as_str())
            .collect()
    }

    fn mount_destinations(config: &Value) -> Vec<&str> {
        mounts(config)
            .into_iter()
            .filter_map(|mount| mount["destination"].as_str())
            .collect()
    }

    fn assert_device_mount_has_no_nodev(config: &Value, path: &str) {
        assert!(mounts(config).iter().any(|mount| {
            mount["destination"] == path && !array_contains(&mount["options"], "nodev")
        }));
    }

    fn assert_optional_device_mounts_have_no_nodev(config: &Value, prefix: &str) {
        for mount in mounts(config) {
            if mount["destination"]
                .as_str()
                .map(|destination| destination.starts_with(prefix))
                .unwrap_or(false)
            {
                assert!(!array_contains(&mount["options"], "nodev"));
            }
        }
    }

    fn assert_accelerator_common_mounts(image_ref: &str, config: &Value) {
        assert!(
            mount_destinations(config).contains(&"/sys"),
            "{image_ref} should mount /sys"
        );
        assert!(
            mount_destinations(config).contains(&"/dev/shm"),
            "{image_ref} should mount /dev/shm"
        );
        assert!(
            env(config).contains(&"N_GPU_LAYERS=-1"),
            "{image_ref} should enable GPU offload"
        );
    }

    fn assert_cpu_runtime_mounts(image_ref: &str, config: &Value) {
        assert!(
            !mount_destinations(config).contains(&"/sys"),
            "{image_ref} should not mount accelerator topology"
        );
        assert!(
            !env(config)
                .iter()
                .any(|entry| entry.starts_with("AILERON_DEVICE=")),
            "{image_ref} should not set accelerator device env"
        );
        assert!(
            !env(config).contains(&"N_GPU_LAYERS=-1"),
            "{image_ref} should not enable GPU offload"
        );
    }

    fn namespaces(config: &Value) -> Vec<&str> {
        config["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|namespace| namespace["type"].as_str())
            .collect()
    }

    fn array_contains(value: &Value, expected: &str) -> bool {
        value
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(expected))
    }

    #[test]
    fn structured_response_error_returns_immediately() {
        let resp = ContainerResponse {
            id: "request-1".to_string(),
            token: None,
            completion: None,
            completions: None,
            result: None,
            snapshot: None,
            tool_calls: None,
            embedding: None,
            error: Some("schema_validation_failed".to_string()),
            reason: Some("expected object".to_string()),
            done: None,
        };

        let err = structured_response_result(resp, &serde_json::json!({}))
            .expect_err("structured error response should fail");

        assert!(err.to_string().contains("expected object"));
    }

    #[test]
    fn structured_response_validates_result() {
        let resp = ContainerResponse {
            id: "request-1".to_string(),
            token: None,
            completion: None,
            completions: None,
            result: Some(r#"{"name":"Ada"}"#.to_string()),
            snapshot: None,
            tool_calls: None,
            embedding: None,
            error: None,
            reason: None,
            done: Some(true),
        };
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            }
        });

        let result = structured_response_result(resp, &schema)
            .expect("valid structured response should succeed");

        assert_eq!(result.as_deref(), Some(r#"{"name":"Ada"}"#));
    }

    #[test]
    fn schema_validation_rejects_missing_and_additional_object_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            },
            "additionalProperties": false
        });

        let missing =
            validate_json_schema(r#"{}"#, &schema).expect_err("missing required field should fail");
        let additional = validate_json_schema(r#"{"name":"Ada","extra":true}"#, &schema)
            .expect_err("additional property should fail");

        assert!(missing.to_string().contains("missing required field"));
        assert!(
            additional
                .to_string()
                .contains("unexpected additional property")
        );
    }

    #[test]
    fn schema_validation_checks_array_item_types_and_bounds() {
        let schema = serde_json::json!({
            "type": "array",
            "items": { "type": "integer" },
            "minItems": 1,
            "maxItems": 2
        });

        validate_json_schema("[1, 2]", &schema).expect("valid bounded array");

        assert!(
            validate_json_schema("[]", &schema)
                .expect_err("too few items should fail")
                .to_string()
                .contains("minItems")
        );
        assert!(
            validate_json_schema("[1, 2, 3]", &schema)
                .expect_err("too many items should fail")
                .to_string()
                .contains("maxItems")
        );
        assert!(
            validate_json_schema("[1.5]", &schema)
                .expect_err("non-integer item should fail")
                .to_string()
                .contains("expected integer")
        );
    }

    #[hegel::test]
    fn schema_validation_accepts_generated_values_inside_string_and_number_bounds(tc: TestCase) {
        let count = tc.draw(gs::integers::<u64>().min_value(1).max_value(10));
        let name = "x".repeat(count as usize);
        let score = tc.draw(gs::integers::<i64>().min_value(0).max_value(100));
        let data = serde_json::json!({ "name": name, "score": score }).to_string();
        let schema = serde_json::json!({
            "type": "object",
            "required": ["name", "score"],
            "properties": {
                "name": { "type": "string", "minLength": 1, "maxLength": 10 },
                "score": { "type": "integer", "minimum": 0, "maximum": 100 }
            }
        });

        validate_json_schema(&data, &schema).expect("generated data should satisfy schema");
    }

    #[test]
    fn schema_validation_checks_enum_and_unsupported_schema_keywords() {
        let enum_schema = serde_json::json!({ "enum": ["red", "blue"] });
        let ref_schema = serde_json::json!({ "$ref": "#/defs/name" });
        let unsupported_schema = serde_json::json!({ "type": "date" });

        validate_json_schema(r#""red""#, &enum_schema).expect("enum member should pass");
        assert!(
            validate_json_schema(r#""green""#, &enum_schema)
                .expect_err("non-member should fail")
                .to_string()
                .contains("not in enum")
        );
        assert!(
            validate_json_schema(r#""red""#, &ref_schema)
                .expect_err("$ref should fail explicitly")
                .to_string()
                .contains("$ref is not supported")
        );
        assert!(
            validate_json_schema(r#""2026-06-20""#, &unsupported_schema)
                .expect_err("unsupported type should fail")
                .to_string()
                .contains("unsupported schema type")
        );
    }

    #[test]
    fn rocm_tag_detection_ignores_registry_port() {
        assert!(image_ref_uses_tag(
            "localhost:5000/aileron/summarize:rocm",
            "rocm"
        ));
        assert!(!image_ref_uses_tag(
            "localhost:5000/aileron/summarize",
            "rocm"
        ));
    }

    #[test]
    #[ignore = "requires a prebuilt stub runtime rootfs and crun"]
    fn stub_runtime_roundtrip_through_container_wrapper() {
        let image_ref = std::env::var("AILERON_STUB_IMAGE")
            .unwrap_or_else(|_| "localhost/aileron/stub:ci".to_string());
        let oci_store = std::env::var("AILERON_OCI_STORE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_oci_store());
        let artifact_path =
            std::env::temp_dir().join(format!("aileron-stub-artifacts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&artifact_path).expect("create temporary artifact directory");

        let mut container = Container::spawn(
            &image_ref,
            Variant::Cpu,
            &artifact_path,
            &HashMap::new(),
            "512m",
            &oci_store,
            &default_system_oci_store(),
            |_| {},
        )
        .expect("spawn stub runtime");

        let mut generated = String::new();
        container
            .generate(None, "hello world", 16, |token| generated.push_str(&token))
            .expect("generate through container wrapper");
        assert!(!generated.is_empty());

        let schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            }
        });
        let structured = container
            .generate_structured(None, "extract", 64, &schema)
            .expect("generate structured through container wrapper");
        let structured_json: serde_json::Value =
            serde_json::from_str(&structured).expect("structured result is JSON");
        assert!(structured_json.get("name").is_some());

        let transcript = container
            .transcribe(Vec::new(), None, "transcribe")
            .expect("transcribe through container wrapper");
        assert!(!transcript.is_empty());

        let translation = container
            .transcribe(Vec::new(), None, "translate")
            .expect("translate through container wrapper");
        assert!(!translation.is_empty());

        let embedding = container
            .embed("hello world")
            .expect("embed through container wrapper");
        assert!(!embedding.is_empty());

        let description = container
            .describe(Vec::new())
            .expect("describe through container wrapper");
        assert!(!description.is_empty());

        let extracted = container
            .ocr(Vec::new())
            .expect("ocr through container wrapper");
        assert!(!extracted.is_empty());

        let segments = container
            .segment(Vec::new())
            .expect("segment through container wrapper");
        assert!(!segments.is_empty());

        let _ = std::fs::remove_dir_all(&artifact_path);
    }

    fn variant_for_image_ref(image_ref: &str) -> Variant {
        if image_ref_uses_tag(image_ref, "cuda") {
            Variant::Cuda
        } else if image_ref_uses_tag(image_ref, "rocm") {
            Variant::Rocm
        } else if image_ref_uses_tag(image_ref, "vulkan") {
            Variant::Vulkan
        } else {
            Variant::Cpu
        }
    }
}
