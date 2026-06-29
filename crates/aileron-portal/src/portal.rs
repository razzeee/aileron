/// D-Bus portal backend for task-oriented local model capabilities.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tracing::{info, warn};
use zbus::zvariant::Type;
use zbus::{connection, interface, message::Header, object_server::SignalEmitter};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.aileron";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const FRONTEND_BUS_NAME: &str = "org.freedesktop.portal.Desktop";

pub async fn run() -> Result<()> {
    info!("registering D-Bus portal backend");

    let state = Arc::new(PortalState::default());
    let _conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, LanguagePortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, SpeechPortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, VisionPortalBackend::new(state))?
        .build()
        .await?;

    info!("D-Bus connection established; serving portal interfaces");
    std::future::pending::<()>().await;
    Ok(())
}

#[derive(Default)]
struct PortalState {
    warm_profiles: Mutex<HashSet<String>>,
    sessions: Mutex<HashMap<String, SessionRecord>>,
    requests: Mutex<HashMap<String, RequestRecord>>,
}

#[derive(Debug, Clone)]
struct RequestRecord {
    session_id: Option<String>,
    cancelled: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PortalInterface {
    Language,
    Speech,
    Vision,
}

impl PortalInterface {
    fn label(self) -> &'static str {
        match self {
            Self::Language => "Language",
            Self::Speech => "Speech",
            Self::Vision => "Vision",
        }
    }
}

