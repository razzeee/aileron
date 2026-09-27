use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};

use crate::{ContentPart, Message, Request};

const DEFAULT_SYSTEM: &str = "You are a helpful assistant. Always respond in the same language as the user's message. Be concise and accurate.";

pub(super) fn maximum_tokens(req: &Request) -> u32 {
    req.max_tokens.unwrap_or(
        if matches!(req.request_type.as_str(), "generate" | "describe") {
            512
        } else {
            1024
        },
    )
}

pub(super) fn convert(req: &Request) -> Result<Value> {
    let stored_input: Option<Vec<Message>> = req
        .tool_context
        .as_ref()
        .filter(|context| !context["input"].is_null())
        .map(|context| serde_json::from_value(context["input"].clone()))
        .transpose()?;
    let input = if req.tool_context.is_some() {
        stored_input.as_deref()
    } else {
        req.input.as_deref()
    };
    let prompt = req
        .tool_context
        .as_ref()
        .map(|context| context["prompt"].as_str().unwrap_or_default())
        .unwrap_or_else(|| req.prompt.as_deref().unwrap_or_default());
    ensure!(
        matches!(
            req.request_type.as_str(),
            "generate"
                | "generate_structured"
                | "generate_structured_stream"
                | "describe"
                | "ocr"
                | "detect"
        ),
        "unsupported request type: {}",
        req.request_type
    );
    ensure!(
        req.audio.is_none(),
        "audio input is not supported by this adapter"
    );
    let structured = matches!(
        req.request_type.as_str(),
        "generate_structured" | "generate_structured_stream" | "detect"
    );
    let dedicated_vision = matches!(req.request_type.as_str(), "describe" | "ocr" | "detect");
    let max_tokens = maximum_tokens(req);
    ensure!(max_tokens > 0, "max_tokens must be positive");
    let temperature = req.temperature.unwrap_or(0.0);
    ensure!(
        temperature.is_finite() && temperature >= 0.0,
        "temperature must be finite and non-negative"
    );

    let mut messages = Vec::new();
    let has_system = input.is_some_and(|input| input.iter().any(|m| m.role == "system"));
    if dedicated_vision {
        let image = req
            .image
            .as_ref()
            .ok_or_else(|| super::request_failure("invalid_image", "image is required"))?;
        let image = super::vision::image_part(image)
            .map_err(|err| super::request_failure("invalid_image", err.to_string()))?;
        let mut message = json!({"role":"user"});
        message["content"] = Value::Array(vec![
            json!({"type":"text","text":super::vision::prompt(req)}),
            image,
        ]);
        messages.push(message);
    } else if let Some(input) = input {
        ensure!(!input.is_empty(), "input must not be empty");
        for message in input {
            ensure!(
                matches!(message.role.as_str(), "system" | "user" | "assistant"),
                "unsupported message role: {}",
                message.role
            );
            let mut parts = Vec::new();
            let mut text_parts = Vec::new();
            let has_image = message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::InputImage { .. }));
            for part in &message.content {
                match part {
                    ContentPart::InputText { text } | ContentPart::OutputText { text } => {
                        text_parts.push(text.as_str());
                        if has_image {
                            parts.push(json!({"type":"text", "text":text}));
                        }
                    }
                    ContentPart::InputImage { image, .. } => {
                        parts.push(super::vision::encoded_image_part(image).map_err(|err| {
                            super::request_failure("invalid_image", err.to_string())
                        })?);
                    }
                    _ => bail!("audio input is not supported by this adapter"),
                }
            }
            let content = if has_image {
                Value::Array(parts)
            } else {
                Value::String(text_parts.join("\n"))
            };
            let mut converted = json!({"role":message.role});
            converted["content"] = content;
            messages.push(converted);
        }
    } else {
        messages.push(json!({"role": "user", "content": prompt}));
    }
    if let Some(system) = req
        .system
        .as_deref()
        .or((!has_system).then_some(DEFAULT_SYSTEM))
        && !messages
            .iter()
            .any(|m| m["role"] == "system" && m["content"] == system)
    {
        messages.insert(0, json!({"role":"system", "content":system}));
    }
    let mut body = json!({
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_tokens": max_tokens,
        "temperature": temperature,
        "top_k": 40,
        "top_p": 0.95,
        "min_p": 0.0,
        "seed": 1234,
        "reasoning_format": "deepseek",
        "cache_prompt": false
    });
    body["messages"] = Value::Array(messages);
    body["aileron_reserve_output"] = json!(true);
    body["aileron_decode_delay_ms"] =
        json!(if req.execution_mode.as_deref() == Some("background") {
            20
        } else {
            0
        });
    if structured {
        if req.request_type == "detect" {
            body["response_format"] = json!({"type":"json_schema", "json_schema":{"name":"detections", "schema":super::vision::detection_schema()}});
            return Ok(body);
        }
        let format = req
            .response_format
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("structured generation requires response_format"))?;
        ensure!(
            format
                .format_type
                .as_deref()
                .is_none_or(|kind| kind == "json_schema"),
            "unsupported response format"
        );
        ensure!(format.schema.is_object(), "schema must be an object");
        body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "response", "schema": format.schema}});
    }
    super::tools::apply(req, &mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> Request {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn canonical_roles_override_flattened_prompt_and_duplicate_system() {
        let req = request(
            json!({"type":"generate", "system":"instructions", "prompt":"flattened",
            "input":[{"role":"system","content":[{"type":"input_text","text":"instructions"}]},
                {"role":"user","content":[{"type":"input_text","text":"question"}]},
                {"role":"assistant","content":[{"type":"output_text","text":"answer"}]}]}),
        );
        let body = convert(&req).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(body["messages"][0]["content"], "instructions");
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn translates_schema_and_preserves_explicit_limits() {
        let body = convert(&request(
            json!({"type":"generate_structured", "prompt":"extract", "temperature":0.7,
            "max_tokens":42, "response_format":{"type":"json_schema","schema":{"type":"object"}}}),
        ))
        .unwrap();
        assert_eq!(
            body["response_format"]["json_schema"]["schema"],
            json!({"type":"object"})
        );
        assert_eq!(body["max_tokens"], 42);
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["min_p"], 0.0, "native sampling has no min-p filter");
    }

    #[test]
    fn rejects_inputs_instead_of_silently_degrading() {
        for value in [
            json!({"type":"embed"}),
            json!({"type":"generate","max_tokens":0}),
            json!({"type":"generate","tools":[{"name":""}]}),
            json!({"type":"generate","input":[{"role":"user","content":[{"type":"input_image","image":"abc","mime_type":"image/png"}]}]}),
            json!({"type":"generate_structured"}),
        ] {
            assert!(convert(&request(value)).is_err());
        }
    }

    #[test]
    fn background_policy_is_server_side_and_images_use_inline_content() {
        let body = convert(&request(
            json!({"type":"describe", "execution_mode":"background",
            "image":[137,80,78,71,13,10,26,10]}),
        ))
        .unwrap();
        assert_eq!(body["aileron_decode_delay_ms"], 20);
        assert_eq!(body["aileron_reserve_output"], true);
        assert!(
            body["messages"][1]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(body["max_tokens"], 512);
        let body = convert(&request(
            json!({"type":"detect", "image":[137,80,78,71,13,10,26,10]}),
        ))
        .unwrap();
        assert!(
            body["response_format"]["json_schema"]["schema"]["properties"]["detections"]
                .is_object()
        );
    }
}
