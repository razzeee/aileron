use aileron_runtime::llama_runtime::{
    DEFAULT_SYSTEM, LlamaRuntimeConfig, clean_inline_completion, embedding, generate_chat,
    generate_completion, generate_from_evaluated_prompt, initialize_llama, load_model, new_context,
    render_chat_prompt,
};
use aileron_runtime::{Request, clamp_choices, first_json_value, send, send_unsupported};
use anyhow::{Context, Result, bail};
use base64::Engine;
use llama_cpp_2::context::LlamaContext;
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

    aileron_runtime::serve_requests("aileron-vision", |req| {
        handle_request(&model, &mtmd, &mut ctx, req)
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

fn handle_request(
    model: &LlamaModel,
    mtmd: &MtmdContext,
    ctx: &mut LlamaContext<'_>,
    req: Request,
) -> Result<()> {
    match req.request_type.as_str() {
        "generate" => handle_generate(model, ctx, &req),
        "predict_next" => handle_predict_next(model, ctx, &req),
        "generate_structured" => handle_generate_structured(model, ctx, &req),
        "generate_structured_stream" => handle_generate_structured_stream(model, ctx, &req),
        "embed" => handle_embed(model, ctx, &req),
        "describe" => handle_describe(model, mtmd, ctx, &req),
        "ocr" => handle_ocr(model, mtmd, ctx, &req),
        "segment" => handle_segment(model, mtmd, ctx, &req),
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
    let schema = req
        .response_format
        .as_ref()
        .map(|format| &format.schema)
        .unwrap_or(&Value::Null);
    let prompt = structured_prompt(req.prompt.as_deref().unwrap_or_default(), schema);
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
    let bitmap = MtmdBitmap::from_buffer(mtmd, &image, false).context("decode image for mtmd")?;
    let media_prompt = match schema {
        Some(schema) => format!(
            "{prompt}\n\nReturn only valid JSON matching this schema:\n{}\n{}",
            schema,
            mtmd_default_marker()
        ),
        None => format!("{prompt}\n{}", mtmd_default_marker()),
    };
    let rendered = render_chat_prompt(model, DEFAULT_SYSTEM, &media_prompt)?;
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
        |_| Ok(()),
    )?)
}

fn image_bytes(req: &Request) -> std::result::Result<Vec<u8>, String> {
    let Some(value) = req.image.as_ref() else {
        return Err("image is required".to_string());
    };

    let bytes = if let Some(text) = value.as_str() {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|err| err.to_string())?
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
