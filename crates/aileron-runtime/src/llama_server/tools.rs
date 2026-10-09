use crate::Request;

pub(super) fn generate(
    transport: &super::transport::Transport,
    req: &Request,
    body: &Value,
    mut send: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    use std::io::BufReader;
    let guided = matches!(
        req.request_type.as_str(),
        "generate_structured" | "generate_structured_stream"
    );
    if !guided || req.tools.as_ref().is_none_or(Vec::is_empty) {
        return super::response::translate(req, BufReader::new(transport.generate(body)?), send);
    }
    // A final-answer grammar can prevent some templates from emitting their
    // tool-call syntax. Select tools first, then constrain a final answer only
    // if the model chose to answer. Both phases share the caller's token cap.
    let mut selection = body.clone();
    selection.as_object_mut().unwrap().remove("response_format");
    let mut selection_request = req.clone();
    selection_request.request_type = "generate".into();
    let mut answer = String::new();
    let mut terminal = None;
    super::response::translate(
        &selection_request,
        BufReader::new(transport.generate(&selection)?),
        |event| {
            if event.get("reasoning").is_some() {
                send(event.clone())?;
            }
            if let Some(text) = event["token"].as_str() {
                ensure!(
                    answer.len() + text.len() <= 16 * 1024 * 1024,
                    "tool-selection answer exceeds size limit"
                );
                answer.push_str(text);
            }
            if event["done"] == true {
                terminal = Some(event);
            }
            Ok(())
        },
    )?;
    let terminal = terminal.ok_or_else(|| anyhow::anyhow!("tool selection did not terminate"))?;
    if terminal.get("tool_calls").is_some() {
        return send(terminal);
    }
    let used = terminal["usage"]["completion_tokens"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("tool selection omitted token usage"))?;
    let budget = u64::from(super::request::maximum_tokens(req));
    if used >= budget {
        return Err(super::request_failure(
            "schema_validation_failed",
            "output budget exhausted before a structured final answer",
        ));
    }
    let mut final_body = body.clone();
    final_body.as_object_mut().unwrap().remove("tools");
    final_body["tool_choice"] = json!("none");
    final_body["max_tokens"] = json!(budget - used);
    let messages = final_body["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant", "content":answer}));
    messages.push(json!({"role":"user", "content":"Return the final answer as JSON matching the required response schema."}));
    let mut final_request = req.clone();
    final_request.max_tokens = Some((budget - used) as u32);
    super::response::translate(
        &final_request,
        BufReader::new(transport.generate(&final_body)?),
        |mut event| {
            if event["done"] == true {
                for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
                    if let (Some(first), Some(second)) = (
                        terminal["usage"][key].as_u64(),
                        event["usage"][key].as_u64(),
                    ) {
                        event["usage"][key] = json!(
                            first
                                .checked_add(second)
                                .ok_or_else(|| anyhow::anyhow!("usage overflow"))?
                        );
                    }
                }
            }
            send(event)
        },
    )
}
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

