use aileron_runtime::{
    ContentPart, Request, select_tool_name, send, send_unsupported, stub_synthesis_chunks,
};
use anyhow::Result;
use base64::Engine;
use serde_json::json;

fn main() -> Result<()> {
    aileron_runtime::serve_requests("aileron-stub", handle_request)
}

fn handle_request(req: Request) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(&req),
        "generate_structured" => handle_generate_structured(&req),
        "generate_structured_stream" => handle_generate_structured_stream(&req),
        "embed" => send(json!({
            "id": req.id,
            "embedding": [0.0, 0.1, 0.2, 0.3],
            "done": true,
        })),
        "transcribe" => handle_transcribe(&req),
        "synthesize" => handle_synthesize(&req),
        "describe" => send(json!({
            "id": req.id,
            "token": "Stub description: an image was received.",
            "done": true,
        })),
        "ocr" => send(json!({
            "id": req.id,
            "token": "Stub OCR: extracted text from image.",
            "done": true,
        })),
        "detect" => handle_detect(&req),
        "segment" => handle_segment(&req),
        "depth" => handle_depth(&req),
        _ => send_unsupported(&req, false),
    }
}

fn handle_synthesize(req: &Request) -> Result<()> {
    for response in synthesis_responses(req)? {
        send(response)?;
    }
    Ok(())
}

fn synthesis_responses(req: &Request) -> Result<Vec<serde_json::Value>> {
    let text = req
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("synthesis text must not be empty"))?;
    if !matches!(
        req.execution_mode.as_deref(),
        None | Some("") | Some("interactive") | Some("background")
    ) {
        anyhow::bail!("unsupported execution mode");
    }

    let mut responses = Vec::new();
    for (index, pcm) in stub_synthesis_chunks(text).into_iter().enumerate() {
        let mut response = json!({
            "id": req.id,
            "audio": base64::engine::general_purpose::STANDARD.encode(pcm),
        });
        if index == 0 {
            response["sample_rate"] = 24_000.into();
            response["channels"] = 1.into();
            response["sample_format"] = "s16le".into();
        }
        responses.push(response);
    }
    responses.push(json!({"id": req.id, "audio": "", "done": true}));
    Ok(responses)
}

