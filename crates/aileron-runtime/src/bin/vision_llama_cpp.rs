use aileron_runtime::llama_runtime::{
    DEFAULT_SYSTEM, LlamaRuntimeConfig, embedding, generate_chat, generate_from_evaluated_prompt,
    initialize_llama, load_model, new_context, new_embedding_context, render_chat_prompt,
    render_tool_results,
};
use aileron_runtime::{
    ContentPart, Request, first_json_value, is_context_window_error, send, send_unsupported,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use serde_json::{Value, json};

const DEFAULT_VISION_PROMPT: &str = "Describe this image clearly and concisely. Include visible objects, people, text, and relevant context.";
const DEFAULT_OCR_PROMPT: &str = "Extract all text visible in this image exactly as written. Preserve the reading order and line breaks. Return only the transcribed text with no commentary. If there is no text, return an empty response.";
const DEFAULT_SEGMENT_PROMPT: &str = "Identify the main visible objects in this image. Return only JSON matching the schema. Use normalized bounding boxes where x and y are the top-left corner and width and height are relative to the image size.";

fn main() -> Result<()> {
    let config = LlamaRuntimeConfig::from_env();
    let mmproj_path =
        std::env::var("MMPROJ_PATH").unwrap_or_else(|_| "/model/mmproj.gguf".to_string());
    let backend = initialize_llama("aileron-vision")?;
    let model = load_model(
        "aileron-vision",
        &backend,
        &config,
        &format!("with {mmproj_path}"),
    )?;
    let mtmd = init_mtmd(&model, &config, &mmproj_path)?;
    let mut ctx = new_context(&backend, &model, &config)?;
    let mut embed_ctx: Option<LlamaContext<'_>> = None;

    aileron_runtime::serve_requests("aileron-vision", |req| {
        handle_request(
            &backend,
            &model,
            &mtmd,
            &mut ctx,
            &mut embed_ctx,
            &config,
            req,
        )
    })
}

fn init_mtmd(
    model: &LlamaModel,
    config: &LlamaRuntimeConfig,
    mmproj_path: &str,
) -> Result<MtmdContext> {
    let params = MtmdContextParams {
        use_gpu: config.n_gpu_layers != 0,
        print_timings: false,
        n_threads: config.n_threads,
        media_marker: std::ffi::CString::new(mtmd_default_marker())?,
        image_min_tokens: -1,
        image_max_tokens: -1,
    };
    let mtmd = MtmdContext::init_from_file(mmproj_path, model, &params)
        .with_context(|| format!("initialize mtmd from {mmproj_path}"))?;
    if !mtmd.support_vision() {
        bail!("mtmd projection does not report vision support: {mmproj_path}");
    }
    eprintln!(
        "[aileron-vision] mtmd ready (marker={})",
        mtmd_default_marker()
    );
    Ok(mtmd)
}

fn handle_request<'model>(
    backend: &LlamaBackend,
    model: &'model LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    embed_ctx: &mut Option<LlamaContext<'model>>,
    config: &LlamaRuntimeConfig,
    req: Request,
) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(model, mtmd, ctx, &req),
        "generate_structured" => handle_generate_structured(model, mtmd, ctx, &req),
        "generate_structured_stream" => handle_generate_structured_stream(model, mtmd, ctx, &req),
        "embed" => handle_embed(backend, model, embed_ctx, config, &req),
        "describe" => handle_describe(model, mtmd, ctx, &req),
        "ocr" => handle_ocr(model, mtmd, ctx, &req),
        "segment" => handle_segment(model, mtmd, ctx, &req),
        _ => send_unsupported(&req, false),
    }
}

