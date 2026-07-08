use std::env;
use std::num::NonZeroU32;
use std::path::Path;

use anyhow::{Context, Result, bail};
use encoding_rs::UTF_8;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LogOptions, json_schema_to_grammar, send_logs_to_tracing};
use serde_json::Value;

use crate::{ContextWindowExceeded, default_threads_for_device, field_as_string};

pub const DEFAULT_SYSTEM: &str = "You are a helpful assistant. Always respond in the same language as the user's message. Be concise and accurate.";

#[derive(Clone, Debug)]
pub struct LlamaRuntimeConfig {
    pub model_path: String,
    pub n_ctx: u32,
    pub n_threads: i32,
    pub n_gpu_layers: i32,
    pub device: String,
}

impl LlamaRuntimeConfig {
    pub fn from_env() -> Self {
        let device = env::var("AILERON_DEVICE").unwrap_or_else(|_| "cpu".to_string());
        Self {
            model_path: env::var("MODEL_PATH").unwrap_or_else(|_| "/model/model.gguf".to_string()),
            n_ctx: env::var("N_CTX")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(4096),
            n_threads: env::var("N_THREADS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| default_threads_for_device(&device)),
            n_gpu_layers: env::var("N_GPU_LAYERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            device,
        }
    }
}

pub fn initialize_llama(log_prefix: &str) -> Result<LlamaBackend> {
    send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
    let backend = LlamaBackend::init().context("initialize llama.cpp backend")?;
    let devices = llama_cpp_2::list_llama_ggml_backend_devices();
    if devices.is_empty() {
        eprintln!("[{log_prefix}] backend devices: none reported");
    } else {
        for device in devices {
            eprintln!(
                "[{log_prefix}] backend device {}: {} ({}, {:?})",
                device.index, device.name, device.backend, device.device_type
            );
        }
    }
    Ok(backend)
}

pub fn load_model(
    log_prefix: &str,
    backend: &LlamaBackend,
    config: &LlamaRuntimeConfig,
    loading_suffix: &str,
) -> Result<LlamaModel> {
    let gpu_layers = if config.n_gpu_layers < 0 {
        1000
    } else {
        config.n_gpu_layers as u32
    };
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    let suffix = if loading_suffix.is_empty() {
        String::new()
    } else {
        format!(" {loading_suffix}")
    };
    eprintln!(
        "[{log_prefix}] loading {}{} (device={}, ctx={}, gpu_layers={}, threads={})",
        config.model_path,
        suffix,
        config.device,
        config.n_ctx,
        config.n_gpu_layers,
        config.n_threads
    );
    LlamaModel::load_from_file(backend, Path::new(&config.model_path), &model_params)
        .with_context(|| format!("load llama model from {}", config.model_path))
}

/// Build a context for autoregressive text generation.
///
/// Embeddings and pooling are left disabled here on purpose: enabling them
/// forces llama.cpp to produce pooled outputs instead of per-token logits,
/// which the sampler depends on. Use [`new_embedding_context`] for `embed`.
pub fn new_context<'model>(
    backend: &LlamaBackend,
    model: &'model LlamaModel,
    config: &LlamaRuntimeConfig,
) -> Result<LlamaContext<'model>> {
    let params = base_context_params(config)
        .with_embeddings(false)
        .with_pooling_type(LlamaPoolingType::None);

    model
        .new_context(backend, params)
        .context("create llama generation context")
}

pub fn new_embedding_context<'model>(
    backend: &LlamaBackend,
    model: &'model LlamaModel,
    config: &LlamaRuntimeConfig,
) -> Result<LlamaContext<'model>> {
    let params = base_context_params(config)
        .with_embeddings(true)
        .with_pooling_type(LlamaPoolingType::Mean);

    model
        .new_context(backend, params)
        .context("create llama embedding context")
}

fn base_context_params(config: &LlamaRuntimeConfig) -> LlamaContextParams {
    let n_ctx = NonZeroU32::new(config.n_ctx);
    LlamaContextParams::default()
        .with_n_ctx(n_ctx)
        .with_n_threads(config.n_threads)
        .with_n_threads_batch(config.n_threads)
}

