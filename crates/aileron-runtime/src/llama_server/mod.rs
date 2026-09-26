//! Text generation through a container-private llama-server Unix socket.
mod embeddings;
mod gguf;
mod process;
mod reasoning;
mod request;
mod response;
mod stream;
mod tools;
mod transport;
mod vision;

use crate::{ContextWindowExceeded, Request, send};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::BufRead;

#[derive(Debug)]
struct RequestFailure {
    code: &'static str,
    reason: String,
}

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for RequestFailure {}

fn request_failure(code: &'static str, reason: impl Into<String>) -> anyhow::Error {
    RequestFailure {
        code,
        reason: reason.into(),
    }
    .into()
}

fn server_error(value: &Value, max_tokens: Option<u32>) -> anyhow::Error {
    let error = value.get("error").unwrap_or(value);
    if error["type"] == "exceed_context_size_error"
        && let (Some(prompt), Some(context)) =
            (error["n_prompt_tokens"].as_u64(), error["n_ctx"].as_u64())
        && let (Ok(prompt), Ok(context)) = (usize::try_from(prompt), usize::try_from(context))
        && prompt > 0
        && context > 0
    {
        return match max_tokens {
            Some(max) => ContextWindowExceeded::generation(prompt, max, context),
            None => ContextWindowExceeded::embedding(prompt, context),
        }
        .into();
    }
    if error["type"] == "invalid_request_error" {
        return request_failure(
            "invalid_input",
            error["message"]
                .as_str()
                .unwrap_or("invalid server request"),
        );
    }
    if error["type"] == "not_supported_error" {
        return request_failure(
            "unsupported_request",
            error["message"]
                .as_str()
                .unwrap_or("unsupported server operation"),
        );
    }
    anyhow::anyhow!("llama-server error: {error}")
}

pub fn run() -> Result<()> {
    let mut server = process::Server::start()?;
    let properties = server.transport.get_json("/props")?;
    let capabilities = reasoning::Capabilities::from_props(&properties, &server.model_name);
    let template = properties["chat_template"].as_str().unwrap_or_default();
    let legacy_llama3 = template.contains("<|start_header_id|>")
        && template.contains("<|eot_id|>")
        && capabilities.thinking_modes.is_empty();
    let mut provenance: Value = serde_json::from_slice(&std::fs::read(
        "/usr/share/aileron/runtime-provenance.json",
    )?)?;
    provenance["reasoning"] = capabilities.json();
    eprintln!("[aileron-runtime-metadata] {provenance}");
    eprintln!("[llama-server-adapter] ready");
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut req: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(error) => {
                send(
                    json!({"id":"unknown","error":"invalid_request","reason":error.to_string(),"done":true}),
                )?;
                continue;
            }
        };
        drop(line);
        if req.request_type == "capabilities" {
            send(json!({"id":req.id,"capabilities":capabilities.json(),"done":true}))?;
            continue;
        }
        if vision::has_image(&req) && !server.has_vision {
            send(
                json!({"id":req.id,"error":"unsupported_modality","reason":"model has no vision projector","done":true}),
            )?;
            continue;
        }
        if req.request_type == "embed" {
            match embeddings::embed(&server.transport, &req) {
                Ok(value) => send(value)?,
                Err(error) => {
                    if let Some(context) = error.downcast_ref::<ContextWindowExceeded>() {
                        send(context.response(&req.id))?;
                    } else if let Some(failure) = error.downcast_ref::<RequestFailure>() {
                        send(
                            json!({"id":req.id,"error":failure.code,"reason":failure.reason,"done":true}),
                        )?;
                    } else {
                        send(
                            json!({"id":req.id,"error":"inference_failed","reason":format!("{error:#}"),"done":true}),
                        )?;
                        return Err(error);
                    }
                }
            }
            continue;
        }
        let mut body = match request::convert(&req) {
            Ok(body) => body,
            Err(error) => {
                let code = error
                    .downcast_ref::<RequestFailure>()
                    .map_or("unsupported_request", |failure| failure.code);
                send(json!({"id":req.id,"error":code,"reason":error.to_string(),"done":true}))?;
                continue;
            }
        };
        if let Err(error) = capabilities.apply(&req, &mut body) {
            send(
                json!({"id":req.id,"error":"invalid_input","reason":error.to_string(),"done":true}),
            )?;
            continue;
        }
        if legacy_llama3
            && req.request_type == "generate"
            && !vision::has_image(&req)
            && req.tool_context.is_none()
            && req.tools.as_ref().is_none_or(Vec::is_empty)
            && req.thinking.is_none()
            && req.reasoning_effort.is_none()
            && !req.include_reasoning
            && req
                .input
                .as_ref()
                .is_none_or(|messages| messages.len() == 1 && messages[0].role == "user")
        {
            body["aileron_legacy_chat"] = json!(true);
        }
        // The rendered HTTP body owns the input now. Keep only response context
        // while decoding, especially for large base64 images and tool history.
        req.input = None;
        req.image = None;
        req.tool_context = None;
        req.prompt = None;
        req.system = None;
        let mut write_failed = false;
        let result = server.ensure_running().and_then(|_| {
            tools::generate(&server.transport, &req, &body, |value| {
                let result = send(value);
                if result.is_err() {
                    write_failed = true;
                }
                result
            })
        });
        if write_failed {
            return result;
        }
        if let Err(error) = result {
            if let Some(failure) = error.downcast_ref::<RequestFailure>() {
                send(
                    json!({"id":req.id,"error":failure.code,"reason":failure.reason,"done":true}),
                )?;
                continue;
            }
            if let Some(context) = error.downcast_ref::<ContextWindowExceeded>() {
                // A rejected prompt leaves the server usable. Preserve the
                // daemon's existing context telemetry and allow a shorter retry.
                send(context.response(&req.id))?;
                continue;
            }
            send(
                json!({"id":req.id,"error":"inference_failed","reason":format!("{error:#}"),"done":true}),
            )?;
            // Broken transport state must not be reused or partially replayed.
            return Err(error);
        }
    }
    Ok(())
}