fn handle_generate(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    let system = req.system.as_deref().unwrap_or(DEFAULT_SYSTEM);
    let prompt = req.prompt.as_deref().unwrap_or_default();
    let max_tokens = req.max_tokens.unwrap_or(512);
    let temperature = req.temperature.unwrap_or(0.0);

    if input_contains_audio(req) {
        return send(json!({
            "id": req.id,
            "error": "unsupported_modality",
            "reason": "input_audio is not supported by this runtime generate path",
            "done": true,
        }));
    }

    if input_image_count(req) > 1 {
        return send(json!({
            "id": req.id,
            "error": "unsupported_modality",
            "reason": "multiple input_image parts are not supported by this runtime generate path",
            "done": true,
        }));
    }

    if let Some(image) = first_input_image(req) {
        return match generate_for_image_bytes(
            model, mtmd, ctx, req, system, prompt, image, max_tokens, None,
        ) {
            Ok(text) => send(json!({"id": req.id, "token": text.trim(), "done": true})),
            Err(ImageRequestError::InvalidImage(reason)) => send_invalid_image(req, reason),
            Err(ImageRequestError::Runtime(err)) => Err(err),
        };
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

fn handle_generate_structured(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    match structured_result(model, mtmd, ctx, req) {
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
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    match structured_result(model, mtmd, ctx, req) {
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
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<String> {
    let system = req.system.as_deref().unwrap_or(DEFAULT_SYSTEM);
    let schema = req
        .response_format
        .as_ref()
        .map(|format| &format.schema)
        .unwrap_or(&Value::Null);
    let mut source_prompt = req.prompt.as_deref().unwrap_or_default().to_string();
    if let Some(tool_results) = req.tool_results.as_deref() {
        source_prompt = render_tool_results(&source_prompt, tool_results);
    }
    let prompt = structured_prompt(&source_prompt, schema);
    let max_tokens = req.max_tokens.unwrap_or(1024);
    if input_contains_audio(req) {
        return Err(anyhow!(
            "input_audio is not supported by this runtime structured path"
        ));
    }
    if input_image_count(req) > 1 {
        return Err(anyhow!(
            "multiple input_image parts are not supported by this runtime structured path"
        ));
    }
    if let Some(image) = first_input_image(req) {
        return generate_for_image_bytes(
            model,
            mtmd,
            ctx,
            req,
            system,
            &prompt,
            image,
            max_tokens,
            Some(schema),
        )
        .map(|result| result.trim().to_string())
        .map_err(|err| match err {
            ImageRequestError::InvalidImage(reason) => anyhow!(reason),
            ImageRequestError::Runtime(err) => err,
        })
        .and_then(|result| first_json_value(&result).map_err(|err| anyhow!(err)));
    }
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

fn handle_describe(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    let prompt = prompt_from_request_or_env(req, "VISION_PROMPT", DEFAULT_VISION_PROMPT);
    match generate_for_image(
        model,
        mtmd,
        ctx,
        req,
        &prompt,
        req.max_tokens.unwrap_or(512),
        None,
    ) {
        Ok(text) => send(json!({"id": req.id, "token": text.trim(), "done": true})),
        Err(ImageRequestError::InvalidImage(reason)) => send_invalid_image(req, reason),
        Err(ImageRequestError::Runtime(err)) => Err(err),
    }
}

fn handle_ocr(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    let prompt = prompt_from_request_or_env(req, "VISION_OCR_PROMPT", DEFAULT_OCR_PROMPT);
    match generate_for_image(
        model,
        mtmd,
        ctx,
        req,
        &prompt,
        req.max_tokens.unwrap_or(1024),
        None,
    ) {
        Ok(text) => send(json!({"id": req.id, "token": text.trim(), "done": true})),
        Err(ImageRequestError::InvalidImage(reason)) => send_invalid_image(req, reason),
        Err(ImageRequestError::Runtime(err)) => Err(err),
    }
}

fn handle_segment(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
) -> Result<()> {
    let prompt = prompt_from_request_or_env(req, "VISION_SEGMENT_PROMPT", DEFAULT_SEGMENT_PROMPT);
    let schema = segment_schema();
    match generate_for_image(
        model,
        mtmd,
        ctx,
        req,
        &prompt,
        req.max_tokens.unwrap_or(1024),
        Some(&schema),
    ) {
        Ok(text) => match first_json_value(text.trim()) {
            Ok(result) => send(json!({"id": req.id, "result": result, "done": true})),
            Err(err) => send(json!({
                "id": req.id,
                "error": "schema_validation_failed",
                "reason": err,
                "done": true,
            })),
        },
        Err(ImageRequestError::InvalidImage(reason)) => send_invalid_image(req, reason),
        Err(ImageRequestError::Runtime(err)) => Err(err),
    }
}

fn prompt_from_request_or_env(req: &Request, env_name: &str, default: &str) -> String {
    req.prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(env_name)
                .ok()
                .filter(|prompt| !prompt.is_empty())
        })
        .unwrap_or_else(|| default.to_string())
}

fn first_input_image(req: &Request) -> Option<&str> {
    req.input.as_ref()?.iter().find_map(|message| {
        message.content.iter().find_map(|part| match part {
            ContentPart::InputImage { image, .. } => Some(image.as_str()),
            _ => None,
        })
    })
}

fn input_image_count(req: &Request) -> usize {
    req.input
        .as_ref()
        .map(|messages| {
            messages
                .iter()
                .flat_map(|message| &message.content)
                .filter(|part| matches!(part, ContentPart::InputImage { .. }))
                .count()
        })
        .unwrap_or(0)
}

fn input_contains_audio(req: &Request) -> bool {
    req.input.as_ref().is_some_and(|messages| {
        messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::InputAudio { .. }))
        })
    })
}

