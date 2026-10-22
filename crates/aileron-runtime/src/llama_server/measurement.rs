//! Tokenize the same rendered request as generation, without evaluating the model.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::transport::Transport;

pub(super) fn measure(transport: &Transport, body: &Value) -> Result<Value> {
    // /tokenize cannot account for image embeddings. Never return a text-only
    // count for a multimodal request and call it exact.
    if body["messages"].as_array().is_none_or(|messages| {
        messages
            .iter()
            .any(|message| !message["content"].is_string())
    }) {
        return Err(super::request_failure(
            "unsupported_request",
            "context measurement currently supports text-only messages",
        ));
    }
    let props = transport.get_json("/props")?;
    let capacity = props["default_generation_settings"]["n_ctx"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| anyhow::anyhow!("server omitted context capacity"))?;
    // The pinned server uses the same OpenAI request parser here as for chat
    // completions, including tools, response schema and template kwargs.
    let mut initial_body = body.clone();
    if body["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty())
    {
        // tools::generate removes the final-answer grammar for tool selection.
        // Its later answer pass depends on generated content, so cannot be
        // measured in advance.
        initial_body
            .as_object_mut()
            .unwrap()
            .remove("response_format");
    }
    let rendered = transport.post_json("/apply-template", &initial_body)?;
    let prompt = rendered["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("server omitted rendered prompt"))?;
    let tokenized = transport.post_json(
        "/tokenize",
        &json!({
            "content":prompt, "add_special":true, "parse_special":true
        }),
    )?;
    let tokens = tokenized["tokens"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("server omitted tokens"))?;
    ensure!(
        tokens.iter().all(|token| token.as_i64().is_some()),
        "invalid tokenization response"
    );
    Ok(
        json!({"input_tokens":tokens.len(), "context_window_tokens":capacity,
        "requested_output_tokens":body["max_tokens"], "measurement_kind":"exact"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::unix::net::UnixListener,
        thread,
    };

    #[test]
    fn counts_rendered_request_without_generation_and_preserves_options() {
        check_measurement(false);
        check_measurement(true);
    }

    fn check_measurement(with_tools: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = thread::spawn(move || {
            for route in ["/props", "/apply-template", "/tokenize"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.split_whitespace().nth(1), Some(route));
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let body: Value = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                let response = match route {
                    "/props" => json!({"default_generation_settings":{"n_ctx":4096}}),
                    "/apply-template" => {
                        assert_eq!(body["messages"][0]["content"], "instructions");
                        if with_tools {
                            assert_eq!(body["tools"][0]["function"]["name"], "lookup");
                            assert!(body.get("response_format").is_none());
                        } else {
                            assert!(body["response_format"]["json_schema"]["schema"].is_object());
                        }
                        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
                        json!({"prompt":"rendered including tools"})
                    }
                    _ => {
                        assert_eq!(body["content"], "rendered including tools");
                        assert_eq!(body["add_special"], true);
                        assert_eq!(body["parse_special"], true);
                        json!({"tokens":[1,2,3,4]})
                    }
                }
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .unwrap();
            }
        });
        let mut req: crate::Request = serde_json::from_value(json!({"type":"measure_request",
            "system":"instructions", "prompt":"question", "max_tokens":128,
            "response_format":{"schema":{"type":"object"}},
            "tools":[{"name":"lookup","description":"lookup","schema_json":"{}"}]}))
        .unwrap();
        if !with_tools {
            req.tools = None;
        }
        let mut body = super::super::request::convert(&req).unwrap();
        body["chat_template_kwargs"]["enable_thinking"] = json!(false);
        let transport = Transport::new(&path).unwrap();
        let result = measure(&transport, &body).unwrap();
        assert_eq!(result["input_tokens"], 4);
        assert_eq!(result["context_window_tokens"], 4096);
        assert_eq!(result["requested_output_tokens"], 128);
        worker.join().unwrap();
        assert!(measure(&transport, &json!({"messages":[{"content":[]}]})).is_err());
    }
}
