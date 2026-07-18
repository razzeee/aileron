/// Varlink handler for `aileron.Inference`.
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{
    Arc, MutexGuard, TryLockError,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use uuid::Uuid;

use crate::container::{Container, ContainerHandle, InputMessage, InputPart};
use crate::observability::{self, ObservabilityFailure};
use crate::profiles::{ArtifactHash, RuntimeCandidate};
use crate::request_execution::{self, ActiveContainerRequest, RequestCancellation};
use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects.
use aileron_varlink::aileron_Inference::{
    Call_CreateSession, Call_EndSession, Call_GetUseCaseAvailability, Call_Prewarm,
    Call_StreamDepth, Call_StreamDescribe, Call_StreamDetect, Call_StreamEmbed, Call_StreamOcr,
    Call_StreamRespondGuided, Call_StreamResponse, Call_StreamSegment,
    Call_StreamSubmitToolResultsGuided, Call_StreamTranscribe, EmbedOptions, GuidedField,
    GuidedOptions, ModelAvailability, ResponseOptions, SpeechOptions, ToolCall, ToolDefinition,
    ToolResult, VarlinkCallError, VarlinkInterface, VisionBoxPrompt, VisionDepthMap,
    VisionDetection, VisionMask, VisionOptions, VisionPointPrompt, VisionSegmentOptions,
};

pub struct InferenceHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

type ProfileRuntime = (
    String,
    u64,
    String,
    String,
    Vec<ArtifactHash>,
    String,
    Vec<RuntimeCandidate>,
    PathBuf,
    HashMap<String, String>,
);

const STREAM_RESPONSE_MEDIA_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ResolvedSessionRuntime {
    app_id: String,
    use_case: String,
    profile_id: String,
    profile_epoch: u64,
    model_id: String,
    installed_at: String,
    artifact_hashes: Vec<ArtifactHash>,
    runtime_id: String,
    image_refs: Vec<RuntimeCandidate>,
    artifact_path: PathBuf,
    runtime_options: HashMap<String, String>,
    instructions: String,
}

struct EmbeddingResult {
    embedding: Vec<f64>,
    pipeline_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestExecutionMode {
    Interactive,
    Background,
}

impl RequestExecutionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }
}

struct InteractiveExecution<'a> {
    state: &'a SharedState,
    profile_id: String,
    active: bool,
}

impl Drop for InteractiveExecution<'_> {
    fn drop(&mut self) {
        if self.active {
            self.state.end_interactive_execution(&self.profile_id);
        } else {
            self.state
                .cancel_pending_interactive_execution(&self.profile_id);
        }
    }
}

struct BackgroundExecution<'a> {
    state: &'a SharedState,
    profile_id: String,
    handle: ContainerHandle,
    preempted: Arc<AtomicBool>,
}

impl BackgroundExecution<'_> {
    fn was_preempted(&self) -> bool {
        self.preempted.load(Ordering::SeqCst)
    }
}

