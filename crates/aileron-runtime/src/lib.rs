use std::io::{BufRead, Write};

use std::fmt;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

#[cfg(feature = "llama")]
pub mod llama_runtime;

#[derive(Clone, Debug, Deserialize)]
pub struct Request {
    #[serde(default = "unknown_id")]
    pub id: String,
    #[serde(default, rename = "type")]
    pub request_type: String,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub input: Option<Vec<Message>>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub execution_mode: Option<String>,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub image: Option<Value>,
    #[serde(default)]
    pub points: Option<Vec<Value>>,
    #[serde(default)]
    pub boxes: Option<Vec<Value>>,
    #[serde(default)]
    pub language_hint: Option<String>,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_results: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Vec<ContentPart>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "input_image")]
    InputImage { image: String, mime_type: String },
    #[serde(rename = "input_audio")]
    InputAudio { audio: String, mime_type: String },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResponseFormat {
    #[serde(default, rename = "type")]
    pub format_type: Option<String>,
    #[serde(default)]
    pub schema: Value,
}

fn unknown_id() -> String {
    "unknown".to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextWindowExceeded {
    pub prompt_tokens: usize,
    pub max_tokens: Option<u32>,
    pub context_tokens: usize,
    pub operation: &'static str,
}

impl ContextWindowExceeded {
    pub fn generation(prompt_tokens: usize, max_tokens: u32, context_tokens: usize) -> Self {
        Self {
            prompt_tokens,
            max_tokens: Some(max_tokens),
            context_tokens,
            operation: "generate",
        }
    }

    pub fn continuation(prompt_tokens: usize, max_tokens: u32, context_tokens: usize) -> Self {
        Self {
            prompt_tokens,
            max_tokens: Some(max_tokens),
            context_tokens,
            operation: "generate_continuation",
        }
    }

    pub fn embedding(prompt_tokens: usize, context_tokens: usize) -> Self {
        Self {
            prompt_tokens,
            max_tokens: None,
            context_tokens,
            operation: "embed",
        }
    }

    pub fn response(&self, id: &str) -> Value {
        let mut response = json!({
            "id": id,
            "error": "context_window_exceeded",
            "reason": self.to_string(),
            "prompt_tokens": self.prompt_tokens,
            "context_tokens": self.context_tokens,
            "operation": self.operation,
            "done": true,
        });
        if let Some(max_tokens) = self.max_tokens {
            response["max_tokens"] = json!(max_tokens);
        }
        response
    }
}

impl fmt::Display for ContextWindowExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max_tokens {
            Some(max_tokens) => write!(
                f,
                "prompt plus requested output exceeds context: {} + {} > {}",
                self.prompt_tokens, max_tokens, self.context_tokens
            ),
            None => write!(
                f,
                "embedding input exceeds context: {} > {}",
                self.prompt_tokens, self.context_tokens
            ),
        }
    }
}

impl std::error::Error for ContextWindowExceeded {}

pub fn is_context_window_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ContextWindowExceeded>().is_some()
}

pub fn send(value: Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub fn serve_requests(
    log_prefix: &str,
    mut handler: impl FnMut(Request) -> Result<()>,
) -> Result<()> {
    eprintln!("[{log_prefix}] ready");

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(err) => {
                eprintln!("[{log_prefix}] bad request JSON: {err}");
                continue;
            }
        };

        let req_id = req.id.clone();
        let req_type = req.request_type.clone();
        if let Err(err) = handler(req) {
            if let Some(context_error) = err.downcast_ref::<ContextWindowExceeded>() {
                eprintln!("[{log_prefix}] context window exceeded handling {req_type}: {err}");
                send(context_error.response(&req_id))?;
                continue;
            }
            eprintln!("[{log_prefix}] error handling {req_type}: {err:?}");
            send(json!({
                "id": req_id,
                "error": "internal_error",
                "reason": err.to_string(),
                "done": true,
            }))?;
        }
    }

    Ok(())
}

pub fn send_unsupported(req: &Request, done: bool) -> Result<()> {
    let mut response = json!({
        "id": req.id,
        "error": "unsupported_type",
        "reason": req.request_type,
    });
    if done {
        response["done"] = Value::Bool(true);
    }
    send(response)
}

/// Deterministic framed s16le PCM used by the contract stub.
pub fn stub_synthesis_chunks(text: &str) -> Vec<Vec<u8>> {
    const FRAMES_PER_CHUNK: usize = 480;
    let frames = (text.chars().count().max(1) * 240).max(FRAMES_PER_CHUNK + 1);
    let seed = text.bytes().fold(0_u16, |value, byte| {
        value.wrapping_mul(31).wrapping_add(byte as u16)
    });
    let pcm = (0..frames)
        .flat_map(|frame| {
            let sample = seed.wrapping_add((frame as u16).wrapping_mul(257)) as i16;
            sample.to_le_bytes()
        })
        .collect::<Vec<_>>();
    pcm.chunks(FRAMES_PER_CHUNK * size_of::<i16>())
        .map(<[u8]>::to_vec)
        .collect()
}