enum ImageRequestError {
    InvalidImage(String),
    Runtime(anyhow::Error),
}

impl From<anyhow::Error> for ImageRequestError {
    fn from(err: anyhow::Error) -> Self {
        Self::Runtime(err)
    }
}

fn generate_for_image(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
    prompt: &str,
    max_tokens: u32,
    schema: Option<&Value>,
) -> std::result::Result<String, ImageRequestError> {
    let image = image_bytes(req).map_err(ImageRequestError::InvalidImage)?;
    generate_for_image_data(
        model,
        mtmd,
        ctx,
        req,
        DEFAULT_SYSTEM,
        prompt,
        &image,
        max_tokens,
        schema,
    )
}

fn generate_for_image_bytes(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
    system: &str,
    prompt: &str,
    image: &str,
    max_tokens: u32,
    schema: Option<&Value>,
) -> std::result::Result<String, ImageRequestError> {
    let image = decode_and_validate_image(image).map_err(ImageRequestError::InvalidImage)?;
    generate_for_image_data(
        model, mtmd, ctx, req, system, prompt, &image, max_tokens, schema,
    )
}

fn generate_for_image_data(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: &Request,
    system: &str,
    prompt: &str,
    image: &[u8],
    max_tokens: u32,
    schema: Option<&Value>,
) -> std::result::Result<String, ImageRequestError> {
    let bitmap = MtmdBitmap::from_buffer(mtmd, image, false).context("decode image for mtmd")?;
    let media_prompt = match schema {
        Some(schema) => format!(
            "{prompt}\n\nReturn only valid JSON matching this schema:\n{}\n{}",
            schema,
            mtmd_default_marker()
        ),
        None => format!("{prompt}\n{}", mtmd_default_marker()),
    };
    let rendered = render_chat_prompt(model, system, &media_prompt)?;
    let chunks = mtmd
        .tokenize(
            MtmdInputText {
                text: rendered,
                add_special: true,
                parse_special: true,
            },
            &[&bitmap],
        )
        .map_err(|err| anyhow::anyhow!(err))?;

    ctx.clear_kv_cache();
    let n_past = chunks
        .eval_chunks(mtmd, ctx, 0, 0, ctx.n_batch() as i32, true)
        .map_err(|err| anyhow::anyhow!(err))?;
    Ok(generate_from_evaluated_prompt(
        model,
        ctx,
        n_past,
        max_tokens,
        req.temperature.unwrap_or(0.0),
        schema,
        req.execution_mode.as_deref(),
        |_| Ok(()),
    )?)
}

fn image_bytes(req: &Request) -> std::result::Result<Vec<u8>, String> {
    let Some(value) = req.image.as_ref() else {
        return Err("image is required".to_string());
    };

    let bytes = if let Some(text) = value.as_str() {
        decode_base64_image(text)?
    } else if let Some(items) = value.as_array() {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let byte = item
                .as_u64()
                .filter(|value| *value <= u8::MAX as u64)
                .ok_or_else(|| {
                    "image byte array must contain integers from 0 to 255".to_string()
                })?;
            out.push(byte as u8);
        }
        out
    } else {
        return Err("image must be a base64 string or byte array".to_string());
    };

    validate_image_bytes(bytes)
}

fn decode_base64_image(text: &str) -> std::result::Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|err| err.to_string())
}

fn decode_and_validate_image(text: &str) -> std::result::Result<Vec<u8>, String> {
    validate_image_bytes(decode_base64_image(text)?)
}

fn validate_image_bytes(bytes: Vec<u8>) -> std::result::Result<Vec<u8>, String> {
    if bytes.starts_with(b"\xff\xd8\xff") || bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(bytes)
    } else {
        Err("image must be PNG or JPEG".to_string())
    }
}

fn send_invalid_image(req: &Request, reason: String) -> Result<()> {
    send(json!({
        "id": req.id,
        "error": "invalid_image",
        "reason": reason,
        "done": true,
    }))
}

fn structured_prompt(prompt: &str, schema: &Value) -> String {
    if schema.is_null() {
        prompt.to_string()
    } else {
        format!("{prompt}\n\nReturn only valid JSON matching this schema:\n{schema}")
    }
}

fn segment_schema() -> Value {
    json!({
        "type": "object",
        "required": ["segments"],
        "additionalProperties": false,
        "properties": {
            "segments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label", "confidence", "x", "y", "width", "height"],
                    "additionalProperties": false,
                    "properties": {
                        "label": {"type": "string"},
                        "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                        "x": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                        "y": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                        "width": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                        "height": {"type": "number", "minimum": 0.0, "maximum": 1.0}
                    }
                }
            }
        }
    })
}
