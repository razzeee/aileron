/// Varlink handler for `aileron.Inference`.
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects.
use aileron_varlink::aileron_Inference::{
    Call_Chat, Call_CreateSession, Call_Describe, Call_EndSession, Call_GetUseCaseAvailability,
    Call_Prewarm, Call_Respond, Call_RespondGuided, Call_Segment, Call_StreamChat,
    Call_StreamResponse, Call_Transcribe, ChatMessage, GenerationOptions, GuidedField,
    ModelAvailability, VarlinkCallError, VarlinkInterface, VisionSegment,
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
    fn get_use_case_availability(
        &self,
        call: &mut dyn Call_GetUseCaseAvailability,
        _app_id: String,
        use_case: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let (image_ref, artifact_path, oci_store) = {
                let guard = self.state.0.lock().await;
                let profile_id = match guard.assignments.get(&use_case) {
                    Some(profile_id) => profile_id,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            reason: format!("no profile assigned for {use_case}"),
                        });
                    }
                };
                let profile = match guard.profiles.get(profile_id) {
                    Some(profile) => profile,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            reason: format!("assigned profile {profile_id} is not installed"),
                        });
                    }
                };
                let image_ref = match resolve_runtime_image(&guard, profile) {
                    Some(image_ref) => image_ref.to_string(),
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            reason: format!(
                                "runtime {} does not support {}",
                                profile.runtime_id,
                                guard.variant.as_tag()
                            ),
                        });
                    }
                };
                (
                    image_ref,
                    profile.artifact_path.clone(),
                    guard.containers.oci_store.clone(),
                )
            };

            if !artifact_path.exists() {
                return call.reply(ModelAvailability {
                    is_available: false,
                    reason: format!("artifact path {} is missing", artifact_path.display()),
                });
            }

            let runtime_exists = crate::container::runtime_rootfs_path(&oci_store, &image_ref).is_some();

            call.reply(ModelAvailability {
                is_available: runtime_exists,
                reason: if runtime_exists {
                    "available".to_string()
                } else {
                    format!("runtime rootfs for {image_ref} is not present in the user or system OCI store")
                },
            })
        })
    }

    fn create_session(
        &self,
        call: &mut dyn Call_CreateSession,
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
                            if let Err(e) = guard.permissions.deny_if_missing(&app_id, &use_case) {
                                tracing::warn!("failed to persist denied permission: {e}");
                            }
                            return call.reply_permission_denied(app_id, use_case);
                        }
                    }
                }
            }

            let profile_id = match guard.assignments.get(&use_case) {
                Some(profile_id) => profile_id.to_string(),
                None => {
                    return call
                        .reply_model_unavailable(format!("no profile assigned for {use_case}"));
                }
            };
            if guard.profiles.get(&profile_id).is_none() {
                return call.reply_model_unavailable(format!(
                    "assigned profile {profile_id} is not installed"
                ));
            }

            let session_id = Uuid::new_v4().to_string();
            let session = crate::state::Session {
                session_id: session_id.clone(),
                app_id,
                use_case,
                profile_id,
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
            let (profile_id, image_ref, artifact_path, runtime_options) =
                match guard.sessions.get(&session_id) {
                    Some(s) => match profile_runtime(&guard, &s.profile_id) {
                        Ok(resolved) => resolved,
                        Err(reason) => return call.reply_model_unavailable(reason),
                    },
                    None => return call.reply_session_not_found(session_id),
                };

            match guard.containers.get_or_spawn(
                &profile_id,
                &image_ref,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(_) => {}
                Err(e) => return call.reply_generation_failed(e.to_string()),
            }

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
                Err(GenerationError::Reply(e)) => Err(e),
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
            match stream_tokens(&self.state, call, session_id, prompt, options).await {
                Ok(()) => Ok(()),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_generation_failed(reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn chat(
        &self,
        call: &mut dyn Call_Chat,
        session_id: String,
        messages: Vec<ChatMessage>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut content = String::new();
            match generate_chat_tokens(&self.state, &mut content, session_id, messages, options)
                .await
            {
                Ok(()) => call.reply(content),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_generation_failed(reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_chat(
        &self,
        call: &mut dyn Call_StreamChat,
        session_id: String,
        messages: Vec<ChatMessage>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_chat_tokens(&self.state, call, session_id, messages, options).await {
                Ok(()) => Ok(()),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_generation_failed(reason),
                Err(GenerationError::Reply(e)) => Err(e),
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
            let (
                app_id,
                use_case,
                profile_id,
                image_ref,
                artifact_path,
                runtime_options,
                instructions,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) = ensure_llm_use_case(&s.use_case) {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, image_ref, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        image_ref,
                        artifact_path,
                        runtime_options,
                        s.instructions.clone(),
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let container = guard
                .containers
                .get_or_spawn(
                    &profile_id,
                    &image_ref,
                    &artifact_path,
                    &runtime_options,
                    |_| {},
                )
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
        language_hint: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options) =
                match guard.sessions.get(&session_id) {
                    Some(s) => {
                        if let Err(reason) =
                            ensure_exact_use_case(&s.use_case, "asr.transcribe", "Transcribe")
                        {
                            return call.reply_model_unavailable(reason);
                        }
                        let (profile_id, image_ref, artifact_path, runtime_options) =
                            match profile_runtime(&guard, &s.profile_id) {
                                Ok(resolved) => resolved,
                                Err(reason) => return call.reply_model_unavailable(reason),
                            };
                        (
                            s.app_id.clone(),
                            s.use_case.clone(),
                            profile_id,
                            image_ref,
                            artifact_path,
                            runtime_options,
                        )
                    }
                    None => return call.reply_session_not_found(session_id),
                };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let audio_bytes = base64_decode(&audio).map_err(io_err)?;
            let container = match guard.containers.get_or_spawn(
                &profile_id,
                &image_ref,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.transcribe(audio_bytes, Some(&language_hint)) {
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

            let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options) =
                match guard.sessions.get(&session_id) {
                    Some(s) => {
                        if let Err(reason) =
                            ensure_exact_use_case(&s.use_case, "vision.describe", "Describe")
                        {
                            return call.reply_model_unavailable(reason);
                        }
                        let (profile_id, image_ref, artifact_path, runtime_options) =
                            match profile_runtime(&guard, &s.profile_id) {
                                Ok(resolved) => resolved,
                                Err(reason) => return call.reply_model_unavailable(reason),
                            };
                        (
                            s.app_id.clone(),
                            s.use_case.clone(),
                            profile_id,
                            image_ref,
                            artifact_path,
                            runtime_options,
                        )
                    }
                    None => return call.reply_session_not_found(session_id),
                };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = base64_decode(&image).map_err(io_err)?;
            let container = match guard.containers.get_or_spawn(
                &profile_id,
                &image_ref,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.describe(image_bytes) {
                Ok(text) => call.reply(text),
                Err(e) => call.reply_generation_failed(e.to_string()),
            }
        })
    }

    fn segment(
        &self,
        call: &mut dyn Call_Segment,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options) =
                match guard.sessions.get(&session_id) {
                    Some(s) => {
                        if let Err(reason) =
                            ensure_exact_use_case(&s.use_case, "vision.segment", "Segment")
                        {
                            return call.reply_model_unavailable(reason);
                        }
                        let (profile_id, image_ref, artifact_path, runtime_options) =
                            match profile_runtime(&guard, &s.profile_id) {
                                Ok(resolved) => resolved,
                                Err(reason) => return call.reply_model_unavailable(reason),
                            };
                        (
                            s.app_id.clone(),
                            s.use_case.clone(),
                            profile_id,
                            image_ref,
                            artifact_path,
                            runtime_options,
                        )
                    }
                    None => return call.reply_session_not_found(session_id),
                };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = base64_decode(&image).map_err(io_err)?;
            let container = match guard.containers.get_or_spawn(
                &profile_id,
                &image_ref,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.segment(image_bytes) {
                Ok(segments) => call.reply(
                    segments
                        .into_iter()
                        .map(|segment| VisionSegment {
                            label: segment.label,
                            confidence: segment.confidence,
                            x: segment.x,
                            y: segment.y,
                            width: segment.width,
                            height: segment.height,
                        })
                        .collect(),
                ),
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

async fn stream_chat_tokens(
    state: &SharedState,
    call: &mut dyn Call_StreamChat,
    session_id: String,
    messages: Vec<ChatMessage>,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let messages = runtime_chat_messages(messages).map_err(GenerationError::InvalidOptions)?;
    let mut guard = state.0.lock().await;

    let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options, instructions) =
        match guard.sessions.get(&session_id) {
            Some(s) => {
                ensure_exact_use_case(&s.use_case, "llm.chat", "Chat")
                    .map_err(GenerationError::ModelUnavailable)?;
                let (profile_id, image_ref, artifact_path, runtime_options) =
                    profile_runtime(&guard, &s.profile_id)
                        .map_err(GenerationError::ModelUnavailable)?;
                (
                    s.app_id.clone(),
                    s.use_case.clone(),
                    profile_id,
                    image_ref,
                    artifact_path,
                    runtime_options,
                    s.instructions.clone(),
                )
            }
            None => return Err(GenerationError::SessionNotFound(session_id)),
        };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn(
            &profile_id,
            &image_ref,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    let wants_more = call.wants_more();
    let mut pending_token: Option<String> = None;
    let mut reply_error: Option<varlink::Error> = None;
    let mut saw_token = false;

    let result = container.chat(Some(&instructions), &messages, max_tokens, |token| {
        if !token.is_empty() {
            saw_token = true;
        }
        if !wants_more {
            pending_token = Some(token);
            return;
        }

        if reply_error.is_some() {
            return;
        }

        if let Some(previous) = pending_token.replace(token) {
            call.set_continues(true);
            if let Err(e) = call.reply(previous) {
                reply_error = Some(e);
            }
        }
    });

    if let Some(e) = reply_error {
        return Err(GenerationError::Reply(e));
    }

    if let Err(e) = result {
        if wants_more {
            call.set_continues(false);
        }
        return Err(GenerationError::Failed(e.to_string()));
    }
    if !saw_token {
        return Err(GenerationError::Failed(
            "model returned no output".to_string(),
        ));
    }

    if wants_more {
        call.set_continues(false);
    }
    call.reply(pending_token.unwrap_or_default())
        .map_err(GenerationError::Reply)
}

async fn stream_tokens(
    state: &SharedState,
    call: &mut dyn Call_StreamResponse,
    session_id: String,
    prompt: String,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let mut guard = state.0.lock().await;

    let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options, instructions) =
        match guard.sessions.get(&session_id) {
            Some(s) => {
                ensure_llm_use_case(&s.use_case).map_err(GenerationError::ModelUnavailable)?;
                let (profile_id, image_ref, artifact_path, runtime_options) =
                    profile_runtime(&guard, &s.profile_id)
                        .map_err(GenerationError::ModelUnavailable)?;
                (
                    s.app_id.clone(),
                    s.use_case.clone(),
                    profile_id,
                    image_ref,
                    artifact_path,
                    runtime_options,
                    s.instructions.clone(),
                )
            }
            None => return Err(GenerationError::SessionNotFound(session_id)),
        };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn(
            &profile_id,
            &image_ref,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    let wants_more = call.wants_more();
    let mut pending_token: Option<String> = None;
    let mut reply_error: Option<varlink::Error> = None;
    let mut saw_token = false;

    let instructions = apply_translation_hints(&use_case, instructions, &options);
    let result = container.generate(Some(&instructions), &prompt, max_tokens, |token| {
        if !token.is_empty() {
            saw_token = true;
        }
        if !wants_more {
            pending_token = Some(token);
            return;
        }

        if reply_error.is_some() {
            return;
        }

        if let Some(previous) = pending_token.replace(token) {
            call.set_continues(true);
            if let Err(e) = call.reply(previous) {
                reply_error = Some(e);
            }
        }
    });

    if let Some(e) = reply_error {
        return Err(GenerationError::Reply(e));
    }

    if let Err(e) = result {
        if wants_more {
            call.set_continues(false);
        }
        return Err(GenerationError::Failed(e.to_string()));
    }
    if !saw_token {
        return Err(GenerationError::Failed(
            "model returned no output".to_string(),
        ));
    }

    if wants_more {
        call.set_continues(false);
    }
    call.reply(pending_token.unwrap_or_default())
        .map_err(GenerationError::Reply)
}

enum GenerationError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidOptions(String),
    Failed(String),
    Reply(varlink::Error),
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

    let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options, instructions) =
        match guard.sessions.get(&session_id) {
            Some(s) => {
                ensure_llm_use_case(&s.use_case).map_err(GenerationError::ModelUnavailable)?;
                let (profile_id, image_ref, artifact_path, runtime_options) =
                    profile_runtime(&guard, &s.profile_id)
                        .map_err(GenerationError::ModelUnavailable)?;
                (
                    s.app_id.clone(),
                    s.use_case.clone(),
                    profile_id,
                    image_ref,
                    artifact_path,
                    runtime_options,
                    s.instructions.clone(),
                )
            }
            None => return Err(GenerationError::SessionNotFound(session_id)),
        };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn(
            &profile_id,
            &image_ref,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    let instructions = apply_translation_hints(&use_case, instructions, &options);
    container
        .generate(Some(&instructions), &prompt, max_tokens, |token| {
            sink.push_token(token)
        })
        .map_err(|e| GenerationError::Failed(e.to_string()))
}

async fn generate_chat_tokens(
    state: &SharedState,
    sink: &mut impl TokenSink,
    session_id: String,
    messages: Vec<ChatMessage>,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let messages = runtime_chat_messages(messages).map_err(GenerationError::InvalidOptions)?;
    let mut guard = state.0.lock().await;

    let (app_id, use_case, profile_id, image_ref, artifact_path, runtime_options, instructions) =
        match guard.sessions.get(&session_id) {
            Some(s) => {
                ensure_exact_use_case(&s.use_case, "llm.chat", "Chat")
                    .map_err(GenerationError::ModelUnavailable)?;
                let (profile_id, image_ref, artifact_path, runtime_options) =
                    profile_runtime(&guard, &s.profile_id)
                        .map_err(GenerationError::ModelUnavailable)?;
                (
                    s.app_id.clone(),
                    s.use_case.clone(),
                    profile_id,
                    image_ref,
                    artifact_path,
                    runtime_options,
                    s.instructions.clone(),
                )
            }
            None => return Err(GenerationError::SessionNotFound(session_id)),
        };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn(
            &profile_id,
            &image_ref,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    container
        .chat(Some(&instructions), &messages, max_tokens, |token| {
            sink.push_token(token)
        })
        .map_err(|e| GenerationError::Failed(e.to_string()))
}

fn runtime_chat_messages(
    messages: Vec<ChatMessage>,
) -> Result<Vec<crate::container::ChatMessage>, String> {
    if messages.is_empty() {
        return Err("messages must not be empty".to_string());
    }

    messages
        .into_iter()
        .map(|message| {
            let role = message.role.trim();
            if !matches!(role, "user" | "assistant") {
                return Err(format!(
                    "chat message role must be 'user' or 'assistant', got '{}'",
                    message.role
                ));
            }
            if message.content.trim().is_empty() {
                return Err("chat message content must not be empty".to_string());
            }
            Ok(crate::container::ChatMessage {
                role: role.to_string(),
                content: message.content,
            })
        })
        .collect()
}

fn apply_translation_hints(
    use_case: &str,
    instructions: String,
    options: &GenerationOptions,
) -> String {
    if use_case != "llm.translate" {
        return instructions;
    }

    let source = options.source_language_hint.trim();
    let target = options.target_language_hint.trim();
    match (source.is_empty(), target.is_empty()) {
        (true, true) => instructions,
        (false, true) => format!("{instructions}\nSource language hint: {source}."),
        (true, false) => format!("{instructions}\nTarget language hint: {target}."),
        (false, false) => format!(
            "{instructions}\nSource language hint: {source}. Target language hint: {target}."
        ),
    }
}

fn profile_runtime(
    guard: &crate::state::Inner,
    profile_id: &str,
) -> Result<(String, String, PathBuf, HashMap<String, String>), String> {
    let profile = guard
        .profiles
        .get(profile_id)
        .ok_or_else(|| format!("assigned profile {profile_id} is not installed"))?;
    let image_ref = resolve_runtime_image(guard, profile)
        .ok_or_else(|| {
            format!(
                "runtime {} does not support {}",
                profile.runtime_id,
                guard.variant.as_tag()
            )
        })?
        .to_string();
    if !profile.artifact_path.exists() {
        return Err(format!(
            "artifact path {} is missing",
            profile.artifact_path.display()
        ));
    }
    Ok((
        profile.profile_id.clone(),
        image_ref,
        profile.artifact_path.clone(),
        profile.runtime_options.clone(),
    ))
}

fn resolve_runtime_image<'a>(
    guard: &'a crate::state::Inner,
    profile: &'a crate::profiles::Profile,
) -> Option<&'a str> {
    guard
        .runtimes
        .resolve(&profile.runtime_id, guard.variant)
        .or_else(|| profile.runtime_image_for(guard.variant))
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

fn ensure_llm_use_case(use_case: &str) -> Result<(), String> {
    if use_case.starts_with("llm.") {
        Ok(())
    } else {
        Err(format!(
            "text generation requires an llm.* use-case, got {use_case}"
        ))
    }
}

fn ensure_exact_use_case(use_case: &str, expected: &str, method: &str) -> Result<(), String> {
    if use_case == expected {
        Ok(())
    } else {
        Err(format!(
            "{method} requires use-case {expected}, got {use_case}"
        ))
    }
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

        if !field.description.trim().is_empty()
            && let Some(obj) = schema.as_object_mut()
        {
            obj.insert(
                "description".to_string(),
                Value::String(field.description.clone()),
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn generation_options() -> GenerationOptions {
        GenerationOptions {
            maximum_response_tokens: 128,
            temperature: 0.7,
            sampling_mode: "default".to_string(),
            source_language_hint: String::new(),
            target_language_hint: String::new(),
        }
    }

    #[test]
    fn validate_options_accepts_normal_generation_options() {
        assert_eq!(validate_options(&generation_options()), Ok(128));
    }

    #[test]
    fn validate_options_rejects_zero_tokens() {
        let mut options = generation_options();
        options.maximum_response_tokens = 0;

        assert_eq!(
            validate_options(&options),
            Err("maximum_response_tokens must be greater than zero".to_string())
        );
    }

    #[test]
    fn validate_options_rejects_invalid_temperature() {
        let mut options = generation_options();
        options.temperature = f64::NAN;

        assert_eq!(
            validate_options(&options),
            Err("temperature must be a finite non-negative number".to_string())
        );
    }

    #[test]
    fn validate_options_rejects_empty_sampling_mode() {
        let mut options = generation_options();
        options.sampling_mode = "  ".to_string();

        assert_eq!(
            validate_options(&options),
            Err("sampling_mode must not be empty".to_string())
        );
    }

    #[test]
    fn llm_generation_is_limited_to_llm_use_cases() {
        assert!(ensure_llm_use_case("llm.chat").is_ok());
        assert_eq!(
            ensure_llm_use_case("vision.describe"),
            Err("text generation requires an llm.* use-case, got vision.describe".to_string())
        );
    }

    #[test]
    fn guided_fields_schema_rejects_duplicate_names() {
        let fields = vec![
            GuidedField {
                name: "answer".to_string(),
                kind: "string".to_string(),
                description: String::new(),
                required: true,
            },
            GuidedField {
                name: "answer".to_string(),
                kind: "string".to_string(),
                description: String::new(),
                required: false,
            },
        ];

        assert_eq!(
            guided_fields_schema(&fields),
            Err("duplicate guided field 'answer'".to_string())
        );
    }

    #[test]
    fn base64_decode_rejects_invalid_input() {
        assert_eq!(
            base64_decode("not base64!"),
            Err("invalid base64 char:  ".to_string())
        );
    }
}