pub fn stub_value_for_schema(schema: &Value) -> Value {
    match schema_type(schema) {
        Some("object") => {
            let props = schema
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| props.keys().cloned().collect());
            let mut out = serde_json::Map::new();
            for key in required {
                let prop_schema = props.get(&key).unwrap_or(&Value::Null);
                out.insert(key, stub_value_for_schema(prop_schema));
            }
            Value::Object(out)
        }
        Some("array") => json!([stub_value_for_schema(
            schema.get("items").unwrap_or(&Value::Null)
        )]),
        Some("string") => schema
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or_else(|| json!("stub")),
        Some("integer") => json!(schema.get("minimum").and_then(Value::as_i64).unwrap_or(0)),
        Some("number") => json!(schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0)),
        Some("boolean") => json!(true),
        Some("null") => Value::Null,
        _ => json!("stub"),
    }
}

fn schema_type(schema: &Value) -> Option<&str> {
    let type_value = schema.get("type")?;
    if let Some(type_name) = type_value.as_str() {
        return Some(type_name);
    }
    type_value
        .as_array()
        .and_then(|items| items.iter().find_map(Value::as_str))
}

pub fn select_tool_name(tools: Option<&[Value]>, prompt: Option<&str>) -> String {
    let Some(tools) = tools else {
        return "stub_tool".to_string();
    };
    let Some(first_tool) = tools.first() else {
        return "stub_tool".to_string();
    };

    let prompt = prompt.unwrap_or_default().to_lowercase();
    tools
        .iter()
        .find_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            if prompt.contains(&name.to_lowercase()) {
                Some(name.to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            first_tool
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "stub_tool".to_string())
}

pub fn field_as_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).map(|item| {
        item.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| item.to_string())
    })
}

pub fn first_json_value(raw: &str) -> std::result::Result<String, String> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```JSON"))
        .or_else(|| raw.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(raw);

    for (index, _) in raw.match_indices(['{', '[']) {
        let candidate = &raw[index..];
        let mut values = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) => return serde_json::to_string(&value).map_err(|err| err.to_string()),
            Some(Err(_)) | None => continue,
        }
    }

    Err("model output did not contain a JSON object or array".to_string())
}

pub fn available_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4) as i32
}

pub fn default_threads_for_device(device: &str) -> i32 {
    let available = available_threads();
    if device == "cpu" {
        available
    } else {
        available.clamp(1, 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesis_request_fields_deserialize() {
        let request: Request = serde_json::from_value(json!({
            "id": "request-1",
            "type": "synthesize",
            "text": "Hello",
            "voice_id": "",
            "language_hint": "en",
            "execution_mode": "interactive"
        }))
        .unwrap();

        assert_eq!(request.text.as_deref(), Some("Hello"));
        assert_eq!(request.voice_id.as_deref(), Some(""));
    }

    #[test]
    fn stub_synthesis_is_deterministic_multichunk_framed_pcm() {
        let first = stub_synthesis_chunks("H");
        let second = stub_synthesis_chunks("H");

        assert_eq!(first, second);
        assert!(first.len() > 1);
        assert!(
            first
                .iter()
                .all(|chunk| !chunk.is_empty() && chunk.len() % 2 == 0)
        );
    }

    #[test]
    fn stub_value_satisfies_required_object_shape() {
        let schema = json!({
            "type": "object",
            "required": ["name", "count", "ok"],
            "properties": {
                "name": {"type": "string", "enum": ["demo"]},
                "count": {"type": "integer", "minimum": 3},
                "ok": {"type": "boolean"},
                "ignored": {"type": "string"}
            }
        });

        assert_eq!(
            stub_value_for_schema(&schema),
            json!({"name": "demo", "count": 3, "ok": true})
        );
    }

    #[test]
    fn select_tool_name_chooses_named_tool_from_multiple_tools() {
        let tools = vec![
            json!({"name": "weather_lookup"}),
            json!({"name": "calendar_search"}),
            json!({"name": "file_search"}),
        ];

        assert_eq!(
            select_tool_name(Some(&tools), Some("Use calendar_search for tomorrow")),
            "calendar_search"
        );
    }

    #[test]
    fn select_tool_name_falls_back_to_first_available_tool() {
        let tools = vec![
            json!({"name": "weather_lookup"}),
            json!({"name": "file_search"}),
        ];

        assert_eq!(
            select_tool_name(Some(&tools), Some("Pick the best available tool")),
            "weather_lookup"
        );
    }

    #[test]
    fn accelerator_thread_default_does_not_exceed_four() {
        assert!(default_threads_for_device("vulkan") <= 4);
        assert!(default_threads_for_device("cuda") <= 4);
        assert!(default_threads_for_device("rocm") <= 4);
        assert!(default_threads_for_device("vulkan") >= 1);
    }

    #[test]
    fn first_json_value_ignores_surrounding_text() {
        assert_eq!(
            first_json_value("Sure: {\"ok\":true} trailing text").unwrap(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn first_json_value_accepts_fenced_json() {
        assert_eq!(
            first_json_value("```json\n{\"ok\":true}\n```").unwrap(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn context_window_error_response_includes_budget_fields() {
        let response = ContextWindowExceeded::generation(4200, 512, 4096).response("req-1");

        assert_eq!(response["id"], "req-1");
        assert_eq!(response["error"], "context_window_exceeded");
        assert_eq!(response["prompt_tokens"], 4200);
        assert_eq!(response["max_tokens"], 512);
        assert_eq!(response["context_tokens"], 4096);
        assert_eq!(response["operation"], "generate");
        assert_eq!(response["done"], true);
    }
}