impl Drop for BackgroundExecution<'_> {
    fn drop(&mut self) {
        self.state
            .end_background_execution(&self.profile_id, &self.handle);
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
                "Prewarm",
                &self.state,
                &session_id,
                resolved.clone(),
                RequestExecutionMode::Interactive,
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
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_tokens(
                &self.state,
                call,
                session_id,
                input_json,
                media_paths,
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
                Err(GenerationError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(GenerationError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_transcribe(
        &self,
        call: &mut dyn Call_StreamTranscribe,
        session_id: String,
        audio_path: String,
        options: SpeechOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_transcription(&self.state, call, session_id, audio_path, options).await {
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
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_vision_text(
                &self.state,
                call,
                session_id,
                image_path,
                instructions,
                options,
                "vision.describe",
            )
            .await
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
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_vision_text(
                &self.state,
                call,
                session_id,
                image_path,
                instructions,
                options,
                "vision.ocr",
            )
            .await
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

    fn stream_detect(
        &self,
        call: &mut dyn Call_StreamDetect,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match vision_detections(&self.state, session_id, image_path, instructions, options)
                .await
            {
                Ok(detections) => call.reply(detections),
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
        image_path: String,
        instructions: String,
        options: VisionSegmentOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match vision_masks(&self.state, session_id, image_path, instructions, options).await {
                Ok(masks) => call.reply(masks),
                Err(VisionError::SessionNotFound(id)) => call.reply_session_not_found(id),
                Err(VisionError::ModelUnavailable(reason)) => call.reply_model_unavailable(reason),
                Err(VisionError::InvalidInput(reason)) => call.reply_invalid_input(reason),
                Err(VisionError::Failed(reason)) => reply_generation_failure(call, reason),
                Err(VisionError::Reply(e)) => Err(e),
            }
        })
    }

    fn stream_depth(
        &self,
        call: &mut dyn Call_StreamDepth,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match vision_depth(&self.state, session_id, image_path, instructions, options).await {
                Ok(depth) => call.reply(depth),
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
        options: EmbedOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match embedding_vector(&self.state, session_id, text, options).await {
                Ok(result) => call.reply(result.embedding, result.pipeline_id),
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
        media_paths: Vec<String>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_guided_snapshots(
                &self.state,
                call,
                GuidedStreamRequest {
                    session_id,
                    prompt,
                    media_paths,
                    fields,
                    tools,
                    options,
                },
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
        media_paths: Vec<String>,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            match stream_guided_tool_results(
                &self.state,
                call,
                session_id,
                prompt,
                media_paths,
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
            let (profile_id, app_id, use_case) = {
                let mut guard = self.state.0.lock().await;
                let Some(session) = guard.sessions.remove(&session_id) else {
                    return call.reply_session_not_found(session_id);
                };
                request_execution::mark_session_closed(&self.state, &session_id);
                (session.profile_id, session.app_id, session.use_case)
            };
            request_execution::terminate_active_container_handles_for_session(
                &self.state,
                &profile_id,
                &session_id,
            )
            .await;
            observability::log_session_ended(observability::SessionFields {
                session_id: &session_id,
                app_id: &app_id,
                use_case: &use_case,
                profile_id: &profile_id,
            });
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
    observability::log_session_created(observability::SessionFields {
        session_id: &session.session_id,
        app_id: &session.app_id,
        use_case: &session.use_case,
        profile_id: &session.profile_id,
    });
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
    audio_path: String,
    options: SpeechOptions,
) -> Result<(), SpeechError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(SpeechError::InvalidInput)?;
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_speech_use_case(use_case).map(|_| ())
    })
    .await
    .map_err(SpeechError::from)?;
    let task = ensure_speech_use_case(&resolved.use_case).map_err(SpeechError::InvalidInput)?;
    let audio_bytes = read_media_path(&audio_path).map_err(SpeechError::InvalidInput)?;
    with_locked_container(
        "StreamTranscribe",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        SpeechError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut cancelled = false;

            let result = container.stream_transcribe(
                audio_bytes,
                Some(&options.source_language_hint),
                task,
                execution_mode.as_str(),
                |token| {
                    if cancelled
                        || RequestCancellation::for_session(state, &session_id).is_cancelled()
                    {
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
            if cancelled || RequestCancellation::for_session(state, &session_id).is_cancelled() {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(SpeechError::Failed(
                    request_execution::request_cancelled_reason(),
                ));
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
    image_path: String,
    instructions: String,
    options: VisionOptions,
    expected_use_case: &str,
) -> Result<(), VisionError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(VisionError::InvalidInput)?;
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
    let image_bytes = read_media_path(&image_path).map_err(VisionError::InvalidInput)?;
    with_locked_container(
        method,
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        VisionError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut saw_token = false;
            let mut cancelled = false;

            let result = if expected_use_case == "vision.describe" {
                container.stream_describe(
                    image_bytes,
                    &instructions,
                    execution_mode.as_str(),
                    |token| {
                        if cancelled
                            || RequestCancellation::for_session(state, &session_id).is_cancelled()
                        {
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
                    },
                )
            } else {
                container.stream_ocr(
                    image_bytes,
                    &instructions,
                    execution_mode.as_str(),
                    |token| {
                        if cancelled
                            || RequestCancellation::for_session(state, &session_id).is_cancelled()
                        {
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
                    },
                )
            };

            if let Some(e) = reply_error {
                return Err(VisionError::Reply(e));
            }
            if cancelled || RequestCancellation::for_session(state, &session_id).is_cancelled() {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(VisionError::Failed(
                    request_execution::request_cancelled_reason(),
                ));
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

async fn vision_detections(
    state: &SharedState,
    session_id: String,
    image_path: String,
    instructions: String,
    options: VisionOptions,
) -> Result<Vec<VisionDetection>, VisionError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(VisionError::InvalidInput)?;
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "vision.detect", "StreamDetect")
    })
    .await
    .map_err(VisionError::from)?;
    let image_bytes = read_media_path(&image_path).map_err(VisionError::InvalidInput)?;
    with_locked_container(
        "StreamDetect",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        VisionError::Failed,
        |container, handle, spawned| {
            let result = container
                .detect(image_bytes, &instructions, execution_mode.as_str())
                .map(|detections| {
                    detections
                        .into_iter()
                        .map(|detection| VisionDetection {
                            label: detection.label,
                            confidence: detection.confidence,
                            x: detection.x,
                            y: detection.y,
                            width: detection.width,
                            height: detection.height,
                        })
                        .collect()
                })
                .map_err(|e| VisionError::Failed(e.to_string()));
            RequestCancellation::for_session(state, &session_id)
                .ensure_not_cancelled_or_terminate_spawned(handle, spawned)
                .map_err(VisionError::Failed)?;
            result
        },
    )
    .await
}

async fn vision_masks(
    state: &SharedState,
    session_id: String,
    image_path: String,
    instructions: String,
    options: VisionSegmentOptions,
) -> Result<Vec<VisionMask>, VisionError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(VisionError::InvalidInput)?;
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "vision.segment", "StreamSegment")
    })
    .await
    .map_err(VisionError::from)?;
    let image_bytes = read_media_path(&image_path).map_err(VisionError::InvalidInput)?;
    let points = options
        .points
        .into_iter()
        .map(|point| crate::container::VisionPointPrompt {
            x: point.x,
            y: point.y,
            positive: point.positive,
        })
        .collect::<Vec<_>>();
    let boxes = options
        .boxes
        .into_iter()
        .map(|bbox| crate::container::VisionBoxPrompt {
            x: bbox.x,
            y: bbox.y,
            width: bbox.width,
            height: bbox.height,
        })
        .collect::<Vec<_>>();
    with_locked_container(
        "StreamSegment",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        VisionError::Failed,
        |container, handle, spawned| {
            let result = container
                .segment(
                    image_bytes,
                    &instructions,
                    execution_mode.as_str(),
                    points,
                    boxes,
                )
                .map(|masks| {
                    masks
                        .into_iter()
                        .map(|mask| VisionMask {
                            label: mask.label,
                            confidence: mask.confidence,
                            x: mask.x,
                            y: mask.y,
                            width: mask.width,
                            height: mask.height,
                            mask_base64: mask.mask_base64,
                            mask_width: mask.mask_width,
                            mask_height: mask.mask_height,
                        })
                        .collect()
                })
                .map_err(|e| VisionError::Failed(e.to_string()));
            RequestCancellation::for_session(state, &session_id)
                .ensure_not_cancelled_or_terminate_spawned(handle, spawned)
                .map_err(VisionError::Failed)?;
            result
        },
    )
    .await
}

async fn vision_depth(
    state: &SharedState,
    session_id: String,
    image_path: String,
    instructions: String,
    options: VisionOptions,
) -> Result<VisionDepthMap, VisionError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(VisionError::InvalidInput)?;
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "vision.depth", "StreamDepth")
    })
    .await
    .map_err(VisionError::from)?;
    let image_bytes = read_media_path(&image_path).map_err(VisionError::InvalidInput)?;
    with_locked_container(
        "StreamDepth",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        VisionError::Failed,
        |container, handle, spawned| {
            let result = container
                .depth(image_bytes, &instructions, execution_mode.as_str())
                .map(|depth| VisionDepthMap {
                    width: depth.width,
                    height: depth.height,
                    values: depth.values,
                    minimum: depth.minimum,
                    maximum: depth.maximum,
                })
                .map_err(|e| VisionError::Failed(e.to_string()));
            RequestCancellation::for_session(state, &session_id)
                .ensure_not_cancelled_or_terminate_spawned(handle, spawned)
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
    options: EmbedOptions,
) -> Result<EmbeddingResult, GenerationError> {
    let execution_mode =
        parse_execution_mode(&options.execution_mode).map_err(GenerationError::InvalidInput)?;
    let resolved = resolve_session_runtime(state, &session_id, |use_case| {
        ensure_exact_use_case(use_case, "language.embed", "StreamEmbed")
    })
    .await
    .map_err(GenerationError::from)?;
    let (embedding, pipeline_id): (Vec<f64>, String) = with_locked_container(
        "StreamEmbed",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        GenerationError::Failed,
        |container, handle, spawned| {
            let pipeline_id = embedding_pipeline_id(&resolved, container);
            let result = container
                .embed(&text, execution_mode.as_str())
                .map(|embedding| (embedding.into_iter().map(f64::from).collect(), pipeline_id))
                .map_err(|e| GenerationError::Failed(e.to_string()));
            RequestCancellation::for_session(state, &session_id)
                .ensure_not_cancelled_or_terminate_spawned(handle, spawned)
                .map_err(GenerationError::Failed)?;
            result
        },
    )
    .await?;
    Ok(EmbeddingResult {
        embedding,
        pipeline_id,
    })
}

fn normalize_stream_input(
    input_json: &str,
    media_paths: &[String],
) -> Result<Vec<InputMessage>, String> {
    let value: Value =
        serde_json::from_str(input_json).map_err(|e| format!("invalid input_json: {e}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "input_json must be a JSON array".to_string())?;
    if items.is_empty() {
        return Err("input_json must not be empty".to_string());
    }

    let first = items[0]
        .as_object()
        .ok_or_else(|| "top-level entries must be objects".to_string())?;
    let is_message_array = first.contains_key("role") || first.contains_key("content");

    if is_message_array {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| normalize_message(item, media_paths, index))
            .collect()
    } else {
        Ok(vec![InputMessage {
            role: "user".to_string(),
            content: items
                .iter()
                .enumerate()
                .map(|(index, item)| normalize_part(item, media_paths, index))
                .collect::<Result<Vec<_>, _>>()?,
        }])
    }
}

fn normalize_guided_input(
    prompt: &str,
    media_paths: &[String],
) -> Result<Option<Vec<InputMessage>>, String> {
    if !prompt.trim_start().starts_with('[') {
        if media_paths.is_empty() {
            return Ok(None);
        }
        return Err(
            "guided media requires prompt to be content-part or role-message JSON".to_string(),
        );
    }

    match normalize_stream_input(prompt, media_paths) {
        Ok(input) => Ok(Some(input)),
        Err(_error) if media_paths.is_empty() => Ok(None),
        Err(error) => Err(error),
    }
}

fn normalize_message(
    value: &Value,
    media_paths: &[String],
    index: usize,
) -> Result<InputMessage, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("message {index} must be an object"))?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("message {index} is missing role"))?;
    if !matches!(role, "system" | "user" | "assistant" | "tool") {
        return Err(format!("message {index} has unsupported role {role}"));
    }
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("message {index} content must be an array"))?;
    if content.is_empty() {
        return Err(format!("message {index} content must not be empty"));
    }

    Ok(InputMessage {
        role: role.to_string(),
        content: content
            .iter()
            .enumerate()
            .map(|(part_index, part)| normalize_part(part, media_paths, part_index))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn normalize_part(
    value: &Value,
    media_paths: &[String],
    index: usize,
) -> Result<InputPart, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("content part {index} must be an object"))?;
    if object.contains_key("role") || object.contains_key("content") {
        return Err("top-level input must not mix messages and content parts".to_string());
    }
    let part_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("content part {index} is missing type"))?;

    match part_type {
        "input_text" => Ok(InputPart::InputText {
            text: non_empty_string(object, "text", index)?,
        }),
        "output_text" => Ok(InputPart::OutputText {
            text: non_empty_string(object, "text", index)?,
        }),
        "input_image" => media_part(object, media_paths, index, "image/", |data, mime_type| {
            InputPart::InputImage {
                image: data,
                mime_type,
            }
        }),
        "input_audio" => media_part(object, media_paths, index, "audio/", |data, mime_type| {
            InputPart::InputAudio {
                audio: data,
                mime_type,
            }
        }),
        other => Err(format!("content part {index} has unsupported type {other}")),
    }
}

fn non_empty_string(
    object: &Map<String, Value>,
    key: &str,
    index: usize,
) -> Result<String, String> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("content part {index} is missing {key}"))?;
    if value.is_empty() {
        return Err(format!("content part {index} {key} must not be empty"));
    }
    Ok(value.to_string())
}

