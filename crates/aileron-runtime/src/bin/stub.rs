use std::time::Duration;

use aileron_runtime::{
    ContentPart, Request, clamp_choices, select_tool_name, send, send_unsupported,
};
use anyhow::Result;
use serde_json::json;

fn main() -> Result<()> {
    aileron_runtime::serve_requests("aileron-stub", handle_request)
}

fn handle_request(req: Request) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(&req),
        "predict_next" => handle_predict_next(&req),
        "generate_structured" => handle_generate_structured(&req),
        "generate_structured_stream" => handle_generate_structured_stream(&req),
        "embed" => send(json!({
            "id": req.id,
            "embedding": [0.0, 0.1, 0.2, 0.3],
            "done": true,
        })),
        "transcribe" => handle_transcribe(&req),
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
        "segment" => handle_segment(&req),
        _ => send_unsupported(&req, false),
    }
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
        std::thread::sleep(Duration::from_millis(20));
    }

    Ok(())
}

fn handle_predict_next(req: &Request) -> Result<()> {
    let prompt = req.prompt.as_deref().unwrap_or_default();
    let choices = clamp_choices(req.choices);
    let suffix_mode = prompt
        .chars()
        .next_back()
        .map(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-')
        .unwrap_or(false);
    let candidates = if suffix_mode {
        ["bed", "bing", "ble"]
    } else {
        [" stub", " demo", " local"]
    };

    send(json!({
        "id": req.id,
        "completions": candidates[..choices].to_vec(),
        "done": true,
    }))
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

fn handle_segment(req: &Request) -> Result<()> {
    let result = json!({
        "segments": [{
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
