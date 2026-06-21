use aileron_runtime::{Request, available_threads, send, send_unsupported};
use anyhow::{Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn main() -> Result<()> {
    let model_path = std::env::var("MODEL_PATH").unwrap_or_else(|_| "/model/model.bin".to_string());
    let n_threads = std::env::var("N_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(available_threads);
    let device = std::env::var("AILERON_DEVICE").unwrap_or_else(|_| "cpu".to_string());
    let use_gpu = device != "cpu";
    eprintln!(
        "[aileron-asr] loading whisper model: {model_path} (device={device}, use_gpu={use_gpu}, threads={n_threads})"
    );

    let mut context_params = WhisperContextParameters::new();
    context_params.use_gpu(use_gpu);
    let context = WhisperContext::new_with_params(&model_path, context_params)?;
    let mut state = context.create_state()?;

    aileron_runtime::serve_requests("aileron-asr", |req| {
        handle_request(&mut state, n_threads, req)
    })
}

fn handle_request(
    state: &mut whisper_rs::WhisperState,
    n_threads: i32,
    req: Request,
) -> Result<()> {
    match req.request_type.as_str() {
        "transcribe" => handle_transcribe(state, n_threads, &req),
        _ => send_unsupported(&req, false),
    }
}

fn handle_transcribe(
    state: &mut whisper_rs::WhisperState,
    n_threads: i32,
    req: &Request,
) -> Result<()> {
    let audio_b64 = req.audio.as_deref().unwrap_or_default();
    let raw_pcm = match STANDARD.decode(audio_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            return send(json!({
                "id": req.id,
                "error": "invalid_audio",
                "reason": err.to_string(),
            }));
        }
    };
    let audio = match decode_pcm_f32le(&raw_pcm) {
        Ok(audio) => audio,
        Err(err) => {
            return send(json!({
                "id": req.id,
                "error": "invalid_audio",
                "reason": err.to_string(),
            }));
        }
    };

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_timestamps(true);
    params.set_translate(req.task.as_deref() == Some("translate"));
    match req.language_hint.as_deref().filter(|hint| !hint.is_empty()) {
        Some(language) => params.set_language(Some(language)),
        None => params.set_language(None),
    }

    state.full(params, &audio)?;
    for segment in state.as_iter() {
        let text = segment.to_str_lossy()?.to_string();
        if !text.is_empty() {
            send(json!({"id": req.id, "token": text}))?;
        }
    }
    send(json!({"id": req.id, "done": true}))
}

fn decode_pcm_f32le(raw: &[u8]) -> Result<Vec<f32>> {
    if raw.len() % 4 != 0 {
        bail!("audio byte length is not a multiple of f32 sample size");
    }

    Ok(raw
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_f32le_pcm() {
        let bytes = [0.0f32.to_le_bytes(), 0.5f32.to_le_bytes()].concat();
        assert_eq!(decode_pcm_f32le(&bytes).unwrap(), vec![0.0, 0.5]);
    }

    #[test]
    fn rejects_partial_samples() {
        assert!(decode_pcm_f32le(&[0, 1, 2]).is_err());
    }
}
