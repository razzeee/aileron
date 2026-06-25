/// D-Bus portal backend for task-oriented local model capabilities.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tracing::info;
use zbus::zvariant::Type;
use zbus::{connection, interface, object_server::SignalEmitter};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.aileron";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

pub async fn run() -> Result<()> {
    info!("registering D-Bus portal backend");

    let state = Arc::new(PortalState::default());
    let conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, LanguagePortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, SpeechPortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, VisionPortalBackend::new(state))?
        .build()
        .await?;

    info!("D-Bus connection established; serving portal interfaces");
    let _ = conn;
    std::future::pending::<()>().await;
    Ok(())
}

#[derive(Default)]
struct PortalState {
    warm: Mutex<HashSet<String>>,
    sessions: Mutex<HashMap<String, PortalSession>>,
    session_recreation: Mutex<()>,
}

#[derive(Clone)]
struct PortalSession {
    app_id: String,
    use_case: String,
    instructions: String,
    daemon_session_id: String,
}

struct LanguagePortalBackend {
    state: Arc<PortalState>,
}

struct SpeechPortalBackend {
    state: Arc<PortalState>,
}

struct VisionPortalBackend {
    state: Arc<PortalState>,
}

impl LanguagePortalBackend {
    fn new(state: Arc<PortalState>) -> Self {
        Self { state }
    }
}

impl SpeechPortalBackend {
    fn new(state: Arc<PortalState>) -> Self {
        Self { state }
    }
}