fn media_part(
    object: &Map<String, Value>,
    media_paths: &[String],
    index: usize,
    mime_prefix: &str,
    build: impl FnOnce(String, String) -> InputPart,
) -> Result<InputPart, String> {
    let fd_index = object
        .get("fd_index")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("content part {index} is missing fd_index"))?
        as usize;
    let mime_type = non_empty_string(object, "mime_type", index)?;
    if !mime_type.starts_with(mime_prefix) {
        return Err(format!(
            "content part {index} has unsupported MIME type {mime_type}"
        ));
    }
    let path = media_paths
        .get(fd_index)
        .ok_or_else(|| format!("content part {index} fd_index {fd_index} is out of range"))?;
    let data = read_stream_response_media_path(path, fd_index)?;
    Ok(build(base64_encode(&data), mime_type))
}

fn read_stream_response_media_path(path: &str, fd_index: usize) -> Result<Vec<u8>, String> {
    if path.trim().is_empty() {
        return Err(format!("media fd_index {fd_index} path must not be empty"));
    }

    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to read media fd_index {fd_index}: {e}"))?;
    let mut reader = file.take(STREAM_RESPONSE_MEDIA_MAX_BYTES + 1);
    let mut data = Vec::new();
    reader
        .read_to_end(&mut data)
        .map_err(|e| format!("failed to read media fd_index {fd_index}: {e}"))?;

    if data.len() as u64 > STREAM_RESPONSE_MEDIA_MAX_BYTES {
        return Err(format!(
            "media fd_index {fd_index} exceeds maximum size of {STREAM_RESPONSE_MEDIA_MAX_BYTES} bytes"
        ));
    }

    Ok(data)
}

