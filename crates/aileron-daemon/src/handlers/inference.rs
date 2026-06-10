/// Varlink handler for `aileron.Inference`.
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects.
use aileron_varlink::aileron_Inference::{
    Call_CreateLanguageModelSession, Call_Describe, Call_EndSession,
    Call_GetLanguageModelAvailability, Call_Prewarm, Call_Respond, Call_RespondGuided,
    Call_StreamResponse, Call_Transcribe, GenerationOptions, GuidedField, ModelAvailability,
    VarlinkCallError, VarlinkInterface,
};

pub struct InferenceHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

impl InferenceHandler {
    pub fn new(state: SharedState, rt: tokio::runtime::Handle) -> Self {
        Self { state, rt }
    }
}

fn io_err(_msg: impl std::fmt::Display) -> varlink::Error {
    varlink::Error::from(varlink::ErrorKind::Io(std::io::ErrorKind::Other))
}

impl VarlinkInterface for InferenceHandler {
    fn get_language_model_availability(
        &self,
        call: &mut dyn Call_GetLanguageModelAvailability,
        _app_id: String,
        use_case: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let image_ref = {
                let guard = self.state.0.lock().await;
                match guard.assignments.get(&use_case) {
                    Some(r) => crate::hardware::resolve(r, guard.variant),
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            reason: format!("no model assigned for {use_case}"),
                        })
                    }
                }
            };

            let status = tokio::process::Command::new("podman")
                .args(["image", "exists", &image_ref])
                .status()
                .await
                .map_err(io_err)?;

            call.reply(ModelAvailability {
                is_available: status.success(),
                reason: if status.success() {
                    "available".to_string()
                } else {
                    format!("assigned image {image_ref} is not present locally")
                },
            })
        })
    }

    fn create_language_model_session(
        &self,
        call: &mut dyn Call_CreateLanguageModelSession,
        app_id: String,
        use_case: String,
        instructions: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            if !guard.config.allow_all {
                match guard.permissions.check(&app_id, &use_case) {
                    Some(true) => {}
                    Some(false) => return call.reply_permission_denied(app_id, use_case),
                    None => {
                        if guard.config.auto_grant {
                            tracing::info!(
                                "auto-granting {app_id} / {use_case} (AILERON_AUTO_GRANT)"
                            );
                            if let Err(e) =
                                guard
                                    .permissions
                                    .set(app_id.clone(), use_case.clone(), true)
                            {
                                tracing::warn!("failed to persist auto-grant: {e}");
                            }
                        } else {
                            return call.reply_permission_denied(app_id, use_case);
                        }
                    }
                }
            }

            if guard.assignments.get(&use_case).is_none() {
                return call.reply_model_unavailable(format!("no model assigned for {use_case}"));
            }

            let session_id = Uuid::new_v4().to_string();
            let session = crate::state::Session {
                session_id: session_id.clone(),
                app_id,
                use_case,
                instructions,
                started_at: chrono::Utc::now(),
            };
            guard.sessions.insert(session_id.clone(), session);
            call.reply(session_id)
        })
    }

    fn prewarm(
        &self,
        call: &mut dyn Call_Prewarm,
        session_id: String,
        _prompt_prefix: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            let use_case = match guard.sessions.get(&session_id) {
                Some(s) => s.use_case.clone(),
                None => return call.reply_session_not_found(session_id),
            };
            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => {
                    return call
                        .reply_model_unavailable(format!("no model assigned for {use_case}"))
                }
            };

            guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            call.reply()
        })
    }

    fn respond(
        &self,
        call: &mut dyn Call_Respond,
        session_id: String,
        prompt: String,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut content = String::new();
            match generate_tokens(&self.state, &mut content, session_id, prompt, options).await {
                Ok(()) => call.reply(content),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_generation_failed(reason),
            }
        })
    }

    fn stream_response(
        &self,
        call: &mut dyn Call_StreamResponse,
        session_id: String,
        prompt: String,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut tokens = Vec::new();
            match generate_tokens(&self.state, &mut tokens, session_id, prompt, options).await {
                Ok(()) => {
                    if call.wants_more() && tokens.len() > 1 {
                        call.set_continues(true);
                        for token in &tokens[..tokens.len() - 1] {
                            call.reply(token.clone())?;
                        }
                        call.set_continues(false);
                    }
                    call.reply(tokens.into_iter().last().unwrap_or_default())
                }
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_generation_failed(reason),
            }
        })
    }

    fn respond_guided(
        &self,
        call: &mut dyn Call_RespondGuided,
        session_id: String,
        prompt: String,
        fields: Vec<GuidedField>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let max_tokens = match validate_options(&options) {
                Ok(v) => v,
                Err(reason) => return call.reply_invalid_generation_options(reason),
            };
            let schema = match guided_fields_schema(&fields) {
                Ok(v) => v,
                Err(reason) => return call.reply_guided_generation_failed(reason),
            };

            let mut guard = self.state.0.lock().await;
            let (app_id, use_case, instructions) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone(), s.instructions.clone()),
                None => return call.reply_session_not_found(session_id),
            };
            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => {
                    return call
                        .reply_model_unavailable(format!("no model assigned for {use_case}"))
                }
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            match container.generate_structured(Some(&instructions), &prompt, max_tokens, &schema) {
                Ok(result) => call.reply(result),
                Err(e) => call.reply_guided_generation_failed(e.to_string()),
            }
        })
    }

    fn transcribe(
        &self,
        call: &mut dyn Call_Transcribe,
        session_id: String,
        audio: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => {
                    return call
                        .reply_model_unavailable(format!("no model assigned for {use_case}"))
                }
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let audio_bytes = base64_decode(&audio).map_err(io_err)?;
            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            match container.transcribe(audio_bytes) {
                Ok(text) => call.reply(text),
                Err(e) => call.reply_generation_failed(e.to_string()),
            }
        })
    }

    fn describe(
        &self,
        call: &mut dyn Call_Describe,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => {
                    return call
                        .reply_model_unavailable(format!("no model assigned for {use_case}"))
                }
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = base64_decode(&image).map_err(io_err)?;
            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            match container.describe(image_bytes) {
                Ok(text) => call.reply(text),
                Err(e) => call.reply_generation_failed(e.to_string()),
            }
        })
    }

    fn end_session(
        &self,
        call: &mut dyn Call_EndSession,
        session_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if guard.sessions.remove(&session_id).is_none() {
                return call.reply_session_not_found(session_id);
            }
            call.reply()
        })
    }
}

