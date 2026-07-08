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
///   {"id":"<uuid>","type":"describe","image":"<base64 PNG/JPEG>","prompt":"optional instructions"}
/// Response (same as generate):
///   {"id":"<uuid>","token":"A cat sitting...","done":true}
///
/// ### Image OCR (text extraction)
/// Request:
///   {"id":"<uuid>","type":"ocr","image":"<base64 PNG/JPEG>","prompt":"optional instructions"}
/// Response (same as generate):
///   {"id":"<uuid>","token":"extracted text...","done":true}
///
/// ### Image segmentation
/// Request:
///   {"id":"<uuid>","type":"segment","image":"<base64 PNG/JPEG>","prompt":"optional instructions"}
/// Response (single line, no streaming):
///   {"id":"<uuid>","result":"{\"segments\":[...]}","done":true}
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::hardware::Variant;
use crate::observability;
use crate::profiles::RuntimeCandidate;

const NVIDIA_LIBRARY_DIR: &str = "/usr/local/nvidia/lib64";
const ML_RUNTIME_ID: &str = "llm-vision-whisper";
const PREDICTION_COMPLETION_COUNT: u32 = 3;
const VULKAN_ICD_DIR: &str = "/usr/share/vulkan/icd.d";
const NVIDIA_DRIVER_LIBRARIES: &[&str] = &[
    "libcuda.so.1",
    "libcuda.so",
    "libnvidia-ml.so.1",
    "libnvidia-ml.so",
    "libnvidia-ptxjitcompiler.so.1",
    "libnvidia-ptxjitcompiler.so",
    "libnvidia-nvvm.so.4",
    "libnvidia-nvvm.so",
];
const NVIDIA_DEVICE_PATHS: &[&str] = &[
    "/dev/nvidia0",
    "/dev/nvidia1",
    "/dev/nvidiactl",
    "/dev/nvidia-modeset",
    "/dev/nvidia-uvm",
    "/dev/nvidia-uvm-tools",
    "/dev/nvidia-caps",
];
const COMMON_LIBRARY_DIRS: &[&str] = &[
    "/usr/lib/x86_64-linux-gnu",
    "/usr/lib64",
    "/usr/lib",
    "/lib/x86_64-linux-gnu",
    "/lib64",
    "/run/opengl-driver/lib",
];
const VULKAN_ICD_DIRS: &[&str] = &[
    "/run/opengl-driver/share/vulkan/icd.d",
    "/etc/vulkan/icd.d",
    "/usr/local/share/vulkan/icd.d",
    "/usr/share/vulkan/icd.d",
];
/// A running container for a single use-case.
pub struct Container {
    #[allow(dead_code)]
    pub variant: Variant,
    #[allow(dead_code)]
    runtime_id: String,
    #[allow(dead_code)]
    profile_epoch: u64,
    #[allow(dead_code)]
    pub image_ref: String,
    #[allow(dead_code)]
    pub artifact_path: PathBuf,
    #[allow(dead_code)]
    runtime_options: HashMap<String, String>,
    /// Kept alive to prevent the container process from being killed on drop.
    #[allow(dead_code)]
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub last_used: std::time::Instant,
}

