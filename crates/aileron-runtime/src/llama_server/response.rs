use std::io::BufRead;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::stream::read_events;
use crate::Request;

const MAX_STRUCTURED_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn translate(
    req: &Request,
    input: impl BufRead,
    mut send: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let structured = matches!(
        req.request_type.as_str(),
        "generate_structured" | "generate_structured_stream" | "detect"
    );
    let mut answer = String::new();
    let mut finish = None;
    let mut usage = None;
    let mut tool_calls = super::tools::Calls::default();
    let mut tool_reasoning = String::new();
    read_events(input, |data| {
        let chunk: Value = serde_json::from_str(data).context("invalid JSON in server event")?;
        if let Some(error) = chunk.get("error") {
            return Err(super::server_error(
                error,
                Some(super::request::maximum_tokens(req)),
            ));
        }
        if let Some(value) = chunk.get("usage").filter(|v| v.is_object()) {
            usage = Some(value.clone());
        }
        let choices = chunk["choices"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("server event has no choices array"))?;
        ensure!(choices.len() <= 1, "server returned multiple choices");
        for choice in choices {
            ensure!(
                choice["index"].as_u64() == Some(0),
                "unexpected server choice index"
            );
            let delta = &choice["delta"];
            if req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
                && let Some(text) = delta["reasoning_content"].as_str()
            {
                ensure!(
                    tool_reasoning.len() + text.len() <= 1024 * 1024,
                    "tool reasoning exceeds pending-history limit"
                );
                tool_reasoning.push_str(text);
            }
            if req.include_reasoning
                && let Some(text) = delta["reasoning_content"]
                    .as_str()
                    .filter(|text| !text.is_empty())
            {
                send(json!({"id":req.id,"reasoning":text}))?;
            }
            tool_calls.extend(delta)?;
            if let Some(content) = delta.get("content").filter(|v| !v.is_null()) {
                let text = content
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("non-text server content"))?;
                ensure!(
                    finish.is_none() || text.is_empty(),
                    "content after server finish reason"
                );
                if structured {
                    ensure!(
                        answer.len() + text.len() <= MAX_STRUCTURED_BYTES,
                        "structured answer exceeds size limit"
                    );
                    answer.push_str(text);
                } else if !text.is_empty() {
                    send(json!({"id":req.id,"token":text}))?;
                }
            }
            // reasoning_content is deliberately excluded from answer text and JSON.
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                let reason = reason
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid finish reason"))?;
                ensure!(
                    matches!(reason, "stop" | "length" | "tool_calls"),
                    "unsupported finish reason: {reason}"
                );
                ensure!(finish.is_none(), "duplicate finish reason");
                finish = Some(reason.to_owned());
            }
        }
        Ok(())
    })?;
    let finish =
        finish.ok_or_else(|| anyhow::anyhow!("server completed without a finish reason"))?;
    let mut terminal = json!({"id":req.id,"done":true,"finish_reason":finish});
    if let Some(usage) = usage {
        terminal["usage"] = usage;
    }
    if finish == "tool_calls" {
        terminal["tool_calls"] = tool_calls.finish(req)?;
        if !tool_reasoning.is_empty() {
            // Internal continuation context, stripped by the daemon's public
            // ToolCall conversion. Retain once per assistant turn, not per call.
            terminal["tool_calls"][0]["reasoning"] = json!(tool_reasoning);
        }
    } else if structured {
        let _: Value = serde_json::from_str(&answer).map_err(|err| {
            super::request_failure(
                "schema_validation_failed",
                format!("server final answer is not complete JSON: {err}"),
            )
        })?;
        if req.request_type == "generate_structured_stream" {
            send(json!({"id":req.id,"snapshot":answer}))?;
            terminal["snapshot"] = json!(answer);
        } else {
            terminal["result"] = json!(answer);
        }
    }
    send(terminal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run(kind: &str, chunks: Vec<Value>, done: bool) -> (Result<()>, Vec<Value>) {
        let mut data = chunks
            .into_iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>();
        if done {
            data.push_str("data: [DONE]\n\n");
        }
        let req: Request = serde_json::from_value(json!({"id":"test","type":kind})).unwrap();
        let mut output = Vec::new();
        let result = translate(&req, Cursor::new(data), |v| {
            output.push(v);
            Ok(())
        });
        (result, output)
    }

    fn chunk(delta: Value, reason: Value) -> Value {
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    }

    #[test]
    fn separates_reasoning_and_waits_for_usage() {
        let (result, output) = run(
            "generate",
            vec![
                chunk(json!({"reasoning_content":"secret"}), Value::Null),
                chunk(json!({"content":"answer"}), json!("length")),
                json!({"choices":[],"usage":{"completion_tokens":42}}),
            ],
            true,
        );
        result.unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0], json!({"id":"test","token":"answer"}));
        assert_eq!(output[1]["usage"]["completion_tokens"], 42);
        assert_eq!(output[1]["finish_reason"], "length");
    }

    #[test]
    fn structured_result_never_extracts_json_from_reasoning() {
        let (result, output) = run(
            "generate_structured",
            vec![chunk(
                json!({"reasoning_content":"{}", "content":"not JSON"}),
                json!("stop"),
            )],
            true,
        );
        assert!(result.is_err());
        assert!(output.is_empty());
        let (result, output) = run(
            "generate_structured_stream",
            vec![chunk(json!({"content":"{\"ok\":true}"}), json!("stop"))],
            true,
        );
        result.unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["snapshot"], output[1]["snapshot"]);
        assert_eq!(output[1]["done"], true);
    }

    #[test]
    fn incomplete_stream_never_emits_success() {
        for (chunks, done) in [
            (
                vec![chunk(json!({"content":"partial"}), Value::Null)],
                false,
            ),
            (vec![], true),
            (vec![json!({"error":"failure"})], true),
        ] {
            let (result, output) = run("generate", chunks, done);
            assert!(result.is_err());
            assert!(output.iter().all(|v| v.get("done").is_none()));
        }
    }

    #[test]
    fn preserves_context_limit_error_and_token_counts() {
        let (result, output) = run(
            "generate",
            vec![json!({"error": {
                "type":"exceed_context_size_error", "message":"prompt too long",
                "n_prompt_tokens":5000, "n_ctx":4096
            }})],
            false,
        );
        assert!(output.is_empty());
        let error = result.unwrap_err();
        let context = error
            .downcast_ref::<crate::ContextWindowExceeded>()
            .expect("context errors must retain the existing runtime error type");
        assert_eq!(context.prompt_tokens, 5000);
        assert_eq!(context.context_tokens, 4096);
    }

    #[test]
    fn tool_reasoning_is_internal_continuation_context_without_opt_in() {
        let req: Request = serde_json::from_value(json!({"id":"test","type":"generate_structured",
            "tools":[{"name":"lookup"}]}))
        .unwrap();
        let chunk = chunk(
            json!({"reasoning_content":"private tool planning",
            "tool_calls":[{"index":0,"id":"call-1","function":{"name":"lookup","arguments":"{}"}}]}),
            json!("tool_calls"),
        );
        let data = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        let mut events = Vec::new();
        translate(&req, Cursor::new(data), |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].get("reasoning").is_none());
        assert_eq!(
            events[0]["tool_calls"][0]["reasoning"],
            "private tool planning"
        );
    }
}
