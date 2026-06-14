/// Container lifecycle management.
///
/// One `podman` process is maintained per use-case. The process receives
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
/// ### Audio transcription
/// Request:
///   {"id":"<uuid>","type":"transcribe","audio":"<base64 PCM>","language_hint":"en"}
/// Response (streamed tokens, same as generate):
///   {"id":"<uuid>","token":"Hello world","done":true}
///
/// ### Image description
/// Request:
///   {"id":"<uuid>","type":"describe","image":"<base64 PNG/JPEG>"}
/// Response (same as generate):
///   {"id":"<uuid>","token":"A cat sitting...","done":true}
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

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

#[derive(Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl Container {
    /// Spawn a hardened `podman run` for the given image and block until the
    /// entrypoint signals it is ready by writing `ready` to stderr.
    ///
    /// The `on_status` callback is called with human-readable progress lines
    /// from the container's stderr while we wait (e.g. model loading messages).
    /// It may be called from a background thread.
    ///
    /// Isolation flags applied:
    ///   --network=none       no network access whatsoever
    ///   --read-only          root filesystem is read-only
    ///   --tmpfs /tmp         writable scratch space (in-memory only)
    ///   --no-new-privileges  prevents setuid/setcap escalation
    ///   --cap-drop=all       drops every Linux capability
    ///   --security-opt=no-new-privileges  belt-and-suspenders with the kernel
    ///   --pids-limit=256     limits fork bombs
    ///   --memory=<limit>     caps RAM usage
    pub fn spawn(
        image_ref: &str,
        artifact_path: &Path,
        runtime_options: &HashMap<String, String>,
        memory_limit: &str,
        mut on_status: impl FnMut(String) + Send + 'static,
    ) -> Result<Self> {
        info!("spawning container for {}", image_ref);
        let args = podman_run_args(image_ref, artifact_path, runtime_options, memory_limit);
        let mut child = std::process::Command::new("podman")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn podman for {}", image_ref))?;

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
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "generate".to_string(),
            system: system.map(str::to_string),
            prompt: Some(prompt.to_string()),
            messages: None,
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
            language_hint: None,
            response_format: None,
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
                let reason = resp.reason.unwrap_or(error);
                bail!("container returned error: {reason}");
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

    /// Send a chat request and collect streamed token responses.
    /// `on_token` is called once per token as it arrives.
    pub fn chat(
        &mut self,
        system: Option<&str>,
        messages: &[ChatMessage],
        max_tokens: u32,
        mut on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "chat".to_string(),
            system: system.map(str::to_string),
            prompt: None,
            messages: Some(messages.to_vec()),
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
            language_hint: None,
            response_format: None,
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
                let reason = resp.reason.unwrap_or(error);
                bail!("container returned error: {reason}");
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
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "generate_structured".to_string(),
            system: system.map(str::to_string),
            prompt: Some(prompt.to_string()),
            messages: None,
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
            language_hint: None,
            response_format: Some(ResponseFormat {
                r#type: "json_schema".to_string(),
                schema: schema.clone(),
            }),
        };
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

    /// Send a transcribe request and return the full transcript.
    pub fn transcribe(&mut self, audio: Vec<u8>, language_hint: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "transcribe".to_string(),
            system: None,
            prompt: None,
            messages: None,
            max_tokens: None,
            audio: Some(base64_encode(&audio)),
            image: None,
            language_hint: language_hint
                .filter(|hint| !hint.is_empty())
                .map(str::to_string),
            response_format: None,
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    /// Send a vision describe request and return the full description.
    pub fn describe(&mut self, image: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "describe".to_string(),
            system: None,
            prompt: None,
            messages: None,
            max_tokens: None,
            audio: None,
            image: Some(base64_encode(&image)),
            language_hint: None,
            response_format: None,
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
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
                let reason = resp.reason.unwrap_or(error);
                bail!("container returned error: {reason}");
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
        let reason = resp.reason.unwrap_or(error);
        bail!("container returned error: {reason}");
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

fn podman_run_args(
    image_ref: &str,
    artifact_path: &Path,
    runtime_options: &HashMap<String, String>,
    memory_limit: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        "--network=none".to_string(),
        "--read-only".to_string(),
        "--tmpfs=/tmp:rw,noexec,nosuid,size=256m".to_string(),
        "--cap-drop=all".to_string(),
        "--security-opt=no-new-privileges".to_string(),
        "--pids-limit=256".to_string(),
        format!("--memory={memory_limit}"),
        format!("--volume={}:/model:ro,z", artifact_path.display()),
    ];

    if image_ref_uses_tag(image_ref, "cuda") {
        args.push("--device=nvidia.com/gpu=all".to_string());
    } else if image_ref_uses_tag(image_ref, "rocm") {
        args.extend([
            "--device=/dev/kfd".to_string(),
            "--device=/dev/dri".to_string(),
            "--group-add=keep-groups".to_string(),
            "--ipc=host".to_string(),
            "--env=HSA_OVERRIDE_GFX_VERSION=10.3.0".to_string(),
        ]);
    } else if image_ref_uses_tag(image_ref, "vulkan") {
        args.extend([
            "--device=/dev/dri".to_string(),
            "--group-add=keep-groups".to_string(),
        ]);
    }

    let mut runtime_options: Vec<_> = runtime_options.iter().collect();
    runtime_options.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in runtime_options {
        args.push(format!("--env={key}={value}"));
    }

    args.push(image_ref.to_string());
    args
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
    /// Podman memory limit applied to each model container.
    pub memory_limit: String,
}

impl ContainerPool {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
            idle_timeout_secs: 300,
            memory_limit: "8g".to_string(),
        }
    }

    /// Get or spawn a container for a profile + runtime image + artifact path.
    /// `on_status` receives human-readable loading messages while the container
    /// starts up (only called on a cold start, not for warm containers).
    pub fn get_or_spawn(
        &mut self,
        profile_id: &str,
        image_ref: &str,
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
                artifact_path,
                runtime_options,
                &self.memory_limit,
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
    messages: Option<Vec<ChatMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_hint: Option<String>,
    /// Present only for `generate_structured` requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
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
    /// Present in structured output responses (the full JSON string).
    result: Option<String>,
    error: Option<String>,
    reason: Option<String>,
    done: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podman_args_expose_rocm_devices_for_rocm_tag() {
        let args = podman_run_args(
            "localhost/aileron/summarize:rocm",
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        );

        assert!(args.contains(&"--device=/dev/kfd".to_string()));
        assert!(args.contains(&"--device=/dev/dri".to_string()));
        assert!(args.contains(&"--group-add=keep-groups".to_string()));
        assert!(args.contains(&"--ipc=host".to_string()));
        assert!(args.contains(&"--env=HSA_OVERRIDE_GFX_VERSION=10.3.0".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("localhost/aileron/summarize:rocm")
        );
    }

    #[test]
    fn podman_args_expose_cuda_device_for_cuda_tag() {
        let args = podman_run_args(
            "localhost/aileron/summarize:cuda",
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        );

        assert!(args.contains(&"--device=nvidia.com/gpu=all".to_string()));
        assert!(!args.contains(&"--device=/dev/kfd".to_string()));
        assert!(!args.contains(&"--device=/dev/dri".to_string()));
        assert!(!args.contains(&"--ipc=host".to_string()));
        assert!(!args.contains(&"--env=HSA_OVERRIDE_GFX_VERSION=10.3.0".to_string()));
        assert!(!args.contains(&"--group-add=keep-groups".to_string()));
    }

    #[test]
    fn podman_args_expose_vulkan_device_for_vulkan_tag() {
        let args = podman_run_args(
            "localhost/aileron/summarize:vulkan",
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        );

        assert!(args.contains(&"--device=/dev/dri".to_string()));
        assert!(args.contains(&"--group-add=keep-groups".to_string()));
        assert!(!args.contains(&"--device=/dev/kfd".to_string()));
        assert!(!args.contains(&"--device=nvidia.com/gpu=all".to_string()));
        assert!(!args.contains(&"--ipc=host".to_string()));
        assert!(!args.contains(&"--env=HSA_OVERRIDE_GFX_VERSION=10.3.0".to_string()));
    }

    #[test]
    fn podman_args_do_not_expose_gpu_devices_for_cpu_tag() {
        let args = podman_run_args(
            "localhost/aileron/summarize:cpu",
            Path::new("/models/foo"),
            &HashMap::new(),
            "8g",
        );

        assert!(!args.contains(&"--device=/dev/kfd".to_string()));
        assert!(!args.contains(&"--device=/dev/dri".to_string()));
        assert!(!args.contains(&"--device=nvidia.com/gpu=all".to_string()));
        assert!(!args.contains(&"--ipc=host".to_string()));
        assert!(!args.contains(&"--env=HSA_OVERRIDE_GFX_VERSION=10.3.0".to_string()));
        assert!(!args.contains(&"--group-add=keep-groups".to_string()));
    }

    #[test]
    fn podman_args_include_runtime_options_as_env() {
        let mut runtime_options = HashMap::new();
        runtime_options.insert("VISION_HANDLER".to_string(), "gemma4".to_string());

        let args = podman_run_args(
            "localhost/aileron/vision:cpu",
            Path::new("/models/foo"),
            &runtime_options,
            "8g",
        );

        assert!(args.contains(&"--env=VISION_HANDLER=gemma4".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("localhost/aileron/vision:cpu")
        );
    }

    #[test]
    fn structured_response_error_returns_immediately() {
        let resp = ContainerResponse {
            id: "request-1".to_string(),
            token: None,
            result: None,
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
            result: Some(r#"{"name":"Ada"}"#.to_string()),
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
    #[ignore = "requires a prebuilt stub runtime image and podman"]
    fn stub_runtime_roundtrip_through_container_wrapper() {
        let image_ref = std::env::var("AILERON_STUB_IMAGE")
            .unwrap_or_else(|_| "localhost/aileron/stub:ci".to_string());
        let artifact_path =
            std::env::temp_dir().join(format!("aileron-stub-artifacts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&artifact_path).expect("create temporary artifact directory");

        let mut container =
            Container::spawn(&image_ref, &artifact_path, &HashMap::new(), "512m", |_| {})
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
            .transcribe(Vec::new(), None)
            .expect("transcribe through container wrapper");
        assert!(!transcript.is_empty());

        let description = container
            .describe(Vec::new())
            .expect("describe through container wrapper");
        assert!(!description.is_empty());

        let _ = std::fs::remove_dir_all(&artifact_path);
    }
}
