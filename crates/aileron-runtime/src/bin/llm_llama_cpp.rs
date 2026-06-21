use aileron_runtime::llama_runtime::{
    DEFAULT_SYSTEM, LlamaRuntimeConfig, clean_inline_completion, embedding, generate_chat,
    generate_completion, initialize_llama, load_model, new_context, render_tool_results,
};
use aileron_runtime::{
    Request, clamp_choices, first_json_value, first_tool_name, send, send_unsupported,
};
use anyhow::Result;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::model::LlamaModel;
use serde_json::{Value, json};

fn main() -> Result<()> {
    let config = LlamaRuntimeConfig::from_env();
    let backend = initialize_llama("aileron-llm")?;
    let model = load_model("aileron-llm", &backend, &config, "")?;
    let mut ctx = new_context(&backend, &model, &config)?;

    aileron_runtime::serve_requests("aileron-llm", |req| handle_request(&model, &mut ctx, req))
}

fn handle_request(model: &LlamaModel, ctx: &mut LlamaContext<'_>, req: Request) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(model, ctx, &req),
        "predict_next" => handle_predict_next(model, ctx, &req),
        "generate_structured" => handle_generate_structured(model, ctx, &req),
        "generate_structured_stream" => handle_generate_structured_stream(model, ctx, &req),
        "embed" => handle_embed(model, ctx, &req),
        _ => send_unsupported(&req, false),
    }
}

fn handle_generate(model: &LlamaModel, ctx: &mut LlamaContext<'_>, req: &Request) -> Result<()> {
    let system = req.system.as_deref().unwrap_or(DEFAULT_SYSTEM);
    let prompt = req.prompt.as_deref().unwrap_or_default();
    let max_tokens = req.max_tokens.unwrap_or(512);
    let temperature = req.temperature.unwrap_or(0.0);

    generate_chat(
        model,
        ctx,
        system,
        prompt,
        max_tokens,
        temperature,
        None,
        |token| send(json!({"id": req.id, "token": token})),
    )?;
    send(json!({"id": req.id, "done": true}))
}

fn handle_predict_next(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    let prefix = req.prompt.as_deref().unwrap_or_default();
    let max_tokens = req.max_tokens.unwrap_or(4);
    let choices = clamp_choices(req.choices);
    let base_temperature = req.temperature.unwrap_or(0.0);
    let temperatures = [
        base_temperature,
        base_temperature.max(0.4),
        base_temperature.max(0.8),
    ];
    let mut completions = Vec::new();

    for temperature in temperatures {
        let raw =
            generate_completion(
                model,
                ctx,
                prefix,
                max_tokens,
                temperature,
                None,
                |_| Ok(()),
            )?;
        let completion = clean_inline_completion(prefix, &raw);
        if !completion.is_empty() && !completions.contains(&completion) {
            completions.push(completion);
        }
        if completions.len() >= choices {
            break;
        }
    }

    send(json!({"id": req.id, "completions": completions, "done": true}))
}

fn handle_generate_structured(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    if req.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && req.tool_results.as_ref().is_none_or(Vec::is_empty)
    {
        return send(json!({
            "id": req.id,
            "tool_calls": [{
                "id": "stub-tool-call-1",
                "name": first_tool_name(req.tools.as_deref()),
                "arguments_json": "{}",
            }],
            "done": true,
        }));
    }

    match structured_result(model, ctx, req) {
        Ok(result) => send(json!({"id": req.id, "result": result, "done": true})),
        Err(reason) => send(json!({
            "id": req.id,
            "error": "schema_validation_failed",
            "reason": reason,
            "done": true,
        })),
    }
}

fn handle_generate_structured_stream(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    match structured_result(model, ctx, req) {
        Ok(result) => {
            send(json!({"id": req.id, "snapshot": result}))?;
            send(json!({"id": req.id, "snapshot": result, "done": true}))
        }
        Err(reason) => send(json!({
            "id": req.id,
            "error": "schema_validation_failed",
            "reason": reason,
            "done": true,
        })),
    }
}

fn structured_result(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> std::result::Result<String, String> {
    let system = req.system.as_deref().unwrap_or(DEFAULT_SYSTEM);
    let mut prompt = req.prompt.as_deref().unwrap_or_default().to_string();
    if let Some(tool_results) = req.tool_results.as_deref() {
        prompt = render_tool_results(&prompt, tool_results);
    }
    let schema = req
        .response_format
        .as_ref()
        .map(|format| &format.schema)
        .unwrap_or(&Value::Null);
    if !schema.is_null() {
        prompt = format!("{prompt}\n\nReturn only valid JSON matching this schema:\n{schema}");
    }
    let max_tokens = req.max_tokens.unwrap_or(1024);

    let result = generate_chat(
        model,
        ctx,
        system,
        &prompt,
        max_tokens,
        0.0,
        Some(schema),
        |_| Ok(()),
    )
    .map_err(|err| err.to_string())?
    .trim()
    .to_string();

    first_json_value(&result)
}

fn handle_embed(model: &LlamaModel, ctx: &mut LlamaContext<'_>, req: &Request) -> Result<()> {
    let values = embedding(model, ctx, req.prompt.as_deref().unwrap_or_default())?;
    send(json!({"id": req.id, "embedding": values, "done": true}))
}