impl Drop for Container {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSpawnAttempt {
    variant: Variant,
    image_ref: String,
    runtime_options: HashMap<String, String>,
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
    pub fn matches_any_runtime(
        &self,
        runtime_id: &str,
        profile_epoch: u64,
        candidates: &[RuntimeCandidate],
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
    ) -> bool {
        self.runtime_id == runtime_id
            && self.profile_epoch == profile_epoch
            && self.artifact_path == artifact_path
            && runtime_spawn_attempts(runtime_id, candidates, runtime_options)
                .iter()
                .any(|attempt| {
                    attempt.image_ref == self.image_ref
                        && attempt.variant == self.variant
                        && attempt.runtime_options == self.runtime_options
                })
    }

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
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        runtime_id: &str,
        profile_epoch: u64,
        candidate: &RuntimeCandidate,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        memory_limit: &str,
        oci_store: &Path,
        system_oci_store: &Path,
        mut on_status: impl FnMut(String) + Send + 'static,
        mut should_continue: impl FnMut() -> Result<(), String>,
    ) -> Result<Self> {
        let image_ref = candidate.image_ref.as_str();
        let started_at = observability::log_runtime_starting(
            runtime_id,
            image_ref,
            candidate.variant.as_tag(),
            runtime_options.len(),
        );
        let bundle = OciRuntimeManager::new(oci_store, system_oci_store).prepare_bundle(
            candidate.variant,
            image_ref,
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
        let status_runtime_id = runtime_id.to_string();
        let status_image_ref = image_ref.to_string();
        let status_variant = candidate.variant.as_tag().to_string();
        thread::spawn(move || {
            let mut signalled = false;
            let mut recent = std::collections::VecDeque::with_capacity(8);
            for line in stderr.lines() {
                match line {
                    Ok(l) => {
                        observability::log_runtime_status(
                            &status_runtime_id,
                            &status_image_ref,
                            &status_variant,
                            &l,
                        );
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

        // Block until the container is ready or fails, but poll cancellation so
        // session close/kill can abort a cold start before the ready sentinel.
        loop {
            if let Err(reason) = should_continue() {
                let _ = child.kill();
                let _ = child.wait();
                bail!(reason);
            }
            match ready_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Ok(())) => {
                    if let Err(reason) = should_continue() {
                        let _ = child.kill();
                        let _ = child.wait();
                        bail!(reason);
                    }
                    observability::log_runtime_ready(
                        runtime_id,
                        image_ref,
                        candidate.variant.as_tag(),
                        started_at,
                    );
                    break;
                }
                Ok(Err(e)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("container failed to start: {}", e);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("container stderr thread dropped before ready");
                }
            }
        }

        Ok(Self {
            variant: candidate.variant,
            runtime_id: runtime_id.to_string(),
            profile_epoch,
            image_ref: image_ref.to_string(),
            artifact_path: artifact_path.to_path_buf(),
            runtime_options: runtime_options.clone(),
            child: Arc::new(Mutex::new(child)),
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
        input: Option<&[InputMessage]>,
        max_tokens: u32,
        execution_mode: &str,
        mut on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate");
        req.system = system.map(str::to_string);
        req.prompt = Some(prompt.to_string());
        req.input = input.map(|messages| messages.to_vec());
        req.max_tokens = Some(max_tokens);
        req.execution_mode = Some(execution_mode.to_string());
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some((error, reason)) = response_error(&resp) {
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
        max_tokens: u32,
        temperature: f64,
        execution_mode: &str,
    ) -> Result<Vec<String>> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "predict_next");
        req.prompt = Some(prefix.to_string());
        req.max_tokens = Some(max_tokens);
        req.choices = Some(PREDICTION_COMPLETION_COUNT);
        req.temperature = Some(temperature);
        req.execution_mode = Some(execution_mode.to_string());
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some((error, reason)) = response_error(&resp) {
                bail!("container returned error {error}: {reason}");
            }
            if resp.done.unwrap_or(false) {
                return Ok(limit_completions(
                    resp.completions
                        .or_else(|| resp.completion.map(|c| vec![c]))
                        .unwrap_or_default(),
                    PREDICTION_COMPLETION_COUNT,
                ));
            }
            if let Some(completions) = resp.completions {
                return Ok(limit_completions(completions, PREDICTION_COMPLETION_COUNT));
            }
            if let Some(completion) = resp.completion {
                return Ok(limit_completions(
                    vec![completion],
                    PREDICTION_COMPLETION_COUNT,
                ));
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
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        // Structured responses arrive as a single line with `result`.
        let mut buf = String::new();
        loop {
            buf.clear();
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

    #[allow(clippy::too_many_arguments)]
    pub fn generate_structured_with_tools(
        &mut self,
        system: Option<&str>,
        prompt: Option<&str>,
        max_tokens: u32,
        schema: &Value,
        execution_mode: &str,
        tools: Vec<ToolDefinition>,
        tool_results: Vec<ToolResult>,
    ) -> Result<GuidedToolResponse> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate_structured");
        req.system = system.map(str::to_string);
        req.prompt = prompt.map(str::to_string);
        req.max_tokens = Some(max_tokens);
        req.execution_mode = Some(execution_mode.to_string());
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
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some((error, reason)) = response_error(&resp) {
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
    #[allow(clippy::too_many_arguments)]
    pub fn stream_structured(
        &mut self,
        system: Option<&str>,
        prompt: &str,
        input: Option<&[InputMessage]>,
        max_tokens: u32,
        schema: &Value,
        execution_mode: &str,
        tools: Vec<ToolDefinition>,
        tool_results: Vec<ToolResult>,
        mut on_event: impl FnMut(String, Vec<ToolCall>, bool),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "generate_structured_stream");
        req.system = system.map(str::to_string);
        req.prompt = Some(prompt.to_string());
        req.input = input.map(|messages| messages.to_vec());
        req.max_tokens = Some(max_tokens);
        req.execution_mode = Some(execution_mode.to_string());
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
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some((error, reason)) = response_error(&resp) {
                bail!("container returned error {error}: {reason}");
            }
            if let Some(tool_calls) = resp.tool_calls {
                on_event(String::new(), tool_calls, resp.done.unwrap_or(true));
            }
            if let Some(snapshot) = resp.snapshot.or(resp.result) {
                validate_json_schema(&snapshot, schema)?;
                on_event(snapshot, Vec::new(), resp.done.unwrap_or(false));
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
        let mut result = String::new();
        self.stream_transcribe(audio, language_hint, task, "interactive", |token| {
            result.push_str(&token)
        })?;
        Ok(result)
    }

    /// Send a transcribe request and call `on_token` for each returned segment.
    pub fn stream_transcribe(
        &mut self,
        audio: Vec<u8>,
        language_hint: Option<&str>,
        task: &str,
        execution_mode: &str,
        on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        write_transcribe_request(
            &mut self.stdin,
            &id,
            &audio,
            language_hint,
            task,
            execution_mode,
        )?;
        self.last_used = std::time::Instant::now();
        read_text_stream_response(&mut self.stdout, &id, on_token)
    }

    /// Send a vision describe request and return the full description.
    pub fn describe(&mut self, image: Vec<u8>, instructions: &str) -> Result<String> {
        let mut result = String::new();
        self.stream_describe(image, instructions, "interactive", |token| {
            result.push_str(&token)
        })?;
        Ok(result)
    }

    /// Send a vision describe request and call `on_token` for each returned token.
    pub fn stream_describe(
        &mut self,
        image: Vec<u8>,
        instructions: &str,
        execution_mode: &str,
        on_token: impl FnMut(String),
    ) -> Result<()> {
        self.stream_vision_text("describe", image, instructions, execution_mode, on_token)
    }

    /// Send a vision OCR request and return the extracted text.
    pub fn ocr(&mut self, image: Vec<u8>, instructions: &str) -> Result<String> {
        let mut result = String::new();
        self.stream_ocr(image, instructions, "interactive", |token| {
            result.push_str(&token)
        })?;
        Ok(result)
    }

    /// Send a vision OCR request and call `on_token` for each returned token.
    pub fn stream_ocr(
        &mut self,
        image: Vec<u8>,
        instructions: &str,
        execution_mode: &str,
        on_token: impl FnMut(String),
    ) -> Result<()> {
        self.stream_vision_text("ocr", image, instructions, execution_mode, on_token)
    }

    /// Send a vision segment request and return normalized object boxes.
    pub fn segment(
        &mut self,
        image: Vec<u8>,
        instructions: &str,
        execution_mode: &str,
    ) -> Result<Vec<VisionSegment>> {
        let id = Uuid::new_v4().to_string();
        let schema = vision_segment_schema();
        let req = vision_request(id.clone(), "segment", &image, instructions, execution_mode);
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
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
    pub fn embed(&mut self, text: &str, execution_mode: &str) -> Result<Vec<f32>> {
        let id = Uuid::new_v4().to_string();
        let mut req = ContainerRequest::new(id.clone(), "embed");
        req.prompt = Some(text.to_string());
        req.execution_mode = Some(execution_mode.to_string());
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some((error, reason)) = response_error(&resp) {
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

    fn stream_vision_text(
        &mut self,
        request_type: &str,
        image: Vec<u8>,
        instructions: &str,
        execution_mode: &str,
        on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let req = vision_request(
            id.clone(),
            request_type,
            &image,
            instructions,
            execution_mode,
        );
        write_request_line(&mut self.stdin, &req)?;
        self.last_used = std::time::Instant::now();
        read_text_stream_response(&mut self.stdout, &id, on_token)
    }
}

fn write_request_line(writer: &mut impl Write, req: &ContainerRequest) -> Result<()> {
    serde_json::to_writer(&mut *writer, req)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn vision_request(
    id: String,
    request_type: &str,
    image: &[u8],
    instructions: &str,
    execution_mode: &str,
) -> ContainerRequest {
    let mut req = ContainerRequest::new(id, request_type);
    req.image = Some(base64_encode(image));
    req.execution_mode = Some(execution_mode.to_string());
    if !instructions.trim().is_empty() {
        req.prompt = Some(instructions.to_string());
    }
    req
}

fn write_transcribe_request(
    writer: &mut impl Write,
    id: &str,
    audio: &[u8],
    language_hint: Option<&str>,
    task: &str,
    execution_mode: &str,
) -> Result<()> {
    let mut req = ContainerRequest::new(id.to_string(), "transcribe");
    req.audio = Some(base64_encode(audio));
    req.task = Some(task.to_string());
    req.execution_mode = Some(execution_mode.to_string());
    req.language_hint = language_hint
        .filter(|hint| !hint.is_empty())
        .map(str::to_string);
    write_request_line(writer, &req)
}

fn read_text_stream_response(
    reader: &mut impl BufRead,
    id: &str,
    mut on_token: impl FnMut(String),
) -> Result<()> {
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            bail!("container stdout closed unexpectedly");
        }
        let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
        if resp.id != id {
            continue;
        }
        if let Some((error, reason)) = response_error(&resp) {
            bail!("container returned error {error}: {reason}");
        }
        if let Some(token) = resp.token
            && !token.is_empty()
        {
            on_token(token);
        }
        if resp.done.unwrap_or(false) {
            break;
        }
    }
    Ok(())
}

#[doc(hidden)]
pub fn benchmark_read_text_stream_response(input: &[u8], id: &str) -> Result<usize> {
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let mut token_count = 0;
    read_text_stream_response(&mut reader, id, |_| token_count += 1)?;
    Ok(token_count)
}

#[doc(hidden)]
pub fn benchmark_read_response_for_use_case(use_case: &str, input: &[u8]) -> Result<usize> {
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    match use_case {
        "language.generate" | "speech.transcribe" | "vision.describe" | "vision.ocr" => {
            let mut token_count = 0;
            read_text_stream_response(&mut reader, "request-1", |_| token_count += 1)?;
            Ok(token_count)
        }
        "language.predict_next" => {
            let resp = read_matching_response(&mut reader, "request-1")?;
            Ok(resp
                .completions
                .or_else(|| resp.completion.map(|completion| vec![completion]))
                .unwrap_or_default()
                .len())
        }
        "language.structured" | "language.tool" => {
            let schema = serde_json::json!({
                "type": "object",
                "required": ["answer"],
                "properties": { "answer": { "type": "string" } }
            });
            let resp = read_matching_response(&mut reader, "request-1")?;
            Ok(structured_response_result(resp, &schema)?.map_or(0, |result| result.len()))
        }
        "language.embed" => {
            let resp = read_matching_response(&mut reader, "request-1")?;
            Ok(resp.embedding.unwrap_or_default().len())
        }
        "vision.segment" => {
            let schema = vision_segment_schema();
            let resp = read_matching_response(&mut reader, "request-1")?;
            let Some(result) = structured_response_result(resp, &schema)? else {
                return Ok(0);
            };
            let value: VisionSegmentResult = serde_json::from_str(&result)?;
            Ok(value.segments.len())
        }
        other => bail!("unsupported benchmark use-case: {other}"),
    }
}

fn read_matching_response(reader: &mut impl BufRead, id: &str) -> Result<ContainerResponse> {
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            bail!("container stdout closed unexpectedly");
        }
        let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
        if resp.id == id {
            return Ok(resp);
        }
    }
}

#[doc(hidden)]
pub fn benchmark_write_request_for_use_case(
    use_case: &str,
    input: &[InputMessage],
) -> Result<usize> {
    let mut output = Vec::new();
    match use_case {
        "language.generate" => {
            let mut req = ContainerRequest::new("request-1".to_string(), "generate");
            req.system = Some("system instructions".to_string());
            req.prompt = Some("summarize this".to_string());
            req.input = Some(input.to_vec());
            req.max_tokens = Some(256);
            req.execution_mode = Some("interactive".to_string());
            write_request_line(&mut output, &req)?;
        }
        "language.predict_next" => {
            let mut req = ContainerRequest::new("request-1".to_string(), "predict_next");
            req.prompt = Some("The next words are".to_string());
            req.max_tokens = Some(8);
            req.choices = Some(PREDICTION_COMPLETION_COUNT);
            req.temperature = Some(0.4);
            req.execution_mode = Some("interactive".to_string());
            write_request_line(&mut output, &req)?;
        }
        "language.structured" => {
            let mut req =
                ContainerRequest::new("request-1".to_string(), "generate_structured_stream");
            req.system = Some("system instructions".to_string());
            req.prompt = Some("extract fields".to_string());
            req.input = Some(input.to_vec());
            req.max_tokens = Some(256);
            req.execution_mode = Some("interactive".to_string());
            req.response_format = Some(ResponseFormat {
                r#type: "json_schema".to_string(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["summary"],
                    "properties": { "summary": { "type": "string" } }
                }),
            });
            write_request_line(&mut output, &req)?;
        }
        "language.tool" => {
            let mut req = ContainerRequest::new("request-1".to_string(), "generate_structured");
            req.system = Some("system instructions".to_string());
            req.prompt = Some("call the lookup tool".to_string());
            req.max_tokens = Some(256);
            req.execution_mode = Some("interactive".to_string());
            req.response_format = Some(ResponseFormat {
                r#type: "json_schema".to_string(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["answer"],
                    "properties": { "answer": { "type": "string" } }
                }),
            });
            req.tools = Some(vec![ToolDefinition {
                name: "lookup".to_string(),
                description: "Look up a fact".to_string(),
                schema_json: r#"{"type":"object","required":["query"],"properties":{"query":{"type":"string"}}}"#.to_string(),
            }]);
            req.tool_results = Some(vec![ToolResult {
                id: "tool-1".to_string(),
                content: "result".to_string(),
                content_json: r#"{"result":"ok"}"#.to_string(),
            }]);
            write_request_line(&mut output, &req)?;
        }
        "language.embed" => {
            let mut req = ContainerRequest::new("request-1".to_string(), "embed");
            req.prompt = Some("text to embed".to_string());
            req.execution_mode = Some("interactive".to_string());
            write_request_line(&mut output, &req)?;
        }
        "speech.transcribe" => {
            write_transcribe_request(
                &mut output,
                "request-1",
                b"fake-pcm-audio",
                Some("en"),
                "transcribe",
                "interactive",
            )?;
        }
        "vision.describe" => {
            let req = vision_request(
                "request-1".to_string(),
                "describe",
                b"fake-image-bytes",
                "describe the image",
                "interactive",
            );
            write_request_line(&mut output, &req)?;
        }
        "vision.ocr" => {
            let req = vision_request(
                "request-1".to_string(),
                "ocr",
                b"fake-image-bytes",
                "extract text",
                "interactive",
            );
            write_request_line(&mut output, &req)?;
        }
        "vision.segment" => {
            let req = vision_request(
                "request-1".to_string(),
                "segment",
                b"fake-image-bytes",
                "segment objects",
                "interactive",
            );
            write_request_line(&mut output, &req)?;
        }
        other => bail!("unsupported benchmark use-case: {other}"),
    }
    Ok(output.len())
}

fn structured_response_result(resp: ContainerResponse, schema: &Value) -> Result<Option<String>> {
    if let Some((error, reason)) = response_error(&resp) {
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

fn response_error(resp: &ContainerResponse) -> Option<(String, String)> {
    let error = resp.error.as_ref()?;
    if error == "context_window_exceeded" {
        observability::log_context_window_exceeded(
            resp.prompt_tokens,
            resp.max_tokens,
            resp.context_tokens,
            resp.operation.as_deref(),
        );
    }
    let reason = resp.reason.as_ref().unwrap_or(error).clone();
    Some((error.clone(), reason))
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
        variant: Variant,
        image_ref: &str,
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
            variant,
            image_ref,
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
    runtime_rootfs_path_in_stores(user_store, &default_system_oci_store(), image_ref)
}

pub fn runtime_rootfs_path_in_stores(
    user_store: &Path,
    system_store: &Path,
    image_ref: &str,
) -> Option<PathBuf> {
    runtime_rootfs_path_from_stores(user_store, system_store, image_ref)
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
    variant: Variant,
    _image_ref: &str,
    rootfs: &Path,
    artifact_path: &Path,
    runtime_options: &HashMap<String, String>,
    memory_limit: &str,
) -> Result<Value> {
    let mut env = vec![
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        "XDG_CACHE_HOME=/tmp".to_string(),
    ];
    let mut env_options = runtime_options.clone();
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

    if variant == Variant::Cuda {
        add_cuda_mounts(&mut mounts, &mut env_options);
        add_readonly_mount(&mut mounts, "/sys");
        push_default_env(&mut env, &env_options, "N_GPU_LAYERS", "-1");
        push_default_env(&mut env, &env_options, "AILERON_DEVICE", "cuda");
    } else if variant == Variant::Rocm {
        add_device_mount(&mut mounts, "/dev/kfd");
        add_device_mount(&mut mounts, "/dev/dri");
        add_readonly_mount(&mut mounts, "/sys");
        push_default_env(&mut env, &env_options, "N_GPU_LAYERS", "-1");
        push_default_env(&mut env, &env_options, "AILERON_DEVICE", "rocm");
    } else if variant == Variant::Vulkan {
        add_device_mount(&mut mounts, "/dev/dri");
        add_host_vulkan_mounts(&mut mounts, &mut env_options);
        add_readonly_mount(&mut mounts, "/sys");
        push_default_env(&mut env, &env_options, "N_GPU_LAYERS", "-1");
        push_default_env(&mut env, &env_options, "AILERON_DEVICE", "vulkan");
    }

    let mut runtime_options: Vec<_> = env_options.iter().collect();
    runtime_options.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in runtime_options {
        env.push(format!("{key}={value}"));
    }

    let mut config = serde_json::json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0 },
            "args": ["/entrypoint"],
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
    });

    if is_gpu_variant(variant) {
        config["annotations"] = serde_json::json!({
            "run.oci.keep_original_groups": "1"
        });
    }

    Ok(config)
}

fn push_default_env(
    env: &mut Vec<String>,
    runtime_options: &HashMap<String, String>,
    key: &str,
    value: &str,
) {
    if !runtime_options.contains_key(key) {
        env.push(format!("{key}={value}"));
    }
}

fn add_device_mount(mounts: &mut Vec<Value>, path: &str) {
    mounts.push(serde_json::json!({
        "destination": path,
        "type": "bind",
        "source": path,
        "options": ["rbind", "rw", "nosuid"]
    }));
}

fn add_cuda_mounts(mounts: &mut Vec<Value>, runtime_options: &mut HashMap<String, String>) {
    add_nvidia_device_mounts(mounts);
    add_nvidia_driver_library_mounts(mounts, runtime_options);
}

fn add_nvidia_vulkan_mounts(
    mounts: &mut Vec<Value>,
    runtime_options: &mut HashMap<String, String>,
) -> Vec<String> {
    if !has_nvidia_devices() {
        return Vec::new();
    }

    add_nvidia_device_mounts(mounts);
    add_nvidia_driver_library_mounts(mounts, runtime_options);
    add_nvidia_vulkan_icd_mounts(mounts)
}

fn add_host_vulkan_mounts(mounts: &mut Vec<Value>, runtime_options: &mut HashMap<String, String>) {
    let driver_files = add_nvidia_vulkan_mounts(mounts, runtime_options);
    // Mesa userspace drivers must match the container libc/LLVM stack. Use the
    // runtime image's Mesa drivers for AMD/Intel instead of host bind mounts.
    set_default_vulkan_driver_files(runtime_options, driver_files);
}

fn add_nvidia_device_mounts(mounts: &mut Vec<Value>) {
    add_existing_device_mounts(mounts, NVIDIA_DEVICE_PATHS);
    if Path::new("/proc/driver/nvidia").exists() {
        add_readonly_mount(mounts, "/proc/driver/nvidia");
    }
}

fn has_nvidia_devices() -> bool {
    NVIDIA_DEVICE_PATHS
        .iter()
        .any(|path| Path::new(path).exists())
}

fn add_existing_device_mounts(mounts: &mut Vec<Value>, paths: &[&str]) {
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

fn add_nvidia_driver_library_mounts(
    mounts: &mut Vec<Value>,
    runtime_options: &mut HashMap<String, String>,
) {
    add_nvidia_driver_library_mounts_from(mounts, runtime_options, nvidia_driver_library_mounts());
}

fn add_nvidia_driver_library_mounts_from(
    mounts: &mut Vec<Value>,
    runtime_options: &mut HashMap<String, String>,
    library_mounts: Vec<(PathBuf, String)>,
) {
    if library_mounts.is_empty() {
        return;
    }

    for (source, library_name) in library_mounts {
        add_readonly_file_mount(
            mounts,
            &source,
            &Path::new(NVIDIA_LIBRARY_DIR).join(library_name),
        );
    }
    prepend_ld_library_path(runtime_options, NVIDIA_LIBRARY_DIR);
}

fn add_nvidia_vulkan_icd_mounts(mounts: &mut Vec<Value>) -> Vec<String> {
    add_nvidia_vulkan_icd_mounts_from(mounts, nvidia_vulkan_icd_files())
}

fn add_nvidia_vulkan_icd_mounts_from(
    mounts: &mut Vec<Value>,
    icd_files: Vec<PathBuf>,
) -> Vec<String> {
    let mut driver_files = Vec::new();
    for source in icd_files {
        let Some(file_name) = source.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_nvidia_vulkan_icd_file_name(file_name) {
            continue;
        }

        let destination = Path::new(VULKAN_ICD_DIR).join(file_name);
        let destination_string = destination.display().to_string();
        if driver_files.contains(&destination_string) {
            continue;
        }

        add_readonly_file_mount(mounts, &source, &destination);
        driver_files.push(destination_string);
    }

    driver_files
}

fn set_default_vulkan_driver_files(
    runtime_options: &mut HashMap<String, String>,
    driver_files: Vec<String>,
) {
    if driver_files.is_empty()
        || runtime_options.contains_key("VK_DRIVER_FILES")
        || runtime_options.contains_key("VK_ICD_FILENAMES")
    {
        return;
    }

    let driver_files = driver_files.join(":");
    runtime_options.insert("VK_DRIVER_FILES".to_string(), driver_files.clone());
    runtime_options.insert("VK_ICD_FILENAMES".to_string(), driver_files);
}

fn prepend_ld_library_path(runtime_options: &mut HashMap<String, String>, path: &str) {
    runtime_options
        .entry("LD_LIBRARY_PATH".to_string())
        .and_modify(|existing| {
            if !existing.split(':').any(|part| part == path) {
                *existing = if existing.is_empty() {
                    path.to_string()
                } else {
                    format!("{path}:{existing}")
                };
            }
        })
        .or_insert_with(|| path.to_string());
}

fn add_readonly_file_mount(mounts: &mut Vec<Value>, source: &Path, destination: &Path) {
    if mounts.iter().any(|mount| {
        mount["source"] == source.display().to_string()
            && mount["destination"] == destination.display().to_string()
    }) {
        return;
    }

    mounts.push(serde_json::json!({
        "destination": destination.display().to_string(),
        "type": "bind",
        "source": source.display().to_string(),
        "options": ["bind", "ro", "nosuid", "nodev"]
    }));
}

fn nvidia_driver_library_mounts() -> Vec<(PathBuf, String)> {
    let ldconfig = ldconfig_library_cache();
    let mut mounts = Vec::new();
    for library_name in NVIDIA_DRIVER_LIBRARIES {
        let Some(path) = find_host_library(library_name, &ldconfig) else {
            continue;
        };
        push_unique_library_mount(&mut mounts, path, library_name);
    }

    let mut ldconfig_entries = ldconfig.iter().collect::<Vec<_>>();
    ldconfig_entries.sort_by(|a, b| a.0.cmp(b.0));
    for (library_name, path) in ldconfig_entries {
        push_unique_library_mount(&mut mounts, path.clone(), library_name);
    }

    for dir in COMMON_LIBRARY_DIRS {
        for (path, library_name) in nvidia_driver_library_files_in_dir(Path::new(dir)) {
            push_unique_library_mount(&mut mounts, path, &library_name);
        }
    }
    mounts
}

fn push_unique_library_mount(
    mounts: &mut Vec<(PathBuf, String)>,
    path: PathBuf,
    library_name: &str,
) {
    if path.exists() && !mounts.iter().any(|(_, name)| name == library_name) {
        mounts.push((path, library_name.to_string()));
    }
}

fn nvidia_driver_library_files_in_dir(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut files = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() && !file_type.is_symlink() {
                return None;
            }
            let library_name = entry.file_name().to_str()?.to_string();
            if is_nvidia_driver_library_name(&library_name) {
                Some((entry.path(), library_name))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files
}

fn nvidia_vulkan_icd_files() -> Vec<PathBuf> {
    vulkan_icd_files_matching(is_nvidia_vulkan_icd_file_name)
}

fn vulkan_icd_files_matching(matches: fn(&str) -> bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in VULKAN_ICD_DIRS {
        let mut dir_files = fs::read_dir(dir)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if !file_type.is_file() && !file_type.is_symlink() {
                    return None;
                }
                let file_name = entry.file_name().to_str()?.to_string();
                if matches(&file_name) {
                    Some(entry.path())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        dir_files.sort();
        for file in dir_files {
            let Some(file_name) = file.file_name() else {
                continue;
            };
            if !files.iter().any(|existing: &PathBuf| {
                existing
                    .file_name()
                    .is_some_and(|existing| existing == file_name)
            }) {
                files.push(file);
            }
        }
    }
    files
}

fn is_nvidia_driver_library_name(name: &str) -> bool {
    NVIDIA_DRIVER_LIBRARIES.contains(&name)
        || (name.starts_with("libnvidia-") && name.contains(".so"))
        || name.starts_with("libGLX_nvidia.so")
        || name.starts_with("libEGL_nvidia.so")
}

fn is_nvidia_vulkan_icd_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".json") && lower.contains("nvidia")
}

fn find_host_library(library_name: &str, ldconfig: &HashMap<String, PathBuf>) -> Option<PathBuf> {
    ldconfig
        .get(library_name)
        .filter(|path| path.exists())
        .cloned()
        .or_else(|| {
            COMMON_LIBRARY_DIRS
                .iter()
                .map(|dir| Path::new(dir).join(library_name))
                .find(|path| path.exists())
        })
}

fn ldconfig_library_cache() -> HashMap<String, PathBuf> {
    std::process::Command::new("ldconfig")
        .arg("-p")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .map(|output| parse_ldconfig_cache(&output))
        .unwrap_or_default()
}

fn parse_ldconfig_cache(output: &str) -> HashMap<String, PathBuf> {
    let mut libraries = HashMap::new();
    for line in output.lines() {
        let Some((name_and_abi, path)) = line.split_once("=>") else {
            continue;
        };
        let Some(name) = name_and_abi.split_whitespace().next() else {
            continue;
        };
        if is_nvidia_driver_library_name(name) {
            libraries
                .entry(name.to_string())
                .or_insert_with(|| PathBuf::from(path.trim()));
        }
    }
    libraries
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

#[cfg(test)]
fn image_ref_uses_tag(image_ref: &str, tag: &str) -> bool {
    let tagged_ref = image_ref
        .split_once('@')
        .map_or(image_ref, |(tagged_ref, _)| tagged_ref);
    tagged_ref
        .rsplit_once('/')
        .map_or(tagged_ref, |(_, after_slash)| after_slash)
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

/// Pool handle for a running container.
#[derive(Clone)]
pub struct ContainerHandle {
    inner: Arc<Mutex<Container>>,
    child: Arc<Mutex<Child>>,
    terminating: Arc<AtomicBool>,
    published: Arc<AtomicBool>,
}

impl ContainerHandle {
    fn new(container: Container) -> Self {
        Self::new_with_published(container, true)
    }

    fn new_pending(container: Container) -> Self {
        Self::new_with_published(container, false)
    }

    fn new_with_published(container: Container, published: bool) -> Self {
        let child = container.child.clone();
        Self {
            inner: Arc::new(Mutex::new(container)),
            child,
            terminating: Arc::new(AtomicBool::new(false)),
            published: Arc::new(AtomicBool::new(published)),
        }
    }

    pub fn lock(&self) -> LockResult<MutexGuard<'_, Container>> {
        self.inner.lock()
    }

    pub(crate) fn try_lock(&self) -> TryLockResult<MutexGuard<'_, Container>> {
        self.inner.try_lock()
    }

    fn strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    pub(crate) fn publish(&self) {
        self.published.store(true, Ordering::SeqCst);
    }

    fn is_published(&self) -> bool {
        self.published.load(Ordering::SeqCst)
    }

    pub(crate) fn terminate(&self) {
        self.terminating.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub(crate) fn is_terminating(&self) -> bool {
        if self.terminating.load(Ordering::SeqCst) {
            return true;
        }

        match self.child.try_lock() {
            Ok(mut child) => match child.try_wait() {
                Ok(Some(_)) => {
                    self.terminating.store(true, Ordering::SeqCst);
                    true
                }
                Ok(None) => false,
                Err(e) => {
                    warn!("failed to inspect container child status: {e}");
                    self.terminating.store(true, Ordering::SeqCst);
                    true
                }
            },
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Poisoned(_)) => {
                self.terminating.store(true, Ordering::SeqCst);
                true
            }
        }
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

pub struct ContainerPool {
    containers: HashMap<String, ContainerHandle>,
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
        variant: Variant,
        image_ref: &str,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        on_status: impl FnMut(String) + Send + 'static,
    ) -> Result<ContainerHandle> {
        let should_replace = self.containers.get(profile_id).is_some_and(|container| {
            if container.is_terminating() {
                return true;
            }
            let Ok(container) = container.try_lock() else {
                return false;
            };
            container.variant != variant
                || container.image_ref != image_ref
                || container.artifact_path != artifact_path
                || container.runtime_options != *runtime_options
        });
        if should_replace {
            observability::log_runtime_replacing_image(profile_id, image_ref, variant.as_tag());
            if let Some(container) = self.containers.remove(profile_id) {
                container.terminate();
            }
        }

        if !self.containers.contains_key(profile_id) {
            let candidate = RuntimeCandidate {
                variant,
                image_ref: image_ref.to_string(),
            };
            let c = Container::spawn(
                "",
                0,
                &candidate,
                artifact_path,
                runtime_options,
                &self.memory_limit,
                &self.oci_store,
                &self.system_oci_store,
                on_status,
                || Ok(()),
            )?;
            self.containers
                .insert(profile_id.to_string(), ContainerHandle::new(c));
        } else if let Some(container) = self.containers.get(profile_id)
            && let Ok(mut container) = container.try_lock()
        {
            container.last_used = std::time::Instant::now();
        }
        Ok(self.containers.get(profile_id).unwrap().clone())
    }

    /// Get or spawn a container using the first runtime image that starts.
    /// Existing containers are reused when their image is still in the candidate
    /// set, so a working fallback is not churned on every request.
    pub fn get_or_spawn_any(
        &mut self,
        profile_id: &str,
        runtime_id: &str,
        candidates: &[RuntimeCandidate],
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        on_status: impl FnMut(String) + Send + 'static,
    ) -> Result<ContainerHandle> {
        self.get_or_spawn_any_checked(
            profile_id,
            0,
            runtime_id,
            candidates,
            artifact_path,
            runtime_options,
            on_status,
            || Ok(()),
        )
        .map(|(handle, _)| {
            handle.publish();
            handle
        })
    }

    /// Like `get_or_spawn_any`, but checks cancellation before cold starts.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_spawn_any_checked(
        &mut self,
        profile_id: &str,
        profile_epoch: u64,
        runtime_id: &str,
        candidates: &[RuntimeCandidate],
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        on_status: impl FnMut(String) + Send + 'static,
        mut should_continue: impl FnMut() -> Result<(), String>,
    ) -> Result<(ContainerHandle, bool)> {
        if candidates.is_empty() {
            bail!("no runtime images resolved for profile {profile_id}");
        }
        if let Err(reason) = should_continue() {
            bail!(reason);
        }
        let attempts = runtime_spawn_attempts(runtime_id, candidates, runtime_options);

        let mut replace_existing = false;
        if let Some(handle) = self.containers.get(profile_id) {
            if handle.is_terminating() {
                replace_existing = true;
            } else if !handle.is_published() {
                bail!(
                    "container startup is being finalized for profile {profile_id}; retry request"
                );
            } else {
                match handle.try_lock() {
                    Ok(mut container) => {
                        let matches = attempts.iter().any(|attempt| {
                            attempt.image_ref == container.image_ref
                                && attempt.variant == container.variant
                                && container.runtime_id == runtime_id
                                && container.profile_epoch == profile_epoch
                                && attempt.runtime_options == container.runtime_options
                                && container.artifact_path == artifact_path
                        });
                        if matches {
                            container.last_used = std::time::Instant::now();
                            return Ok((handle.clone(), false));
                        }
                        replace_existing = true;
                    }
                    Err(TryLockError::WouldBlock) => {
                        return Ok((handle.clone(), false));
                    }
                    Err(TryLockError::Poisoned(_)) => bail!("container mutex poisoned"),
                }
            }
        }

        if replace_existing {
            if let Err(reason) = should_continue() {
                bail!(reason);
            }
            observability::log_runtime_replacing_candidates(
                profile_id,
                runtime_id,
                candidates.len(),
            );
            if let Some(container) = self.containers.remove(profile_id) {
                container.terminate();
            }
        }

        let on_status = std::sync::Arc::new(std::sync::Mutex::new(on_status));
        let mut errors = Vec::new();
        let mut missing = Vec::new();
        let mut attempted = false;
        for attempt in attempts {
            if runtime_rootfs_path_in_stores(
                &self.oci_store,
                &self.system_oci_store,
                &attempt.image_ref,
            )
            .is_none()
            {
                if missing.contains(&attempt.image_ref) {
                    continue;
                }
                missing.push(attempt.image_ref.clone());
                let role = if attempt.image_ref == candidates[0].image_ref {
                    "preferred"
                } else {
                    "fallback"
                };
                errors.push(format!(
                    "{role} runtime image {} ({}) is not installed in the user or system OCI store",
                    attempt.image_ref,
                    attempt.variant.as_tag()
                ));
                continue;
            }
            attempted = true;
            if let Err(reason) = should_continue() {
                bail!(reason);
            }
            let status_callback = on_status.clone();
            let candidate = RuntimeCandidate {
                variant: attempt.variant,
                image_ref: attempt.image_ref.clone(),
            };
            match Container::spawn(
                runtime_id,
                profile_epoch,
                &candidate,
                artifact_path,
                &attempt.runtime_options,
                &self.memory_limit,
                &self.oci_store,
                &self.system_oci_store,
                move |line| {
                    if let Ok(mut callback) = status_callback.lock() {
                        (*callback)(line);
                    }
                },
                &mut should_continue,
            ) {
                Ok(container) => {
                    let handle = ContainerHandle::new_pending(container);
                    self.containers
                        .insert(profile_id.to_string(), handle.clone());
                    return Ok((handle, true));
                }
                Err(error) => {
                    let error_text = error.to_string();
                    if error_text.starts_with("container returned error request_cancelled:")
                        || error_text.ends_with("; retry request")
                    {
                        return Err(error);
                    }
                    observability::log_runtime_start_failed(
                        profile_id,
                        runtime_id,
                        &attempt.image_ref,
                        attempt.variant.as_tag(),
                        attempt
                            .runtime_options
                            .get("N_GPU_LAYERS")
                            .map(String::as_str),
                        &error_text,
                    );
                    let role = if attempt.image_ref == candidates[0].image_ref {
                        "preferred"
                    } else {
                        "fallback"
                    };
                    errors.push(format!(
                        "{role} runtime image {} failed to start: {error_text}",
                        describe_spawn_attempt(&attempt)
                    ));
                }
            }
        }

        if attempted {
            bail!(
                "failed to start any installed runtime image for profile {profile_id}: {}",
                errors.join(" | ")
            )
        } else {
            bail!(
                "no installed runtime fallback was available for profile {profile_id}: {}",
                errors.join(" | ")
            )
        }
    }

    /// Kill and remove the container for a profile.
    pub fn kill(&mut self, profile_id: &str) {
        if let Some(container) = self.containers.remove(profile_id) {
            container.terminate();
            info!("terminated container for profile {}", profile_id);
        }
    }

    pub fn kill_handle(&mut self, profile_id: &str, handle: &ContainerHandle) {
        let should_remove = self
            .containers
            .get(profile_id)
            .is_some_and(|current| current.ptr_eq(handle));
        if should_remove {
            self.kill(profile_id);
        } else {
            handle.terminate();
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
            .filter(|(_, c)| {
                if c.strong_count() > 1 {
                    return false;
                }
                let Ok(c) = c.try_lock() else {
                    return false;
                };
                now.duration_since(c.last_used) > timeout
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in idle {
            observability::log_runtime_evicted_idle(&k, self.idle_timeout_secs);
            self.containers.remove(&k);
        }
    }
}

fn runtime_spawn_attempts(
    runtime_id: &str,
    candidates: &[RuntimeCandidate],
    runtime_options: &HashMap<String, String>,
) -> Vec<RuntimeSpawnAttempt> {
    let mut attempts = Vec::new();
    for candidate in candidates {
        attempts.push(RuntimeSpawnAttempt {
            variant: candidate.variant,
            image_ref: candidate.image_ref.clone(),
            runtime_options: runtime_options.clone(),
        });

        if !is_gpu_variant(candidate.variant)
            || !runtime_supports_llama_runtime_options(runtime_id)
            || runtime_options.contains_key("N_GPU_LAYERS")
        {
            continue;
        }

        for layers in ["64", "32", "16", "8", "4", "0"] {
            let mut fallback_options = runtime_options.clone();
            fallback_options.insert("N_GPU_LAYERS".to_string(), layers.to_string());
            attempts.push(RuntimeSpawnAttempt {
                variant: candidate.variant,
                image_ref: candidate.image_ref.clone(),
                runtime_options: fallback_options,
            });
        }
    }
    attempts
}

fn is_gpu_variant(variant: Variant) -> bool {
    matches!(variant, Variant::Cuda | Variant::Rocm | Variant::Vulkan)
}

pub(crate) fn runtime_supports_llama_runtime_options(runtime_id: &str) -> bool {
    runtime_id == ML_RUNTIME_ID
}

fn describe_spawn_attempt(attempt: &RuntimeSpawnAttempt) -> String {
    match attempt.runtime_options.get("N_GPU_LAYERS") {
        Some(layers) if is_gpu_variant(attempt.variant) => {
            format!("{} (N_GPU_LAYERS={layers})", attempt.image_ref)
        }
        _ => attempt.image_ref.clone(),
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
    input: Option<Vec<InputMessage>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_mode: Option<String>,
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
            input: None,
            max_tokens: None,
            choices: None,
            temperature: None,
            audio: None,
            task: None,
            image: None,
            language_hint: None,
            execution_mode: None,
            response_format: None,
            tools: None,
            tool_results: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputPart>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum InputPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "input_image")]
    InputImage { image: String, mime_type: String },
    #[serde(rename = "input_audio")]
    InputAudio { audio: String, mime_type: String },
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
    prompt_tokens: Option<u64>,
    max_tokens: Option<u64>,
    context_tokens: Option<u64>,
    operation: Option<String>,
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
            Variant::Cpu,
            "localhost/aileron/summarize:cpu",
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
            config["process"]["args"],
            serde_json::json!(["/entrypoint"])
        );
        assert!(env(&config).contains(&"XDG_CACHE_HOME=/tmp"));
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
            Variant::Rocm,
            "localhost/aileron/summarize:rocm",
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
            Variant::Cuda,
            "localhost/aileron/summarize:cuda",
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
            Variant::Vulkan,
            "localhost/aileron/summarize:vulkan",
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
    fn oci_config_does_not_expose_gpu_devices_for_cpu_tag() {
        let config = runtime_config_json(
            Variant::Cpu,
            "localhost/aileron/summarize:cpu",
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
    fn explicit_variant_controls_accelerator_mounts_for_untagged_digest_ref() {
        let config = runtime_config_json(
            Variant::Vulkan,
            "ghcr.io/example/aileron-runtime@sha256:abcdef",
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(device_mounts(&config).contains(&"/dev/dri"));
        assert!(env(&config).contains(&"AILERON_DEVICE=vulkan"));
        assert!(env(&config).contains(&"N_GPU_LAYERS=-1"));
    }

    #[test]
    fn explicit_cpu_variant_does_not_expose_gpu_for_gpu_tagged_ref() {
        let config = runtime_config_json(
            Variant::Cpu,
            "ghcr.io/example/aileron-runtime:vulkan",
            Path::new("/store/rootfs/runtime"),
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        )
        .expect("build OCI config");

        assert!(!device_mounts(&config).contains(&"/dev/dri"));
        assert!(!mount_destinations(&config).contains(&"/sys"));
        assert!(
            !env(&config)
                .iter()
                .any(|entry| entry.starts_with("AILERON_DEVICE="))
        );
    }

    #[test]
    fn all_declared_runtime_refs_get_expected_accelerator_mounts() {
        let refs = [
            "ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:cpu",
            "ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:cuda",
            "ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:rocm",
            "ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:vulkan",
        ];

        for image_ref in refs {
            let config = runtime_config_json(
                Variant::from_tag(if image_ref_uses_tag(image_ref, "cuda") {
                    "cuda"
                } else if image_ref_uses_tag(image_ref, "rocm") {
                    "rocm"
                } else if image_ref_uses_tag(image_ref, "vulkan") {
                    "vulkan"
                } else {
                    "cpu"
                })
                .expect("declared variant tag"),
                image_ref,
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
            Variant::Cpu,
            "localhost/aileron/vision:cpu",
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
            Variant::Cuda,
            "localhost/aileron/llm:cuda",
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

        assert_eq!(n_gpu_layers, ["N_GPU_LAYERS=16"]);
        assert_eq!(devices, ["AILERON_DEVICE=cpu"]);
    }

    #[test]
    fn runtime_spawn_attempts_retry_gpu_images_with_reduced_offload() {
        let candidates = vec![
            RuntimeCandidate {
                variant: Variant::Cuda,
                image_ref: "registry.example/ai/summarizer:cuda".to_string(),
            },
            RuntimeCandidate {
                variant: Variant::Cpu,
                image_ref: "registry.example/ai/summarizer:cpu".to_string(),
            },
        ];

        let attempts = runtime_spawn_attempts(ML_RUNTIME_ID, &candidates, &HashMap::new());
        let attempt_summary = attempts
            .iter()
            .map(|attempt| {
                (
                    attempt.image_ref.as_str(),
                    attempt
                        .runtime_options
                        .get("N_GPU_LAYERS")
                        .map(String::as_str),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            attempt_summary,
            vec![
                ("registry.example/ai/summarizer:cuda", None),
                ("registry.example/ai/summarizer:cuda", Some("64")),
                ("registry.example/ai/summarizer:cuda", Some("32")),
                ("registry.example/ai/summarizer:cuda", Some("16")),
                ("registry.example/ai/summarizer:cuda", Some("8")),
                ("registry.example/ai/summarizer:cuda", Some("4")),
                ("registry.example/ai/summarizer:cuda", Some("0")),
                ("registry.example/ai/summarizer:cpu", None),
            ]
        );
    }

    #[test]
    fn runtime_spawn_attempts_do_not_retry_non_layered_gpu_images() {
        let candidates = vec![
            RuntimeCandidate {
                variant: Variant::Cuda,
                image_ref: "localhost/aileron/aileron-runtime-stub:cuda".to_string(),
            },
            RuntimeCandidate {
                variant: Variant::Cpu,
                image_ref: "localhost/aileron/aileron-runtime-stub:cpu".to_string(),
            },
        ];

        let attempts = runtime_spawn_attempts("stub", &candidates, &HashMap::new());

        assert_eq!(
            attempts
                .iter()
                .map(|attempt| attempt.image_ref.as_str())
                .collect::<Vec<_>>(),
            vec![
                "localhost/aileron/aileron-runtime-stub:cuda",
                "localhost/aileron/aileron-runtime-stub:cpu",
            ]
        );
    }

    #[test]
    fn runtime_spawn_attempts_preserve_explicit_gpu_layer_override() {
        let candidates = vec![
            RuntimeCandidate {
                variant: Variant::Cuda,
                image_ref: "localhost/aileron/aileron-runtime-llm-vision-whisper:cuda".to_string(),
            },
            RuntimeCandidate {
                variant: Variant::Vulkan,
                image_ref: "localhost/aileron/aileron-runtime-llm-vision-whisper:vulkan"
                    .to_string(),
            },
        ];
        let mut runtime_options = HashMap::new();
        runtime_options.insert("N_GPU_LAYERS".to_string(), "12".to_string());

        let attempts = runtime_spawn_attempts(ML_RUNTIME_ID, &candidates, &runtime_options);

        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().all(|attempt| {
            attempt
                .runtime_options
                .get("N_GPU_LAYERS")
                .map(String::as_str)
                == Some("12")
        }));
    }

    #[test]
    fn runtime_spawn_attempts_use_variant_for_untagged_gpu_ref() {
        let candidates = vec![RuntimeCandidate {
            variant: Variant::Vulkan,
            image_ref: "registry.example/ai/summarizer@sha256:abcdef".to_string(),
        }];

        let attempts = runtime_spawn_attempts(ML_RUNTIME_ID, &candidates, &HashMap::new());

        assert_eq!(attempts.len(), 7);
        assert_eq!(attempts[0].runtime_options.get("N_GPU_LAYERS"), None);
        assert_eq!(
            attempts[1]
                .runtime_options
                .get("N_GPU_LAYERS")
                .map(String::as_str),
            Some("64")
        );
    }

    #[test]
    fn container_reuse_requires_matching_variant() {
        let artifact_path = test_dir("variant-reuse-artifacts");
        let _ = std::fs::remove_dir_all(&artifact_path);
        std::fs::create_dir_all(&artifact_path).expect("create artifact path");
        let mut pool = ContainerPool {
            containers: HashMap::from([(
                "profile".to_string(),
                ContainerHandle::new(test_container(
                    Variant::Cpu,
                    "registry.example/runtime@sha256:abcdef",
                    &artifact_path,
                )),
            )]),
            idle_timeout_secs: 300,
            memory_limit: "512m".to_string(),
            oci_store: test_dir("variant-reuse-user-store"),
            system_oci_store: test_dir("variant-reuse-system-store"),
        };
        let candidates = vec![RuntimeCandidate {
            variant: Variant::Vulkan,
            image_ref: "registry.example/runtime@sha256:abcdef".to_string(),
        }];

        let err = match pool.get_or_spawn_any(
            "profile",
            ML_RUNTIME_ID,
            &candidates,
            &artifact_path,
            &HashMap::new(),
            |_| {},
        ) {
            Ok(_) => panic!("different variant must not reuse existing container"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("no installed runtime fallback"));
        assert!(!pool.containers.contains_key("profile"));

        let _ = std::fs::remove_dir_all(artifact_path);
        let _ = std::fs::remove_dir_all(pool.oci_store);
        let _ = std::fs::remove_dir_all(pool.system_oci_store);
    }

    #[test]
    fn get_or_spawn_any_reports_when_no_candidate_rootfs_is_installed() {
        let user_store = test_dir("missing-user-rootfs");
        let system_store = test_dir("missing-system-rootfs");
        let artifact_path = test_dir("missing-rootfs-artifacts");
        let _ = std::fs::remove_dir_all(&user_store);
        let _ = std::fs::remove_dir_all(&system_store);
        let _ = std::fs::remove_dir_all(&artifact_path);
        std::fs::create_dir_all(&artifact_path).expect("create artifact path");
        let mut pool = ContainerPool {
            containers: HashMap::new(),
            idle_timeout_secs: 300,
            memory_limit: "512m".to_string(),
            oci_store: user_store.clone(),
            system_oci_store: system_store.clone(),
        };
        let candidates = vec![
            RuntimeCandidate {
                variant: Variant::Cuda,
                image_ref: "localhost/aileron/runtime:cuda".to_string(),
            },
            RuntimeCandidate {
                variant: Variant::Cpu,
                image_ref: "localhost/aileron/runtime:cpu".to_string(),
            },
        ];

        let err = match pool.get_or_spawn_any(
            "profile",
            ML_RUNTIME_ID,
            &candidates,
            &artifact_path,
            &HashMap::new(),
            |_| {},
        ) {
            Ok(_) => panic!("missing rootfs candidates should fail before spawn"),
            Err(err) => err,
        };

        let reason = err.to_string();
        assert!(reason.contains("no installed runtime fallback was available"));
        assert!(reason.contains("preferred runtime image"));
        assert!(reason.contains("fallback runtime image"));

        let _ = std::fs::remove_dir_all(user_store);
        let _ = std::fs::remove_dir_all(system_store);
        let _ = std::fs::remove_dir_all(artifact_path);
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
            &[present.to_str().unwrap(), missing.to_str().unwrap()],
        );

        let destinations = mounts
            .iter()
            .filter_map(|mount| mount["destination"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(destinations, vec![present.to_str().unwrap()]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_ldconfig_nvidia_driver_library_entries() {
        let cache = parse_ldconfig_cache(
            r#"
            123 libs found in cache `/etc/ld.so.cache'
            libcuda.so.1 (libc6,x86-64) => /usr/lib64/libcuda.so.550.54
            libnvidia-ml.so.1 (libc6,x86-64) => /usr/lib64/libnvidia-ml.so.550.54
            libGLX_nvidia.so.0 (libc6,x86-64) => /usr/lib64/libGLX_nvidia.so.550.54
            libnvidia-glcore.so.550.54 (libc6,x86-64) => /usr/lib64/libnvidia-glcore.so.550.54
            libc.so.6 (libc6,x86-64) => /usr/lib64/libc.so.6
            "#,
        );

        assert_eq!(
            cache.get("libcuda.so.1"),
            Some(&PathBuf::from("/usr/lib64/libcuda.so.550.54"))
        );
        assert_eq!(
            cache.get("libnvidia-ml.so.1"),
            Some(&PathBuf::from("/usr/lib64/libnvidia-ml.so.550.54"))
        );
        assert_eq!(
            cache.get("libGLX_nvidia.so.0"),
            Some(&PathBuf::from("/usr/lib64/libGLX_nvidia.so.550.54"))
        );
        assert_eq!(
            cache.get("libnvidia-glcore.so.550.54"),
            Some(&PathBuf::from("/usr/lib64/libnvidia-glcore.so.550.54"))
        );
        assert!(!cache.contains_key("libc.so.6"));
    }

    #[test]
    fn nvidia_driver_library_mounts_are_readonly_and_searchable() {
        let root = std::env::temp_dir().join(format!("aileron-nvidia-lib-test-{}", Uuid::new_v4()));
        let libcuda = root.join("libcuda.so.1");
        let libnvml = root.join("libnvidia-ml.so.1");
        std::fs::create_dir_all(&root).expect("create temp dir");
        std::fs::write(&libcuda, "cuda driver placeholder").expect("write libcuda");
        std::fs::write(&libnvml, "nvml placeholder").expect("write nvml");

        let mut mounts = Vec::new();
        let mut runtime_options = HashMap::new();
        add_nvidia_driver_library_mounts_from(
            &mut mounts,
            &mut runtime_options,
            vec![
                (libcuda.clone(), "libcuda.so.1".to_string()),
                (libnvml.clone(), "libnvidia-ml.so.1".to_string()),
            ],
        );

        assert_eq!(
            runtime_options.get("LD_LIBRARY_PATH").map(String::as_str),
            Some("/usr/local/nvidia/lib64")
        );
        assert!(mounts.iter().any(|mount| {
            mount["source"] == libcuda.display().to_string()
                && mount["destination"] == "/usr/local/nvidia/lib64/libcuda.so.1"
                && array_contains(&mount["options"], "ro")
                && !array_contains(&mount["options"], "noexec")
        }));
        assert!(mounts.iter().any(|mount| {
            mount["source"] == libnvml.display().to_string()
                && mount["destination"] == "/usr/local/nvidia/lib64/libnvidia-ml.so.1"
                && array_contains(&mount["options"], "ro")
                && !array_contains(&mount["options"], "noexec")
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nvidia_vulkan_icd_mounts_force_host_nvidia_driver_files() {
        let root = std::env::temp_dir().join(format!("aileron-vulkan-icd-test-{}", Uuid::new_v4()));
        let icd = root.join("nvidia_icd.json");
        std::fs::create_dir_all(&root).expect("create temp dir");
        std::fs::write(&icd, "{}").expect("write icd");

        let mut mounts = Vec::new();
        let mut runtime_options = HashMap::new();
        let driver_files = add_nvidia_vulkan_icd_mounts_from(&mut mounts, vec![icd.clone()]);
        set_default_vulkan_driver_files(&mut runtime_options, driver_files);

        let driver_files = "/usr/share/vulkan/icd.d/nvidia_icd.json";
        assert_eq!(
            runtime_options.get("VK_DRIVER_FILES").map(String::as_str),
            Some(driver_files)
        );
        assert_eq!(
            runtime_options.get("VK_ICD_FILENAMES").map(String::as_str),
            Some(driver_files)
        );
        assert!(mounts.iter().any(|mount| {
            mount["source"] == icd.display().to_string()
                && mount["destination"] == driver_files
                && array_contains(&mount["options"], "ro")
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nvidia_vulkan_icd_mounts_preserve_explicit_driver_files() {
        let mut mounts = Vec::new();
        let mut runtime_options = HashMap::new();
        runtime_options.insert(
            "VK_DRIVER_FILES".to_string(),
            "/custom/nvidia_icd.json".to_string(),
        );

        let driver_files = add_nvidia_vulkan_icd_mounts_from(
            &mut mounts,
            vec![PathBuf::from("/host/nvidia_icd.json")],
        );
        set_default_vulkan_driver_files(&mut runtime_options, driver_files);

        assert_eq!(
            runtime_options.get("VK_DRIVER_FILES").map(String::as_str),
            Some("/custom/nvidia_icd.json")
        );
        assert!(!runtime_options.contains_key("VK_ICD_FILENAMES"));
    }

    #[test]
    fn host_vulkan_mounts_include_only_nvidia_driver_files() {
        let root = std::env::temp_dir().join(format!("aileron-mixed-icd-test-{}", Uuid::new_v4()));
        let nvidia_icd = root.join("nvidia_icd.json");
        std::fs::create_dir_all(&root).expect("create temp dir");
        std::fs::write(&nvidia_icd, "{}").expect("write nvidia icd");

        let mut mounts = Vec::new();
        let mut runtime_options = HashMap::new();
        let driver_files = add_nvidia_vulkan_icd_mounts_from(&mut mounts, vec![nvidia_icd]);
        set_default_vulkan_driver_files(&mut runtime_options, driver_files);

        assert_eq!(
            runtime_options.get("VK_DRIVER_FILES").map(String::as_str),
            Some("/usr/share/vulkan/icd.d/nvidia_icd.json")
        );
        assert_eq!(
            runtime_options.get("VK_ICD_FILENAMES").map(String::as_str),
            runtime_options.get("VK_DRIVER_FILES").map(String::as_str)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nvidia_driver_library_path_merges_with_profile_library_path() {
        let mut mounts = Vec::new();
        let mut runtime_options = HashMap::new();
        runtime_options.insert(
            "LD_LIBRARY_PATH".to_string(),
            "/runtime/lib:/usr/local/nvidia/lib64-extra".to_string(),
        );

        add_nvidia_driver_library_mounts_from(
            &mut mounts,
            &mut runtime_options,
            vec![(
                PathBuf::from("/host/libcuda.so.1"),
                "libcuda.so.1".to_string(),
            )],
        );

        assert_eq!(
            runtime_options.get("LD_LIBRARY_PATH").map(String::as_str),
            Some("/usr/local/nvidia/lib64:/runtime/lib:/usr/local/nvidia/lib64-extra")
        );
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

    #[test]
    fn image_ref_uses_tag_accepts_tagged_digest_refs() {
        assert!(image_ref_uses_tag(
            "ghcr.io/example/aileron-runtime:vulkan@sha256:abcdef",
            "vulkan"
        ));
        assert!(!image_ref_uses_tag(
            "ghcr.io/example/aileron-runtime:cpu@sha256:abcdef",
            "vulkan"
        ));
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

    #[test]
    fn vision_request_includes_nonempty_instructions_as_prompt() {
        let request = vision_request(
            "request-id".to_string(),
            "describe",
            b"image bytes",
            "focus on text labels",
            "interactive",
        );
        let value = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(value["type"], "describe");
        assert_eq!(value["prompt"], "focus on text labels");
        assert!(value.get("image").and_then(Value::as_str).is_some());
    }

    #[test]
    fn vision_request_omits_empty_instructions_prompt() {
        let request = vision_request(
            "request-id".to_string(),
            "ocr",
            b"image bytes",
            "  ",
            "interactive",
        );
        let value = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(value["type"], "ocr");
        assert!(value.get("prompt").is_none());
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
        assert_eq!(
            config["annotations"]["run.oci.keep_original_groups"].as_str(),
            Some("1"),
            "{image_ref} should preserve render-device group access"
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
        assert!(
            config["annotations"].is_null(),
            "{image_ref} should not preserve accelerator groups"
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

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    fn test_container(variant: Variant, image_ref: &str, artifact_path: &Path) -> Container {
        let child = std::process::Command::new("true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn inert test process");
        let mut child = child;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Container {
            variant,
            runtime_id: ML_RUNTIME_ID.to_string(),
            profile_epoch: 0,
            image_ref: image_ref.to_string(),
            artifact_path: artifact_path.to_path_buf(),
            runtime_options: HashMap::new(),
            child: Arc::new(Mutex::new(child)),
            stdin,
            stdout,
            last_used: std::time::Instant::now(),
        }
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
            prompt_tokens: None,
            max_tokens: None,
            context_tokens: None,
            operation: None,
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
            prompt_tokens: None,
            max_tokens: None,
            context_tokens: None,
            operation: None,
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
    fn container_response_deserializes_context_telemetry() {
        let resp: ContainerResponse = serde_json::from_str(
            r#"{
                "id":"request-1",
                "error":"context_window_exceeded",
                "reason":"prompt plus requested output exceeds context: 4200 + 512 > 4096",
                "prompt_tokens":4200,
                "max_tokens":512,
                "context_tokens":4096,
                "operation":"generate",
                "done":true
            }"#,
        )
        .expect("deserialize context telemetry response");

        assert_eq!(resp.error.as_deref(), Some("context_window_exceeded"));
        assert_eq!(resp.prompt_tokens, Some(4200));
        assert_eq!(resp.max_tokens, Some(512));
        assert_eq!(resp.context_tokens, Some(4096));
        assert_eq!(resp.operation.as_deref(), Some("generate"));
    }

    #[test]
    fn text_stream_response_forwards_multiple_transcription_tokens() {
        let input = concat!(
            r#"{"id":"other","token":"skip","done":true}"#,
            "\n",
            r#"{"id":"request-1","token":"Hello "}"#,
            "\n",
            r#"{"id":"request-1","token":"world","done":true}"#,
            "\n"
        );
        let mut reader = BufReader::new(std::io::Cursor::new(input.as_bytes()));
        let mut tokens = Vec::new();

        read_text_stream_response(&mut reader, "request-1", |token| tokens.push(token))
            .expect("streamed tokens should be read");

        assert_eq!(tokens, vec!["Hello ".to_string(), "world".to_string()]);
    }

    #[test]
    fn text_stream_response_preserves_transcribe_aggregate_result() {
        let input = concat!(
            r#"{"id":"request-1","token":"Segment one. "}"#,
            "\n",
            r#"{"id":"request-1","token":"Segment two.","done":true}"#,
            "\n"
        );
        let mut reader = BufReader::new(std::io::Cursor::new(input.as_bytes()));
        let mut transcript = String::new();

        read_text_stream_response(&mut reader, "request-1", |token| {
            transcript.push_str(&token)
        })
        .expect("streamed tokens should aggregate");

        assert_eq!(transcript, "Segment one. Segment two.");
    }

    #[test]
    fn transcribe_request_matches_existing_container_shape() {
        let mut output = Vec::new();

        write_transcribe_request(
            &mut output,
            "request-1",
            b"pcm",
            Some("en"),
            "translate",
            "interactive",
        )
        .expect("request should serialize");
        let value: Value = serde_json::from_slice(&output).expect("request should be JSON");

        assert_eq!(value["id"], "request-1");
        assert_eq!(value["type"], "transcribe");
        assert_eq!(value["audio"], "cGNt");
        assert_eq!(value["task"], "translate");
        assert_eq!(value["language_hint"], "en");
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
        let candidate = RuntimeCandidate {
            variant: Variant::Cpu,
            image_ref: image_ref.clone(),
        };

        let mut container = Container::spawn(
            ML_RUNTIME_ID,
            0,
            &candidate,
            &artifact_path,
            &HashMap::new(),
            "512m",
            &oci_store,
            &default_system_oci_store(),
            |_| {},
            || Ok(()),
        )
        .expect("spawn stub runtime");

        let mut generated = String::new();
        container
            .generate(None, "hello world", None, 16, "interactive", |token| {
                generated.push_str(&token)
            })
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

        let mut streamed_transcript = String::new();
        container
            .stream_transcribe(Vec::new(), None, "transcribe", "interactive", |token| {
                streamed_transcript.push_str(&token)
            })
            .expect("stream transcribe through container wrapper");
        assert!(!streamed_transcript.is_empty());

        let translation = container
            .transcribe(Vec::new(), None, "translate")
            .expect("translate through container wrapper");
        assert!(!translation.is_empty());

        let embedding = container
            .embed("hello world", "interactive")
            .expect("embed through container wrapper");
        assert!(!embedding.is_empty());

        let description = container
            .describe(Vec::new(), "")
            .expect("describe through container wrapper");
        assert!(!description.is_empty());

        let extracted = container
            .ocr(Vec::new(), "")
            .expect("ocr through container wrapper");
        assert!(!extracted.is_empty());

        let segments = container
            .segment(Vec::new(), "", "interactive")
            .expect("segment through container wrapper");
        assert!(!segments.is_empty());

        let _ = std::fs::remove_dir_all(&artifact_path);
    }
}