#[derive(Debug, Clone)]
struct SessionRecord {
    profile_id: String,
    interface: PortalInterface,
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

struct RequestPortalBackend {
    state: Arc<PortalState>,
    request_id: String,
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

impl RequestPortalBackend {
    fn new(state: Arc<PortalState>, request_id: &str) -> Self {
        Self {
            state,
            request_id: request_id.to_string(),
        }
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

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestPortalBackend {
    async fn close(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        cancel_request(&self.state, &self.request_id);
        Ok(())
    }
}

#[interface(name = "org.freedesktop.impl.portal.Language")]
impl LanguagePortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        4
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "language.", "Language")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_id: &str,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "language.", "Language")?;
        begin_request(conn, &self.state, request_id, None).await?;
        let result = create_session_impl(
            &self.state,
            request_id,
            app_id,
            parent_window,
            use_case,
            instructions,
            PortalInterface::Language,
        );
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_id: &str,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_request_active(&self.state, request_id)?;
            LanguagePortalBackend::model_loading(
                &emitter,
                request_id,
                session_id,
                "preparing model",
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(&self.state, session_id, PortalInterface::Language)
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        message: &str,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_response(
        &self,
        request_id: &str,
        session_id: &str,
        prompt: &str,
        options: GenerationOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_response(
                session_id.clone(),
                prompt.to_string(),
                options.into_varlink(),
            );
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_token: Option<String> = None;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let token = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .token;

                if let Some(previous) = pending_token.replace(token) {
                    LanguagePortalBackend::token_received(
                        &emitter,
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if let Some(token) = pending_token {
                LanguagePortalBackend::token_received(
                    &emitter,
                    request_id,
                    &session_id,
                    &token,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn token_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        token: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_predict_next(
        &self,
        request_id: &str,
        session_id: &str,
        prefix: &str,
        options: GenerationOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_predict_next(
                session_id.clone(),
                prefix.to_string(),
                options.into_varlink(),
            );
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut completions = Vec::new();
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                completions = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .completions;
            }

            ensure_request_active(&self.state, request_id)?;
            if completions.is_empty() {
                LanguagePortalBackend::prediction_received(
                    &emitter,
                    request_id,
                    &session_id,
                    "",
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            } else {
                let last_index = completions.len() - 1;
                for (index, completion) in completions.iter().enumerate() {
                    LanguagePortalBackend::prediction_received(
                        &emitter,
                        request_id,
                        &session_id,
                        completion,
                        index == last_index,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }
            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn prediction_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        completion: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_respond_guided(
        &self,
        request_id: &str,
        session_id: &str,
        prompt: &str,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GenerationOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_respond_guided(
                session_id.clone(),
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
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_snapshot: Option<String> = None;
            let mut emitted_terminal_tool_calls = false;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let reply = reply.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                let snapshot = reply.snapshot_json;
                let tool_calls = reply
                    .tool_calls
                    .into_iter()
                    .map(ToolCallDbus::from_varlink)
                    .collect::<Vec<_>>();

                if !tool_calls.is_empty() {
                    pending_snapshot = None;
                    emitted_terminal_tool_calls = true;
                    LanguagePortalBackend::guided_tool_calls_received(
                        &emitter,
                        request_id,
                        &session_id,
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
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if !emitted_terminal_tool_calls && let Some(snapshot) = pending_snapshot {
                LanguagePortalBackend::guided_snapshot_received(
                    &emitter,
                    request_id,
                    &session_id,
                    &snapshot,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn guided_snapshot_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        snapshot_json: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn guided_tool_calls_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        tool_calls: &[ToolCallDbus],
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_submit_tool_results_guided(
        &self,
        request_id: &str,
        session_id: &str,
        prompt: &str,
        results: Vec<ToolResultDbus>,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GenerationOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_submit_tool_results_guided(
                session_id.clone(),
                prompt.to_string(),
                results
                    .into_iter()
                    .map(ToolResultDbus::into_varlink)
                    .collect(),
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
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_snapshot: Option<String> = None;
            let mut emitted_terminal_tool_calls = false;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let reply = reply.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                let snapshot = reply.snapshot_json;
                let tool_calls = reply
                    .tool_calls
                    .into_iter()
                    .map(ToolCallDbus::from_varlink)
                    .collect::<Vec<_>>();

                if !tool_calls.is_empty() {
                    pending_snapshot = None;
                    emitted_terminal_tool_calls = true;
                    LanguagePortalBackend::guided_tool_calls_received(
                        &emitter,
                        request_id,
                        &session_id,
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
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if !emitted_terminal_tool_calls && let Some(snapshot) = pending_snapshot {
                LanguagePortalBackend::guided_snapshot_received(
                    &emitter,
                    request_id,
                    &session_id,
                    &snapshot,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn stream_embed(
        &self,
        request_id: &str,
        session_id: &str,
        text: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_embed(session_id.clone(), text.to_string());
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut last_embedding = Vec::new();
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                last_embedding = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .embedding;
            }

            ensure_request_active(&self.state, request_id)?;
            LanguagePortalBackend::embedding_received(
                &emitter,
                request_id,
                &session_id,
                &last_embedding,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn embedding_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        embedding: &[f64],
        done: bool,
    ) -> zbus::Result<()>;

    async fn end_session(
        &self,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        end_session_impl(&self.state, session_id, PortalInterface::Language)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Speech")]
impl SpeechPortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        4
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "speech.", "Speech")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_id: &str,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "speech.", "Speech")?;
        begin_request(conn, &self.state, request_id, None).await?;
        let result = create_session_impl(
            &self.state,
            request_id,
            app_id,
            parent_window,
            use_case,
            instructions,
            PortalInterface::Speech,
        );
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_id: &str,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            ensure_known_session(&self.state, session_id, PortalInterface::Speech)?;
            ensure_request_active(&self.state, request_id)?;
            SpeechPortalBackend::model_loading(&emitter, request_id, session_id, "preparing model")
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(&self.state, session_id, PortalInterface::Speech)
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        message: &str,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_transcribe(
        &self,
        request_id: &str,
        session_id: &str,
        audio_b64: &str,
        source_language_hint: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_transcribe(
                session_id.clone(),
                audio_b64.to_string(),
                source_language_hint.to_string(),
            );
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_text: Option<String> = None;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let text = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .token;

                if let Some(previous) = pending_text.replace(text) {
                    SpeechPortalBackend::transcription_received(
                        &emitter,
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            SpeechPortalBackend::transcription_received(
                &emitter,
                request_id,
                &session_id,
                pending_text.as_deref().unwrap_or_default(),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn transcription_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        text: &str,
        done: bool,
    ) -> zbus::Result<()>;

    async fn end_session(
        &self,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        end_session_impl(&self.state, session_id, PortalInterface::Speech)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Vision")]
impl VisionPortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        4
    }

    #[zbus(out_args("availability"))]
    async fn get_use_case_availability(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        app_id: &str,
        use_case: &str,
    ) -> zbus::fdo::Result<(ModelAvailabilityDbus,)> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "vision.", "Vision")?;
        Ok((get_use_case_availability_impl(app_id, use_case)?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_id: &str,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_use_case_prefix(use_case, "vision.", "Vision")?;
        begin_request(conn, &self.state, request_id, None).await?;
        let result = create_session_impl(
            &self.state,
            request_id,
            app_id,
            parent_window,
            use_case,
            instructions,
            PortalInterface::Vision,
        );
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_id: &str,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::model_loading(&emitter, request_id, session_id, "preparing model")
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(&self.state, session_id, PortalInterface::Vision)
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        message: &str,
    ) -> zbus::Result<()>;

    async fn stream_describe(
        &self,
        request_id: &str,
        session_id: &str,
        image_b64: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_describe(session_id.clone(), image_b64.to_string());
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_text: Option<String> = None;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let text = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .token;

                if let Some(previous) = pending_text.replace(text) {
                    VisionPortalBackend::vision_text_received(
                        &emitter,
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if let Some(text) = pending_text {
                VisionPortalBackend::vision_text_received(
                    &emitter,
                    request_id,
                    &session_id,
                    &text,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn stream_ocr(
        &self,
        request_id: &str,
        session_id: &str,
        image_b64: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_ocr(session_id.clone(), image_b64.to_string());
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut pending_text: Option<String> = None;
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                let text = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                    .token;

                if let Some(previous) = pending_text.replace(text) {
                    VisionPortalBackend::vision_text_received(
                        &emitter,
                        request_id,
                        &session_id,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if let Some(text) = pending_text {
                VisionPortalBackend::vision_text_received(
                    &emitter,
                    request_id,
                    &session_id,
                    &text,
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn stream_segment(
        &self,
        request_id: &str,
        session_id: &str,
        image_b64: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        ensure_portal_frontend(conn, &header).await?;
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            self.emit_loading_if_cold(request_id, session_id, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let session_id = session_id.to_string();
            let ipc_conn = aileron_ipc::client::connect()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(ipc_conn);
            let mut call = client.stream_segment(session_id.clone(), image_b64.to_string());
            let iter = call
                .more()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            let mut last_segments = Vec::new();
            for reply in iter {
                ensure_request_active(&self.state, request_id)?;
                last_segments = reply
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
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
                    .collect();
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_segments_received(
                &emitter,
                request_id,
                &session_id,
                &last_segments,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            self.mark_warm(&session_id);
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn vision_text_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        text: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vision_segments_received(
        emitter: &SignalEmitter<'_>,
        request_id: &str,
        session_id: &str,
        segments: &[VisionSegmentDbus],
        done: bool,
    ) -> zbus::Result<()>;

    async fn end_session(
        &self,
        session_id: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        end_session_impl(&self.state, session_id, PortalInterface::Vision)
    }
}

async fn ensure_portal_frontend(
    conn: &zbus::Connection,
    header: &Header<'_>,
) -> zbus::fdo::Result<()> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("Missing D-Bus sender".to_string()))?;
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let frontend_owner = dbus
        .get_name_owner(
            FRONTEND_BUS_NAME
                .try_into()
                .map_err(|e| zbus::fdo::Error::Failed(format!("invalid portal bus name: {e}")))?,
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

    if sender.as_str() == frontend_owner.as_str() {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(
            "Aileron implementation portal calls must come from xdg-desktop-portal".to_string(),
        ))
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

async fn begin_request(
    conn: &zbus::Connection,
    state: &Arc<PortalState>,
    request_id: &str,
    session_id: Option<&str>,
) -> zbus::fdo::Result<()> {
    state.requests.lock().unwrap().insert(
        request_id.to_string(),
        RequestRecord {
            session_id: session_id.map(str::to_string),
            cancelled: false,
        },
    );

    let added = conn
        .object_server()
        .at(
            request_id,
            RequestPortalBackend::new(state.clone(), request_id),
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    if !added {
        finish_request_record(state, request_id);
        return Err(zbus::fdo::Error::Failed(format!(
            "request object {request_id} already exists"
        )));
    }

    Ok(())
}

async fn finish_request(conn: &zbus::Connection, state: &PortalState, request_id: &str) {
    finish_request_record(state, request_id);
    if let Err(e) = conn
        .object_server()
        .remove::<RequestPortalBackend, _>(request_id)
        .await
    {
        warn!("failed to remove portal request {request_id}: {e}");
    }
}

fn finish_request_record(state: &PortalState, request_id: &str) {
    state.requests.lock().unwrap().remove(request_id);
}

fn cancel_request(state: &PortalState, request_id: &str) {
    if let Some(record) = state.requests.lock().unwrap().get_mut(request_id) {
        record.cancelled = true;
    }
}

fn cancel_session_requests(state: &PortalState, session_id: &str) {
    for record in state.requests.lock().unwrap().values_mut() {
        if record.session_id.as_deref() == Some(session_id) {
            record.cancelled = true;
        }
    }
}

fn ensure_request_active(state: &PortalState, request_id: &str) -> zbus::fdo::Result<()> {
    if state
        .requests
        .lock()
        .unwrap()
        .get(request_id)
        .map(|record| record.cancelled)
        .unwrap_or(false)
    {
        return Err(request_cancelled_error());
    }

    Ok(())
}

fn request_cancelled_error() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(
        "aileron.Inference.RequestCancelled: request was cancelled".to_string(),
    )
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
    request_id: &str,
    app_id: &str,
    parent_window: &str,
    use_case: &str,
    instructions: &str,
    interface: PortalInterface,
) -> zbus::fdo::Result<String> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    ensure_request_active(state, request_id)?;
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
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "Permission denied for {app_id} / {use_case}"
            )));
        }
        Err(e) if is_permission_prompt_required(&e) => {
            ensure_request_active(state, request_id)?;
            if !prompt_permission(state, request_id, app_id, parent_window, use_case)? {
                set_permission(app_id, use_case, false)?;
                return Err(zbus::fdo::Error::AccessDenied(format!(
                    "Permission denied for {app_id} / {use_case}"
                )));
            }
            ensure_request_active(state, request_id)?;
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

    if let Err(e) = ensure_request_active(state, request_id) {
        end_daemon_session_async(reply.session_id.clone());
        return Err(e);
    }

    state.sessions.lock().unwrap().insert(
        reply.session_id.clone(),
        SessionRecord {
            profile_id: reply.profile_id,
            interface,
        },
    );
    Ok(reply.session_id)
}

fn session_record(state: &PortalState, session_id: &str) -> Option<SessionRecord> {
    state.sessions.lock().unwrap().get(session_id).cloned()
}

fn ensure_known_session(
    state: &PortalState,
    session_id: &str,
    interface: PortalInterface,
) -> zbus::fdo::Result<SessionRecord> {
    let record = session_record(state, session_id)
        .ok_or_else(|| zbus::fdo::Error::AccessDenied(format!("Unknown session {session_id}")))?;
    if record.interface == interface {
        Ok(record)
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "Session {session_id} belongs to {} portal, not {}",
            record.interface.label(),
            interface.label()
        )))
    }
}

fn prewarm_impl(
    state: &PortalState,
    session_id: &str,
    interface: PortalInterface,
) -> zbus::fdo::Result<()> {
    use aileron_varlink::aileron_Inference::VarlinkClientInterface;

    ensure_known_session(state, session_id, interface)?;
    let conn =
        aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    client
        .prewarm(session_id.to_string())
        .call()
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

    if let Some(record) = session_record(state, session_id) {
        state
            .warm_profiles
            .lock()
            .unwrap()
            .insert(record.profile_id);
    }
    Ok(())
}

fn end_session_impl(
    state: &PortalState,
    session_id: &str,
    interface: PortalInterface,
) -> zbus::fdo::Result<()> {
    ensure_known_session(state, session_id, interface)?;
    cancel_session_requests(state, session_id);
    state.sessions.lock().unwrap().remove(session_id);
    end_daemon_session_async(session_id.to_string());
    Ok(())
}

fn end_daemon_session_async(session_id: String) {
    thread::spawn(move || {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let Ok(conn) = aileron_ipc::client::connect() else {
            warn!("failed to connect to daemon while closing session {session_id}");
            return;
        };
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        if let Err(e) = client.end_session(session_id.clone()).call() {
            warn!("failed to close daemon session {session_id}: {e}");
        }
    });
}

fn is_permission_denied(error: &impl std::fmt::Display) -> bool {
    error.to_string().contains("PermissionDenied")
}

fn is_permission_prompt_required(error: &impl std::fmt::Display) -> bool {
    error.to_string().contains("PermissionPromptRequired")
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

fn prompt_permission(
    state: &PortalState,
    request_id: &str,
    app_id: &str,
    parent_window: &str,
    use_case: &str,
) -> zbus::fdo::Result<bool> {
    let text = format!(
        "Allow {app_id} to use the local model capability {use_case}?\n\nAileron will process this request locally using the assigned model."
    );
    let parent_xid = x11_parent_window_id(parent_window);

    if parent_xid.is_some()
        && let Ok(result) = run_kdialog_permission_prompt(state, request_id, &text, parent_xid)
    {
        return result;
    }

    let mut zenity = Command::new("zenity");
    zenity.args([
        "--question",
        "--title=Aileron Permission Request",
        "--ok-label=Allow",
        "--cancel-label=Deny",
        "--text",
        &text,
    ]);
    if let Some(xid) = parent_xid {
        zenity.arg(format!("--attach={xid}"));
    }
    if let Ok(result) = run_prompt_command(state, request_id, &mut zenity) {
        return result;
    }

    if parent_xid.is_none()
        && let Ok(result) = run_kdialog_permission_prompt(state, request_id, &text, None)
    {
        return result;
    }

    Err(zbus::fdo::Error::Failed(
        "No permission prompt helper found; install zenity or kdialog, grant permission in the Aileron Permissions page, or start the daemon with AILERON_AUTO_GRANT=true for development".to_string(),
    ))
}

fn run_kdialog_permission_prompt(
    state: &PortalState,
    request_id: &str,
    text: &str,
    parent_xid: Option<&str>,
) -> std::io::Result<zbus::fdo::Result<bool>> {
    let mut kdialog = Command::new("kdialog");
    kdialog.args(["--title", "Aileron Permission Request"]);
    if let Some(xid) = parent_xid {
        kdialog.args(["--attach", xid]);
    }
    kdialog.args(["--yesno", text]);
    run_prompt_command(state, request_id, &mut kdialog)
}

fn run_prompt_command(
    state: &PortalState,
    request_id: &str,
    command: &mut Command,
) -> std::io::Result<zbus::fdo::Result<bool>> {
    let mut child = command.spawn()?;
    loop {
        if let Err(e) = ensure_request_active(state, request_id) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Err(e));
        }

        if let Some(status) = child.try_wait()? {
            return Ok(Ok(status.success()));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn x11_parent_window_id(parent_window: &str) -> Option<&str> {
    parent_window
        .strip_prefix("x11:")
        .filter(|xid| !xid.is_empty())
}

impl LanguagePortalBackend {
    async fn emit_loading_if_cold(
        &self,
        request_id: &str,
        session_id: &str,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
        if self
            .state
            .warm_profiles
            .lock()
            .unwrap()
            .contains(&record.profile_id)
        {
            return Ok(());
        }

        LanguagePortalBackend::model_loading(emitter, request_id, session_id, "preparing model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(profile_id) = self
            .state
            .sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|record| record.profile_id.clone())
        {
            self.state.warm_profiles.lock().unwrap().insert(profile_id);
        }
    }
}

impl SpeechPortalBackend {
    async fn emit_loading_if_cold(
        &self,
        request_id: &str,
        session_id: &str,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let record = ensure_known_session(&self.state, session_id, PortalInterface::Speech)?;
        if self
            .state
            .warm_profiles
            .lock()
            .unwrap()
            .contains(&record.profile_id)
        {
            return Ok(());
        }

        SpeechPortalBackend::model_loading(emitter, request_id, session_id, "preparing model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(profile_id) = self
            .state
            .sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|record| record.profile_id.clone())
        {
            self.state.warm_profiles.lock().unwrap().insert(profile_id);
        }
    }
}

impl VisionPortalBackend {
    async fn emit_loading_if_cold(
        &self,
        request_id: &str,
        session_id: &str,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
        if self
            .state
            .warm_profiles
            .lock()
            .unwrap()
            .contains(&record.profile_id)
        {
            return Ok(());
        }

        VisionPortalBackend::model_loading(emitter, request_id, session_id, "preparing model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(profile_id) = self
            .state
            .sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|record| record.profile_id.clone())
        {
            self.state.warm_profiles.lock().unwrap().insert(profile_id);
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
        assert!(!is_permission_denied(
            &"aileron.Inference.PermissionPromptRequired"
        ));
        assert!(is_permission_prompt_required(
            &"aileron.Inference.PermissionPromptRequired"
        ));
    }

    #[test]
    fn x11_parent_window_id_extracts_xid_only_for_x11_handles() {
        assert_eq!(x11_parent_window_id("x11:1234"), Some("1234"));
        assert_eq!(x11_parent_window_id("x11:1a2b"), Some("1a2b"));
        assert_eq!(x11_parent_window_id("wayland:surface"), None);
        assert_eq!(x11_parent_window_id(""), None);
    }

    #[test]
    fn ensure_known_session_rejects_wrong_interface() {
        let state = PortalState::default();
        state.sessions.lock().unwrap().insert(
            "session-1".to_string(),
            SessionRecord {
                profile_id: "profile-1".to_string(),
                interface: PortalInterface::Language,
            },
        );

        assert!(
            ensure_known_session(&state, "session-1", PortalInterface::Language).is_ok(),
            "session should be valid on its owning interface"
        );
        let err = ensure_known_session(&state, "session-1", PortalInterface::Speech)
            .expect_err("wrong interface should be rejected");

        assert!(err.to_string().contains("Language portal"));
        assert!(err.to_string().contains("Speech"));
    }

    #[test]
    fn request_cancellation_rejects_active_request() {
        let state = PortalState::default();
        state.requests.lock().unwrap().insert(
            "request-1".to_string(),
            RequestRecord {
                session_id: None,
                cancelled: false,
            },
        );

        assert!(ensure_request_active(&state, "request-1").is_ok());
        cancel_request(&state, "request-1");

        let err =
            ensure_request_active(&state, "request-1").expect_err("cancelled request should fail");
        assert!(
            err.to_string()
                .contains("aileron.Inference.RequestCancelled")
        );
    }

    #[test]
    fn session_cancellation_marks_only_matching_requests() {
        let state = PortalState::default();
        state.requests.lock().unwrap().insert(
            "request-1".to_string(),
            RequestRecord {
                session_id: Some("session-1".to_string()),
                cancelled: false,
            },
        );
        state.requests.lock().unwrap().insert(
            "request-2".to_string(),
            RequestRecord {
                session_id: Some("session-2".to_string()),
                cancelled: false,
            },
        );

        cancel_session_requests(&state, "session-1");

        assert!(ensure_request_active(&state, "request-1").is_err());
        assert!(ensure_request_active(&state, "request-2").is_ok());
    }
}