impl VisionPortalBackend {
    fn new(state: Arc<PortalState>) -> Self {
        Self { state }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct ModelAvailabilityDbus {
    is_available: bool,
    code: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct GenerationOptionsDbus {
    maximum_response_tokens: i64,
    temperature: f64,
    sampling_mode: String,
    source_language_hint: String,
    target_language_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct GuidedFieldDbus {
    name: String,
    kind: String,
    description: String,
    required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct ToolDefinitionDbus {
    name: String,
    description: String,
    schema_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct ToolCallDbus {
    id: String,
    name: String,
    arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct ToolResultDbus {
    id: String,
    content: String,
    content_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionSegmentDbus {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[interface(name = "org.freedesktop.impl.portal.Language")]
impl LanguagePortalBackend {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_use_case_prefix(use_case, "language.", "Language")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    async fn create_session(
        &self,
        app_id: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_use_case_prefix(use_case, "language.", "Language")?;
        create_session_impl(&self.state, app_id, use_case, instructions)
    }

    async fn prewarm(
        &self,
        session_id: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        LanguagePortalBackend::model_loading(&emitter, "starting model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        prewarm_impl(&self.state, session_id)
    }

    #[zbus(signal)]
    async fn model_loading(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;

    async fn respond(
        &self,
        session_id: &str,
        prompt: &str,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .respond(
                    daemon_session_id,
                    prompt.to_string(),
                    options.clone().into_varlink(),
                )
                .call()
        })?;
        self.mark_warm(session_id);
        Ok(reply.content)
    }

    async fn predict_next(
        &self,
        session_id: &str,
        prefix: &str,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<Vec<String>> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .predict_next(
                    daemon_session_id,
                    prefix.to_string(),
                    options.clone().into_varlink(),
                )
                .call()
        })?;
        self.mark_warm(session_id);
        Ok(reply.completions)
    }

    async fn stream_response(
        &self,
        session_id: &str,
        prompt: &str,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let public_session_id = session_id.to_string();
        let daemon_session_id = daemon_session_id_or_public(&self.state, session_id);
        let attempted_daemon_session_id = daemon_session_id.clone();
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.stream_response(
            daemon_session_id,
            prompt.to_string(),
            options.clone().into_varlink(),
        );
        let iter = match call.more() {
            Ok(iter) => iter,
            Err(e) if is_session_not_found(&e) => {
                let daemon_session_id = recreate_daemon_session(
                    &self.state,
                    session_id,
                    &attempted_daemon_session_id,
                    &e,
                )?;
                call = client.stream_response(
                    daemon_session_id,
                    prompt.to_string(),
                    options.into_varlink(),
                );
                call.more()
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            }
            Err(e) => return Err(zbus::fdo::Error::Failed(e.to_string())),
        };

        let mut pending_token: Option<String> = None;
        for reply in iter {
            let token = reply
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                .token;

            if let Some(previous) = pending_token.replace(token) {
                LanguagePortalBackend::token_received(
                    &emitter,
                    &public_session_id,
                    &previous,
                    false,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }
        }

        if let Some(token) = pending_token {
            LanguagePortalBackend::token_received(&emitter, &public_session_id, &token, true)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }

        self.mark_warm(&public_session_id);
        Ok(())
    }

    #[zbus(signal)]
    async fn token_received(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        token: &str,
        done: bool,
    ) -> zbus::Result<()>;

    async fn stream_respond_guided(
        &self,
        session_id: &str,
        prompt: &str,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let public_session_id = session_id.to_string();
        let daemon_session_id = daemon_session_id_or_public(&self.state, session_id);
        let attempted_daemon_session_id = daemon_session_id.clone();
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.stream_respond_guided(
            daemon_session_id,
            prompt.to_string(),
            fields
                .clone()
                .into_iter()
                .map(GuidedFieldDbus::into_varlink)
                .collect(),
            tools
                .clone()
                .into_iter()
                .map(ToolDefinitionDbus::into_varlink)
                .collect(),
            options.clone().into_varlink(),
        );
        let iter = match call.more() {
            Ok(iter) => iter,
            Err(e) if is_session_not_found(&e) => {
                let daemon_session_id = recreate_daemon_session(
                    &self.state,
                    session_id,
                    &attempted_daemon_session_id,
                    &e,
                )?;
                call = client.stream_respond_guided(
                    daemon_session_id,
                    prompt.to_string(),
                    fields
                        .into_iter()
                        .map(GuidedFieldDbus::into_varlink)
                        .collect(),
                    tools
                        .into_iter()
                        .map(ToolDefinitionDbus::into_varlink)
                        .collect(),
                    options.into_varlink(),
                );
                call.more()
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            }
            Err(e) => return Err(zbus::fdo::Error::Failed(e.to_string())),
        };

        let mut pending_snapshot: Option<String> = None;
        for reply in iter {
            let reply = reply.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let snapshot = reply.snapshot_json;
            let tool_calls = reply
                .tool_calls
                .into_iter()
                .map(ToolCallDbus::from_varlink)
                .collect::<Vec<_>>();

            if !tool_calls.is_empty() {
                LanguagePortalBackend::guided_tool_calls_received(
                    &emitter,
                    &public_session_id,
                    &tool_calls,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                continue;
            }

            if let Some(previous) = pending_snapshot.replace(snapshot) {
                LanguagePortalBackend::guided_snapshot_received(
                    &emitter,
                    &public_session_id,
                    &previous,
                    false,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }
        }

        if let Some(snapshot) = pending_snapshot {
            LanguagePortalBackend::guided_snapshot_received(
                &emitter,
                &public_session_id,
                &snapshot,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }

        self.mark_warm(&public_session_id);
        Ok(())
    }

    #[zbus(signal)]
    async fn guided_snapshot_received(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        snapshot_json: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn guided_tool_calls_received(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        tool_calls: &[ToolCallDbus],
        done: bool,
    ) -> zbus::Result<()>;

    async fn respond_guided(
        &self,
        session_id: &str,
        prompt: &str,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(String, Vec<ToolCallDbus>)> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .respond_guided(
                    daemon_session_id,
                    prompt.to_string(),
                    fields
                        .clone()
                        .into_iter()
                        .map(GuidedFieldDbus::into_varlink)
                        .collect(),
                    tools
                        .clone()
                        .into_iter()
                        .map(ToolDefinitionDbus::into_varlink)
                        .collect(),
                    options.clone().into_varlink(),
                )
                .call()
        })?;
        self.mark_warm(session_id);
        Ok((
            reply.content,
            reply
                .tool_calls
                .into_iter()
                .map(ToolCallDbus::from_varlink)
                .collect(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_tool_results_guided(
        &self,
        session_id: &str,
        prompt: &str,
        results: Vec<ToolResultDbus>,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(String, Vec<ToolCallDbus>)> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .submit_tool_results_guided(
                    daemon_session_id,
                    prompt.to_string(),
                    results
                        .clone()
                        .into_iter()
                        .map(ToolResultDbus::into_varlink)
                        .collect(),
                    fields
                        .clone()
                        .into_iter()
                        .map(GuidedFieldDbus::into_varlink)
                        .collect(),
                    tools
                        .clone()
                        .into_iter()
                        .map(ToolDefinitionDbus::into_varlink)
                        .collect(),
                    options.clone().into_varlink(),
                )
                .call()
        })?;
        self.mark_warm(session_id);
        Ok((
            reply.content,
            reply
                .tool_calls
                .into_iter()
                .map(ToolCallDbus::from_varlink)
                .collect(),
        ))
    }

    async fn embed(&self, session_id: &str, text: &str) -> zbus::fdo::Result<Vec<f64>> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client.embed(daemon_session_id, text.to_string()).call()
        })?;
        Ok(reply.embedding)
    }

    async fn end_session(&self, session_id: &str) -> zbus::fdo::Result<()> {
        end_session_impl(&self.state, session_id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Speech")]
impl SpeechPortalBackend {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_use_case_prefix(use_case, "speech.", "Speech")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    async fn create_session(
        &self,
        app_id: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_use_case_prefix(use_case, "speech.", "Speech")?;
        create_session_impl(&self.state, app_id, use_case, instructions)
    }

    async fn prewarm(
        &self,
        session_id: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        SpeechPortalBackend::model_loading(&emitter, "starting model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        prewarm_impl(&self.state, session_id)
    }

    #[zbus(signal)]
    async fn model_loading(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;

    async fn transcribe(
        &self,
        session_id: &str,
        audio_b64: &str,
        source_language_hint: &str,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .transcribe(
                    daemon_session_id,
                    audio_b64.to_string(),
                    source_language_hint.to_string(),
                )
                .call()
        })?;
        Ok(reply.text)
    }

    async fn stream_transcribe(
        &self,
        session_id: &str,
        audio_b64: &str,
        source_language_hint: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &emitter).await?;
        let public_session_id = session_id.to_string();
        let daemon_session_id = daemon_session_id_or_public(&self.state, session_id);
        let attempted_daemon_session_id = daemon_session_id.clone();
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.stream_transcribe(
            daemon_session_id,
            audio_b64.to_string(),
            source_language_hint.to_string(),
        );
        let iter = match call.more() {
            Ok(iter) => iter,
            Err(e) if is_session_not_found(&e) => {
                let daemon_session_id = recreate_daemon_session(
                    &self.state,
                    session_id,
                    &attempted_daemon_session_id,
                    &e,
                )?;
                call = client.stream_transcribe(
                    daemon_session_id,
                    audio_b64.to_string(),
                    source_language_hint.to_string(),
                );
                call.more()
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            }
            Err(e) => return Err(zbus::fdo::Error::Failed(e.to_string())),
        };

        let mut pending_text: Option<String> = None;
        for reply in iter {
            let text = reply
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                .token;

            if let Some(previous) = pending_text.replace(text) {
                SpeechPortalBackend::transcription_received(
                    &emitter,
                    &public_session_id,
                    &previous,
                    false,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }
        }

        SpeechPortalBackend::transcription_received(
            &emitter,
            &public_session_id,
            pending_text.as_deref().unwrap_or_default(),
            true,
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        self.mark_warm(&public_session_id);
        Ok(())
    }

    #[zbus(signal)]
    async fn transcription_received(
        emitter: &SignalEmitter<'_>,
        session_id: &str,
        text: &str,
        done: bool,
    ) -> zbus::Result<()>;

    async fn end_session(&self, session_id: &str) -> zbus::fdo::Result<()> {
        end_session_impl(&self.state, session_id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Vision")]
impl VisionPortalBackend {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_use_case_prefix(use_case, "vision.", "Vision")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    async fn create_session(
        &self,
        app_id: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_use_case_prefix(use_case, "vision.", "Vision")?;
        create_session_impl(&self.state, app_id, use_case, instructions)
    }

    async fn prewarm(
        &self,
        session_id: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        VisionPortalBackend::model_loading(&emitter, "starting model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        prewarm_impl(&self.state, session_id)
    }

    #[zbus(signal)]
    async fn model_loading(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;

    async fn describe(&self, session_id: &str, image_b64: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .describe(daemon_session_id, image_b64.to_string())
                .call()
        })?;
        Ok(reply.text)
    }

    async fn ocr(&self, session_id: &str, image_b64: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client.ocr(daemon_session_id, image_b64.to_string()).call()
        })?;
        Ok(reply.text)
    }

    async fn segment(
        &self,
        session_id: &str,
        image_b64: &str,
    ) -> zbus::fdo::Result<Vec<VisionSegmentDbus>> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = call_with_recreated_session(&self.state, session_id, |daemon_session_id| {
            client
                .segment(daemon_session_id, image_b64.to_string())
                .call()
        })?;
        Ok(reply
            .segments
            .into_iter()
            .map(|segment| VisionSegmentDbus {
                label: segment.label,
                confidence: segment.confidence,
                x: segment.x,
                y: segment.y,
                width: segment.width,
                height: segment.height,
            })
            .collect())
    }

    async fn end_session(&self, session_id: &str) -> zbus::fdo::Result<()> {
        end_session_impl(&self.state, session_id)
    }
}

fn ensure_use_case_prefix(use_case: &str, prefix: &str, interface: &str) -> zbus::fdo::Result<()> {
    if use_case.starts_with(prefix) {
        return Ok(());
    }

    Err(zbus::fdo::Error::Failed(format!(
        "{interface} portal expects {prefix} use-cases, got {use_case}"
    )))
}

fn get_use_case_availability_impl(
    app_id: &str,
    use_case: &str,
) -> zbus::fdo::Result<ModelAvailabilityDbus> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    let reply = client
        .get_use_case_availability(app_id.to_string(), use_case.to_string())
        .call()
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

    Ok(ModelAvailabilityDbus {
        is_available: reply.availability.is_available,
        code: reply.availability.code,
        reason: reply.availability.reason,
    })
}

fn create_session_impl(
    state: &PortalState,
    app_id: &str,
    use_case: &str,
    instructions: &str,
) -> zbus::fdo::Result<String> {
    let daemon_session_id = create_daemon_session(app_id, use_case, instructions)?;
    state.sessions.lock().unwrap().insert(
        daemon_session_id.clone(),
        PortalSession {
            app_id: app_id.to_string(),
            use_case: use_case.to_string(),
            instructions: instructions.to_string(),
            daemon_session_id: daemon_session_id.clone(),
        },
    );
    Ok(daemon_session_id)
}

fn create_daemon_session(
    app_id: &str,
    use_case: &str,
    instructions: &str,
) -> zbus::fdo::Result<String> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    let reply = match client
        .create_session(
            app_id.to_string(),
            use_case.to_string(),
            instructions.to_string(),
        )
        .call()
    {
        Ok(reply) => reply,
        Err(e) if is_permission_denied(&e) => {
            if !prompt_permission(app_id, use_case)? {
                set_permission(app_id, use_case, false)?;
                return Err(zbus::fdo::Error::AccessDenied(format!(
                    "Permission denied for {app_id} / {use_case}"
                )));
            }
            set_permission(app_id, use_case, true)?;
            client
                .create_session(
                    app_id.to_string(),
                    use_case.to_string(),
                    instructions.to_string(),
                )
                .call()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        }
        Err(e) => return Err(zbus::fdo::Error::Failed(e.to_string())),
    };

    Ok(reply.session_id)
}

fn prewarm_impl(state: &PortalState, session_id: &str) -> zbus::fdo::Result<()> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    call_with_recreated_session(state, session_id, |daemon_session_id| {
        client.prewarm(daemon_session_id).call()
    })?;

    if let Some(use_case) = use_case_for_session(state, session_id) {
        state.warm.lock().unwrap().insert(use_case);
    }
    Ok(())
}

fn end_session_impl(state: &PortalState, session_id: &str) -> zbus::fdo::Result<()> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    let _recreation_guard = state.session_recreation.lock().unwrap();
    let daemon_session_id = daemon_session_id_or_public(state, session_id);

    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    match client.end_session(daemon_session_id).call() {
        Ok(_) => {}
        Err(e) if is_session_not_found(&e) => {}
        Err(e) => return Err(zbus::fdo::Error::Failed(e.to_string())),
    }
    state.sessions.lock().unwrap().remove(session_id);
    Ok(())
}

fn daemon_session_id_or_public(state: &PortalState, session_id: &str) -> String {
    state
        .sessions
        .lock()
        .unwrap()
        .get(session_id)
        .map(|session| session.daemon_session_id.clone())
        .unwrap_or_else(|| session_id.to_string())
}

fn call_with_recreated_session<T, E>(
    state: &PortalState,
    session_id: &str,
    mut call: impl FnMut(String) -> Result<T, E>,
) -> zbus::fdo::Result<T>
where
    E: std::fmt::Display,
{
    let daemon_session_id = daemon_session_id_or_public(state, session_id);
    let attempted_daemon_session_id = daemon_session_id.clone();
    match call(daemon_session_id) {
        Ok(reply) => Ok(reply),
        Err(e) if is_session_not_found(&e) => {
            let daemon_session_id =
                recreate_daemon_session(state, session_id, &attempted_daemon_session_id, &e)?;
            call(daemon_session_id).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
        }
        Err(e) => Err(zbus::fdo::Error::Failed(e.to_string())),
    }
}

fn use_case_for_session(state: &PortalState, session_id: &str) -> Option<String> {
    state
        .sessions
        .lock()
        .unwrap()
        .get(session_id)
        .map(|session| session.use_case.clone())
}

fn recreate_daemon_session(
    state: &PortalState,
    session_id: &str,
    attempted_daemon_session_id: &str,
    original_error: &impl std::fmt::Display,
) -> zbus::fdo::Result<String> {
    let _recreation_guard = state.session_recreation.lock().unwrap();
    let session = match refresh_state_for_session(state, session_id, attempted_daemon_session_id) {
        Ok(SessionRefreshState::AlreadyRefreshed(daemon_session_id)) => {
            return Ok(daemon_session_id);
        }
        Ok(SessionRefreshState::Stale(session)) => session,
        Err(()) => return Err(zbus::fdo::Error::Failed(original_error.to_string())),
    };

    let daemon_session_id =
        create_daemon_session(&session.app_id, &session.use_case, &session.instructions)?;

    if !update_daemon_session_id(state, session_id, &daemon_session_id) {
        best_effort_end_daemon_session(&daemon_session_id);
        return Err(zbus::fdo::Error::Failed(original_error.to_string()));
    }

    Ok(daemon_session_id)
}

enum SessionRefreshState {
    AlreadyRefreshed(String),
    Stale(PortalSession),
}

fn refresh_state_for_session(
    state: &PortalState,
    session_id: &str,
    attempted_daemon_session_id: &str,
) -> Result<SessionRefreshState, ()> {
    let session = state
        .sessions
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .ok_or(())?;

    if session.daemon_session_id == attempted_daemon_session_id {
        Ok(SessionRefreshState::Stale(session))
    } else {
        Ok(SessionRefreshState::AlreadyRefreshed(
            session.daemon_session_id,
        ))
    }
}

fn update_daemon_session_id(
    state: &PortalState,
    session_id: &str,
    daemon_session_id: &str,
) -> bool {
    if let Some(current) = state.sessions.lock().unwrap().get_mut(session_id) {
        current.daemon_session_id = daemon_session_id.to_string();
        true
    } else {
        false
    }
}

fn best_effort_end_daemon_session(daemon_session_id: &str) {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    if let Ok(conn) = aileron_ipc::client::connect() {
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let _ = client.end_session(daemon_session_id.to_string()).call();
    }
}

fn is_session_not_found(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string();
    message.contains("aileron.Inference.SessionNotFound")
        || message.contains("aileron.Inference.SessionNotFound_Args")
        || message.contains("SessionNotFound_Args")
}

fn is_permission_denied(error: &impl std::fmt::Display) -> bool {
    error.to_string().contains("PermissionDenied")
}

fn set_permission(app_id: &str, use_case: &str, allowed: bool) -> zbus::fdo::Result<()> {
    use aileron_varlink::aileron_Permissions::VarlinkClientInterface;

    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Permissions::VarlinkClient::new(conn);
    client
        .set_app_permission(app_id.to_string(), use_case.to_string(), allowed)
        .call()
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    Ok(())
}

fn prompt_permission(app_id: &str, use_case: &str) -> zbus::fdo::Result<bool> {
    let text = format!(
        "Allow {app_id} to use the local model capability {use_case}?\n\nAileron will process this request locally using the assigned model."
    );

    if let Ok(status) = Command::new("zenity")
        .args([
            "--question",
            "--title=Aileron Permission Request",
            "--ok-label=Allow",
            "--cancel-label=Deny",
            "--text",
            &text,
        ])
        .status()
    {
        return Ok(status.success());
    }

    if let Ok(status) = Command::new("kdialog")
        .args(["--title", "Aileron Permission Request", "--yesno", &text])
        .status()
    {
        return Ok(status.success());
    }

    Err(zbus::fdo::Error::Failed(
        "No permission prompt helper found; install zenity or kdialog, grant permission in the Aileron Permissions page, or start the daemon with AILERON_AUTO_GRANT=true for development".to_string(),
    ))
}

impl LanguagePortalBackend {
    async fn emit_loading_if_cold(
        &self,
        session_id: &str,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let use_case = use_case_for_session(&self.state, session_id);
        let is_warm = use_case
            .as_ref()
            .is_some_and(|u| self.state.warm.lock().unwrap().contains(u));
        if !is_warm {
            LanguagePortalBackend::model_loading(emitter, "starting model")
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(use_case) = use_case_for_session(&self.state, session_id) {
            self.state.warm.lock().unwrap().insert(use_case);
        }
    }
}

impl SpeechPortalBackend {
    async fn emit_loading_if_cold(
        &self,
        session_id: &str,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let use_case = use_case_for_session(&self.state, session_id);
        let is_warm = use_case
            .as_ref()
            .is_some_and(|u| self.state.warm.lock().unwrap().contains(u));
        if !is_warm {
            SpeechPortalBackend::model_loading(emitter, "starting model")
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(use_case) = use_case_for_session(&self.state, session_id) {
            self.state.warm.lock().unwrap().insert(use_case);
        }
    }
}

impl GenerationOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::GenerationOptions {
        aileron_varlink::aileron_Inference::GenerationOptions {
            maximum_response_tokens: self.maximum_response_tokens,
            temperature: self.temperature,
            sampling_mode: self.sampling_mode,
            source_language_hint: self.source_language_hint,
            target_language_hint: self.target_language_hint,
        }
    }
}

impl GuidedFieldDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::GuidedField {
        aileron_varlink::aileron_Inference::GuidedField {
            name: self.name,
            kind: self.kind,
            description: self.description,
            required: self.required,
        }
    }
}

impl ToolDefinitionDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::ToolDefinition {
        aileron_varlink::aileron_Inference::ToolDefinition {
            name: self.name,
            description: self.description,
            schema_json: self.schema_json,
        }
    }
}

impl ToolCallDbus {
    fn from_varlink(call: aileron_varlink::aileron_Inference::ToolCall) -> Self {
        Self {
            id: call.id,
            name: call.name,
            arguments_json: call.arguments_json,
        }
    }
}

impl ToolResultDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::ToolResult {
        aileron_varlink::aileron_Inference::ToolResult {
            id: self.id,
            content: self.content,
            content_json: self.content_json,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn use_case_prefix_accepts_matching_interface_prefix(tc: TestCase) {
        let (prefix, interface) = tc.draw(gs::sampled_from(vec![
            ("language.", "Language"),
            ("speech.", "Speech"),
            ("vision.", "Vision"),
        ]));
        let suffix = tc.draw(gs::sampled_from(vec!["summarize", "translate", "describe"]));
        let use_case = format!("{prefix}{suffix}");

        assert!(ensure_use_case_prefix(&use_case, prefix, interface).is_ok());
    }

    #[hegel::test]
    fn use_case_prefix_rejects_mismatched_interface_prefix(tc: TestCase) {
        let (prefix, interface, use_case) = tc.draw(gs::sampled_from(vec![
            ("language.", "Language", "speech.transcribe"),
            ("speech.", "Speech", "vision.describe"),
            ("vision.", "Vision", "language.summarize"),
        ]));

        let err = ensure_use_case_prefix(use_case, prefix, interface)
            .expect_err("mismatched prefix should fail");

        assert!(err.to_string().contains(interface));
        assert!(err.to_string().contains(prefix));
        assert!(err.to_string().contains(use_case));
    }

    #[hegel::test]
    fn generation_options_conversion_preserves_generated_fields(tc: TestCase) {
        let maximum_response_tokens = tc.draw(gs::integers::<i64>().min_value(1).max_value(4096));
        let temperature_tenths = tc.draw(gs::integers::<i64>().min_value(0).max_value(20));
        let sampling_mode = tc.draw(gs::sampled_from(vec![
            "default".to_string(),
            "greedy".to_string(),
            "creative".to_string(),
        ]));
        let options = GenerationOptionsDbus {
            maximum_response_tokens,
            temperature: temperature_tenths as f64 / 10.0,
            sampling_mode: sampling_mode.clone(),
            source_language_hint: "en".to_string(),
            target_language_hint: "es".to_string(),
        };

        let converted = options.into_varlink();

        assert_eq!(converted.maximum_response_tokens, maximum_response_tokens);
        assert_eq!(converted.temperature, temperature_tenths as f64 / 10.0);
        assert_eq!(converted.sampling_mode, sampling_mode);
        assert_eq!(converted.source_language_hint, "en");
        assert_eq!(converted.target_language_hint, "es");
    }

    #[hegel::test]
    fn guided_field_conversion_preserves_generated_fields(tc: TestCase) {
        let required = tc.draw(gs::booleans());
        let field = GuidedFieldDbus {
            name: "answer".to_string(),
            kind: "string".to_string(),
            description: "generated answer".to_string(),
            required,
        };

        let converted = field.into_varlink();

        assert_eq!(converted.name, "answer");
        assert_eq!(converted.kind, "string");
        assert_eq!(converted.description, "generated answer");
        assert_eq!(converted.required, required);
    }

    #[test]
    fn tool_definition_and_result_conversion_preserve_fields() {
        let definition = ToolDefinitionDbus {
            name: "count".to_string(),
            description: "Count things".to_string(),
            schema_json: "{}".to_string(),
        }
        .into_varlink();
        let result = ToolResultDbus {
            id: "tool-1".to_string(),
            content: "done".to_string(),
            content_json: "{}".to_string(),
        }
        .into_varlink();

        assert_eq!(definition.name, "count");
        assert_eq!(definition.description, "Count things");
        assert_eq!(definition.schema_json, "{}");
        assert_eq!(result.id, "tool-1");
        assert_eq!(result.content, "done");
        assert_eq!(result.content_json, "{}");
    }

    #[test]
    fn tool_call_conversion_preserves_varlink_fields() {
        let call = ToolCallDbus::from_varlink(aileron_varlink::aileron_Inference::ToolCall {
            id: "tool-1".to_string(),
            name: "count".to_string(),
            arguments_json: "{}".to_string(),
        });

        assert_eq!(call.id, "tool-1");
        assert_eq!(call.name, "count");
        assert_eq!(call.arguments_json, "{}");
    }

    #[test]
    fn permission_denied_detection_matches_error_text() {
        assert!(is_permission_denied(&"aileron.Inference.PermissionDenied"));
        assert!(!is_permission_denied(
            &"aileron.Inference.ProfileUnavailable"
        ));
    }

    #[test]
    fn session_not_found_detection_matches_error_text() {
        assert!(is_session_not_found(
            &"aileron.Inference.SessionNotFound: Some(SessionNotFound_Args { session_id: \"old\" })"
        ));
        assert!(!is_session_not_found(&"aileron.Inference.GenerationFailed"));
    }

    #[test]
    fn daemon_session_lookup_uses_updated_internal_id() {
        let state = PortalState::default();
        state.sessions.lock().unwrap().insert(
            "public-session".to_string(),
            PortalSession {
                app_id: "org.example.App".to_string(),
                use_case: "language.extract".to_string(),
                instructions: "answer".to_string(),
                daemon_session_id: "stale-daemon-session".to_string(),
            },
        );

        assert_eq!(
            daemon_session_id_or_public(&state, "public-session"),
            "stale-daemon-session"
        );
        update_daemon_session_id(&state, "public-session", "replacement-daemon-session");
        assert_eq!(
            daemon_session_id_or_public(&state, "public-session"),
            "replacement-daemon-session"
        );
        assert_eq!(
            daemon_session_id_or_public(&state, "unknown-session"),
            "unknown-session"
        );
        assert_eq!(
            use_case_for_session(&state, "public-session").as_deref(),
            Some("language.extract")
        );
        assert!(!update_daemon_session_id(
            &state,
            "missing-session",
            "orphan-daemon-session"
        ));
    }

    #[test]
    fn refresh_state_reuses_concurrent_replacement() {
        let state = PortalState::default();
        state.sessions.lock().unwrap().insert(
            "public-session".to_string(),
            PortalSession {
                app_id: "org.example.App".to_string(),
                use_case: "language.extract".to_string(),
                instructions: "answer".to_string(),
                daemon_session_id: "replacement-daemon-session".to_string(),
            },
        );

        let refresh_state =
            refresh_state_for_session(&state, "public-session", "stale-daemon-session")
                .expect("known portal session should resolve");

        match refresh_state {
            SessionRefreshState::AlreadyRefreshed(id) => {
                assert_eq!(id, "replacement-daemon-session");
            }
            SessionRefreshState::Stale(_) => panic!("replacement should be reused"),
        }
    }

    #[test]
    fn refresh_state_marks_matching_attempt_as_stale() {
        let state = PortalState::default();
        state.sessions.lock().unwrap().insert(
            "public-session".to_string(),
            PortalSession {
                app_id: "org.example.App".to_string(),
                use_case: "language.extract".to_string(),
                instructions: "answer".to_string(),
                daemon_session_id: "stale-daemon-session".to_string(),
            },
        );

        let refresh_state =
            refresh_state_for_session(&state, "public-session", "stale-daemon-session")
                .expect("known portal session should resolve");

        match refresh_state {
            SessionRefreshState::Stale(session) => {
                assert_eq!(session.daemon_session_id, "stale-daemon-session");
                assert_eq!(session.use_case, "language.extract");
            }
            SessionRefreshState::AlreadyRefreshed(_) => panic!("matching attempt should be stale"),
        }
    }
}
