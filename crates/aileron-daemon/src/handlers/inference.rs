/// Varlink handler for `aileron.Inference`.
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{MutexGuard, TryLockError};
use std::time::Duration;
use uuid::Uuid;

use crate::container::{Container, ContainerHandle};
use crate::profiles::RuntimeCandidate;
use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects.
use aileron_varlink::aileron_Inference::{
    Call_CreateSession, Call_EndSession, Call_GetUseCaseAvailability, Call_Prewarm,
    Call_StreamDescribe, Call_StreamEmbed, Call_StreamOcr, Call_StreamPredictNext,
    Call_StreamRespondGuided, Call_StreamResponse, Call_StreamSegment,
    Call_StreamSubmitToolResultsGuided, Call_StreamTranscribe, GenerationOptions, GuidedField,
    ModelAvailability, ToolCall, ToolDefinition, ToolResult, VarlinkCallError, VarlinkInterface,
    VisionSegment,
};

pub struct InferenceHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

type ProfileRuntime = (
    String,
    u64,
    String,
    Vec<RuntimeCandidate>,
    PathBuf,
    HashMap<String, String>,
);

#[derive(Debug, Clone)]
struct ResolvedSessionRuntime {
    app_id: String,
    use_case: String,
    profile_id: String,
    profile_epoch: u64,
    runtime_id: String,
    image_refs: Vec<RuntimeCandidate>,
    artifact_path: PathBuf,
    runtime_options: HashMap<String, String>,
    instructions: String,
}

struct ActiveContainerRequest<'a> {
    state: &'a SharedState,
    profile_id: &'a str,
    session_id: &'a str,
    handle: ContainerHandle,
}

impl<'a> ActiveContainerRequest<'a> {
    fn new(
        state: &'a SharedState,
        profile_id: &'a str,
        session_id: &'a str,
        handle: ContainerHandle,
    ) -> Self {
        state.begin_container_request(profile_id, session_id, handle.clone());
        Self {
            state,
            profile_id,
            session_id,
            handle,
        }
    }
}

