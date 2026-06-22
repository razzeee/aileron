/// Varlink handler for `aileron.Inference`.
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects.
use aileron_varlink::aileron_Inference::{
    Call_CreateSession, Call_Describe, Call_Embed, Call_EndSession, Call_GetUseCaseAvailability,
    Call_Ocr, Call_PredictNext, Call_Prewarm, Call_Respond, Call_RespondGuided, Call_Segment,
    Call_StreamGuidedResponse, Call_StreamResponse, Call_SubmitToolResultsGuided, Call_Transcribe,
    GenerationOptions, GuidedField, ModelAvailability, ToolCall, ToolDefinition, ToolResult,
    VarlinkCallError, VarlinkInterface, VisionSegment,
};

pub struct InferenceHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

type ProfileRuntime = (
    String,
    String,
    Vec<String>,
    PathBuf,
    HashMap<String, String>,
);

impl InferenceHandler {
    pub fn new(state: SharedState, rt: tokio::runtime::Handle) -> Self {
        Self { state, rt }
    }
}

impl VarlinkInterface for InferenceHandler {
    fn get_use_case_availability(
        &self,
        call: &mut dyn Call_GetUseCaseAvailability,
        app_id: String,
        use_case: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let (image_refs, artifact_path, oci_store) = {
                let guard = self.state.0.lock().await;
                if !guard.config.allow_all
                    && matches!(guard.permissions.check(&app_id, &use_case), Some(false))
                {
                    return call.reply(ModelAvailability {
                        is_available: false,
                        code: "permission_denied".to_string(),
                        reason: format!("{app_id} is denied permission for {use_case}"),
                    });
                }
                let profile_id = match guard.assignments.get(&use_case) {
                    Some(profile_id) => profile_id,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            code: "no_profile_assigned".to_string(),
                            reason: format!("no profile assigned for {use_case}"),
                        });
                    }
                };
                let profile = match guard.profiles.get(profile_id) {
                    Some(profile) => profile,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            code: "profile_not_installed".to_string(),
                            reason: format!("assigned profile {profile_id} is not installed"),
                        });
                    }
                };
                let image_refs = resolve_runtime_images(&guard, profile)
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if image_refs.is_empty() {
                    return call.reply(ModelAvailability {
                        is_available: false,
                        code: "runtime_unsupported".to_string(),
                        reason: format!(
                            "runtime {} does not support {}",
                            profile.runtime_id,
                            guard.variant.as_tag()
                        ),
                    });
                }
                (
                    image_refs,
                    profile.artifact_path.clone(),
                    guard.containers.oci_store.clone(),
                )
            };

            if !artifact_path.exists() {
                return call.reply(ModelAvailability {
                    is_available: false,
                    code: "artifact_missing".to_string(),
                    reason: format!("artifact path {} is missing", artifact_path.display()),
                });
            }

            let runtime_exists = image_refs.iter().any(|image_ref| {
                crate::container::runtime_rootfs_path(&oci_store, image_ref).is_some()
            });

            call.reply(ModelAvailability {
                is_available: runtime_exists,
                code: if runtime_exists {
                    "available".to_string()
                } else {
                    "runtime_missing".to_string()
                },
                reason: if runtime_exists {
                    "available".to_string()
                } else {
                    runtime_missing_reason(&image_refs)
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
            match create_session_record(&self.state, app_id, use_case, instructions).await {
                Ok(session_id) => call.reply(session_id),
                Err(CreateSessionError::PermissionDenied(app_id, use_case)) => {
                    call.reply_permission_denied(app_id, use_case)
                }
                Err(CreateSessionError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
            }
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
            let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                match guard.sessions.get(&session_id) {
                    Some(s) => match profile_runtime(&guard, &s.profile_id) {
                        Ok(resolved) => resolved,
                        Err(reason) => return call.reply_model_unavailable(reason),
                    },
                    None => return call.reply_session_not_found(session_id),
                };

            match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
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
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
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
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn predict_next(
        &self,
        call: &mut dyn Call_PredictNext,
        session_id: String,
        prefix: String,
        count: i64,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match predict_next_completions(&self.state, session_id, prefix, count, options).await {
                Ok(completions) => call.reply(completions),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
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
        tools: Vec<ToolDefinition>,
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
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
                instructions,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) = ensure_language_generation_use_case(&s.use_case) {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                        s.instructions.clone(),
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => {
                    tracing::error!("RespondGuided failed to start container: {e:#}");
                    return call.reply_model_unavailable(e.to_string());
                }
            };

            let runtime_tools = tools.into_iter().map(varlink_tool_definition).collect();
            match container.generate_structured_with_tools(
                Some(&instructions),
                Some(&prompt),
                max_tokens,
                &schema,
                runtime_tools,
                Vec::new(),
            ) {
                Ok(result) => call.reply(result.content, varlink_tool_calls(result.tool_calls)),
                Err(e) => {
                    tracing::error!("RespondGuided failed: {e:#}");
                    call.reply_guided_generation_failed(e.to_string())
                }
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

            let (
                app_id,
                use_case,
                task,
                profile_id,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    let task = match ensure_speech_use_case(&s.use_case) {
                        Ok(task) => task,
                        Err(reason) => return call.reply_model_unavailable(reason),
                    };
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        task,
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let audio_bytes = match base64_decode(&audio) {
                Ok(bytes) => bytes,
                Err(reason) => return call.reply_invalid_input(reason),
            };
            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.transcribe(audio_bytes, Some(&language_hint), task) {
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

            let (
                app_id,
                use_case,
                profile_id,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) =
                        ensure_exact_use_case(&s.use_case, "vision.describe", "Describe")
                    {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = match base64_decode(&image) {
                Ok(bytes) => bytes,
                Err(reason) => return call.reply_invalid_input(reason),
            };
            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
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

    fn ocr(
        &self,
        call: &mut dyn Call_Ocr,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (
                app_id,
                use_case,
                profile_id,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) = ensure_exact_use_case(&s.use_case, "vision.ocr", "Ocr") {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = match base64_decode(&image) {
                Ok(bytes) => bytes,
                Err(reason) => return call.reply_invalid_input(reason),
            };
            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.ocr(image_bytes) {
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

            let (
                app_id,
                use_case,
                profile_id,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) =
                        ensure_exact_use_case(&s.use_case, "vision.segment", "Segment")
                    {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let image_bytes = match base64_decode(&image) {
                Ok(bytes) => bytes,
                Err(reason) => return call.reply_invalid_input(reason),
            };
            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
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

    fn embed(
        &self,
        call: &mut dyn Call_Embed,
        session_id: String,
        text: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (
                app_id,
                use_case,
                profile_id,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = match guard.sessions.get(&session_id) {
                Some(s) => {
                    if let Err(reason) =
                        ensure_exact_use_case(&s.use_case, "language.embed", "Embed")
                    {
                        return call.reply_model_unavailable(reason);
                    }
                    let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                        match profile_runtime(&guard, &s.profile_id) {
                            Ok(resolved) => resolved,
                            Err(reason) => return call.reply_model_unavailable(reason),
                        };
                    (
                        s.app_id.clone(),
                        s.use_case.clone(),
                        profile_id,
                        runtime_id,
                        image_refs,
                        artifact_path,
                        runtime_options,
                    )
                }
                None => return call.reply_session_not_found(session_id),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);
            let container = match guard.containers.get_or_spawn_any(
                &profile_id,
                &runtime_id,
                &image_refs,
                &artifact_path,
                &runtime_options,
                |_| {},
            ) {
                Ok(container) => container,
                Err(e) => return call.reply_generation_failed(e.to_string()),
            };

            match container.embed(&text) {
                Ok(embedding) => call.reply(embedding.into_iter().map(f64::from).collect()),
                Err(e) => call.reply_generation_failed(e.to_string()),
            }
        })
    }

    fn stream_guided_response(
        &self,
        call: &mut dyn Call_StreamGuidedResponse,
        session_id: String,
        prompt: String,
        fields: Vec<GuidedField>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_guided_snapshots(&self.state, call, session_id, prompt, fields, options)
                .await
            {
                Ok(()) => Ok(()),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_guided_generation_failed(reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn submit_tool_results_guided(
        &self,
        call: &mut dyn Call_SubmitToolResultsGuided,
        session_id: String,
        prompt: String,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match submit_guided_tool_results(
                &self.state,
                session_id,
                prompt,
                results,
                fields,
                tools,
                options,
            )
            .await
            {
                Ok(response) => {
                    call.reply(response.content, varlink_tool_calls(response.tool_calls))
                }
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::Failed(reason)) => call.reply_guided_generation_failed(reason),
                Err(GenerationError::Reply(e)) => Err(e),
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

enum CreateSessionError {
    PermissionDenied(String, String),
    ModelUnavailable(String),
}

async fn create_session_record(
    state: &SharedState,
    app_id: String,
    use_case: String,
    instructions: String,
) -> Result<String, CreateSessionError> {
    let mut guard = state.0.lock().await;

    if !guard.config.allow_all {
        match guard.permissions.check(&app_id, &use_case) {
            Some(true) => {}
            Some(false) => return Err(CreateSessionError::PermissionDenied(app_id, use_case)),
            None => {
                if guard.config.auto_grant {
                    tracing::info!("auto-granting {app_id} / {use_case} (AILERON_AUTO_GRANT)");
                    if let Err(e) = guard
                        .permissions
                        .set(app_id.clone(), use_case.clone(), true)
                    {
                        tracing::warn!("failed to persist auto-grant: {e}");
                    }
                } else {
                    if let Err(e) = guard.permissions.deny_if_missing(&app_id, &use_case) {
                        tracing::warn!("failed to persist denied permission: {e}");
                    }
                    return Err(CreateSessionError::PermissionDenied(app_id, use_case));
                }
            }
        }
    }

    let profile_id = guard
        .assignments
        .get(&use_case)
        .ok_or_else(|| {
            CreateSessionError::ModelUnavailable(format!("no profile assigned for {use_case}"))
        })?
        .to_string();
    if guard.profiles.get(&profile_id).is_none() {
        return Err(CreateSessionError::ModelUnavailable(format!(
            "assigned profile {profile_id} is not installed"
        )));
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
    Ok(session_id)
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

    let (
        app_id,
        use_case,
        profile_id,
        runtime_id,
        image_refs,
        artifact_path,
        runtime_options,
        instructions,
    ) = match guard.sessions.get(&session_id) {
        Some(s) => {
            ensure_language_generation_use_case(&s.use_case)
                .map_err(GenerationError::ModelUnavailable)?;
            let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                profile_runtime(&guard, &s.profile_id)
                    .map_err(GenerationError::ModelUnavailable)?;
            (
                s.app_id.clone(),
                s.use_case.clone(),
                profile_id,
                runtime_id,
                image_refs,
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
        .get_or_spawn_any(
            &profile_id,
            &runtime_id,
            &image_refs,
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

fn reply_generation_failure(
    call: &mut dyn VarlinkCallError,
    reason: String,
) -> varlink::Result<()> {
    match runtime_error_code(&reason) {
        Some("context_window_exceeded") => call.reply_context_window_exceeded(reason),
        Some("unsupported_language") => call.reply_unsupported_language(reason),
        Some("safety_refusal") => call.reply_safety_refusal(reason),
        Some("request_cancelled") => call.reply_request_cancelled(reason),
        Some("invalid_input") => call.reply_invalid_input(reason),
        _ => call.reply_generation_failed(reason),
    }
}

fn runtime_error_code(reason: &str) -> Option<&str> {
    reason
        .strip_prefix("container returned error ")
        .and_then(|rest| rest.split_once(':'))
        .map(|(code, _)| code.trim())
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

    let (
        app_id,
        use_case,
        profile_id,
        runtime_id,
        image_refs,
        artifact_path,
        runtime_options,
        instructions,
    ) = match guard.sessions.get(&session_id) {
        Some(s) => {
            ensure_language_generation_use_case(&s.use_case)
                .map_err(GenerationError::ModelUnavailable)?;
            let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                profile_runtime(&guard, &s.profile_id)
                    .map_err(GenerationError::ModelUnavailable)?;
            (
                s.app_id.clone(),
                s.use_case.clone(),
                profile_id,
                runtime_id,
                image_refs,
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
        .get_or_spawn_any(
            &profile_id,
            &runtime_id,
            &image_refs,
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

async fn predict_next_completions(
    state: &SharedState,
    session_id: String,
    prefix: String,
    count: i64,
    options: GenerationOptions,
) -> Result<Vec<String>, GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let count = validate_prediction_count(count).map_err(GenerationError::InvalidOptions)?;
    let mut guard = state.0.lock().await;

    let (app_id, use_case, profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
        match guard.sessions.get(&session_id) {
            Some(s) => {
                ensure_language_generation_use_case(&s.use_case)
                    .map_err(GenerationError::ModelUnavailable)?;
                let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                    profile_runtime(&guard, &s.profile_id)
                        .map_err(GenerationError::ModelUnavailable)?;
                (
                    s.app_id.clone(),
                    s.use_case.clone(),
                    profile_id,
                    runtime_id,
                    image_refs,
                    artifact_path,
                    runtime_options,
                )
            }
            None => return Err(GenerationError::SessionNotFound(session_id)),
        };

    let _ = guard.permissions.touch(&app_id, &use_case);
    let container = guard
        .containers
        .get_or_spawn_any(
            &profile_id,
            &runtime_id,
            &image_refs,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    container
        .predict_next(&prefix, count, max_tokens, options.temperature)
        .map_err(|e| GenerationError::Failed(e.to_string()))
}

fn validate_prediction_count(count: i64) -> Result<u32, String> {
    if count < 1 {
        return Err("prediction count must be greater than zero".to_string());
    }
    Ok(count.min(3) as u32)
}

async fn stream_guided_snapshots(
    state: &SharedState,
    call: &mut dyn Call_StreamGuidedResponse,
    session_id: String,
    prompt: String,
    fields: Vec<GuidedField>,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let mut guard = state.0.lock().await;

    let (
        app_id,
        use_case,
        profile_id,
        runtime_id,
        image_refs,
        artifact_path,
        runtime_options,
        instructions,
    ) = match guard.sessions.get(&session_id) {
        Some(s) => {
            ensure_language_generation_use_case(&s.use_case)
                .map_err(GenerationError::ModelUnavailable)?;
            let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                profile_runtime(&guard, &s.profile_id)
                    .map_err(GenerationError::ModelUnavailable)?;
            (
                s.app_id.clone(),
                s.use_case.clone(),
                profile_id,
                runtime_id,
                image_refs,
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
        .get_or_spawn_any(
            &profile_id,
            &runtime_id,
            &image_refs,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    let wants_more = call.wants_more();
    let mut pending_snapshot: Option<String> = None;
    let mut final_snapshot = String::new();
    let mut reply_error: Option<varlink::Error> = None;
    let result = container.stream_structured(
        Some(&instructions),
        &prompt,
        max_tokens,
        &schema,
        |snapshot, done| {
            final_snapshot = snapshot.clone();
            if !wants_more {
                pending_snapshot = Some(snapshot);
                return;
            }
            if reply_error.is_some() {
                return;
            }
            if let Some(previous) = pending_snapshot.replace(snapshot) {
                call.set_continues(true);
                if let Err(e) = call.reply(previous) {
                    reply_error = Some(e);
                }
            }
            if done {
                call.set_continues(false);
            }
        },
    );

    if let Some(e) = reply_error {
        return Err(GenerationError::Reply(e));
    }
    if let Err(e) = result {
        if wants_more {
            call.set_continues(false);
        }
        return Err(GenerationError::Failed(e.to_string()));
    }
    if final_snapshot.is_empty() {
        return Err(GenerationError::Failed(
            "model returned no guided snapshots".to_string(),
        ));
    }
    if wants_more {
        call.set_continues(false);
    }
    call.reply(pending_snapshot.unwrap_or(final_snapshot))
        .map_err(GenerationError::Reply)
}

async fn submit_guided_tool_results(
    state: &SharedState,
    session_id: String,
    prompt: String,
    results: Vec<ToolResult>,
    fields: Vec<GuidedField>,
    tools: Vec<ToolDefinition>,
    options: GenerationOptions,
) -> Result<crate::container::GuidedToolResponse, GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let mut guard = state.0.lock().await;

    let (
        app_id,
        use_case,
        profile_id,
        runtime_id,
        image_refs,
        artifact_path,
        runtime_options,
        instructions,
    ) = match guard.sessions.get(&session_id) {
        Some(s) => {
            ensure_language_generation_use_case(&s.use_case)
                .map_err(GenerationError::ModelUnavailable)?;
            let (profile_id, runtime_id, image_refs, artifact_path, runtime_options) =
                profile_runtime(&guard, &s.profile_id)
                    .map_err(GenerationError::ModelUnavailable)?;
            (
                s.app_id.clone(),
                s.use_case.clone(),
                profile_id,
                runtime_id,
                image_refs,
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
        .get_or_spawn_any(
            &profile_id,
            &runtime_id,
            &image_refs,
            &artifact_path,
            &runtime_options,
            |_| {},
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))?;

    container
        .generate_structured_with_tools(
            Some(&instructions),
            Some(&prompt),
            max_tokens,
            &schema,
            tools.into_iter().map(varlink_tool_definition).collect(),
            results.into_iter().map(varlink_tool_result).collect(),
        )
        .map_err(|e| GenerationError::Failed(e.to_string()))
}

fn varlink_tool_definition(tool: ToolDefinition) -> crate::container::ToolDefinition {
    crate::container::ToolDefinition {
        name: tool.name,
        description: tool.description,
        schema_json: tool.schema_json,
    }
}

fn varlink_tool_result(result: ToolResult) -> crate::container::ToolResult {
    crate::container::ToolResult {
        id: result.id,
        content: result.content,
        content_json: result.content_json,
    }
}

fn varlink_tool_calls(calls: Vec<crate::container::ToolCall>) -> Vec<ToolCall> {
    calls
        .into_iter()
        .map(|call| ToolCall {
            id: call.id,
            name: call.name,
            arguments_json: call.arguments_json,
        })
        .collect()
}

fn apply_translation_hints(
    use_case: &str,
    instructions: String,
    options: &GenerationOptions,
) -> String {
    if use_case != "language.translate" {
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
) -> Result<ProfileRuntime, String> {
    let profile = guard
        .profiles
        .get(profile_id)
        .ok_or_else(|| format!("assigned profile {profile_id} is not installed"))?;
    let image_refs = resolve_runtime_images(guard, profile)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if image_refs.is_empty() {
        return Err(format!(
            "runtime {} does not support {}",
            profile.runtime_id,
            guard.variant.as_tag()
        ));
    }
    if !profile.artifact_path.exists() {
        return Err(format!(
            "artifact path {} is missing",
            profile.artifact_path.display()
        ));
    }
    Ok((
        profile.profile_id.clone(),
        profile.runtime_id.clone(),
        image_refs,
        profile.artifact_path.clone(),
        profile.runtime_options.clone(),
    ))
}

fn resolve_runtime_images<'a>(
    guard: &'a crate::state::Inner,
    profile: &'a crate::profiles::Profile,
) -> Vec<&'a str> {
    let manifest_images = guard
        .runtimes
        .resolve_candidates(&profile.runtime_id, guard.variant);
    if !manifest_images.is_empty() {
        return manifest_images;
    }

    profile.runtime_image_candidates(guard.variant)
}

fn runtime_missing_reason(image_refs: &[String]) -> String {
    match image_refs {
        [] => "no runtime image candidates resolved".to_string(),
        [image_ref] => {
            format!("runtime rootfs for {image_ref} is not present in the user or system OCI store")
        }
        _ => format!(
            "none of the runtime rootfs candidates are present in the user or system OCI store: {}",
            image_refs.join(", ")
        ),
    }
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

fn ensure_language_generation_use_case(use_case: &str) -> Result<(), String> {
    // `language.embed` is a language.* token but produces vectors, not text, so it is
    // not a valid use-case for the text-generation methods.
    if use_case.starts_with("language.") && use_case != "language.embed" {
        Ok(())
    } else {
        Err(format!(
            "text generation requires a language.* use-case, got {use_case}"
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

/// Validate the session use-case for the Transcribe method and return the
/// whisper task to run: "transcribe" (verbatim) or "translate" (to English).
fn ensure_speech_use_case(use_case: &str) -> Result<&'static str, String> {
    match use_case {
        "speech.transcribe" => Ok("transcribe"),
        "speech.translate" => Ok("translate"),
        other => Err(format!(
            "Transcribe requires use-case speech.transcribe or speech.translate, got {other}"
        )),
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
    use hegel::TestCase;
    use hegel::generators as gs;

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

    #[hegel::test]
    fn validate_options_accepts_generated_valid_options(tc: TestCase) {
        let maximum_response_tokens = tc.draw(gs::integers::<i64>().min_value(1).max_value(4096));
        let temperature_tenths = tc.draw(gs::integers::<i64>().min_value(0).max_value(20));
        let sampling_mode = tc.draw(gs::sampled_from(vec![
            "default".to_string(),
            "greedy".to_string(),
            "creative".to_string(),
        ]));
        let options = GenerationOptions {
            maximum_response_tokens,
            temperature: temperature_tenths as f64 / 10.0,
            sampling_mode,
            source_language_hint: String::new(),
            target_language_hint: String::new(),
        };

        assert_eq!(
            validate_options(&options),
            Ok(maximum_response_tokens as u32)
        );
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
    fn language_generation_is_limited_to_language_use_cases() {
        assert!(ensure_language_generation_use_case("language.extract").is_ok());
        assert_eq!(
            ensure_language_generation_use_case("vision.describe"),
            Err("text generation requires a language.* use-case, got vision.describe".to_string())
        );
    }

    #[hegel::test]
    fn language_generation_accepts_generated_text_language_use_cases(tc: TestCase) {
        let use_case = tc.draw(gs::sampled_from(vec![
            "language.summarize".to_string(),
            "language.translate".to_string(),
            "language.rephrase".to_string(),
            "language.classify".to_string(),
            "language.extract".to_string(),
            "language.analyze".to_string(),
        ]));

        assert!(ensure_language_generation_use_case(&use_case).is_ok());
    }

    #[test]
    fn language_generation_excludes_embed_use_case() {
        assert_eq!(
            ensure_language_generation_use_case("language.embed"),
            Err("text generation requires a language.* use-case, got language.embed".to_string())
        );
    }

    #[test]
    fn speech_use_case_maps_to_whisper_task() {
        assert_eq!(
            ensure_speech_use_case("speech.transcribe"),
            Ok("transcribe")
        );
        assert_eq!(ensure_speech_use_case("speech.translate"), Ok("translate"));
        assert!(ensure_speech_use_case("language.extract").is_err());
    }

    #[hegel::test]
    fn speech_use_case_maps_generated_supported_cases_to_tasks(tc: TestCase) {
        let (use_case, task) = tc.draw(gs::sampled_from(vec![
            ("speech.transcribe".to_string(), "transcribe"),
            ("speech.translate".to_string(), "translate"),
        ]));

        assert_eq!(ensure_speech_use_case(&use_case), Ok(task));
    }

    #[hegel::test]
    fn guided_fields_schema_accepts_generated_supported_kinds(tc: TestCase) {
        let kind = tc.draw(gs::sampled_from(vec![
            "string".to_string(),
            "number".to_string(),
            "integer".to_string(),
            "boolean".to_string(),
            "string_array".to_string(),
        ]));
        let required = tc.draw(gs::booleans());
        let field = GuidedField {
            name: "field".to_string(),
            kind: kind.clone(),
            description: "generated field".to_string(),
            required,
        };

        let schema = guided_fields_schema(&[field]).expect("supported kind should build schema");

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("field").is_some());
        assert_eq!(
            schema["required"].as_array().unwrap().len(),
            usize::from(required)
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

    #[hegel::test]
    fn base64_decode_accepts_generated_valid_alphabet(tc: TestCase) {
        let chars = tc.draw(
            gs::vecs(gs::sampled_from(
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                    .chars()
                    .collect::<Vec<_>>(),
            ))
            .max_size(64),
        );
        let encoded = chars.into_iter().collect::<String>();

        assert!(base64_decode(&encoded).is_ok());
    }
}
