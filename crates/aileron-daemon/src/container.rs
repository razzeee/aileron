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
///   {"id":"<uuid>","type":"transcribe","audio":"<base64 PCM>"}
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
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

/// A running container for a single use-case.
pub struct Container {
    pub image_ref: String,
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub last_used: std::time::Instant,
}

impl Container {
    /// Spawn `podman run --rm -i <image_ref>`.
    pub fn spawn(image_ref: &str) -> Result<Self> {
        info!("spawning container for {}", image_ref);
        let mut child = std::process::Command::new("podman")
            .args(["run", "--rm", "-i", image_ref])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn podman for {}", image_ref))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

        Ok(Self {
            image_ref: image_ref.to_string(),
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
        prompt: &str,
        max_tokens: u32,
        mut on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "generate".to_string(),
            prompt: Some(prompt.to_string()),
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
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
        prompt: &str,
        max_tokens: u32,
        schema: &Value,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "generate_structured".to_string(),
            prompt: Some(prompt.to_string()),
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
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
            if let Some(result) = resp.result {
                // Validate the returned JSON against the schema.
                validate_json_schema(&result, schema)?;
                return Ok(result);
            }
            if resp.done.unwrap_or(false) {
                bail!("container sent done without a result field");
            }
        }
    }

    /// Send a transcribe request and return the full transcript.
    pub fn transcribe(&mut self, audio: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "transcribe".to_string(),
            prompt: None,
            max_tokens: None,
            audio: Some(audio),
            image: None,
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
            prompt: None,
            max_tokens: None,
            audio: None,
            image: Some(image),
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

// ── Schema validation ─────────────────────────────────────────────────────────

/// Validate `json_str` against a JSON Schema `schema`.
///
/// This is a structural validator covering the subset of JSON Schema most
/// useful for structured output: type, required, properties, items,
/// minLength/maxLength, minimum/maximum, enum.  It does not implement the full
/// JSON Schema specification.
fn validate_json_schema(json_str: &str, schema: &Value) -> Result<()> {
    let value: Value = serde_json::from_str(json_str)
        .context("model output is not valid JSON")?;
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
                        validate_value(
                            field_val,
                            prop_schema,
                            &format!("{path}.{key}"),
                        )?;
                    }
                }
            }

            // additionalProperties: false
            if schema
                .get("additionalProperties")
                .and_then(|v| v.as_bool())
                == Some(false)
            {
                if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                    for key in obj.keys() {
                        if !props.contains_key(key) {
                            bail!("{path}: unexpected additional property '{key}'");
                        }
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
            if let Some(min) = schema.get("minItems").and_then(|v| v.as_u64()) {
                if arr.len() < min as usize {
                    bail!("{path}: array length {} < minItems {min}", arr.len());
                }
            }
            if let Some(max) = schema.get("maxItems").and_then(|v| v.as_u64()) {
                if arr.len() > max as usize {
                    bail!("{path}: array length {} > maxItems {max}", arr.len());
                }
            }
        }
        Some("string") => {
            let s = value.as_str().with_context(|| {
                format!("{path}: expected string, got {}", value_type_name(value))
            })?;
            if let Some(min) = schema.get("minLength").and_then(|v| v.as_u64()) {
                if s.len() < min as usize {
                    bail!("{path}: string length {} < minLength {min}", s.len());
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(|v| v.as_u64()) {
                if s.len() > max as usize {
                    bail!("{path}: string length {} > maxLength {max}", s.len());
                }
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
            if let Some(min) = schema.get("minimum").and_then(|v| v.as_f64()) {
                if n < min {
                    bail!("{path}: {n} < minimum {min}");
                }
            }
            if let Some(max) = schema.get("maximum").and_then(|v| v.as_f64()) {
                if n > max {
                    bail!("{path}: {n} > maximum {max}");
                }
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
    if let Some(variants) = schema.get("enum").and_then(|v| v.as_array()) {
        if !variants.contains(value) {
            bail!(
                "{path}: value {:?} is not in enum {:?}",
                value,
                variants
            );
        }
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

// ── Pool ──────────────────────────────────────────────────────────────────────

/// Pool of running containers, keyed by use-case.
pub struct ContainerPool {
    containers: HashMap<String, Container>,
    /// Idle timeout in seconds (default 300 = 5 min).
    idle_timeout_secs: u64,
}

impl ContainerPool {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
            idle_timeout_secs: 300,
        }
    }

    /// Get or spawn a container for a use-case + image ref pair.
    pub fn get_or_spawn(&mut self, use_case: &str, image_ref: &str) -> Result<&mut Container> {
        if !self.containers.contains_key(use_case) {
            let c = Container::spawn(image_ref)?;
            self.containers.insert(use_case.to_string(), c);
        }
        Ok(self.containers.get_mut(use_case).unwrap())
    }

    /// Kill and remove the container for a use-case.
    pub fn kill(&mut self, use_case: &str) {
        if self.containers.remove(use_case).is_some() {
            info!("terminated container for use-case {}", use_case);
        }
    }

    /// Kill all containers.
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
            warn!("evicting idle container for use-case {}", k);
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
    prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<Vec<u8>>,
    /// Present only for `generate_structured` requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

/// Instructs the container to constrain output to a JSON Schema.
#[derive(Serialize)]
struct ResponseFormat {
    r#type: String,   // always "json_schema"
    schema: Value,
}

#[derive(Deserialize)]
struct ContainerResponse {
    id: String,
    /// Present in streaming token responses.
    token: Option<String>,
    /// Present in structured output responses (the full JSON string).
    result: Option<String>,
    done: Option<bool>,
}