enum GenerationError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidOptions(String),
    Failed(String),
}

trait TokenSink {
    fn push_token(&mut self, token: String);
}

impl TokenSink for String {
    fn push_token(&mut self, token: String) {
        self.push_str(&token);
    }
}

impl TokenSink for Vec<String> {
    fn push_token(&mut self, token: String) {
        self.push(token);
    }
}

async fn generate_tokens(
    state: &SharedState,
    sink: &mut impl TokenSink,
    session_id: String,
    prompt: String,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let mut guard = state.0.lock().await;

    let (app_id, use_case, instructions) = match guard.sessions.get(&session_id) {
        Some(s) => (s.app_id.clone(), s.use_case.clone(), s.instructions.clone()),
        None => return Err(GenerationError::SessionNotFound(session_id)),
    };
    let image_ref = match guard.assignments.get(&use_case) {
        Some(r) => crate::hardware::resolve(r, guard.variant),
        None => {
            return Err(GenerationError::ModelUnavailable(format!(
                "no model assigned for {use_case}"
            )))
        }
    };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn(&use_case, &image_ref, |_| {})
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    container
        .generate(Some(&instructions), &prompt, max_tokens, |token| {
            sink.push_token(token)
        })
        .map_err(|e| GenerationError::Failed(e.to_string()))
}

fn validate_options(options: &GenerationOptions) -> Result<u32, String> {
    if options.maximum_response_tokens <= 0 {
        return Err("maximum_response_tokens must be greater than zero".to_string());
    }
    if options.maximum_response_tokens > u32::MAX as i64 {
        return Err("maximum_response_tokens is too large".to_string());
    }
    if !options.temperature.is_finite() || options.temperature < 0.0 {
        return Err("temperature must be a finite non-negative number".to_string());
    }
    if options.sampling_mode.trim().is_empty() {
        return Err("sampling_mode must not be empty".to_string());
    }
    Ok(options.maximum_response_tokens as u32)
}

fn guided_fields_schema(fields: &[GuidedField]) -> Result<Value, String> {
    if fields.is_empty() {
        return Err("at least one guided field is required".to_string());
    }

    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in fields {
        if field.name.trim().is_empty() {
            return Err("guided field name must not be empty".to_string());
        }
        if properties.contains_key(&field.name) {
            return Err(format!("duplicate guided field '{}'", field.name));
        }

        let mut schema = match field.kind.as_str() {
            "string" => json!({ "type": "string" }),
            "number" => json!({ "type": "number" }),
            "integer" => json!({ "type": "integer" }),
            "boolean" => json!({ "type": "boolean" }),
            "string_array" => json!({ "type": "array", "items": { "type": "string" } }),
            other => return Err(format!("unsupported guided field kind '{other}'")),
        };

        if !field.description.trim().is_empty() {
            if let Some(obj) = schema.as_object_mut() {
                obj.insert(
                    "description".to_string(),
                    Value::String(field.description.clone()),
                );
            }
        }
        if field.required {
            required.push(Value::String(field.name.clone()));
        }
        properties.insert(field.name.clone(), schema);
    }

    Ok(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    }))
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &b) in alphabet.iter().enumerate() {
        table[b as usize] = i as u8;
    }

    let clean: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;

    for b in clean {
        let v = table[b as usize];
        if v == 255 {
            return Err(format!("invalid base64 char: {}", b as char));
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