pub fn render_chat_prompt(model: &LlamaModel, system: &str, prompt: &str) -> Result<String> {
    let messages = vec![
        LlamaChatMessage::new("system".to_string(), system.to_string())?,
        LlamaChatMessage::new("user".to_string(), prompt.to_string())?,
    ];

    if let Ok(template) = model.chat_template(None) {
        if let Ok(rendered) = model.apply_chat_template(&template, &messages, true) {
            return Ok(rendered);
        }
    }

    Ok(format!("System: {system}\n\nUser: {prompt}\n\nAssistant:"))
}

pub fn generate_chat(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    system: &str,
    prompt: &str,
    max_tokens: u32,
    temperature: f64,
    schema: Option<&Value>,
    execution_mode: Option<&str>,
    on_token: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let prompt = render_chat_prompt(model, system, prompt)?;
    generate_completion(
        model,
        ctx,
        &prompt,
        max_tokens,
        temperature,
        schema,
        execution_mode,
        on_token,
    )
}

pub fn generate_completion(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    prompt: &str,
    max_tokens: u32,
    temperature: f64,
    schema: Option<&Value>,
    execution_mode: Option<&str>,
    mut on_token: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    ctx.clear_kv_cache();
    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .with_context(|| "tokenize prompt")?;
    if tokens.is_empty() {
        bail!("prompt tokenized to zero tokens");
    }

    let n_ctx = ctx.n_ctx() as usize;
    if tokens.len() + max_tokens as usize > n_ctx {
        return Err(ContextWindowExceeded::generation(tokens.len(), max_tokens, n_ctx).into());
    }

    let mut batch = LlamaBatch::new(n_ctx, 1);
    let last_index = tokens.len() - 1;
    for (index, token) in tokens.into_iter().enumerate() {
        batch.add(token, index as i32, &[0], index == last_index)?;
    }
    ctx.decode(&mut batch).context("decode prompt")?;

    let mut sampler = sampler_for(model, temperature, schema);
    let mut decoder = UTF_8.new_decoder();
    let mut output = String::new();
    let mut n_cur = batch.n_tokens();

    for _ in 0..max_tokens {
        let token = sampler.sample(ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }

        let piece = model.token_to_piece(token, &mut decoder, true, None)?;
        if !piece.is_empty() {
            on_token(&piece)?;
            output.push_str(&piece);
        }

        batch.clear();
        batch.add(token, n_cur, &[0], true)?;
        n_cur += 1;
        ctx.decode(&mut batch).context("decode generated token")?;
        throttle_background_generation(execution_mode);
    }

    Ok(output)
}

pub fn generate_from_evaluated_prompt(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
    mut n_past: i32,
    max_tokens: u32,
    temperature: f64,
    schema: Option<&Value>,
    execution_mode: Option<&str>,
    mut on_token: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    if n_past <= 0 {
        bail!("evaluated prompt has no tokens");
    }
    if n_past as usize + max_tokens as usize > ctx.n_ctx() as usize {
        return Err(ContextWindowExceeded::continuation(
            n_past as usize,
            max_tokens,
            ctx.n_ctx() as usize,
        )
        .into());
    }

    let mut sampler = sampler_for(model, temperature, schema);
    let mut decoder = UTF_8.new_decoder();
    let mut output = String::new();
    let mut batch = LlamaBatch::new(1, 1);

    for _ in 0..max_tokens {
        let token = sampler.sample(ctx, -1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }

        let piece = model.token_to_piece(token, &mut decoder, true, None)?;
        if !piece.is_empty() {
            on_token(&piece)?;
            output.push_str(&piece);
        }

        batch.clear();
        batch.add(token, n_past, &[0], true)?;
        n_past += 1;
        ctx.decode(&mut batch).context("decode generated token")?;
        throttle_background_generation(execution_mode);
    }

    Ok(output)
}

