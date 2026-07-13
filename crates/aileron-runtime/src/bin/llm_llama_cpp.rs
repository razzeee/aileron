use aileron_runtime::llama_runtime::{
    DEFAULT_SYSTEM, LlamaRuntimeConfig, embedding, generate_chat, generate_completion,
    initialize_llama, load_model, new_context, new_embedding_context, render_tool_results,
};
use aileron_runtime::{
    ContentPart, Request, first_json_value, is_context_window_error, send, send_unsupported,
};
use anyhow::{Result, anyhow};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::LlamaModel;
use serde_json::{Value, json};

fn main() -> Result<()> {
    let config = LlamaRuntimeConfig::from_env();
    let backend = initialize_llama("aileron-llm")?;
    let model = load_model("aileron-llm", &backend, &config, "")?;
    let mut ctx = new_context(&backend, &model, &config)?;
    let mut embed_ctx: Option<LlamaContext<'_>> = None;

    aileron_runtime::serve_requests("aileron-llm", |req| {
        handle_request(&backend, &model, &mut ctx, &mut embed_ctx, &config, req)
    })
}

fn handle_request<'model>(
    backend: &LlamaBackend,
    model: &'model LlamaModel,
    ctx: &mut LlamaContext<'_>,
    embed_ctx: &mut Option<LlamaContext<'model>>,
    config: &LlamaRuntimeConfig,
    req: Request,
) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(model, ctx, &req),
        "generate_structured" => handle_generate_structured(model, ctx, &req),
        "generate_structured_stream" => handle_generate_structured_stream(model, ctx, &req),
        "embed" => handle_embed(backend, model, embed_ctx, config, &req),
        _ => send_unsupported(&req, false),
    }
}

fn handle_generate(model: &LlamaModel, ctx: &mut LlamaContext<'_>, req: &Request) -> Result<()> {
    let system = req.system.as_deref().unwrap_or(DEFAULT_SYSTEM);
    let prompt = req.prompt.as_deref().unwrap_or_default();
    let max_tokens = req.max_tokens.unwrap_or(512);
    let temperature = req.temperature.unwrap_or(0.0);

    if input_contains_media(req) {
        return send(json!({
            "id": req.id,
            "error": "unsupported_modality",
            "reason": "input_image and input_audio are not supported by this runtime",
            "done": true,
        }));
    }

    generate_chat(
        model,
        ctx,
        system,
        prompt,
        max_tokens,
        temperature,
        None,
        req.execution_mode.as_deref(),
        |token| send(json!({"id": req.id, "token": token})),
    )?;
    send(json!({"id": req.id, "done": true}))
}

fn input_contains_media(req: &Request) -> bool {
    req.input.as_ref().is_some_and(|messages| {
        messages.iter().any(|message| {
            message.content.iter().any(|part| {
                matches!(
                    part,
                    ContentPart::InputImage { .. } | ContentPart::InputAudio { .. }
                )
            })
        })
    })
}

fn handle_generate_structured(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    match structured_result(model, ctx, req) {
        Ok(result) => send(json!({"id": req.id, "result": result, "done": true})),
        Err(err) if is_context_window_error(&err) => Err(err),
        Err(reason) => send(json!({
            "id": req.id,
            "error": "schema_validation_failed",
            "reason": reason.to_string(),
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
        Err(err) if is_context_window_error(&err) => Err(err),
        Err(reason) => send(json!({
            "id": req.id,
            "error": "schema_validation_failed",
            "reason": reason.to_string(),
            "done": true,
        })),
    }
}

fn structured_result(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<String> {
    if input_contains_media(req) {
        return Err(anyhow!(
            "input_image and input_audio are not supported by this runtime"
        ));
    }

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
        req.execution_mode.as_deref(),
        |_| Ok(()),
    )?
    .trim()
    .to_string();

    first_json_value(&result).map_err(|err| anyhow!(err))
}

fn handle_embed<'model>(
    backend: &LlamaBackend,
    model: &'model LlamaModel,
    embed_ctx: &mut Option<LlamaContext<'model>>,
    config: &LlamaRuntimeConfig,
    req: &Request,
) -> Result<()> {
    let ctx = match embed_ctx {
        Some(ctx) => ctx,
        None => embed_ctx.insert(new_embedding_context(backend, model, config)?),
    };
    let values = embedding(model, ctx, req.prompt.as_deref().unwrap_or_default())?;
    send(json!({"id": req.id, "embedding": values, "done": true}))
}