impl Drop for ActiveContainerRequest<'_> {
    fn drop(&mut self) {
        self.state
            .end_container_request(self.profile_id, self.session_id, &self.handle);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolveSessionError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidInput(String),
}

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
            let (candidates, artifact_path) = {
                let guard = self.state.0.lock().await;
                if !is_supported_use_case(&use_case) {
                    return call.reply(ModelAvailability {
                        is_available: false,
                        code: "unsupported_use_case".to_string(),
                        reason: format!("unsupported use-case: {use_case}"),
                    });
                }
                if !guard.config.allow_all
                    && matches!(guard.permissions.check(&app_id, &use_case), Some(false))
                {
                    return call.reply(ModelAvailability {
                        is_available: false,
                        code: "permission_denied".to_string(),
                        reason: format!("{app_id} is denied permission for {use_case}"),
                    });
                }
                let profile_id = match assigned_profile_id_for_use_case(&guard, &use_case) {
                    Some(profile_id) => profile_id,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            code: "no_profile_assigned".to_string(),
                            reason: format!("no profile assigned for {use_case}"),
                        });
                    }
                };
                let profile = match guard.profiles.get(&profile_id) {
                    Some(profile) => profile,
                    None => {
                        return call.reply(ModelAvailability {
                            is_available: false,
                            code: "profile_not_installed".to_string(),
                            reason: format!("assigned profile {profile_id} is not installed"),
                        });
                    }
                };
                let candidates = resolve_runtime_candidates(&guard, profile);
                if candidates.is_empty() {
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
                (candidates, profile.artifact_path.clone())
            };
            let (oci_store, system_oci_store) = {
                let containers = self.state.2.lock().await;
                (
                    containers.oci_store.clone(),
                    containers.system_oci_store.clone(),
                )
            };

            if !artifact_path.exists() {
                return call.reply(ModelAvailability {
                    is_available: false,
                    code: "artifact_missing".to_string(),
                    reason: format!("artifact path {} is missing", artifact_path.display()),
                });
            }

            let runtime_exists = candidates.iter().any(|candidate| {
                crate::container::runtime_rootfs_path_in_stores(
                    &oci_store,
                    &system_oci_store,
                    &candidate.image_ref,
                )
                .is_some()
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
                    runtime_missing_reason(&candidates)
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
                Ok((session_id, profile_id)) => call.reply(session_id, profile_id),
                Err(CreateSessionError::PermissionPromptRequired(app_id, use_case)) => {
                    call.reply_permission_prompt_required(app_id, use_case)
                }
                Err(CreateSessionError::PermissionDenied(app_id, use_case)) => {
                    call.reply_permission_denied(app_id, use_case)
                }
                Err(CreateSessionError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(CreateSessionError::InvalidInput(reason)) => call.reply_invalid_input(reason),
            }
        })
    }

    fn prewarm(&self, call: &mut dyn Call_Prewarm, session_id: String) -> varlink::Result<()> {
        self.rt.block_on(async {
            let resolved = match resolve_session_runtime(&self.state, &session_id, |_| Ok(())).await
            {
                Ok(resolved) => resolved,
                Err(ResolveSessionError::SessionNotFound(id)) => {
                    return call.reply_session_not_found(id);
                }
                Err(ResolveSessionError::ModelUnavailable(reason)) => {
                    return call.reply_model_unavailable(reason);
                }
                Err(ResolveSessionError::InvalidInput(reason)) => {
                    return call.reply_invalid_input(reason);
                }
            };

            if let Err(e) = with_locked_container(
                &self.state,
                &session_id,
                resolved.clone(),
                std::convert::identity,
                |_container, _handle, _spawned| Ok(()),
            )
            .await
            {
                return reply_generation_failure(call, e);
            }

            call.reply()
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
                Err(GenerationError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_predict_next(
        &self,
        call: &mut dyn Call_StreamPredictNext,
        session_id: String,
        prefix: String,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match predict_next_completions(&self.state, session_id, prefix, options).await {
                Ok(completions) => call.reply(completions),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_transcribe(
        &self,
        call: &mut dyn Call_StreamTranscribe,
        session_id: String,
        audio: String,
        source_language_hint: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_transcription(&self.state, call, session_id, audio, source_language_hint)
                .await
            {
                Ok(()) => Ok(()),
                Err(SpeechError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(SpeechError::ModelUnavailable(reason)) => call.reply_model_unavailable(reason),
                Err(SpeechError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(SpeechError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(SpeechError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_describe(
        &self,
        call: &mut dyn Call_StreamDescribe,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_vision_text(&self.state, call, session_id, image, "vision.describe").await
            {
                Ok(()) => Ok(()),
                Err(VisionError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(VisionError::ModelUnavailable(reason)) => call.reply_model_unavailable(reason),
                Err(VisionError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(VisionError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(VisionError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_ocr(
        &self,
        call: &mut dyn Call_StreamOcr,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_vision_text(&self.state, call, session_id, image, "vision.ocr").await {
                Ok(()) => Ok(()),
                Err(VisionError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(VisionError::ModelUnavailable(reason)) => call.reply_model_unavailable(reason),
                Err(VisionError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(VisionError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(VisionError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_segment(
        &self,
        call: &mut dyn Call_StreamSegment,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match vision_segments(&self.state, session_id, image).await {
                Ok(segments) => call.reply(segments),
                Err(VisionError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(VisionError::ModelUnavailable(reason)) => call.reply_model_unavailable(reason),
                Err(VisionError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(VisionError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(VisionError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_embed(
        &self,
        call: &mut dyn Call_StreamEmbed,
        session_id: String,
        text: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match embedding_vector(&self.state, session_id, text).await {
                Ok(embedding) => call.reply(embedding),
                Err(GenerationError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(GenerationError::ModelUnavailable(reason)) => {
                    call.reply_model_unavailable(reason)
                }
                Err(GenerationError::InvalidOptions(reason)) => {
                    call.reply_invalid_generation_options(reason)
                }
                Err(GenerationError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_respond_guided(
        &self,
        call: &mut dyn Call_StreamRespondGuided,
        session_id: String,
        prompt: String,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_guided_snapshots(
                &self.state,
                call,
                session_id,
                prompt,
                fields,
                tools,
                options,
            )
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
                Err(GenerationError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(GenerationError::Failed(reason)) => reply_guided_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_submit_tool_results_guided(
        &self,
        call: &mut dyn Call_StreamSubmitToolResultsGuided,
        session_id: String,
        prompt: String,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GenerationOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_guided_tool_results(
                &self.state,
                call,
                session_id,
                prompt,
                results,
                fields,
                tools,
                options,
            )
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
                Err(GenerationError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(GenerationError::Failed(reason)) => reply_guided_failure(call, reason),
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
            let profile_id = {
                let mut guard = self.state.0.lock().await;
                let Some(session) = guard.sessions.remove(&session_id) else {
                    return call.reply_session_not_found(session_id);
                };
                self.state.cancel_session_requests(&session_id);
                session.profile_id
            };
            self.state.clear_predict_next(&session_id);
            kill_profile_if_session_active(&self.state, &profile_id, &session_id).await;
            call.reply()
        })
    }
}

enum CreateSessionError {
    PermissionPromptRequired(String, String),
    PermissionDenied(String, String),
    ModelUnavailable(String),
    InvalidInput(String),
}

async fn create_session_record(
    state: &SharedState,
    app_id: String,
    use_case: String,
    instructions: String,
) -> Result<(String, String), CreateSessionError> {
    let mut guard = state.0.lock().await;

    if app_id.trim().is_empty() {
        return Err(CreateSessionError::InvalidInput(
            "app id is required".to_string(),
        ));
    }

    if !is_supported_use_case(&use_case) {
        return Err(CreateSessionError::InvalidInput(format!(
            "unsupported use-case: {use_case}"
        )));
    }

    let profile_id = assigned_profile_id_for_use_case(&guard, &use_case).ok_or_else(|| {
        CreateSessionError::ModelUnavailable(format!("no profile assigned for {use_case}"))
    })?;
    if guard.profiles.get(&profile_id).is_none() {
        return Err(CreateSessionError::ModelUnavailable(format!(
            "assigned profile {profile_id} is not installed"
        )));
    }

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
                    return Err(CreateSessionError::PermissionPromptRequired(
                        app_id, use_case,
                    ));
                }
            }
        }
    }

    let session_id = Uuid::new_v4().to_string();
    let session = crate::state::Session {
        session_id: session_id.clone(),
        app_id,
        use_case,
        profile_id: profile_id.clone(),
        instructions,
        started_at: chrono::Utc::now(),
    };
    guard.sessions.insert(session_id.clone(), session);
    state.clear_session_cancelled(&session_id);
    Ok((session_id, profile_id))
}

fn is_supported_use_case(use_case: &str) -> bool {
    crate::manifests::SUPPORTED_USE_CASES.contains(&use_case)
}

fn assigned_profile_id_for_use_case(guard: &crate::state::Inner, use_case: &str) -> Option<String> {
    if let Some(profile_id) = guard.assignments.get(use_case) {
        return Some(profile_id.to_string());
    }

    if use_case == "speech.translate" {
        let profile_id = guard.assignments.get("speech.transcribe")?;
        let profile = guard.profiles.get(profile_id)?;
        if profile.supports_use_case("speech.translate") {
            return Some(profile_id.to_string());
        }
    }

    None
}

async fn stream_transcription(
    state: &SharedState,
    call: &mut dyn Call_StreamTranscribe,
    session_id: String,
    audio: String,
    source_language_hint: String,
) -> Result<(), SpeechError> {
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_speech_use_case(use_case).map(|_| ())
    })
    .await
    .map_err(SpeechError::from)?;
    let task = ensure_speech_use_case(&resolved.use_case).map_err(SpeechError::InvalidInput)?;
    let audio_bytes = base64_decode(&audio).map_err(SpeechError::InvalidInput)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        SpeechError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut cancelled = false;

            let result = container.stream_transcribe(
                audio_bytes,
                Some(&source_language_hint),
                task,
                |token| {
                    if cancelled || state.is_session_cancelled(&session_id) {
                        cancelled = true;
                        return;
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
                },
            );

            if let Some(e) = reply_error {
                return Err(SpeechError::Reply(e));
            }
            if cancelled || state.is_session_cancelled(&session_id) {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(SpeechError::Failed(request_cancelled_reason()));
            }

            if let Err(e) = result {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(SpeechError::Failed(e.to_string()));
            }

            if wants_more {
                call.set_continues(false);
            }
            call.reply(pending_token.unwrap_or_default())
                .map_err(SpeechError::Reply)
        },
    )
    .await
}

async fn stream_vision_text<C: TextStreamCall + ?Sized>(
    state: &SharedState,
    call: &mut C,
    session_id: String,
    image: String,
    expected_use_case: &str,
) -> Result<(), VisionError> {
    let method = if expected_use_case == "vision.describe" {
        "StreamDescribe"
    } else {
        "StreamOcr"
    };
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, expected_use_case, method)
    })
    .await
    .map_err(VisionError::from)?;
    let image_bytes = base64_decode(&image).map_err(VisionError::InvalidInput)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        VisionError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut saw_token = false;
            let mut cancelled = false;

            let result = if expected_use_case == "vision.describe" {
                container.stream_describe(image_bytes, |token| {
                    if cancelled || state.is_session_cancelled(&session_id) {
                        cancelled = true;
                        return;
                    }
                    forward_text_stream_token(
                        call,
                        wants_more,
                        token,
                        &mut pending_token,
                        &mut saw_token,
                        &mut reply_error,
                    );
                })
            } else {
                container.stream_ocr(image_bytes, |token| {
                    if cancelled || state.is_session_cancelled(&session_id) {
                        cancelled = true;
                        return;
                    }
                    forward_text_stream_token(
                        call,
                        wants_more,
                        token,
                        &mut pending_token,
                        &mut saw_token,
                        &mut reply_error,
                    );
                })
            };

            if let Some(e) = reply_error {
                return Err(VisionError::Reply(e));
            }
            if cancelled || state.is_session_cancelled(&session_id) {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(VisionError::Failed(request_cancelled_reason()));
            }
            if let Err(e) = result {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(VisionError::Failed(e.to_string()));
            }
            if !saw_token && !vision_text_allows_empty_output(expected_use_case) {
                return Err(VisionError::Failed("model returned no output".to_string()));
            }

            if wants_more {
                call.set_continues(false);
            }
            call.reply_token(pending_token.unwrap_or_default())
                .map_err(VisionError::Reply)
        },
    )
    .await
}

fn forward_text_stream_token<C: TextStreamCall + ?Sized>(
    call: &mut C,
    wants_more: bool,
    token: String,
    pending_token: &mut Option<String>,
    saw_token: &mut bool,
    reply_error: &mut Option<varlink::Error>,
) {
    if !token.is_empty() {
        *saw_token = true;
    }
    if !wants_more {
        *pending_token = Some(token);
        return;
    }

    if reply_error.is_some() {
        return;
    }

    if let Some(previous) = pending_token.replace(token) {
        call.set_continues(true);
        if let Err(e) = call.reply_token(previous) {
            *reply_error = Some(e);
        }
    }
}

fn vision_text_allows_empty_output(expected_use_case: &str) -> bool {
    expected_use_case == "vision.ocr"
}

async fn vision_segments(
    state: &SharedState,
    session_id: String,
    image: String,
) -> Result<Vec<VisionSegment>, VisionError> {
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "vision.segment", "StreamSegment")
    })
    .await
    .map_err(VisionError::from)?;
    let image_bytes = base64_decode(&image).map_err(VisionError::InvalidInput)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        VisionError::Failed,
        |container, handle, spawned| {
            let result = container
                .segment(image_bytes)
                .map(|segments| {
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
                        .collect()
                })
                .map_err(|e| VisionError::Failed(e.to_string()));
            ensure_session_not_cancelled_or_terminate_spawned(state, &session_id, handle, spawned)
                .map_err(VisionError::Failed)?;
            result
        },
    )
    .await
}

async fn embedding_vector(
    state: &SharedState,
    session_id: String,
    text: String,
) -> Result<Vec<f64>, GenerationError> {
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "language.embed", "StreamEmbed")
    })
    .await
    .map_err(GenerationError::from)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        GenerationError::Failed,
        |container, handle, spawned| {
            let result = container
                .embed(&text)
                .map(|embedding| embedding.into_iter().map(f64::from).collect())
                .map_err(|e| GenerationError::Failed(e.to_string()));
            ensure_session_not_cancelled_or_terminate_spawned(state, &session_id, handle, spawned)
                .map_err(GenerationError::Failed)?;
            result
        },
    )
    .await
}

async fn stream_tokens(
    state: &SharedState,
    call: &mut dyn Call_StreamResponse,
    session_id: String,
    prompt: String,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    let instructions =
        apply_translation_hints(&resolved.use_case, resolved.instructions.clone(), &options);
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        GenerationError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut saw_token = false;
            let mut cancelled = false;

            let result = container.generate(Some(&instructions), &prompt, max_tokens, |token| {
                if cancelled || state.is_session_cancelled(&session_id) {
                    cancelled = true;
                    return;
                }
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
            if cancelled || state.is_session_cancelled(&session_id) {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(request_cancelled_reason()));
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
        },
    )
    .await
}

enum GenerationError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidOptions(String),
    InvalidInput(String),
    Failed(String),
    Reply(varlink::Error),
}

enum SpeechError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidInput(String),
    Failed(String),
    Reply(varlink::Error),
}

enum VisionError {
    SessionNotFound(String),
    ModelUnavailable(String),
    InvalidInput(String),
    Failed(String),
    Reply(varlink::Error),
}

impl From<ResolveSessionError> for GenerationError {
    fn from(error: ResolveSessionError) -> Self {
        match error {
            ResolveSessionError::SessionNotFound(id) => Self::SessionNotFound(id),
            ResolveSessionError::ModelUnavailable(reason) => Self::ModelUnavailable(reason),
            ResolveSessionError::InvalidInput(reason) => Self::InvalidInput(reason),
        }
    }
}

impl From<ResolveSessionError> for SpeechError {
    fn from(error: ResolveSessionError) -> Self {
        match error {
            ResolveSessionError::SessionNotFound(id) => Self::SessionNotFound(id),
            ResolveSessionError::ModelUnavailable(reason) => Self::ModelUnavailable(reason),
            ResolveSessionError::InvalidInput(reason) => Self::InvalidInput(reason),
        }
    }
}

impl From<ResolveSessionError> for VisionError {
    fn from(error: ResolveSessionError) -> Self {
        match error {
            ResolveSessionError::SessionNotFound(id) => Self::SessionNotFound(id),
            ResolveSessionError::ModelUnavailable(reason) => Self::ModelUnavailable(reason),
            ResolveSessionError::InvalidInput(reason) => Self::InvalidInput(reason),
        }
    }
}

enum LockContainerError {
    Retry,
    Failed(String),
}

async fn resolve_session_runtime(
    state: &SharedState,
    session_id: &str,
    validate_use_case: impl FnOnce(&str) -> Result<(), String>,
) -> Result<ResolvedSessionRuntime, ResolveSessionError> {
    let mut guard = state.0.lock().await;
    let resolved = match guard.sessions.get(session_id) {
        Some(session) => {
            validate_use_case(&session.use_case).map_err(ResolveSessionError::InvalidInput)?;
            let (profile_id, profile_epoch, runtime_id, image_refs, artifact_path, runtime_options) =
                profile_runtime(&guard, &session.profile_id)
                    .map_err(ResolveSessionError::ModelUnavailable)?;
            ResolvedSessionRuntime {
                app_id: session.app_id.clone(),
                use_case: session.use_case.clone(),
                profile_id,
                profile_epoch,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
                instructions: session.instructions.clone(),
            }
        }
        None => return Err(ResolveSessionError::SessionNotFound(session_id.to_string())),
    };
    let _ = guard
        .permissions
        .touch(&resolved.app_id, &resolved.use_case);
    Ok(resolved)
}

async fn model_container(
    state: &SharedState,
    session_id: &str,
    resolved: &ResolvedSessionRuntime,
) -> Result<(ContainerHandle, bool), String> {
    ensure_resolved_session_active(state, session_id, resolved).await?;
    let (container, spawned) = state
        .2
        .lock()
        .await
        .get_or_spawn_any_checked(
            &resolved.profile_id,
            resolved.profile_epoch,
            &resolved.runtime_id,
            &resolved.image_refs,
            &resolved.artifact_path,
            &resolved.runtime_options,
            |_| {},
            || {
                ensure_session_not_cancelled(state, session_id)
                    .and_then(|_| ensure_profile_epoch_current(state, resolved))
            },
        )
        .map_err(|e| e.to_string())?;
    if let Err(e) = ensure_resolved_session_active(state, session_id, resolved).await {
        if spawned || profile_is_missing(state, &resolved.profile_id).await {
            let mut containers = state.2.lock().await;
            containers.kill_handle(&resolved.profile_id, &container);
        }
        return Err(e);
    }
    Ok((container, spawned))
}

fn lock_container_for_session<'a>(
    state: &SharedState,
    session_id: &str,
    handle: &'a ContainerHandle,
    spawned: bool,
) -> Result<MutexGuard<'a, Container>, LockContainerError> {
    loop {
        ensure_session_not_cancelled_or_terminate_spawned(state, session_id, handle, spawned)
            .map_err(LockContainerError::Failed)?;
        ensure_handle_ready_for_request(handle, spawned)?;
        match handle.try_lock() {
            Ok(container) => return Ok(container),
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(LockContainerError::Failed(
                    "container mutex poisoned".to_string(),
                ));
            }
        }
    }
}

async fn with_locked_container<T, E>(
    state: &SharedState,
    session_id: &str,
    mut resolved: ResolvedSessionRuntime,
    map_failed: impl Fn(String) -> E,
    op: impl FnOnce(&mut Container, &ContainerHandle, bool) -> Result<T, E>,
) -> Result<T, E> {
    let mut op = Some(op);
    let expected_use_case = resolved.use_case.clone();
    loop {
        let (handle, spawned) = match model_container(state, session_id, &resolved).await {
            Ok(container) => container,
            Err(reason) if is_startup_finalizing_retry(&reason) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(reason) if is_wait_retry(&reason) => {
                resolved = refresh_resolved_session_runtime(state, session_id, &expected_use_case)
                    .await
                    .map_err(&map_failed)?;
                continue;
            }
            Err(reason) => return Err(map_failed(reason)),
        };
        let mut container = match lock_container_for_session(state, session_id, &handle, spawned) {
            Ok(container) => container,
            Err(LockContainerError::Retry) => continue,
            Err(LockContainerError::Failed(reason)) => return Err(map_failed(reason)),
        };

        ensure_session_not_cancelled_or_terminate_spawned(state, session_id, &handle, spawned)
            .map_err(&map_failed)?;
        match ensure_handle_ready_for_request(&handle, spawned) {
            Ok(()) => {}
            Err(LockContainerError::Retry) => continue,
            Err(LockContainerError::Failed(reason)) => return Err(map_failed(reason)),
        }
        if let Err(reason) = ensure_profile_epoch_current(state, &resolved) {
            if !is_wait_retry(&reason) {
                return Err(map_failed(reason));
            }
            drop(container);
            terminate_stale_handle(state, &resolved.profile_id, &handle).await;
            resolved = refresh_resolved_session_runtime(state, session_id, &expected_use_case)
                .await
                .map_err(&map_failed)?;
            continue;
        }
        if let Err(reason) = ensure_container_matches_resolved(&container, &resolved) {
            if !is_wait_retry(&reason) {
                return Err(map_failed(reason));
            }
            drop(container);
            terminate_stale_handle(state, &resolved.profile_id, &handle).await;
            resolved = refresh_resolved_session_runtime(state, session_id, &expected_use_case)
                .await
                .map_err(&map_failed)?;
            continue;
        }
        let _active =
            ActiveContainerRequest::new(state, &resolved.profile_id, session_id, handle.clone());
        ensure_session_not_cancelled_or_terminate_spawned(state, session_id, &handle, spawned)
            .map_err(&map_failed)?;
        if spawned {
            handle.publish();
        }

        let op = op.take().expect("container operation called once");
        return op(&mut container, &handle, spawned);
    }
}

fn is_startup_finalizing_retry(reason: &str) -> bool {
    reason.starts_with("container startup is being finalized for profile ")
        && reason.ends_with("; retry request")
}

fn is_wait_retry(reason: &str) -> bool {
    reason.ends_with("; retry request")
}

async fn terminate_stale_handle(state: &SharedState, profile_id: &str, handle: &ContainerHandle) {
    handle.terminate();
    let mut containers = state.2.lock().await;
    containers.kill_handle(profile_id, handle);
}

async fn refresh_resolved_session_runtime(
    state: &SharedState,
    session_id: &str,
    expected_use_case: &str,
) -> Result<ResolvedSessionRuntime, String> {
    resolve_session_runtime(state, session_id, |use_case| {
        if use_case == expected_use_case {
            Ok(())
        } else {
            Err("session use-case changed while request was waiting; retry request".to_string())
        }
    })
    .await
    .map_err(resolve_session_wait_error)
}

fn resolve_session_wait_error(error: ResolveSessionError) -> String {
    match error {
        ResolveSessionError::SessionNotFound(_) => request_cancelled_reason(),
        ResolveSessionError::ModelUnavailable(reason)
        | ResolveSessionError::InvalidInput(reason) => reason,
    }
}

fn ensure_container_matches_resolved(
    container: &Container,
    resolved: &ResolvedSessionRuntime,
) -> Result<(), String> {
    if container.matches_any_runtime(
        &resolved.runtime_id,
        resolved.profile_epoch,
        &resolved.image_refs,
        &resolved.artifact_path,
        &resolved.runtime_options,
    ) {
        Ok(())
    } else {
        Err("container runtime changed while request was waiting; retry request".to_string())
    }
}

fn ensure_profile_epoch_current(
    state: &SharedState,
    resolved: &ResolvedSessionRuntime,
) -> Result<(), String> {
    if state.current_profile_epoch(&resolved.profile_id) == resolved.profile_epoch {
        Ok(())
    } else {
        Err("container profile changed while request was waiting; retry request".to_string())
    }
}

fn ensure_handle_ready_for_request(
    handle: &ContainerHandle,
    spawned: bool,
) -> Result<(), LockContainerError> {
    if !handle.is_terminating() {
        return Ok(());
    }
    if spawned {
        Err(LockContainerError::Failed(
            "container was terminated before request started".to_string(),
        ))
    } else {
        Err(LockContainerError::Retry)
    }
}

async fn ensure_resolved_session_active(
    state: &SharedState,
    session_id: &str,
    resolved: &ResolvedSessionRuntime,
) -> Result<(), String> {
    ensure_session_not_cancelled(state, session_id)?;
    let guard = state.0.lock().await;
    match guard.sessions.get(session_id) {
        Some(session) if session.profile_id == resolved.profile_id => {
            let current_epoch = guard
                .profile_epochs
                .get(&resolved.profile_id)
                .copied()
                .unwrap_or_default();
            if current_epoch == resolved.profile_epoch {
                Ok(())
            } else {
                Err(
                    "container profile changed while request was waiting; retry request"
                        .to_string(),
                )
            }
        }
        _ => Err(request_cancelled_reason()),
    }
}

async fn profile_is_missing(state: &SharedState, profile_id: &str) -> bool {
    let guard = state.0.lock().await;
    guard.profiles.get(profile_id).is_none()
}

fn ensure_session_not_cancelled(state: &SharedState, session_id: &str) -> Result<(), String> {
    if state.is_session_cancelled(session_id) {
        Err(request_cancelled_reason())
    } else {
        Ok(())
    }
}

fn ensure_session_not_cancelled_or_terminate_spawned(
    state: &SharedState,
    session_id: &str,
    handle: &ContainerHandle,
    spawned: bool,
) -> Result<(), String> {
    if let Err(reason) = ensure_session_not_cancelled(state, session_id) {
        if spawned {
            handle.terminate();
        }
        Err(reason)
    } else {
        Ok(())
    }
}

async fn kill_profile_if_session_active(state: &SharedState, profile_id: &str, session_id: &str) {
    let handles = state.terminate_active_container_handles(profile_id, session_id);
    if handles.is_empty() {
        return;
    }
    let mut containers = state.2.lock().await;
    for handle in handles {
        containers.kill_handle(profile_id, &handle);
    }
}

fn request_cancelled_reason() -> String {
    "container returned error request_cancelled: session was closed".to_string()
}

trait TextStreamCall {
    fn wants_more(&self) -> bool;
    fn set_continues(&mut self, continues: bool);
    fn reply_token(&mut self, token: String) -> varlink::Result<()>;
}

impl TextStreamCall for dyn Call_StreamDescribe + '_ {
    fn wants_more(&self) -> bool {
        let call: &dyn Call_StreamDescribe = self;
        call.wants_more()
    }

    fn set_continues(&mut self, continues: bool) {
        let call: &mut dyn Call_StreamDescribe = self;
        call.set_continues(continues);
    }

    fn reply_token(&mut self, token: String) -> varlink::Result<()> {
        self.reply(token)
    }
}

impl TextStreamCall for dyn Call_StreamOcr + '_ {
    fn wants_more(&self) -> bool {
        let call: &dyn Call_StreamOcr = self;
        call.wants_more()
    }

    fn set_continues(&mut self, continues: bool) {
        let call: &mut dyn Call_StreamOcr = self;
        call.set_continues(continues);
    }

    fn reply_token(&mut self, token: String) -> varlink::Result<()> {
        self.reply(token)
    }
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

fn reply_guided_failure(call: &mut dyn VarlinkCallError, reason: String) -> varlink::Result<()> {
    match runtime_error_code(&reason) {
        Some("request_cancelled") => call.reply_request_cancelled(reason),
        _ => call.reply_guided_generation_failed(reason),
    }
}

fn runtime_error_code(reason: &str) -> Option<&str> {
    reason
        .strip_prefix("container returned error ")
        .and_then(|rest| rest.split_once(':'))
        .map(|(code, _)| code.trim())
}

async fn predict_next_completions(
    state: &SharedState,
    session_id: String,
    prefix: String,
    options: GenerationOptions,
) -> Result<Vec<String>, GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let generation = state.begin_predict_next(&session_id);
    let resolved = match resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "language.complete", "StreamPredictNext")
    })
    .await
    {
        Ok(resolved) => resolved,
        Err(ResolveSessionError::SessionNotFound(id)) => {
            state.clear_predict_next(&session_id);
            return Err(GenerationError::SessionNotFound(id));
        }
        Err(error) => return Err(GenerationError::from(error)),
    };

    let result = with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        GenerationError::Failed,
        |container, _handle, _spawned| {
            container
                .predict_next(&prefix, max_tokens, options.temperature)
                .map_err(|e| GenerationError::Failed(e.to_string()))
        },
    )
    .await;

    if state.is_session_cancelled(&session_id) {
        return Err(GenerationError::Failed(request_cancelled_reason()));
    }
    if !state.is_current_predict_next(&session_id, generation) {
        return Err(GenerationError::Failed(
            "container returned error request_cancelled: superseded by newer StreamPredictNext request"
                .to_string(),
        ));
    }

    result
}

async fn stream_guided_snapshots(
    state: &SharedState,
    call: &mut dyn Call_StreamRespondGuided,
    session_id: String,
    prompt: String,
    fields: Vec<GuidedField>,
    tools: Vec<ToolDefinition>,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        GenerationError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_snapshot: Option<String> = None;
            let mut pending_tool_calls: Option<Vec<ToolCall>> = None;
            let mut final_snapshot = String::new();
            let mut emitted_tool_calls = false;
            let mut reply_error: Option<varlink::Error> = None;
            let mut cancelled = false;
            let tools = tools.into_iter().map(varlink_tool_definition).collect();
            let result = container.stream_structured(
                Some(&resolved.instructions),
                &prompt,
                max_tokens,
                &schema,
                tools,
                Vec::new(),
                |snapshot, tool_calls, done| {
                    if cancelled || state.is_session_cancelled(&session_id) {
                        cancelled = true;
                        return;
                    }
                    if !snapshot.is_empty() {
                        final_snapshot = snapshot.clone();
                    }
                    if !wants_more {
                        if tool_calls.is_empty() {
                            pending_snapshot = Some(snapshot);
                        } else {
                            pending_tool_calls = Some(varlink_tool_calls(tool_calls));
                        }
                        return;
                    }
                    if reply_error.is_some() {
                        return;
                    }
                    if !tool_calls.is_empty() {
                        call.set_continues(false);
                        emitted_tool_calls = true;
                        if let Err(e) = call.reply(String::new(), varlink_tool_calls(tool_calls)) {
                            reply_error = Some(e);
                        }
                        return;
                    }
                    if let Some(previous) = pending_snapshot.replace(snapshot) {
                        call.set_continues(true);
                        if let Err(e) = call.reply(previous, Vec::new()) {
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
            if cancelled || state.is_session_cancelled(&session_id) {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(request_cancelled_reason()));
            }
            if let Err(e) = result {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(e.to_string()));
            }
            if final_snapshot.is_empty() && !emitted_tool_calls && pending_tool_calls.is_none() {
                return Err(GenerationError::Failed(
                    "model returned no guided snapshots".to_string(),
                ));
            }
            if wants_more {
                call.set_continues(false);
            }
            if let Some(tool_calls) = pending_tool_calls {
                call.reply(String::new(), tool_calls)
                    .map_err(GenerationError::Reply)
            } else if emitted_tool_calls {
                Ok(())
            } else {
                call.reply(pending_snapshot.unwrap_or(final_snapshot), Vec::new())
                    .map_err(GenerationError::Reply)
            }
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stream_guided_tool_results(
    state: &SharedState,
    call: &mut dyn Call_StreamSubmitToolResultsGuided,
    session_id: String,
    prompt: String,
    results: Vec<ToolResult>,
    fields: Vec<GuidedField>,
    tools: Vec<ToolDefinition>,
    options: GenerationOptions,
) -> Result<(), GenerationError> {
    let max_tokens = validate_options(&options).map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    with_locked_container(
        state,
        &session_id,
        resolved.clone(),
        GenerationError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_snapshot: Option<String> = None;
            let mut pending_tool_calls: Option<Vec<ToolCall>> = None;
            let mut final_snapshot = String::new();
            let mut emitted_tool_calls = false;
            let mut reply_error: Option<varlink::Error> = None;
            let mut cancelled = false;
            let tools = tools.into_iter().map(varlink_tool_definition).collect();
            let tool_results = results.into_iter().map(varlink_tool_result).collect();
            let result = container.stream_structured(
                Some(&resolved.instructions),
                &prompt,
                max_tokens,
                &schema,
                tools,
                tool_results,
                |snapshot, tool_calls, done| {
                    if cancelled || state.is_session_cancelled(&session_id) {
                        cancelled = true;
                        return;
                    }
                    if !snapshot.is_empty() {
                        final_snapshot = snapshot.clone();
                    }
                    if !wants_more {
                        if tool_calls.is_empty() {
                            pending_snapshot = Some(snapshot);
                        } else {
                            pending_tool_calls = Some(varlink_tool_calls(tool_calls));
                        }
                        return;
                    }
                    if reply_error.is_some() {
                        return;
                    }
                    if !tool_calls.is_empty() {
                        call.set_continues(false);
                        emitted_tool_calls = true;
                        if let Err(e) = call.reply(String::new(), varlink_tool_calls(tool_calls)) {
                            reply_error = Some(e);
                        }
                        return;
                    }
                    if let Some(previous) = pending_snapshot.replace(snapshot) {
                        call.set_continues(true);
                        if let Err(e) = call.reply(previous, Vec::new()) {
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
            if cancelled || state.is_session_cancelled(&session_id) {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(request_cancelled_reason()));
            }
            if let Err(e) = result {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(e.to_string()));
            }
            if final_snapshot.is_empty() && !emitted_tool_calls && pending_tool_calls.is_none() {
                return Err(GenerationError::Failed(
                    "model returned no guided snapshots".to_string(),
                ));
            }
            if wants_more {
                call.set_continues(false);
            }
            if let Some(tool_calls) = pending_tool_calls {
                call.reply(String::new(), tool_calls)
                    .map_err(GenerationError::Reply)
            } else if emitted_tool_calls {
                Ok(())
            } else {
                call.reply(pending_snapshot.unwrap_or(final_snapshot), Vec::new())
                    .map_err(GenerationError::Reply)
            }
        },
    )
    .await
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
    let candidates = resolve_runtime_candidates(guard, profile);
    if candidates.is_empty() {
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
    let runtime_options = profile_runtime_options(profile);
    Ok((
        profile.profile_id.clone(),
        guard
            .profile_epochs
            .get(profile_id)
            .copied()
            .unwrap_or_default(),
        profile.runtime_id.clone(),
        candidates,
        profile.artifact_path.clone(),
        runtime_options,
    ))
}

fn profile_runtime_options(profile: &crate::profiles::Profile) -> HashMap<String, String> {
    let llmfit_model_id = profile_llmfit_model_id(profile);
    let mut runtime_options = profile.runtime_options.clone();
    if crate::container::runtime_supports_llama_runtime_options(&profile.runtime_id) {
        crate::llmfit_metadata::apply_llama_runtime_options(
            &llmfit_model_id,
            &mut runtime_options,
            &crate::llmfit_metadata::detect_system(),
        );
    }
    runtime_options
}

fn profile_llmfit_model_id(profile: &crate::profiles::Profile) -> String {
    if !profile.llmfit_model_id.trim().is_empty() {
        return profile.llmfit_model_id.clone();
    }
    crate::manifests::list_catalog_profiles()
        .ok()
        .and_then(|profiles| {
            profiles
                .into_iter()
                .find(|candidate| candidate.profile_id == profile.profile_id)
        })
        .map(|candidate| candidate.llmfit_model_id)
        .unwrap_or_default()
}

fn resolve_runtime_candidates(
    guard: &crate::state::Inner,
    profile: &crate::profiles::Profile,
) -> Vec<RuntimeCandidate> {
    let manifest_candidates = guard
        .runtimes
        .resolve_runtime_candidates(&profile.runtime_id, guard.variant);
    if !manifest_candidates.is_empty() {
        return manifest_candidates;
    }

    profile.runtime_candidates(guard.variant)
}

fn runtime_missing_reason(candidates: &[RuntimeCandidate]) -> String {
    match candidates {
        [] => "no runtime image candidates resolved".to_string(),
        [candidate] => {
            format!(
                "runtime rootfs for {} ({}) is not present in the user or system OCI store",
                candidate.image_ref,
                candidate.variant.as_tag()
            )
        }
        _ => format!(
            "none of the runtime rootfs candidates are present in the user or system OCI store: {}",
            candidates
                .iter()
                .map(|candidate| format!(
                    "{} ({})",
                    candidate.image_ref,
                    candidate.variant.as_tag()
                ))
                .collect::<Vec<_>>()
                .join(", ")
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
    match use_case {
        "language.summarize" | "language.translate" | "language.rephrase" | "language.classify"
        | "language.extract" | "language.analyze" => Ok(()),
        _ => Err(format!(
            "full text generation requires a language generation use-case, got {use_case}"
        )),
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

/// Validate the session use-case for the StreamTranscribe method and return the
/// whisper task to run: "transcribe" (verbatim) or "translate" (to English).
fn ensure_speech_use_case(use_case: &str) -> Result<&'static str, String> {
    match use_case {
        "speech.transcribe" => Ok("transcribe"),
        "speech.translate" => Ok("translate"),
        other => Err(format!(
            "StreamTranscribe requires use-case speech.transcribe or speech.translate, got {other}"
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
    fn supported_use_case_catalog_accepts_public_tokens() {
        assert!(is_supported_use_case("language.summarize"));
        assert!(is_supported_use_case("speech.translate"));
        assert!(is_supported_use_case("vision.segment"));
    }

    #[test]
    fn supported_use_case_catalog_rejects_unknown_same_prefix_tokens() {
        assert!(!is_supported_use_case("language.typo"));
        assert!(!is_supported_use_case("speech.listen"));
        assert!(!is_supported_use_case("vision.caption"));
    }

    #[test]
    fn language_generation_is_limited_to_language_use_cases() {
        assert!(ensure_language_generation_use_case("language.extract").is_ok());
        assert_eq!(
            ensure_language_generation_use_case("vision.describe"),
            Err(
                "full text generation requires a language generation use-case, got vision.describe"
                    .to_string()
            )
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
            Err(
                "full text generation requires a language generation use-case, got language.embed"
                    .to_string()
            )
        );
    }

    #[test]
    fn language_generation_excludes_completion_use_case() {
        assert_eq!(
            ensure_language_generation_use_case("language.complete"),
            Err(
                "full text generation requires a language generation use-case, got language.complete"
                    .to_string()
            )
        );
    }

    #[test]
    fn predict_next_requires_completion_use_case() {
        assert!(
            ensure_exact_use_case(
                "language.complete",
                "language.complete",
                "StreamPredictNext"
            )
            .is_ok()
        );
        assert_eq!(
            ensure_exact_use_case(
                "language.rephrase",
                "language.complete",
                "StreamPredictNext"
            ),
            Err(
                "StreamPredictNext requires use-case language.complete, got language.rephrase"
                    .to_string()
            )
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

    #[test]
    fn vision_ocr_allows_empty_output() {
        assert!(vision_text_allows_empty_output("vision.ocr"));
        assert!(!vision_text_allows_empty_output("vision.describe"));
    }

    #[test]
    fn profile_runtime_options_do_not_apply_llama_options_to_other_runtimes() {
        let profile = crate::profiles::Profile {
            profile_id: "test-profile".to_string(),
            model_id: "test-model".to_string(),
            llmfit_model_id: "tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf".to_string(),
            runtime_id: "not-a-llama-runtime".to_string(),
            runtime_options: std::collections::HashMap::new(),
            artifact_path: PathBuf::from("/tmp/test-model.gguf"),
            runtime_images: Vec::new(),
            use_cases: vec!["language.generate".to_string()],
            specializations: Vec::new(),
            artifact_hashes: Vec::new(),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            source: "test".to_string(),
        };

        let runtime_options = profile_runtime_options(&profile);

        assert!(!runtime_options.contains_key("N_CTX"));
        assert!(!runtime_options.contains_key("N_GPU_LAYERS"));
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