fn throttle_background_generation(execution_mode: Option<&str>) {
    if execution_mode == Some("background") {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn sampler_for(model: &LlamaModel, temperature: f64, schema: Option<&Value>) -> LlamaSampler {
    let mut samplers = Vec::new();
    // llama.cpp's grammar sampler currently aborts on some valid schemas, so keep it opt-in.
    if let Some(schema) = schema.filter(|_| env::var("AILERON_LLAMA_GRAMMAR").as_deref() == Ok("1"))
    {
        match serde_json::to_string(schema)
            .ok()
            .and_then(|schema_json| json_schema_to_grammar(&schema_json).ok())
        {
            Some(grammar) => match LlamaSampler::grammar(model, &grammar, "root") {
                Ok(sampler) => samplers.push(sampler),
                Err(err) => {
                    eprintln!("[aileron-llama] failed to initialize grammar sampler: {err}")
                }
            },
            None => eprintln!("[aileron-llama] failed to convert JSON schema to grammar"),
        }
    }

    if temperature > 0.0 {
        samplers.push(LlamaSampler::top_k(40));
        samplers.push(LlamaSampler::top_p(0.95, 1));
        samplers.push(LlamaSampler::temp(temperature as f32));
        samplers.push(LlamaSampler::dist(1234));
    } else {
        samplers.push(LlamaSampler::greedy());
    }

    LlamaSampler::chain_simple(samplers)
}

pub fn embedding(model: &LlamaModel, ctx: &mut LlamaContext<'_>, text: &str) -> Result<Vec<f32>> {
    ctx.clear_kv_cache();
    let tokens = model.str_to_token(text, AddBos::Always)?;
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let n_ctx = ctx.n_ctx() as usize;
    if tokens.len() > n_ctx {
        return Err(ContextWindowExceeded::embedding(tokens.len(), n_ctx).into());
    }

    let mut batch = LlamaBatch::new(n_ctx, 1);
    let last_index = tokens.len() - 1;
    for (index, token) in tokens.into_iter().enumerate() {
        batch.add(token, index as i32, &[0], index == last_index)?;
    }
    ctx.decode(&mut batch).context("decode embedding input")?;

    // `ctx` must be an embedding context (see `new_embedding_context`): with
    // mean pooling the sentence embedding is read per-sequence, not per-token.
    Ok(ctx
        .embeddings_seq_ith(0)
        .context("read pooled sequence embedding")?
        .to_vec())
}

pub fn clean_inline_completion(prefix: &str, raw: &str) -> String {
    let starts_with_boundary = raw.chars().next().is_some_and(char::is_whitespace);
    let suffix_mode = !starts_with_boundary
        && prefix
            .chars()
            .next_back()
            .map(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-')
            .unwrap_or(false);
    let text = if suffix_mode {
        raw.trim().to_string()
    } else {
        raw.trim_start().to_string()
    };

    let mut out = String::new();
    let mut started = false;
    for ch in text.chars() {
        let is_word = ch.is_alphanumeric() || matches!(ch, '_' | '-' | '\'');
        if is_word {
            started = true;
            out.push(ch);
        } else if suffix_mode && !started {
            continue;
        } else {
            break;
        }
    }

    if !out.is_empty() && !suffix_mode && !prefix.ends_with([' ', '\n', '\t']) {
        out.insert(0, ' ');
    }

    out.chars().take(20).collect()
}

pub fn render_tool_results(prompt: &str, tool_results: &[Value]) -> String {
    if tool_results.is_empty() {
        return prompt.to_string();
    }

    let mut rendered = Vec::with_capacity(tool_results.len());
    for result in tool_results {
        let id = field_as_string(result, "id").unwrap_or_else(|| "tool".to_string());
        let content = field_as_string(result, "content_json")
            .or_else(|| field_as_string(result, "content"))
            .unwrap_or_default();
        rendered.push(format!("{id}: {content}"));
    }

    format!("{prompt}\n\nTool results:\n{}", rendered.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_completion_keeps_suffix_word_characters() {
        assert_eq!(clean_inline_completion("hel", "lo world"), "lo");
        assert_eq!(clean_inline_completion("runn", "ing"), "ing");
    }

    #[test]
    fn inline_completion_adds_space_for_mid_sentence_prefix() {
        assert_eq!(clean_inline_completion("hello,", "world!"), " world");
        assert_eq!(clean_inline_completion("hello ", "world!"), "world");
        assert_eq!(clean_inline_completion("hello", " world"), " world");
    }
}