fn handle_generate(req: &Request) -> Result<()> {
    let rendered_input = req.input.as_ref().map(|messages| {
        messages
            .iter()
            .map(|message| {
                let content = message
                    .content
                    .iter()
                    .map(|part| match part {
                        ContentPart::InputText { text } | ContentPart::OutputText { text } => {
                            text.as_str()
                        }
                        ContentPart::InputImage { .. } => "[image]",
                        ContentPart::InputAudio { .. } => "[audio]",
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{}: {content}", message.role)
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    let prompt = rendered_input
        .as_deref()
        .or(req.prompt.as_deref())
        .unwrap_or("(empty prompt)");
    let words = prompt.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return send(json!({
            "id": req.id,
            "token": "(stub: no prompt provided)",
            "done": true,
        }));
    }

    for (index, word) in words.iter().take(32).enumerate() {
        let is_last = index == words.len() - 1 || index == 31;
        let token = if is_last {
            (*word).to_string()
        } else {
            format!("{word} ")
        };
        let mut chunk = json!({"id": req.id, "token": token});
        if is_last {
            chunk["done"] = true.into();
        }
        send(chunk)?;
    }

    Ok(())
}

fn handle_generate_structured(req: &Request) -> Result<()> {
    if req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && req.tool_results.as_ref().is_none_or(Vec::is_empty)
    {
        return send(json!({
            "id": req.id,
            "tool_calls": [{
                "id": "stub-tool-call-1",
                "name": select_tool_name(req.tools.as_deref(), req.prompt.as_deref()),
                "arguments_json": "{}",
            }],
            "done": true,
        }));
    }

    let schema = req
        .response_format
        .as_ref()
        .map(|format| &format.schema)
        .unwrap_or(&serde_json::Value::Null);
    let result = aileron_runtime::stub_value_for_schema(schema);
    send(json!({
        "id": req.id,
        "result": serde_json::to_string(&result)?,
        "done": true,
    }))
}

fn handle_generate_structured_stream(req: &Request) -> Result<()> {
    if req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && req.tool_results.as_ref().is_none_or(Vec::is_empty)
    {
        return send(json!({
            "id": req.id,
            "tool_calls": [{
                "id": "stub-tool-call-1",
                "name": select_tool_name(req.tools.as_deref(), req.prompt.as_deref()),
                "arguments_json": "{}",
            }],
            "done": true,
        }));
    }

    let schema = req
        .response_format
        .as_ref()
        .map(|format| &format.schema)
        .unwrap_or(&serde_json::Value::Null);
    let result = serde_json::to_string(&aileron_runtime::stub_value_for_schema(schema))?;
    send(json!({"id": req.id, "snapshot": result}))?;
    send(json!({"id": req.id, "snapshot": result, "done": true}))
}

fn handle_transcribe(req: &Request) -> Result<()> {
    let task = req.task.as_deref().unwrap_or("transcribe");
    let verb = if task == "translate" {
        "translation"
    } else {
        "transcription"
    };
    let suffix = req
        .language_hint
        .as_deref()
        .filter(|hint| !hint.is_empty())
        .map(|hint| format!(" Language hint: {hint}."))
        .unwrap_or_default();

    send(json!({
        "id": req.id,
        "token": format!("Stub {verb}: "),
    }))?;
    send(json!({
        "id": req.id,
        "token": format!("audio received.{suffix}"),
        "done": true,
    }))
}

fn handle_detect(req: &Request) -> Result<()> {
    let result = json!({
        "detections": [{
            "label": "stub object",
            "confidence": 1.0,
            "x": 0.1,
            "y": 0.1,
            "width": 0.8,
            "height": 0.8,
        }]
    });

    send(json!({
        "id": req.id,
        "result": serde_json::to_string(&result)?,
        "done": true,
    }))
}

fn handle_segment(req: &Request) -> Result<()> {
    let label = if req.points.as_ref().is_some_and(|points| !points.is_empty()) {
        "stub prompted object"
    } else if req.boxes.as_ref().is_some_and(|boxes| !boxes.is_empty()) {
        "stub boxed object"
    } else {
        "stub mask"
    };
    let result = json!({
        "masks": [{
            "label": label,
            "confidence": 1.0,
            "x": 0.2,
            "y": 0.2,
            "width": 0.6,
            "height": 0.6,
            "mask_base64": "/w==",
            "mask_width": 1,
            "mask_height": 1,
        }]
    });

    send(json!({
        "id": req.id,
        "result": serde_json::to_string(&result)?,
        "done": true,
    }))
}

fn handle_depth(req: &Request) -> Result<()> {
    let result = json!({
        "depth": {
            "width": 4,
            "height": 4,
            "values": [
                0.0, 0.1, 0.2, 0.3,
                0.1, 0.2, 0.3, 0.4,
                0.2, 0.3, 0.4, 0.5,
                0.3, 0.4, 0.5, 1.0
            ],
            "minimum": 0.0,
            "maximum": 1.0
        }
    });

    send(json!({
        "id": req.id,
        "result": serde_json::to_string(&result)?,
        "done": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesis_responses_preserve_id_emit_multiple_chunks_and_terminate() {
        let request: Request = serde_json::from_value(json!({
            "id": "request-1",
            "type": "synthesize",
            "text": "Hello there",
            "voice_id": "",
            "language_hint": "en",
            "execution_mode": "interactive"
        }))
        .unwrap();

        let responses = synthesis_responses(&request).unwrap();

        assert!(responses.len() > 2);
        assert!(
            responses
                .iter()
                .all(|response| response["id"] == "request-1")
        );
        assert_eq!(responses[0]["sample_rate"], 24_000);
        assert_eq!(responses[0]["channels"], 1);
        assert_eq!(responses[0]["sample_format"], "s16le");
        assert_eq!(responses.last().unwrap()["audio"], "");
        assert_eq!(responses.last().unwrap()["done"], true);
    }

    #[test]
    fn synthesis_responses_reject_empty_text_and_unknown_execution_mode() {
        let mut request: Request = serde_json::from_value(json!({
            "id": "request-1",
            "type": "synthesize",
            "text": "  "
        }))
        .unwrap();
        assert!(synthesis_responses(&request).is_err());

        request.text = Some("hello".to_string());
        request.execution_mode = Some("urgent".to_string());
        assert!(synthesis_responses(&request).is_err());
    }
}