pub(super) fn apply(req: &Request, body: &mut Value) -> Result<()> {
    if let Some(tools) = req.tools.as_ref().filter(|tools| !tools.is_empty()) {
        let mut names = HashSet::new();
        let mut converted = Vec::new();
        for tool in tools {
            let name = tool["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tool name is required"))?;
            ensure!(
                !name.is_empty() && names.insert(name),
                "empty or duplicate tool name"
            );
            let schema: Value = serde_json::from_str(tool["schema_json"].as_str().unwrap_or("{}"))?;
            ensure!(schema.is_object(), "tool argument schema must be an object");
            converted.push(json!({"type":"function", "function":{"name":name,
                "description":tool["description"].as_str().unwrap_or_default(), "parameters":schema}}));
        }
        body["tools"] = json!(converted);
        body["tool_choice"] = json!("auto");
    }
    if let Some(context) = &req.tool_context {
        let rounds = context["rounds"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid tool history"))?;
        let messages = body["messages"].as_array_mut().unwrap();
        for round in rounds {
            let calls = round["calls"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("missing assistant tool calls"))?;
            let reasoning = calls
                .iter()
                .find_map(|call| call["reasoning"].as_str())
                .map(str::to_owned);
            let calls = calls
                .iter()
                .map(|call| {
                    json!({"id":call["id"],"type":"function",
                "function":{"name":call["name"],"arguments":call["arguments_json"]}})
                })
                .collect::<Vec<_>>();
            let mut assistant = json!({"role":"assistant","content":"","tool_calls":calls});
            if let Some(reasoning) = reasoning {
                assistant["reasoning_content"] = json!(reasoning);
            }
            messages.push(assistant);
            for result in round["results"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("missing tool results"))?
            {
                let content = result["content_json"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| result["content"].as_str())
                    .unwrap_or_default();
                messages.push(json!({"role":"tool","tool_call_id":result["id"],"content":content}));
            }
            if let Some(followup) = round.get("followup").filter(|v| !v.is_null()) {
                let mut request: Request = serde_json::from_value(
                    json!({"type":"generate", "prompt":followup["prompt"], "input":followup["input"]}),
                )?;
                request.system = Some(String::new());
                let converted = super::request::convert(&request)?;
                messages.extend(
                    converted["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|message| message["role"] != "system")
                        .cloned(),
                );
            }
        }
    } else {
        ensure!(
            req.tool_results.as_ref().is_none_or(Vec::is_empty),
            "tool results require session-owned history"
        );
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct Calls(BTreeMap<u64, (String, String, String)>);

impl Calls {
    pub fn extend(&mut self, delta: &Value) -> Result<()> {
        let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) else {
            return Ok(());
        };
        for call in calls
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid tool call delta"))?
        {
            let index = call["index"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("tool call index is required"))?;
            ensure!(index < 32, "too many tool calls");
            let entry = self.0.entry(index).or_default();
            for (target, value) in [
                (&mut entry.0, &call["id"]),
                (&mut entry.1, &call["function"]["name"]),
                (&mut entry.2, &call["function"]["arguments"]),
            ] {
                if let Some(fragment) = value.as_str() {
                    ensure!(
                        target.len() + fragment.len() <= 1024 * 1024,
                        "tool call exceeds size limit"
                    );
                    target.push_str(fragment);
                } else {
                    ensure!(value.is_null(), "tool call fragments must be strings");
                }
            }
        }
        Ok(())
    }

    pub fn finish(self, req: &Request) -> Result<Value> {
        ensure!(!self.0.is_empty(), "tool finish reason without tool calls");
        let declared = req.tools.as_deref().unwrap_or_default();
        let mut ids = HashSet::new();
        let mut calls = Vec::new();
        for (_, (id, name, arguments)) in self.0 {
            ensure!(
                !id.is_empty() && ids.insert(id.clone()),
                "empty or duplicate tool call id"
            );
            ensure!(
                declared.iter().any(|tool| tool["name"] == name),
                "model requested an undeclared tool"
            );
            let value: Value = serde_json::from_str(&arguments)?;
            ensure!(value.is_object(), "tool arguments must be an object");
            // Provider IDs may be reused after a restart or in another session.
            // Allocate an opaque Aileron ID and preserve it in subsequent
            // assistant/tool messages, independently of request-ID contents.
            let public_id = format!("call-{}", uuid::Uuid::new_v4().simple());
            calls.push(json!({"id":public_id,"name":name,"arguments_json":arguments}));
        }
        Ok(json!(calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_and_final_answer_share_one_output_budget() {
        use std::{
            io::{BufRead, BufReader, Read, Write},
            os::unix::net::UnixListener,
            time::{Duration, Instant},
        };
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("server.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (content, prompt, tokens) in [("Forty-two.", 10, 7), ("{\"answer\":\"42\"}", 21, 6)]
            {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => panic!("missing request: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut input = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(input.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                input.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice::<Value>(&body).unwrap());
                let chunk = json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":prompt,"completion_tokens":tokens,"total_tokens":prompt+tokens}});
                let data = format!("data: {chunk}\n\ndata: [DONE]\n\n");
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            }
            requests
        });
        let req:Request=serde_json::from_value(json!({"id":"test","type":"generate_structured","prompt":"question","max_tokens":20,
            "tools":[{"name":"lookup","schema_json":"{}"}],"response_format":{"schema":{"type":"object"}}})).unwrap();
        let body = super::super::request::convert(&req).unwrap();
        let transport = super::super::transport::Transport::new(&socket).unwrap();
        let mut events = Vec::new();
        generate(&transport, &req, &body, |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        let requests = worker.join().unwrap();
        assert!(requests[0].get("response_format").is_none());
        assert_eq!(requests[1]["max_tokens"], 13);
        assert!(requests[1].get("tools").is_none());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["usage"]["completion_tokens"], 13);
        assert_eq!(events[0]["usage"]["total_tokens"], 44);
        assert_eq!(events[0]["result"], "{\"answer\":\"42\"}");
    }
    #[test]
    fn joins_fragmented_calls_and_rejects_unknown_tools() {
        let mut calls = Calls::default();
        calls.extend(&json!({"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{\"x\":"}}]})).unwrap();
        calls
            .extend(&json!({"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}))
            .unwrap();
        let req: Request = serde_json::from_value(json!({"tools":[{"name":"lookup"}]})).unwrap();
        assert_eq!(
            calls.finish(&req).unwrap()[0]["arguments_json"],
            "{\"x\":1}"
        );
        let mut calls = Calls::default();
        calls.extend(&json!({"tool_calls":[{"index":0,"id":"call","function":{"name":"unknown","arguments":"{}"}}]})).unwrap();
        assert!(calls.finish(&req).is_err());
    }
}