fn render_text_prompt(input: &[InputMessage]) -> String {
    if input.len() == 1 && input[0].role == "user" {
        return render_text_content(&input[0]);
    }

    let mut out = String::new();
    for (index, message) in input.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&message.role);
        out.push_str(": ");
        push_text_content(&mut out, message);
    }
    out
}

fn render_text_content(message: &InputMessage) -> String {
    let mut out = String::new();
    push_text_content(&mut out, message);
    out
}

fn push_text_content(out: &mut String, message: &InputMessage) {
    let mut wrote_text = false;
    for part in &message.content {
        let text = match part {
            InputPart::InputText { text } | InputPart::OutputText { text } => text,
            InputPart::InputImage { .. } | InputPart::InputAudio { .. } => continue,
        };
        if wrote_text {
            out.push('\n');
        }
        out.push_str(text);
        wrote_text = true;
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

async fn stream_tokens(
    state: &SharedState,
    call: &mut dyn Call_StreamResponse,
    session_id: String,
    input_json: String,
    media_paths: Vec<String>,
    options: ResponseOptions,
) -> Result<(), GenerationError> {
    let (max_tokens, execution_mode) = validate_token_options(
        options.maximum_response_tokens,
        options.temperature,
        &options.execution_mode,
    )
    .map_err(GenerationError::InvalidOptions)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    let input =
        normalize_stream_input(&input_json, &media_paths).map_err(GenerationError::InvalidInput)?;
    let prompt = render_text_prompt(&input);
    let instructions =
        apply_translation_hints(&resolved.use_case, resolved.instructions.clone(), &options);
    with_locked_container(
        "StreamResponse",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
        GenerationError::Failed,
        |container, _handle, _spawned| {
            let wants_more = call.wants_more();
            let mut pending_token: Option<String> = None;
            let mut reply_error: Option<varlink::Error> = None;
            let mut saw_token = false;
            let mut cancelled = false;

            macro_rules! generate_with_instructions {
                ($instructions:expr) => {
                    container.generate(Some($instructions), &prompt, Some(&input), max_tokens, execution_mode.as_str(), |token| {
                        if cancelled
                            || RequestCancellation::for_session(state, &session_id).is_cancelled()
                        {
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
                    })
                };
            }

            let mut result = generate_with_instructions!(&instructions);
            if result.is_ok() && !saw_token && reply_error.is_none() && !cancelled {
                let retry_instructions = format!(
                    "{instructions}\nYou must produce a non-empty plain-text response. Do not return an empty answer."
                );
                result = generate_with_instructions!(&retry_instructions);
            }

            if let Some(e) = reply_error {
                return Err(GenerationError::Reply(e));
            }
            if cancelled || RequestCancellation::for_session(state, &session_id).is_cancelled() {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(
                    request_execution::request_cancelled_reason(),
                ));
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

impl ObservabilityFailure for String {
    fn observability_summary(&self) -> observability::FailureSummary {
        observability::FailureSummary {
            code: observability::inference_failure_code(self),
            reason_len: self.len(),
        }
    }
}

impl ObservabilityFailure for GenerationError {
    fn observability_summary(&self) -> observability::FailureSummary {
        let code = match self {
            Self::SessionNotFound(_) => "session_not_found",
            Self::ModelUnavailable(_) => "model_unavailable",
            Self::InvalidOptions(_) => "invalid_generation_options",
            Self::InvalidInput(_) => "invalid_input",
            Self::Failed(reason) => observability::inference_failure_code(reason),
            Self::Reply(_) => "reply_failed",
        };
        let reason_len = match self {
            Self::SessionNotFound(reason)
            | Self::ModelUnavailable(reason)
            | Self::InvalidOptions(reason)
            | Self::InvalidInput(reason)
            | Self::Failed(reason) => reason.len(),
            Self::Reply(_) => 0,
        };
        observability::FailureSummary { code, reason_len }
    }
}

impl ObservabilityFailure for SpeechError {
    fn observability_summary(&self) -> observability::FailureSummary {
        let code = match self {
            Self::SessionNotFound(_) => "session_not_found",
            Self::ModelUnavailable(_) => "model_unavailable",
            Self::InvalidInput(_) => "invalid_input",
            Self::Failed(reason) => observability::inference_failure_code(reason),
            Self::Reply(_) => "reply_failed",
        };
        let reason_len = match self {
            Self::SessionNotFound(reason)
            | Self::ModelUnavailable(reason)
            | Self::InvalidInput(reason)
            | Self::Failed(reason) => reason.len(),
            Self::Reply(_) => 0,
        };
        observability::FailureSummary { code, reason_len }
    }
}

impl ObservabilityFailure for VisionError {
    fn observability_summary(&self) -> observability::FailureSummary {
        let code = match self {
            Self::SessionNotFound(_) => "session_not_found",
            Self::ModelUnavailable(_) => "model_unavailable",
            Self::InvalidInput(_) => "invalid_input",
            Self::Failed(reason) => observability::inference_failure_code(reason),
            Self::Reply(_) => "reply_failed",
        };
        let reason_len = match self {
            Self::SessionNotFound(reason)
            | Self::ModelUnavailable(reason)
            | Self::InvalidInput(reason)
            | Self::Failed(reason) => reason.len(),
            Self::Reply(_) => 0,
        };
        observability::FailureSummary { code, reason_len }
    }
}

fn inference_request_fields<'a>(
    method: &'static str,
    session_id: &'a str,
    resolved: &'a ResolvedSessionRuntime,
) -> observability::InferenceRequestFields<'a> {
    observability::InferenceRequestFields {
        method,
        session_id,
        app_id: &resolved.app_id,
        use_case: &resolved.use_case,
        profile_id: &resolved.profile_id,
        runtime_id: &resolved.runtime_id,
        candidate_count: resolved.image_refs.len(),
    }
}

fn map_observed_failure<E, F>(
    method: &'static str,
    session_id: &str,
    resolved: &ResolvedSessionRuntime,
    started_at: std::time::Instant,
    container_source: &'static str,
    map_failed: &F,
    reason: String,
) -> E
where
    E: ObservabilityFailure,
    F: Fn(String) -> E,
{
    let error = map_failed(reason);
    observability::log_inference_request_failed(
        inference_request_fields(method, session_id, resolved),
        started_at,
        container_source,
        error.observability_summary(),
    );
    error
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
            let (
                profile_id,
                profile_epoch,
                model_id,
                installed_at,
                artifact_hashes,
                runtime_id,
                image_refs,
                artifact_path,
                runtime_options,
            ) = profile_runtime(&guard, &session.profile_id)
                .map_err(ResolveSessionError::ModelUnavailable)?;
            ResolvedSessionRuntime {
                app_id: session.app_id.clone(),
                use_case: session.use_case.clone(),
                profile_id,
                profile_epoch,
                model_id,
                installed_at,
                artifact_hashes,
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
    execution_mode: RequestExecutionMode,
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
                RequestCancellation::for_session(state, session_id)
                    .ensure_not_cancelled()
                    .and_then(|_| {
                        ensure_background_start_still_allowed(state, resolved, execution_mode)
                    })
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
        RequestCancellation::for_session(state, session_id)
            .ensure_not_cancelled_or_terminate_spawned(handle, spawned)
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

async fn with_locked_container<T, E: ObservabilityFailure>(
    method: &'static str,
    state: &SharedState,
    session_id: &str,
    mut resolved: ResolvedSessionRuntime,
    execution_mode: RequestExecutionMode,
    map_failed: impl Fn(String) -> E,
    op: impl FnOnce(&mut Container, &ContainerHandle, bool) -> Result<T, E>,
) -> Result<T, E> {
    let mut op = Some(op);
    let expected_use_case = resolved.use_case.clone();
    let started_at = observability::log_inference_request_started(inference_request_fields(
        method, session_id, &resolved,
    ));
    let mut interactive_execution = match execution_mode {
        RequestExecutionMode::Interactive => {
            state.begin_interactive_execution(&resolved.profile_id);
            Some(InteractiveExecution {
                state,
                profile_id: resolved.profile_id.clone(),
                active: false,
            })
        }
        RequestExecutionMode::Background => None,
    };
    loop {
        if let Some(execution) = interactive_execution.as_mut()
            && !execution.active
            && execution.profile_id != resolved.profile_id
        {
            state.cancel_pending_interactive_execution(&execution.profile_id);
            state.begin_interactive_execution(&resolved.profile_id);
            execution.profile_id = resolved.profile_id.clone();
        }
        if execution_mode == RequestExecutionMode::Background
            && !state.background_execution_can_start(&resolved.profile_id)
        {
            RequestCancellation::for_session(state, session_id)
                .ensure_not_cancelled()
                .map_err(|reason| {
                    map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        "unavailable",
                        &map_failed,
                        reason,
                    )
                })?;
            tokio::time::sleep(Duration::from_millis(25)).await;
            continue;
        }
        let (handle, spawned) = match model_container(state, session_id, &resolved, execution_mode)
            .await
        {
            Ok(container) => container,
            Err(reason) if is_startup_finalizing_retry(&reason) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            Err(reason) if is_wait_retry(&reason) => {
                resolved =
                    match refresh_resolved_session_runtime(state, session_id, &expected_use_case)
                        .await
                    {
                        Ok(resolved) => resolved,
                        Err(reason) => {
                            return Err(map_observed_failure(
                                method,
                                session_id,
                                &resolved,
                                started_at,
                                "unavailable",
                                &map_failed,
                                reason,
                            ));
                        }
                    };
                continue;
            }
            Err(reason) => {
                return Err(map_observed_failure(
                    method,
                    session_id,
                    &resolved,
                    started_at,
                    "unavailable",
                    &map_failed,
                    reason,
                ));
            }
        };
        {
            let mut container =
                match lock_container_for_session(state, session_id, &handle, spawned) {
                    Ok(container) => container,
                    Err(LockContainerError::Retry) => continue,
                    Err(LockContainerError::Failed(reason)) => {
                        return Err(map_observed_failure(
                            method,
                            session_id,
                            &resolved,
                            started_at,
                            observability::container_source(spawned),
                            &map_failed,
                            reason,
                        ));
                    }
                };

            RequestCancellation::for_session(state, session_id)
                .ensure_not_cancelled_or_terminate_spawned(&handle, spawned)
                .map_err(|reason| {
                    map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        observability::container_source(spawned),
                        &map_failed,
                        reason,
                    )
                })?;
            match ensure_handle_ready_for_request(&handle, spawned) {
                Ok(()) => {}
                Err(LockContainerError::Retry) => continue,
                Err(LockContainerError::Failed(reason)) => {
                    return Err(map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        observability::container_source(spawned),
                        &map_failed,
                        reason,
                    ));
                }
            }
            if let Err(reason) = ensure_profile_epoch_current(state, &resolved) {
                if !is_wait_retry(&reason) {
                    return Err(map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        observability::container_source(spawned),
                        &map_failed,
                        reason,
                    ));
                }
            } else if let Err(reason) = ensure_container_matches_resolved(&container, &resolved) {
                if !is_wait_retry(&reason) {
                    return Err(map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        observability::container_source(spawned),
                        &map_failed,
                        reason,
                    ));
                }
            } else {
                let _active = ActiveContainerRequest::new(
                    state,
                    &resolved.profile_id,
                    session_id,
                    handle.clone(),
                );
                RequestCancellation::for_session(state, session_id)
                    .ensure_not_cancelled_or_terminate_spawned(&handle, spawned)
                    .map_err(|reason| {
                        map_observed_failure(
                            method,
                            session_id,
                            &resolved,
                            started_at,
                            observability::container_source(spawned),
                            &map_failed,
                            reason,
                        )
                    })?;
                if spawned {
                    handle.publish();
                }

                let _background_execution = if execution_mode == RequestExecutionMode::Background {
                    if let Some(preempted) =
                        state.begin_background_execution(&resolved.profile_id, handle.clone())
                    {
                        Some(BackgroundExecution {
                            state,
                            profile_id: resolved.profile_id.clone(),
                            handle: handle.clone(),
                            preempted,
                        })
                    } else {
                        drop(container);
                        continue;
                    }
                } else {
                    if let Some(execution) = interactive_execution.as_mut()
                        && !execution.active
                    {
                        state.activate_interactive_execution(&resolved.profile_id);
                        execution.active = true;
                    }
                    None
                };

                let op = op.take().expect("container operation called once");
                let cancel_watcher =
                    RequestCancellation::for_session(state, session_id).spawn_watcher(&handle);
                let mut result = op(&mut container, &handle, spawned);
                cancel_watcher.stop();
                if result.is_err()
                    && _background_execution
                        .as_ref()
                        .is_some_and(BackgroundExecution::was_preempted)
                {
                    result = Err(map_failed(request_execution::background_preempted_reason()));
                }
                match &result {
                    Ok(_) => observability::log_inference_request_succeeded(
                        inference_request_fields(method, session_id, &resolved),
                        started_at,
                        observability::container_source(spawned),
                    ),
                    Err(error) => observability::log_inference_request_failed(
                        inference_request_fields(method, session_id, &resolved),
                        started_at,
                        observability::container_source(spawned),
                        error.observability_summary(),
                    ),
                }
                return result;
            }
        }

        terminate_stale_handle(state, &resolved.profile_id, &handle).await;
        resolved =
            match refresh_resolved_session_runtime(state, session_id, &expected_use_case).await {
                Ok(resolved) => resolved,
                Err(reason) => {
                    return Err(map_observed_failure(
                        method,
                        session_id,
                        &resolved,
                        started_at,
                        observability::container_source(spawned),
                        &map_failed,
                        reason,
                    ));
                }
            };
        continue;
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
        ResolveSessionError::SessionNotFound(_) => request_execution::request_cancelled_reason(),
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

fn ensure_background_start_still_allowed(
    state: &SharedState,
    resolved: &ResolvedSessionRuntime,
    execution_mode: RequestExecutionMode,
) -> Result<(), String> {
    if execution_mode == RequestExecutionMode::Background
        && !state.background_execution_can_start(&resolved.profile_id)
    {
        return Err(request_execution::background_preempted_reason());
    }
    Ok(())
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
    RequestCancellation::for_session(state, session_id).ensure_not_cancelled()?;
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
        _ => Err(request_execution::request_cancelled_reason()),
    }
}

async fn profile_is_missing(state: &SharedState, profile_id: &str) -> bool {
    let guard = state.0.lock().await;
    guard.profiles.get(profile_id).is_none()
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
    match generation_failure_reply(&reason) {
        FailureReply::ContextWindowExceeded => call.reply_context_window_exceeded(reason),
        FailureReply::UnsupportedLanguage => call.reply_unsupported_language(reason),
        FailureReply::SafetyRefusal => call.reply_safety_refusal(reason),
        FailureReply::RequestCancelled => call.reply_request_cancelled(reason),
        FailureReply::InvalidInput => call.reply_invalid_input(reason),
        FailureReply::GenerationFailed => call.reply_generation_failed(reason),
    }
}

fn reply_guided_failure(call: &mut dyn VarlinkCallError, reason: String) -> varlink::Result<()> {
    match guided_failure_reply(&reason) {
        FailureReply::ContextWindowExceeded => call.reply_context_window_exceeded(reason),
        FailureReply::RequestCancelled => call.reply_request_cancelled(reason),
        _ => call.reply_guided_generation_failed(reason),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureReply {
    ContextWindowExceeded,
    UnsupportedLanguage,
    SafetyRefusal,
    RequestCancelled,
    InvalidInput,
    GenerationFailed,
}

fn generation_failure_reply(reason: &str) -> FailureReply {
    if request_execution::is_request_cancelled_failure(reason) {
        return FailureReply::RequestCancelled;
    }
    match observability::runtime_error_code(reason) {
        Some("context_window_exceeded") => FailureReply::ContextWindowExceeded,
        Some("unsupported_language") => FailureReply::UnsupportedLanguage,
        Some("safety_refusal") => FailureReply::SafetyRefusal,
        Some("invalid_input") => FailureReply::InvalidInput,
        _ => FailureReply::GenerationFailed,
    }
}

fn guided_failure_reply(reason: &str) -> FailureReply {
    if request_execution::is_request_cancelled_failure(reason) {
        return FailureReply::RequestCancelled;
    }
    match observability::runtime_error_code(reason) {
        Some("context_window_exceeded") => FailureReply::ContextWindowExceeded,
        Some("request_cancelled") => FailureReply::RequestCancelled,
        _ => FailureReply::GenerationFailed,
    }
}

struct GuidedStreamRequest {
    session_id: String,
    prompt: String,
    media_paths: Vec<String>,
    fields: Vec<GuidedField>,
    tools: Vec<ToolDefinition>,
    options: GuidedOptions,
}

async fn stream_guided_snapshots(
    state: &SharedState,
    call: &mut dyn Call_StreamRespondGuided,
    request: GuidedStreamRequest,
) -> Result<(), GenerationError> {
    let GuidedStreamRequest {
        session_id,
        prompt,
        media_paths,
        fields,
        tools,
        options,
    } = request;
    let (max_tokens, execution_mode) = validate_token_options(
        options.maximum_response_tokens,
        options.temperature,
        &options.execution_mode,
    )
    .map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    let input =
        normalize_guided_input(&prompt, &media_paths).map_err(GenerationError::InvalidInput)?;
    let prompt = input.as_deref().map(render_text_prompt).unwrap_or(prompt);
    with_locked_container(
        "StreamRespondGuided",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
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
                input.as_deref(),
                max_tokens,
                &schema,
                execution_mode.as_str(),
                tools,
                Vec::new(),
                |snapshot, tool_calls, done| {
                    if cancelled
                        || RequestCancellation::for_session(state, &session_id).is_cancelled()
                    {
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
            if cancelled || RequestCancellation::for_session(state, &session_id).is_cancelled() {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(
                    request_execution::request_cancelled_reason(),
                ));
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
    media_paths: Vec<String>,
    results: Vec<ToolResult>,
    fields: Vec<GuidedField>,
    tools: Vec<ToolDefinition>,
    options: GuidedOptions,
) -> Result<(), GenerationError> {
    let (max_tokens, execution_mode) = validate_token_options(
        options.maximum_response_tokens,
        options.temperature,
        &options.execution_mode,
    )
    .map_err(GenerationError::InvalidOptions)?;
    let schema = guided_fields_schema(&fields).map_err(GenerationError::Failed)?;
    let resolved = resolve_session_runtime(state, &session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    let input =
        normalize_guided_input(&prompt, &media_paths).map_err(GenerationError::InvalidInput)?;
    let prompt = input.as_deref().map(render_text_prompt).unwrap_or(prompt);
    with_locked_container(
        "StreamSubmitToolResultsGuided",
        state,
        &session_id,
        resolved.clone(),
        execution_mode,
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
                input.as_deref(),
                max_tokens,
                &schema,
                execution_mode.as_str(),
                tools,
                tool_results,
                |snapshot, tool_calls, done| {
                    if cancelled
                        || RequestCancellation::for_session(state, &session_id).is_cancelled()
                    {
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
            if cancelled || RequestCancellation::for_session(state, &session_id).is_cancelled() {
                if wants_more {
                    call.set_continues(false);
                }
                return Err(GenerationError::Failed(
                    request_execution::request_cancelled_reason(),
                ));
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
    options: &ResponseOptions,
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
        profile.model_id.clone(),
        profile.installed_at.clone(),
        profile.artifact_hashes.clone(),
        profile.runtime_id.clone(),
        candidates,
        profile.artifact_path.clone(),
        runtime_options,
    ))
}

fn embedding_pipeline_id(resolved: &ResolvedSessionRuntime, container: &Container) -> String {
    let mut hasher = Sha256::new();
    update_hash_field(&mut hasher, "profile_id", &resolved.profile_id);
    update_hash_field(&mut hasher, "model_id", &resolved.model_id);
    update_hash_field(&mut hasher, "runtime_id", &resolved.runtime_id);
    update_hash_field(
        &mut hasher,
        "artifact_path",
        &resolved.artifact_path.display().to_string(),
    );
    update_hash_field(&mut hasher, "installed_at", &resolved.installed_at);

    let mut artifact_hashes = resolved.artifact_hashes.clone();
    artifact_hashes
        .sort_by(|a, b| (&a.role, &a.filename, &a.sha256).cmp(&(&b.role, &b.filename, &b.sha256)));
    for artifact in artifact_hashes {
        update_hash_field(&mut hasher, "artifact_role", &artifact.role);
        update_hash_field(&mut hasher, "artifact_filename", &artifact.filename);
        update_hash_field(&mut hasher, "artifact_sha256", &artifact.sha256);
    }

    update_hash_field(&mut hasher, "runtime_variant", container.variant.as_tag());
    update_hash_field(&mut hasher, "runtime_image", &container.image_ref);

    let mut runtime_options = container.runtime_options().iter().collect::<Vec<_>>();
    runtime_options.sort_by_key(|(key, _)| *key);
    for (key, value) in runtime_options {
        update_hash_field(&mut hasher, key, value);
    }

    format!(
        "{}:{}",
        resolved.profile_id,
        hex_lower(hasher.finalize().as_slice())
    )
}

fn update_hash_field(hasher: &mut Sha256, name: &str, value: &str) {
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    hasher.update([0xff]);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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

fn validate_token_options(
    maximum_response_tokens: i64,
    temperature: f64,
    execution_mode: &str,
) -> Result<(u32, RequestExecutionMode), String> {
    if maximum_response_tokens <= 0 {
        return Err("maximum_response_tokens must be greater than zero".to_string());
    }
    if maximum_response_tokens > u32::MAX as i64 {
        return Err("maximum_response_tokens is too large".to_string());
    }
    if !temperature.is_finite() || temperature < 0.0 {
        return Err("temperature must be a finite non-negative number".to_string());
    }
    let execution_mode = parse_execution_mode(execution_mode)?;
    Ok((maximum_response_tokens as u32, execution_mode))
}

fn parse_execution_mode(value: &str) -> Result<RequestExecutionMode, String> {
    match value {
        "" | "interactive" => Ok(RequestExecutionMode::Interactive),
        "background" => Ok(RequestExecutionMode::Background),
        other => Err(format!(
            "execution_mode must be interactive or background, got {other}"
        )),
    }
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

fn read_media_path(path: &str) -> Result<Vec<u8>, String> {
    if path.trim().is_empty() {
        return Err("media path must not be empty".to_string());
    }

    std::fs::read(path).map_err(|e| format!("failed to read media path {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    fn token_options() -> (i64, f64, String) {
        (128, 0.7, "interactive".to_string())
    }

    #[test]
    fn validate_options_accepts_normal_generation_options() {
        assert_eq!(
            validate_token_options(128, 0.7, "interactive"),
            Ok((128, RequestExecutionMode::Interactive))
        );
    }

    #[test]
    fn validate_options_accepts_background_execution_mode() {
        assert_eq!(
            validate_token_options(128, 0.7, "background"),
            Ok((128, RequestExecutionMode::Background))
        );
    }

    #[test]
    fn generation_failure_reply_maps_request_cancelled() {
        assert_eq!(
            generation_failure_reply(&request_execution::request_cancelled_reason()),
            FailureReply::RequestCancelled
        );
        assert_eq!(
            generation_failure_reply(&request_execution::background_preempted_reason()),
            FailureReply::RequestCancelled
        );
    }

    #[test]
    fn guided_failure_reply_maps_only_request_cancelled_special_case() {
        assert_eq!(
            guided_failure_reply(&request_execution::request_cancelled_reason()),
            FailureReply::RequestCancelled
        );
        assert_eq!(
            guided_failure_reply("container returned error safety_refusal: no"),
            FailureReply::GenerationFailed
        );
    }

    #[test]
    fn stream_input_normalizes_text_shorthand() {
        let input =
            normalize_stream_input(r#"[{"type":"input_text","text":"hello"}]"#, &[]).unwrap();

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].role, "user");
        assert_eq!(
            input[0].content,
            vec![InputPart::InputText {
                text: "hello".to_string()
            }]
        );
        assert_eq!(render_text_prompt(&input), "hello");
    }

    #[test]
    fn stream_input_embeds_media_fd_parts() {
        let dir = tempfile::tempdir().unwrap();
        let media_path = dir.path().join("image.png");
        std::fs::write(&media_path, [1_u8, 2, 3]).unwrap();
        let media_paths = vec![media_path.display().to_string()];

        let input = normalize_stream_input(
            r#"[{"role":"user","content":[{"type":"input_image","fd_index":0,"mime_type":"image/png"}]}]"#,
            &media_paths,
        )
        .unwrap();

        assert_eq!(
            input[0].content,
            vec![InputPart::InputImage {
                image: "AQID".to_string(),
                mime_type: "image/png".to_string()
            }]
        );
    }

    #[test]
    fn stream_input_rejects_out_of_range_fd_index() {
        let error = normalize_stream_input(
            r#"[{"type":"input_audio","fd_index":1,"mime_type":"audio/wav"}]"#,
            &[],
        )
        .unwrap_err();

        assert!(error.contains("fd_index 1 is out of range"));
    }

    #[test]
    fn stream_input_rejects_oversized_media() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file()
            .set_len(STREAM_RESPONSE_MEDIA_MAX_BYTES + 1)
            .unwrap();
        let media_paths = vec![file.path().display().to_string()];

        let error = normalize_stream_input(
            r#"[{"type":"input_image","fd_index":0,"mime_type":"image/png"}]"#,
            &media_paths,
        )
        .unwrap_err();

        assert!(error.contains("exceeds maximum size"));
    }

    #[test]
    fn guided_input_keeps_bracketed_text_plain_without_media() {
        let input = normalize_guided_input("[draft] summarize this", &[]).unwrap();

        assert!(input.is_none());
    }

    #[test]
    fn guided_input_parses_valid_json_without_media() {
        let input = normalize_guided_input(
            r#"[{"role":"user","content":[{"type":"input_text","text":"hi"}]}]"#,
            &[],
        )
        .unwrap()
        .unwrap();

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].role, "user");
        assert_eq!(render_text_prompt(&input), "hi");
    }

    #[test]
    fn guided_input_requires_json_when_media_is_attached() {
        let media_paths = vec!["/tmp/image.png".to_string()];
        let error = normalize_guided_input("describe this image", &media_paths).unwrap_err();

        assert!(error.contains("guided media requires prompt"));
    }

    #[hegel::test]
    fn validate_options_accepts_generated_valid_options(tc: TestCase) {
        let maximum_response_tokens = tc.draw(gs::integers::<i64>().min_value(1).max_value(4096));
        let temperature_tenths = tc.draw(gs::integers::<i64>().min_value(0).max_value(20));
        assert_eq!(
            validate_token_options(
                maximum_response_tokens,
                temperature_tenths as f64 / 10.0,
                "interactive",
            ),
            Ok((
                maximum_response_tokens as u32,
                RequestExecutionMode::Interactive
            ))
        );
    }

    #[test]
    fn validate_options_rejects_zero_tokens() {
        let (_, temperature, execution_mode) = token_options();

        assert_eq!(
            validate_token_options(0, temperature, &execution_mode),
            Err("maximum_response_tokens must be greater than zero".to_string())
        );
    }

    #[test]
    fn validate_options_rejects_invalid_temperature() {
        let (maximum_response_tokens, _, execution_mode) = token_options();

        assert_eq!(
            validate_token_options(maximum_response_tokens, f64::NAN, &execution_mode),
            Err("temperature must be a finite non-negative number".to_string())
        );
    }

    #[test]
    fn validate_options_rejects_unknown_execution_mode() {
        let (maximum_response_tokens, temperature, _) = token_options();

        assert_eq!(
            validate_token_options(maximum_response_tokens, temperature, "urgent"),
            Err("execution_mode must be interactive or background, got urgent".to_string())
        );
    }

    #[test]
    fn supported_use_case_catalog_accepts_public_tokens() {
        assert!(is_supported_use_case("language.summarize"));
        assert!(is_supported_use_case("speech.translate"));
        assert!(is_supported_use_case("vision.detect"));
        assert!(is_supported_use_case("vision.segment"));
        assert!(is_supported_use_case("vision.depth"));
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
    fn read_media_path_reads_file_bytes() {
        let file = tempfile::NamedTempFile::new().expect("temp file should be created");
        std::fs::write(file.path(), b"media").expect("temp media should be written");

        assert_eq!(
            read_media_path(file.path().to_str().expect("path should be utf-8")),
            Ok(b"media".to_vec())
        );
    }

    #[test]
    fn read_media_path_rejects_empty_path() {
        assert_eq!(
            read_media_path("  "),
            Err("media path must not be empty".to_string())
        );
    }
}
