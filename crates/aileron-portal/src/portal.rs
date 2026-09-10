use aileron_varlink::aileron_Inference as inference;
use aileron_varlink::aileron_Inference::VarlinkClientInterface as _;
use aileron_varlink::aileron_Inference::VarlinkStreamingClientInterface as _;
use aileron_varlink::aileron_Permissions as permissions;
use aileron_varlink::aileron_Permissions::VarlinkClientInterface as _;
/// D-Bus portal backend for task-oriented local model capabilities.
use anyhow::Result;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::process::Command;
use tracing::{info, warn};
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Type};
use zbus::{connection, interface, message::Header, object_server::SignalEmitter};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.aileron";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const FRONTEND_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const MAX_PREWARM_WORKERS: usize = 4;
const CANCEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TRANSPORT_WORKERS: usize = 16;
const MAX_BACKGROUND_WORKERS: usize = 8;
static TRANSPORT_WORKERS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_TRANSPORT_WORKERS);
static BACKGROUND_WORKERS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_BACKGROUND_WORKERS);
// Cancellation and session teardown must not queue behind blocked inference reads.
static CONTROL_WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const MAX_SYNTHESIS_TEXT_BYTES: usize = 16 * 1024;
const MAX_SYNTHESIS_VOICE_ID_BYTES: usize = 128;
const MAX_SYNTHESIS_LANGUAGE_HINT_BYTES: usize = 64;
const MAX_AUDIO_CHUNK_BYTES: usize = 256 * 1024;
const MIN_AUDIO_SAMPLE_RATE: u32 = 8_000;
const MAX_AUDIO_SAMPLE_RATE: u32 = 192_000;
const MAX_AUDIO_CHANNELS: u32 = 2;
const LANGUAGE_GENERATION_USE_CASES: &[&str] = &[
    "language.summarize",
    "language.translate",
    "language.rephrase",
    "language.classify",
    "language.extract",
    "language.analyze",
];
const LANGUAGE_USE_CASES: &[&str] = &[
    "language.summarize",
    "language.translate",
    "language.rephrase",
    "language.classify",
    "language.extract",
    "language.analyze",
    "language.embed",
];
const SPEECH_USE_CASES: &[&str] = &["speech.transcribe", "speech.translate", "speech.synthesize"];
const VISION_USE_CASES: &[&str] = &[
    "vision.describe",
    "vision.ocr",
    "vision.detect",
    "vision.segment",
    "vision.depth",
];

pub async fn run() -> Result<()> {
    info!("registering D-Bus portal backend");

    let state = Arc::new(PortalState::default());
    let _conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, LanguagePortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, SpokenLanguagePortalBackend::new(state.clone()))?
        .serve_at(OBJECT_PATH, VisionPortalBackend::new(state))?
        .build()
        .await?;

    info!("D-Bus connection established; serving portal interfaces");
    std::future::pending::<()>().await;
    Ok(())
}

#[derive(Default)]
struct PortalState {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    requests: Mutex<HashMap<String, RequestRecord>>,
    active_synthesis_requests: Mutex<HashMap<String, String>>,
    prewarm_workers: Mutex<usize>,
    #[cfg(test)]
    daemon_address: Option<String>,
}

impl PortalState {
    fn daemon_address(&self) -> String {
        #[cfg(test)]
        if let Some(address) = &self.daemon_address {
            return address.clone();
        }
        aileron_ipc::varlink_address()
    }
}

struct RequestRecord {
    session_handle: Option<String>,
    daemon_session_id: Option<String>,
    cancelled: bool,
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PortalInterface {
    Language,
    SpokenLanguage,
    Vision,
}

impl PortalInterface {
    fn label(self) -> &'static str {
        match self {
            Self::Language => "Language",
            Self::SpokenLanguage => "Speech",
            Self::Vision => "Vision",
        }
    }
}

#[derive(Debug, Clone)]
struct SessionRecord {
    interface: PortalInterface,
    use_case: String,
    daemon_session_id: String,
    closing: bool,
}

struct LanguagePortalBackend {
    state: Arc<PortalState>,
}

struct SpokenLanguagePortalBackend {
    state: Arc<PortalState>,
}

struct VisionPortalBackend {
    state: Arc<PortalState>,
}

struct RequestPortalBackend {
    state: Arc<PortalState>,
    request_id: String,
}

struct SessionPortalBackend {
    state: Arc<PortalState>,
    session_handle: String,
}

struct PrewarmWorkerGuard {
    state: Arc<PortalState>,
}

impl Drop for PrewarmWorkerGuard {
    fn drop(&mut self) {
        let mut workers = self.state.prewarm_workers.lock().unwrap();
        *workers = workers.saturating_sub(1);
    }
}

#[cfg(test)]
async fn blocking<T: Send + 'static>(
    workers: &'static tokio::sync::Semaphore,
    request: Option<(&PortalState, &str)>,
    work: impl FnOnce() -> zbus::fdo::Result<T> + Send + 'static,
) -> zbus::fdo::Result<T> {
    let permit = match request {
        Some((state, request_id)) => while_request_active(state, request_id, workers.acquire())
            .await?
            .unwrap(),
        None => workers.acquire().await.unwrap(),
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
}

struct AsyncReplies<T> {
    receiver: tokio::sync::mpsc::Receiver<zbus::fdo::Result<T>>,
    worker: Option<tokio::task::JoinHandle<()>>,
    state: Arc<PortalState>,
    request_id: String,
    completed: Arc<AtomicBool>,
}

impl<T> AsyncReplies<T> {
    async fn recv(&mut self) -> Option<zbus::fdo::Result<T>> {
        let reply =
            match while_request_active(&self.state, &self.request_id, self.receiver.recv()).await {
                Ok(reply) => reply,
                Err(error) => return Some(Err(error)),
            };
        if let Some(reply) = reply {
            if reply.is_err() {
                // Worker errors terminate the RPC; dropping the handler must not
                // cancel another request on the same daemon session.
                self.worker.take();
            }
            return Some(reply);
        }
        if let Some(worker) = self.worker.take()
            && let Err(error) = worker.await
        {
            return Some(Err(map_request_transport_error(
                &self.state,
                &self.request_id,
                error,
            )));
        }
        None
    }
}

impl<T> Drop for AsyncReplies<T> {
    fn drop(&mut self) {
        self.receiver.close();
        if self.worker.is_some() && !self.completed.load(Ordering::Acquire) {
            let cancellation = cancel_request_record(&self.state, &self.request_id);
            let state = self.state.clone();
            let request_id = self.request_id.clone();
            tokio::spawn(async move {
                complete_request_cancellation(&state, &request_id, cancellation).await;
            });
        }
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

type StreamFuture<R> = std::pin::Pin<
    Box<dyn Future<Output = zlink::Result<inference::InferenceReplyStream<R>>> + Send>,
>;

async fn stream_replies<R>(
    state: Arc<PortalState>,
    request_id: &str,
    fds: Vec<OwnedFd>,
    background: bool,
    make_call: impl FnOnce(zlink::tokio::unix::Connection) -> StreamFuture<R> + Send + 'static,
) -> zbus::fdo::Result<AsyncReplies<R>>
where
    R: serde::de::DeserializeOwned + std::fmt::Debug + Send + 'static,
{
    // Acquire the background quota before shared capacity, leaving room for
    // interactive preemption and availability/session operations.
    let background_permit = if background {
        Some(
            while_request_active(&state, request_id, BACKGROUND_WORKERS.acquire())
                .await?
                .unwrap(),
        )
    } else {
        None
    };
    let permit = while_request_active(&state, request_id, TRANSPORT_WORKERS.acquire())
        .await?
        .unwrap();
    ensure_request_active(&state, request_id)?;
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let mut replies = AsyncReplies {
        receiver,
        worker: None,
        state: state.clone(),
        request_id: request_id.to_string(),
        completed: Arc::new(AtomicBool::new(false)),
    };
    let completed = replies.completed.clone();
    let request_id = request_id.to_string();
    replies.worker = Some(tokio::spawn(async move {
        let _permit = permit;
        let _background_permit = background_permit;
        // The daemon opens /proc paths lazily, so retain the fds through the last read.
        let _fds = fds;
        let result = async {
            let connection = connect_request_daemon(&state, &request_id).await?;
            let mut stream = await_request(&state, &request_id, make_call(connection))
                .await?
                .map_err(|e| map_request_transport_error(&state, &request_id, e))?;
            while let Some(reply) = await_request(&state, &request_id, stream.next()).await? {
                if stream.is_finished() {
                    // Publish completion before a terminal reply can block in the
                    // bounded channel. A delayed drop must not cancel newer work
                    // on this daemon session.
                    if let Some(record) = state.requests.lock().unwrap().get_mut(&request_id) {
                        record.daemon_session_id = None;
                    }
                    completed.store(true, Ordering::Release);
                }
                ensure_request_active(&state, &request_id)?;
                let reply = reply
                    .map_err(|e| map_request_transport_error(&state, &request_id, e))?
                    .map_err(map_inference_error)?;
                if await_request(&state, &request_id, sender.send(Ok(reply)))
                    .await?
                    .is_err()
                {
                    break;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let _ = sender.send(Err(error)).await;
        }
    }));
    Ok(replies)
}

async fn while_request_active<T>(
    state: &PortalState,
    request_id: &str,
    future: impl std::future::Future<Output = T>,
) -> zbus::fdo::Result<T> {
    tokio::pin!(future);
    loop {
        ensure_request_active(state, request_id)?;
        tokio::select! {
            result = &mut future => {
                ensure_request_active(state, request_id)?;
                return Ok(result);
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

impl LanguagePortalBackend {
    fn new(state: Arc<PortalState>) -> Self {
        Self { state }
    }
}

impl SpokenLanguagePortalBackend {
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

impl SessionPortalBackend {
    fn new(state: Arc<PortalState>, session_handle: &str) -> Self {
        Self {
            state,
            session_handle: session_handle.to_string(),
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
struct ResponseOptionsDbus {
    maximum_response_tokens: i64,
    temperature: f64,
    source_language_hint: String,
    target_language_hint: String,
    execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct GuidedOptionsDbus {
    maximum_response_tokens: i64,
    temperature: f64,
    execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct EmbedOptionsDbus {
    execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct SpeechOptionsDbus {
    source_language_hint: String,
    execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct SynthesisOptionsDbus {
    voice_id: String,
    language_hint: String,
    execution_mode: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct AudioMetadata {
    sample_rate: u32,
    channels: u32,
    sample_format: String,
}

struct DecodedAudioChunk {
    audio: Vec<u8>,
    metadata: AudioMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionOptionsDbus {
    execution_mode: String,
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
struct VisionDetectionDbus {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionPointPromptDbus {
    x: f64,
    y: f64,
    positive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionBoxPromptDbus {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionSegmentOptionsDbus {
    execution_mode: String,
    points: Vec<VisionPointPromptDbus>,
    boxes: Vec<VisionBoxPromptDbus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionMaskDbus {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    mask_base64: String,
    mask_width: i32,
    mask_height: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct VisionDepthMapDbus {
    width: i32,
    height: i32,
    values: Vec<f64>,
    unit: String,
    minimum: f64,
    maximum: f64,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestPortalBackend {
    async fn close(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        cancel_request(&self.state, &self.request_id).await;
        Ok(())
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionPortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        close_session_impl(conn, &self.state, &self.session_handle).await
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[interface(name = "org.freedesktop.impl.portal.Language")]
impl LanguagePortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
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
        Ok((get_use_case_availability_impl(app_id, use_case).await?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_interface_use_case(use_case, PortalInterface::Language)?;
        let request_id = request_handle.as_str();
        let session_handle = session_handle.as_str();
        begin_request(conn, &self.state, request_id, None).await?;
        let result = async {
            let daemon_session_id = create_session_impl(
                &self.state,
                request_id,
                app_id,
                parent_window,
                use_case,
                instructions,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                end_daemon_session_async(daemon_session_id);
                return Err(e);
            }
            begin_session(
                conn,
                &self.state,
                session_handle,
                daemon_session_id,
                use_case,
                PortalInterface::Language,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                abandon_created_session(conn, &self.state, session_handle).await;
                return Err(e);
            }
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_request_active(&self.state, request_id)?;
            LanguagePortalBackend::model_loading(
                &emitter,
                &request_handle,
                &session_handle,
                "preparing model",
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(
                self.state.clone(),
                request_id.to_string(),
                session_id.to_string(),
                record.daemon_session_id,
                PortalInterface::Language,
            )
            .await
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        message: &str,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_response(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        input_json: &str,
        media_fds: Vec<OwnedFd>,
        options: ResponseOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_language_generation_session(&record)?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let media_paths = media_fds.iter().map(fd_proc_path).collect::<Vec<_>>();
            let input_json = input_json.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                media_fds,
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_response(
                                daemon_session_id,
                                input_json,
                                media_paths,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_token: Option<String> = None;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let token = reply?.token;

                if let Some(previous) = pending_token.replace(token) {
                    LanguagePortalBackend::token_received(
                        &emitter,
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            LanguagePortalBackend::token_received(
                &emitter,
                &request_handle,
                &session_handle,
                pending_token.as_deref().unwrap_or_default(),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn token_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        token: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_respond_guided(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        prompt: &str,
        media_fds: Vec<OwnedFd>,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GuidedOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_language_generation_session(&record)?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let media_paths = media_fds.iter().map(fd_proc_path).collect::<Vec<_>>();
            let prompt = prompt.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                media_fds,
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_respond_guided(
                                daemon_session_id,
                                prompt,
                                media_paths,
                                fields
                                    .into_iter()
                                    .map(GuidedFieldDbus::into_varlink)
                                    .collect(),
                                tools
                                    .into_iter()
                                    .map(ToolDefinitionDbus::into_varlink)
                                    .collect(),
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_snapshot: Option<String> = None;
            let mut emitted_terminal_tool_calls = false;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let reply = reply?;
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
                        &request_handle,
                        &session_handle,
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
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if !emitted_terminal_tool_calls {
                LanguagePortalBackend::guided_snapshot_received(
                    &emitter,
                    &request_handle,
                    &session_handle,
                    pending_snapshot.as_deref().unwrap_or_default(),
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn guided_snapshot_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        snapshot_json: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn guided_tool_calls_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        tool_calls: &[ToolCallDbus],
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_submit_tool_results_guided(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        prompt: &str,
        media_fds: Vec<OwnedFd>,
        results: Vec<ToolResultDbus>,
        fields: Vec<GuidedFieldDbus>,
        tools: Vec<ToolDefinitionDbus>,
        options: GuidedOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_language_generation_session(&record)?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let media_paths = media_fds.iter().map(fd_proc_path).collect::<Vec<_>>();
            let prompt = prompt.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                media_fds,
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_submit_tool_results_guided(
                                daemon_session_id,
                                prompt,
                                media_paths,
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
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_snapshot: Option<String> = None;
            let mut emitted_terminal_tool_calls = false;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let reply = reply?;
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
                        &request_handle,
                        &session_handle,
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
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            if !emitted_terminal_tool_calls {
                LanguagePortalBackend::guided_snapshot_received(
                    &emitter,
                    &request_handle,
                    &session_handle,
                    pending_snapshot.as_deref().unwrap_or_default(),
                    true,
                )
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_embed(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        text: &str,
        options: EmbedOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Language)?;
            ensure_exact_session_use_case(&record, "language.embed", "StreamEmbed")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let text = text.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_embed(daemon_session_id, text, options.into_varlink())
                            .await
                    })
                },
            )
            .await?;

            let mut last_embedding = Vec::new();
            let mut embedding_pipeline_id = String::new();
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let reply = reply?;
                last_embedding = reply.embedding;
                embedding_pipeline_id = reply.embedding_pipeline_id;
            }

            ensure_request_active(&self.state, request_id)?;
            LanguagePortalBackend::embedding_received(
                &emitter,
                &request_handle,
                &session_handle,
                &last_embedding,
                &embedding_pipeline_id,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn embedding_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        embedding: &[f64],
        embedding_pipeline_id: &str,
        done: bool,
    ) -> zbus::Result<()>;
}

#[interface(name = "org.freedesktop.impl.portal.SpokenLanguage")]
#[allow(clippy::too_many_arguments)]
impl SpokenLanguagePortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
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
        Ok((get_use_case_availability_impl(app_id, use_case).await?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_interface_use_case(use_case, PortalInterface::SpokenLanguage)?;
        let request_id = request_handle.as_str();
        let session_handle = session_handle.as_str();
        begin_request(conn, &self.state, request_id, None).await?;
        let result = async {
            let daemon_session_id = create_session_impl(
                &self.state,
                request_id,
                app_id,
                parent_window,
                use_case,
                instructions,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                end_daemon_session_async(daemon_session_id);
                return Err(e);
            }
            begin_session(
                conn,
                &self.state,
                session_handle,
                daemon_session_id,
                use_case,
                PortalInterface::SpokenLanguage,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                abandon_created_session(conn, &self.state, session_handle).await;
                return Err(e);
            }
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record =
                ensure_known_session(&self.state, session_id, PortalInterface::SpokenLanguage)?;
            ensure_request_active(&self.state, request_id)?;
            SpokenLanguagePortalBackend::model_loading(
                &emitter,
                &request_handle,
                &session_handle,
                "preparing model",
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(
                self.state.clone(),
                request_id.to_string(),
                session_id.to_string(),
                record.daemon_session_id,
                PortalInterface::SpokenLanguage,
            )
            .await
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        message: &str,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_transcribe(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        audio_fd: OwnedFd,
        options: SpeechOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record =
                ensure_known_session(&self.state, session_id, PortalInterface::SpokenLanguage)?;
            ensure_speech_session_use_case(&record, "StreamTranscribe")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let audio_path = fd_proc_path(&audio_fd);
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![audio_fd],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_transcribe(
                                daemon_session_id,
                                audio_path,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_text: Option<String> = None;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let text = reply?.token;

                if let Some(previous) = pending_text.replace(text) {
                    SpokenLanguagePortalBackend::transcription_received(
                        &emitter,
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            SpokenLanguagePortalBackend::transcription_received(
                &emitter,
                &request_handle,
                &session_handle,
                pending_text.as_deref().unwrap_or_default(),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn transcription_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        text: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_synthesize(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        text: &str,
        options: SynthesisOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        validate_synthesis_input(text, &options)?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record =
                ensure_known_session(&self.state, session_id, PortalInterface::SpokenLanguage)?;
            ensure_exact_session_use_case(&record, "speech.synthesize", "StreamSynthesize")?;
            begin_synthesis_request(&self.state, session_id, request_id)?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let text = text.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_synthesize(
                                record.daemon_session_id,
                                text,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut metadata = None;
            let mut received_audio = false;
            let mut terminal_seen = false;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let chunk = reply?.chunk;
                let decoded = decode_audio_chunk(chunk, metadata.as_ref())?;
                metadata = Some(decoded.metadata.clone());
                let done = decoded.audio.is_empty();
                if terminal_seen {
                    return Err(invalid_audio_output("received data after terminal chunk"));
                }
                received_audio |= !done;
                terminal_seen = done;
                emit_audio_chunk(&emitter, &request_handle, &session_handle, &decoded, done)
                    .await?;
            }

            ensure_request_active(&self.state, request_id)?;
            if !received_audio {
                return Err(invalid_audio_output(
                    "synthesis returned no non-empty audio chunk",
                ));
            }
            if !terminal_seen {
                return Err(invalid_audio_output(
                    "synthesis returned no terminal audio chunk",
                ));
            }
            Ok(())
        }
        .await;
        finish_synthesis_request(&self.state, session_id, request_id);
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(signal)]
    async fn audio_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        audio: &[u8],
        sample_rate: u32,
        channels: u32,
        sample_format: &str,
        done: bool,
    ) -> zbus::Result<()>;
}

#[interface(name = "org.freedesktop.impl.portal.Vision")]
impl VisionPortalBackend {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
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
        Ok((get_use_case_availability_impl(app_id, use_case).await?,))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        ensure_interface_use_case(use_case, PortalInterface::Vision)?;
        let request_id = request_handle.as_str();
        let session_handle = session_handle.as_str();
        begin_request(conn, &self.state, request_id, None).await?;
        let result = async {
            let daemon_session_id = create_session_impl(
                &self.state,
                request_id,
                app_id,
                parent_window,
                use_case,
                instructions,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                end_daemon_session_async(daemon_session_id);
                return Err(e);
            }
            begin_session(
                conn,
                &self.state,
                session_handle,
                daemon_session_id,
                use_case,
                PortalInterface::Vision,
            )
            .await?;
            if let Err(e) = ensure_request_active(&self.state, request_id) {
                abandon_created_session(conn, &self.state, session_handle).await;
                return Err(e);
            }
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    async fn prewarm(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::model_loading(
                &emitter,
                &request_handle,
                &session_handle,
                "preparing model",
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            ensure_request_active(&self.state, request_id)?;
            prewarm_impl(
                self.state.clone(),
                request_id.to_string(),
                session_id.to_string(),
                record.daemon_session_id,
                PortalInterface::Vision,
            )
            .await
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn model_loading(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        message: &str,
    ) -> zbus::Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn stream_describe(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        image_fd: OwnedFd,
        instructions: &str,
        options: VisionOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_exact_session_use_case(&record, "vision.describe", "StreamDescribe")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let image_path = fd_proc_path(&image_fd);
            let instructions = instructions.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![image_fd],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_describe(
                                daemon_session_id,
                                image_path,
                                instructions,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_text: Option<String> = None;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let text = reply?.token;

                if let Some(previous) = pending_text.replace(text) {
                    VisionPortalBackend::vision_text_received(
                        &emitter,
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_text_received(
                &emitter,
                &request_handle,
                &session_handle,
                pending_text.as_deref().unwrap_or_default(),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_ocr(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        image_fd: OwnedFd,
        instructions: &str,
        options: VisionOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_exact_session_use_case(&record, "vision.ocr", "StreamOcr")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let image_path = fd_proc_path(&image_fd);
            let instructions = instructions.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![image_fd],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_ocr(
                                daemon_session_id,
                                image_path,
                                instructions,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut pending_text: Option<String> = None;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let text = reply?.token;

                if let Some(previous) = pending_text.replace(text) {
                    VisionPortalBackend::vision_text_received(
                        &emitter,
                        &request_handle,
                        &session_handle,
                        &previous,
                        false,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_text_received(
                &emitter,
                &request_handle,
                &session_handle,
                pending_text.as_deref().unwrap_or_default(),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_detect(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        image_fd: OwnedFd,
        instructions: &str,
        options: VisionOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_exact_session_use_case(&record, "vision.detect", "StreamDetect")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let image_path = fd_proc_path(&image_fd);
            let instructions = instructions.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![image_fd],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_detect(
                                daemon_session_id,
                                image_path,
                                instructions,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut last_detections = Vec::new();
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                last_detections = reply?
                    .detections
                    .into_iter()
                    .map(|detection| VisionDetectionDbus {
                        label: detection.label,
                        confidence: detection.confidence,
                        x: detection.x,
                        y: detection.y,
                        width: detection.width,
                        height: detection.height,
                    })
                    .collect();
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_detections_received(
                &emitter,
                &request_handle,
                &session_handle,
                &last_detections,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_segment(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        image_fd: OwnedFd,
        instructions: &str,
        options: VisionSegmentOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_exact_session_use_case(&record, "vision.segment", "StreamSegment")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let image_path = fd_proc_path(&image_fd);
            let instructions = instructions.to_string();
            let mut replies = stream_replies(
                self.state.clone(), request_id, vec![image_fd], options.execution_mode == "background",
                move |client| Box::pin(async move { client.stream_segment(
                    daemon_session_id,
                    image_path,
                    instructions,
                    options.into_varlink(),
                ).await }),
            )
            .await?;

            let mut last_masks = Vec::new();
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                last_masks = reply?
                    .masks
                    .into_iter()
                    .map(|mask| {
                        Ok(VisionMaskDbus {
                            label: mask.label,
                            confidence: mask.confidence,
                            x: mask.x,
                            y: mask.y,
                            width: mask.width,
                            height: mask.height,
                            mask_base64: mask.mask_base64,
                            mask_width: i32::try_from(mask.mask_width).map_err(|_| {
                                zbus::fdo::Error::Failed(
                                    "aileron.Inference.InvalidOutput: mask width exceeds D-Bus int32"
                                        .to_string(),
                                )
                            })?,
                            mask_height: i32::try_from(mask.mask_height).map_err(|_| {
                                zbus::fdo::Error::Failed(
                                    "aileron.Inference.InvalidOutput: mask height exceeds D-Bus int32"
                                        .to_string(),
                                )
                            })?,
                        })
                    })
                    .collect::<zbus::fdo::Result<Vec<_>>>()?;
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_masks_received(
                &emitter,
                &request_handle,
                &session_handle,
                &last_masks,
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_depth(
        &self,
        request_handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        image_fd: OwnedFd,
        instructions: &str,
        options: VisionOptionsDbus,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        ensure_portal_frontend(conn, &header).await?;
        let request_id = request_handle.as_str();
        let session_id = session_handle.as_str();
        begin_request(conn, &self.state, request_id, Some(session_id)).await?;
        let result = async {
            let record = ensure_known_session(&self.state, session_id, PortalInterface::Vision)?;
            ensure_exact_session_use_case(&record, "vision.depth", "StreamDepth")?;
            ensure_request_active(&self.state, request_id)?;
            self.emit_loading(&request_handle, &session_handle, &emitter)
                .await?;
            ensure_request_active(&self.state, request_id)?;
            let daemon_session_id = record.daemon_session_id;
            let image_path = fd_proc_path(&image_fd);
            let instructions = instructions.to_string();
            let mut replies = stream_replies(
                self.state.clone(),
                request_id,
                vec![image_fd],
                options.execution_mode == "background",
                move |client| {
                    Box::pin(async move {
                        client
                            .stream_depth(
                                daemon_session_id,
                                image_path,
                                instructions,
                                options.into_varlink(),
                            )
                            .await
                    })
                },
            )
            .await?;

            let mut last_depth = None;
            while let Some(reply) = replies.recv().await {
                ensure_request_active(&self.state, request_id)?;
                let depth = reply?.depth;
                last_depth = Some(depth_map_into_dbus(depth)?);
            }

            ensure_request_active(&self.state, request_id)?;
            VisionPortalBackend::vision_depth_received(
                &emitter,
                &request_handle,
                &session_handle,
                &last_depth.unwrap_or(VisionDepthMapDbus {
                    width: 1,
                    height: 1,
                    values: vec![0.0],
                    unit: "meter".to_string(),
                    minimum: 0.0,
                    maximum: 0.0,
                }),
                true,
            )
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            Ok(())
        }
        .await;
        finish_request(conn, &self.state, request_id).await;
        result
    }

    #[zbus(signal)]
    async fn vision_text_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        text: &str,
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vision_detections_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        detections: &[VisionDetectionDbus],
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vision_masks_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        masks: &[VisionMaskDbus],
        done: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vision_depth_received(
        emitter: &SignalEmitter<'_>,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        depth: &VisionDepthMapDbus,
        done: bool,
    ) -> zbus::Result<()>;
}

fn depth_map_into_dbus(
    depth: aileron_varlink::aileron_Inference::VisionDepthMap,
) -> zbus::fdo::Result<VisionDepthMapDbus> {
    Ok(VisionDepthMapDbus {
        width: i32::try_from(depth.width).map_err(|_| {
            zbus::fdo::Error::Failed(
                "aileron.Inference.InvalidOutput: depth width exceeds D-Bus int32".to_string(),
            )
        })?,
        height: i32::try_from(depth.height).map_err(|_| {
            zbus::fdo::Error::Failed(
                "aileron.Inference.InvalidOutput: depth height exceeds D-Bus int32".to_string(),
            )
        })?,
        values: depth.values,
        unit: depth.unit,
        minimum: depth.minimum,
        maximum: depth.maximum,
    })
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

fn ensure_interface_use_case(use_case: &str, interface: PortalInterface) -> zbus::fdo::Result<()> {
    let supported = supported_use_cases(interface);
    if supported.contains(&use_case) {
        return Ok(());
    }

    Err(zbus::fdo::Error::Failed(format!(
        "aileron.Inference.InvalidInput: {} portal does not support use-case {use_case}; supported use-cases: {}",
        interface.label(),
        supported.join(", ")
    )))
}

fn supported_use_cases(interface: PortalInterface) -> &'static [&'static str] {
    match interface {
        PortalInterface::Language => LANGUAGE_USE_CASES,
        PortalInterface::SpokenLanguage => SPEECH_USE_CASES,
        PortalInterface::Vision => VISION_USE_CASES,
    }
}

async fn begin_request(
    conn: &zbus::Connection,
    state: &Arc<PortalState>,
    request_id: &str,
    session_handle: Option<&str>,
) -> zbus::fdo::Result<()> {
    {
        let mut requests = state.requests.lock().unwrap();
        if requests.contains_key(request_id) {
            return Err(zbus::fdo::Error::Failed(format!(
                "request object {request_id} already exists"
            )));
        }

        requests.insert(
            request_id.to_string(),
            RequestRecord {
                session_handle: session_handle.map(str::to_string),
                daemon_session_id: None,
                cancelled: false,
                cancel_tx: tokio::sync::watch::channel(false).0,
            },
        );
    }

    let added = match conn
        .object_server()
        .at(
            request_id,
            RequestPortalBackend::new(state.clone(), request_id),
        )
        .await
    {
        Ok(added) => added,
        Err(e) => {
            finish_request_record(state, request_id);
            return Err(zbus::fdo::Error::Failed(e.to_string()));
        }
    };
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

async fn cancel_request(state: &PortalState, request_id: &str) {
    let cancellation = cancel_request_record(state, request_id);
    complete_request_cancellation(state, request_id, cancellation).await;
}

fn cancel_request_record(
    state: &PortalState,
    request_id: &str,
) -> Option<(Option<String>, Option<String>)> {
    let (daemon_session_id, session_handle) = {
        let mut requests = state.requests.lock().unwrap();
        let record = requests.get_mut(request_id)?;
        record.cancelled = true;
        record.cancel_tx.send_replace(true);
        (
            record.daemon_session_id.take(),
            record.session_handle.clone(),
        )
    };

    Some((daemon_session_id, session_handle))
}

async fn complete_request_cancellation(
    state: &PortalState,
    request_id: &str,
    cancellation: Option<(Option<String>, Option<String>)>,
) {
    let Some((daemon_session_id, session_handle)) = cancellation else {
        return;
    };
    if let Some(session_id) = daemon_session_id {
        let cancel = async {
            let _permit = CONTROL_WORKERS.acquire().await.unwrap();
            match connect_daemon(state).await {
                Ok(mut client) => match client.cancel_active_request(session_id).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!("failed to cancel active daemon request: {error:?}"),
                    Err(error) => warn!("failed to cancel active daemon request: {error}"),
                },
                Err(error) => warn!("failed to connect while cancelling daemon request: {error}"),
            }
        };
        if tokio::time::timeout(CANCEL_REQUEST_TIMEOUT, cancel)
            .await
            .is_err()
        {
            warn!("timed out while cancelling active daemon request");
        }
    }
    if let Some(session_handle) = session_handle {
        finish_synthesis_request(state, &session_handle, request_id);
    }
}

fn attach_request_daemon_session(
    state: &PortalState,
    request_id: &str,
    daemon_session_id: &str,
) -> zbus::fdo::Result<()> {
    let mut requests = state.requests.lock().unwrap();
    let record = requests
        .get_mut(request_id)
        .ok_or_else(request_cancelled_error)?;
    if record.cancelled {
        return Err(request_cancelled_error());
    }
    record.daemon_session_id = Some(daemon_session_id.to_string());
    Ok(())
}

fn begin_synthesis_request(
    state: &PortalState,
    session_id: &str,
    request_id: &str,
) -> zbus::fdo::Result<()> {
    let mut active = state.active_synthesis_requests.lock().unwrap();
    if let Some(existing) = active.get(session_id) {
        return Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: speech synthesis session already has active request {existing}"
        )));
    }
    active.insert(session_id.to_string(), request_id.to_string());
    Ok(())
}

fn finish_synthesis_request(state: &PortalState, session_id: &str, request_id: &str) {
    let mut active = state.active_synthesis_requests.lock().unwrap();
    if active
        .get(session_id)
        .is_some_and(|active| active == request_id)
    {
        active.remove(session_id);
    }
}

fn cancel_session_requests(state: &PortalState, session_id: &str) {
    let mut requests = state.requests.lock().unwrap();
    for record in requests.values_mut() {
        if record.session_handle.as_deref() == Some(session_id) {
            record.cancelled = true;
            record.cancel_tx.send_replace(true);
        }
    }
}

fn request_cancellation(
    state: &PortalState,
    request_id: &str,
) -> zbus::fdo::Result<tokio::sync::watch::Receiver<bool>> {
    state
        .requests
        .lock()
        .unwrap()
        .get(request_id)
        .filter(|record| !record.cancelled)
        .map(|record| record.cancel_tx.subscribe())
        .ok_or_else(request_cancelled_error)
}

async fn await_request<T>(
    state: &PortalState,
    request_id: &str,
    future: impl Future<Output = T>,
) -> zbus::fdo::Result<T> {
    let mut cancellation = request_cancellation(state, request_id)?;
    tokio::select! {
        biased;
        _ = cancellation.wait_for(|cancelled| *cancelled) => Err(request_cancelled_error()),
        output = future => Ok(output),
    }
}

async fn connect_request_daemon(
    state: &PortalState,
    request_id: &str,
) -> zbus::fdo::Result<zlink::tokio::unix::Connection> {
    ensure_request_active(state, request_id)?;
    let session_handle = state
        .requests
        .lock()
        .unwrap()
        .get(request_id)
        .and_then(|record| record.session_handle.clone());
    if let Some(session_handle) = session_handle {
        let record = session_record(state, &session_handle).ok_or_else(request_cancelled_error)?;
        if record.closing {
            return Err(request_cancelled_error());
        }
        attach_request_daemon_session(state, request_id, &record.daemon_session_id)?;
    }
    await_request(state, request_id, connect_daemon(state))
        .await?
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
}

async fn connect_daemon(state: &PortalState) -> zlink::Result<zlink::tokio::unix::Connection> {
    let address = state.daemon_address();
    zlink::tokio::unix::connect(address.strip_prefix("unix:").unwrap_or(&address)).await
}

fn ensure_request_active(state: &PortalState, request_id: &str) -> zbus::fdo::Result<()> {
    if state
        .requests
        .lock()
        .unwrap()
        .get(request_id)
        .map(|record| record.cancelled)
        .unwrap_or(true)
    {
        return Err(request_cancelled_error());
    }

    Ok(())
}

fn request_is_cancelled(state: &PortalState, request_id: &str) -> bool {
    state
        .requests
        .lock()
        .unwrap()
        .get(request_id)
        .map(|record| record.cancelled)
        .unwrap_or(false)
}

fn map_request_transport_error(
    state: &PortalState,
    request_id: &str,
    error: impl std::fmt::Display,
) -> zbus::fdo::Error {
    if request_is_cancelled(state, request_id) {
        request_cancelled_error()
    } else {
        zbus::fdo::Error::Failed(error.to_string())
    }
}

fn map_inference_error(error: inference::Error) -> zbus::fdo::Error {
    use inference::Error;

    match error {
        Error::PermissionDenied { app_id, use_case } => zbus::fdo::Error::AccessDenied(format!(
            "aileron.Inference.PermissionDenied: permission denied for {app_id} / {use_case}"
        )),
        Error::PermissionPromptRequired { app_id, use_case } => zbus::fdo::Error::Failed(format!(
            "aileron.Inference.PermissionPromptRequired: permission prompt required for {app_id} / {use_case}"
        )),
        Error::SessionNotFound { session_id } => zbus::fdo::Error::Failed(format!(
            "aileron.Inference.SessionNotFound: session {session_id} was not found"
        )),
        Error::ModelUnavailable { reason } => inference_failure("ModelUnavailable", reason),
        Error::InvalidGenerationOptions { reason } => {
            inference_failure("InvalidGenerationOptions", reason)
        }
        Error::GuidedGenerationFailed { reason } => {
            inference_failure("GuidedGenerationFailed", reason)
        }
        Error::GenerationFailed { reason } => inference_failure("GenerationFailed", reason),
        Error::ContextWindowExceeded { reason } => {
            inference_failure("ContextWindowExceeded", reason)
        }
        Error::UnsupportedLanguage { reason } => inference_failure("UnsupportedLanguage", reason),
        Error::SafetyRefusal { reason } => inference_failure("SafetyRefusal", reason),
        Error::RequestCancelled { reason } => inference_failure("RequestCancelled", reason),
        Error::InvalidInput { reason } => inference_failure("InvalidInput", reason),
    }
}

fn inference_failure(name: &str, reason: String) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("aileron.Inference.{name}: {reason}"))
}

fn request_cancelled_error() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(
        "aileron.Inference.RequestCancelled: request was cancelled".to_string(),
    )
}

fn fd_proc_path(fd: &OwnedFd) -> String {
    format!("/proc/{}/fd/{}", std::process::id(), fd.as_raw_fd())
}

async fn get_use_case_availability_impl(
    app_id: &str,
    use_case: &str,
) -> zbus::fdo::Result<ModelAvailabilityDbus> {
    let mut client = aileron_ipc::client::connect()
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let reply = client
        .get_use_case_availability(app_id.to_string(), use_case.to_string())
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(map_inference_error)?;

    Ok(ModelAvailabilityDbus {
        is_available: reply.availability.is_available,
        code: reply.availability.code,
        reason: reply.availability.reason,
    })
}

async fn create_session_impl(
    state: &PortalState,
    request_id: &str,
    app_id: &str,
    parent_window: &str,
    use_case: &str,
    instructions: &str,
) -> zbus::fdo::Result<String> {
    let mut client = connect_request_daemon(state, request_id).await?;
    let reply = match await_request(
        state,
        request_id,
        client.create_session(
            app_id.to_string(),
            use_case.to_string(),
            instructions.to_string(),
        ),
    )
    .await?
    .map_err(|e| map_request_transport_error(state, request_id, e))?
    {
        Ok(reply) => reply,
        Err(inference::Error::PermissionDenied { app_id, use_case }) => {
            return Err(zbus::fdo::Error::AccessDenied(format!(
                "aileron.Inference.PermissionDenied: permission denied for {app_id} / {use_case}"
            )));
        }
        Err(inference::Error::PermissionPromptRequired { .. }) => {
            ensure_request_active(state, request_id)?;
            if !prompt_permission(state, request_id, app_id, parent_window, use_case).await? {
                set_permission_for_request(state, request_id, app_id, use_case, false).await?;
                ensure_request_active(state, request_id)?;
                return Err(zbus::fdo::Error::AccessDenied(format!(
                    "aileron.Inference.PermissionDenied: permission denied for {app_id} / {use_case}"
                )));
            }
            ensure_request_active(state, request_id)?;
            set_permission_for_request(state, request_id, app_id, use_case, true).await?;
            await_request(
                state,
                request_id,
                client.create_session(
                    app_id.to_string(),
                    use_case.to_string(),
                    instructions.to_string(),
                ),
            )
            .await?
            .map_err(|e| map_request_transport_error(state, request_id, e))?
            .map_err(map_inference_error)?
        }
        Err(error) => return Err(map_inference_error(error)),
    };

    if let Err(e) = ensure_request_active(state, request_id) {
        end_daemon_session_async(reply.session_id.clone());
        return Err(e);
    }

    Ok(reply.session_id)
}

async fn begin_session(
    conn: &zbus::Connection,
    state: &Arc<PortalState>,
    session_handle: &str,
    daemon_session_id: String,
    use_case: &str,
    interface: PortalInterface,
) -> zbus::fdo::Result<()> {
    {
        let mut sessions = state.sessions.lock().unwrap();
        if sessions.contains_key(session_handle) {
            end_daemon_session_async(daemon_session_id);
            return Err(zbus::fdo::Error::Failed(format!(
                "session object {session_handle} already exists"
            )));
        }

        sessions.insert(
            session_handle.to_string(),
            SessionRecord {
                interface,
                use_case: use_case.to_string(),
                daemon_session_id: daemon_session_id.clone(),
                closing: false,
            },
        );
    }

    let added = match conn
        .object_server()
        .at(
            session_handle,
            SessionPortalBackend::new(state.clone(), session_handle),
        )
        .await
    {
        Ok(added) => added,
        Err(e) => {
            finish_session_record(state, session_handle);
            end_daemon_session_async(daemon_session_id);
            return Err(zbus::fdo::Error::Failed(e.to_string()));
        }
    };
    if !added {
        finish_session_record(state, session_handle);
        end_daemon_session_async(daemon_session_id);
        return Err(zbus::fdo::Error::Failed(format!(
            "session object {session_handle} already exists"
        )));
    }

    Ok(())
}

async fn close_session_impl(
    conn: &zbus::Connection,
    state: &PortalState,
    session_handle: &str,
) -> zbus::fdo::Result<()> {
    let record = start_session_close(state, session_handle)?;
    cancel_session_requests(state, session_handle);
    if let Err(e) = end_daemon_session(record.daemon_session_id).await {
        set_session_closing(state, session_handle, false);
        return Err(e);
    }
    finish_session_record(state, session_handle);
    if let Err(e) = conn
        .object_server()
        .remove::<SessionPortalBackend, _>(session_handle)
        .await
    {
        warn!("failed to remove portal session {session_handle}: {e}");
    }
    Ok(())
}

async fn abandon_created_session(
    conn: &zbus::Connection,
    state: &PortalState,
    session_handle: &str,
) {
    cancel_session_requests(state, session_handle);
    if let Some(record) = finish_session_record(state, session_handle) {
        end_daemon_session_async(record.daemon_session_id);
    }
    if let Err(e) = conn
        .object_server()
        .remove::<SessionPortalBackend, _>(session_handle)
        .await
    {
        warn!("failed to remove cancelled portal session {session_handle}: {e}");
    }
}

fn finish_session_record(state: &PortalState, session_handle: &str) -> Option<SessionRecord> {
    state.sessions.lock().unwrap().remove(session_handle)
}

fn start_session_close(
    state: &PortalState,
    session_handle: &str,
) -> zbus::fdo::Result<SessionRecord> {
    let mut sessions = state.sessions.lock().unwrap();
    let Some(record) = sessions.get_mut(session_handle) else {
        return Err(zbus::fdo::Error::AccessDenied(format!(
            "Unknown session {session_handle}"
        )));
    };
    if record.closing {
        return Err(zbus::fdo::Error::AccessDenied(format!(
            "Session {session_handle} is already closing"
        )));
    }
    record.closing = true;
    Ok(record.clone())
}

fn set_session_closing(state: &PortalState, session_handle: &str, closing: bool) {
    if let Some(record) = state.sessions.lock().unwrap().get_mut(session_handle) {
        record.closing = closing;
    }
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
    if record.closing {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "Session {session_id} is closing"
        )))
    } else if record.interface == interface {
        Ok(record)
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "Session {session_id} belongs to {} portal, not {}",
            record.interface.label(),
            interface.label()
        )))
    }
}

fn ensure_language_generation_session(record: &SessionRecord) -> zbus::fdo::Result<()> {
    if LANGUAGE_GENERATION_USE_CASES.contains(&record.use_case.as_str()) {
        Ok(())
    } else {
        Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: full text generation requires a language generation use-case, got {use_case}",
            use_case = record.use_case
        )))
    }
}

fn ensure_speech_session_use_case(record: &SessionRecord, method: &str) -> zbus::fdo::Result<()> {
    if matches!(
        record.use_case.as_str(),
        "speech.transcribe" | "speech.translate"
    ) {
        Ok(())
    } else {
        Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: {method} requires use-case speech.transcribe or speech.translate, got {use_case}",
            use_case = record.use_case
        )))
    }
}

fn ensure_exact_session_use_case(
    record: &SessionRecord,
    expected: &str,
    method: &str,
) -> zbus::fdo::Result<()> {
    if record.use_case == expected {
        Ok(())
    } else {
        Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: {method} requires use-case {expected}, got {}",
            record.use_case
        )))
    }
}

fn validate_synthesis_input(text: &str, options: &SynthesisOptionsDbus) -> zbus::fdo::Result<()> {
    if text.trim().is_empty() {
        return Err(zbus::fdo::Error::Failed(
            "aileron.Inference.InvalidInput: synthesis text must not be empty".to_string(),
        ));
    }
    if text.len() > MAX_SYNTHESIS_TEXT_BYTES {
        return Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: synthesis text exceeds {MAX_SYNTHESIS_TEXT_BYTES} UTF-8 bytes"
        )));
    }
    if options.voice_id.len() > MAX_SYNTHESIS_VOICE_ID_BYTES {
        return Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: voice ID exceeds {MAX_SYNTHESIS_VOICE_ID_BYTES} UTF-8 bytes"
        )));
    }
    if options.language_hint.len() > MAX_SYNTHESIS_LANGUAGE_HINT_BYTES {
        return Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: language hint exceeds {MAX_SYNTHESIS_LANGUAGE_HINT_BYTES} UTF-8 bytes"
        )));
    }
    if !matches!(
        options.execution_mode.as_str(),
        "interactive" | "background"
    ) {
        return Err(zbus::fdo::Error::Failed(format!(
            "aileron.Inference.InvalidInput: unsupported execution mode {}",
            options.execution_mode
        )));
    }
    Ok(())
}

fn invalid_audio_output(reason: impl Into<String>) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!(
        "aileron.Inference.GenerationFailed: invalid synthesized audio: {}",
        reason.into()
    ))
}

fn decode_audio_chunk(
    chunk: aileron_varlink::aileron_Inference::AudioChunk,
    expected: Option<&AudioMetadata>,
) -> zbus::fdo::Result<DecodedAudioChunk> {
    let sample_rate = u32::try_from(chunk.sample_rate)
        .ok()
        .filter(|rate| (MIN_AUDIO_SAMPLE_RATE..=MAX_AUDIO_SAMPLE_RATE).contains(rate))
        .ok_or_else(|| invalid_audio_output("sample rate is outside the supported range"))?;
    let channels = u32::try_from(chunk.channels)
        .ok()
        .filter(|channels| (1..=MAX_AUDIO_CHANNELS).contains(channels))
        .ok_or_else(|| invalid_audio_output("channel count is outside the supported range"))?;
    if chunk.sample_format != "s16le" {
        return Err(invalid_audio_output(format!(
            "unsupported sample format {}",
            chunk.sample_format
        )));
    }

    let metadata = AudioMetadata {
        sample_rate,
        channels,
        sample_format: chunk.sample_format,
    };
    if expected.is_some_and(|expected| expected != &metadata) {
        return Err(invalid_audio_output(
            "sample metadata changed during synthesis",
        ));
    }

    let max_encoded_len = MAX_AUDIO_CHUNK_BYTES.div_ceil(3) * 4;
    if chunk.audio_base64.len() > max_encoded_len {
        return Err(invalid_audio_output(format!(
            "audio chunk exceeds {MAX_AUDIO_CHUNK_BYTES} decoded bytes"
        )));
    }
    let audio = base64::engine::general_purpose::STANDARD
        .decode(chunk.audio_base64)
        .map_err(|_| invalid_audio_output("audio chunk is not valid base64"))?;
    if audio.len() > MAX_AUDIO_CHUNK_BYTES {
        return Err(invalid_audio_output(format!(
            "audio chunk exceeds {MAX_AUDIO_CHUNK_BYTES} decoded bytes"
        )));
    }
    let frame_size = usize::try_from(channels).unwrap() * 2;
    if audio.len() % frame_size != 0 {
        return Err(invalid_audio_output(
            "audio chunk ends partway through a sample frame",
        ));
    }

    Ok(DecodedAudioChunk { audio, metadata })
}

async fn emit_audio_chunk(
    emitter: &SignalEmitter<'_>,
    request_handle: &OwnedObjectPath,
    session_handle: &OwnedObjectPath,
    chunk: &DecodedAudioChunk,
    done: bool,
) -> zbus::fdo::Result<()> {
    SpokenLanguagePortalBackend::audio_received(
        emitter,
        request_handle,
        session_handle,
        &chunk.audio,
        chunk.metadata.sample_rate,
        chunk.metadata.channels,
        &chunk.metadata.sample_format,
        done,
    )
    .await
    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
}

fn acquire_prewarm_worker(state: &PortalState) -> zbus::fdo::Result<()> {
    let mut workers = state.prewarm_workers.lock().unwrap();
    if *workers >= MAX_PREWARM_WORKERS {
        return Err(zbus::fdo::Error::Failed(
            "aileron.Inference.ModelUnavailable: too many concurrent Prewarm operations"
                .to_string(),
        ));
    }

    *workers += 1;
    Ok(())
}

async fn prewarm_impl(
    state: Arc<PortalState>,
    request_id: String,
    session_handle: String,
    daemon_session_id: String,
    interface: PortalInterface,
) -> zbus::fdo::Result<()> {
    ensure_known_session(&state, &session_handle, interface)?;
    ensure_request_active(&state, &request_id)?;
    acquire_prewarm_worker(&state)?;
    let _guard = PrewarmWorkerGuard {
        state: state.clone(),
    };
    ensure_request_active(&state, &request_id)?;
    let mut client = connect_request_daemon(&state, &request_id).await?;
    await_request(&state, &request_id, client.prewarm(daemon_session_id))
        .await?
        .map_err(|e| map_request_transport_error(&state, &request_id, e))?
        .map_err(map_inference_error)
}

async fn end_daemon_session(session_id: String) -> zbus::fdo::Result<()> {
    let mut client = aileron_ipc::client::connect()
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    client
        .end_session(session_id)
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(map_inference_error)
}

fn end_daemon_session_async(session_id: String) {
    tokio::spawn(async move {
        if let Err(e) = end_daemon_session(session_id.clone()).await {
            warn!("failed to close daemon session {session_id}: {e}");
        }
    });
}

async fn set_permission_for_request(
    state: &PortalState,
    request_id: &str,
    app_id: &str,
    use_case: &str,
    allowed: bool,
) -> zbus::fdo::Result<()> {
    let mut client = connect_request_daemon(state, request_id).await?;
    await_request(
        state,
        request_id,
        client.set_app_permission(app_id.to_string(), use_case.to_string(), allowed),
    )
    .await?
    .map_err(|e| map_request_transport_error(state, request_id, e))?
    .map_err(map_permissions_error)?;
    Ok(())
}

fn map_permissions_error(error: permissions::Error) -> zbus::fdo::Error {
    match error {
        permissions::Error::UpdateFailed { reason, applied } => zbus::fdo::Error::Failed(format!(
            "aileron.Permissions.UpdateFailed: {reason} (applied: {applied})"
        )),
    }
}

async fn prompt_permission(
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
        && let Ok(result) =
            run_kdialog_permission_prompt(state, request_id, &text, parent_xid).await
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
    if let Ok(result) = run_prompt_command(state, request_id, &mut zenity).await {
        return result;
    }

    if parent_xid.is_none()
        && let Ok(result) = run_kdialog_permission_prompt(state, request_id, &text, None).await
    {
        return result;
    }

    Err(zbus::fdo::Error::Failed(
        "No permission prompt helper found; install zenity or kdialog, grant permission in the Aileron Permissions page, or start the daemon with AILERON_AUTO_GRANT=true for development".to_string(),
    ))
}

async fn run_kdialog_permission_prompt(
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
    run_prompt_command(state, request_id, &mut kdialog).await
}

async fn run_prompt_command(
    state: &PortalState,
    request_id: &str,
    command: &mut Command,
) -> std::io::Result<zbus::fdo::Result<bool>> {
    let mut child = command.kill_on_drop(true).spawn()?;
    match await_request(state, request_id, child.wait()).await {
        Ok(status) => Ok(Ok(status?.success())),
        Err(e) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok(Err(e))
        }
    }
}

fn x11_parent_window_id(parent_window: &str) -> Option<&str> {
    parent_window
        .strip_prefix("x11:")
        .filter(|xid| !xid.is_empty())
}

impl LanguagePortalBackend {
    async fn emit_loading(
        &self,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        LanguagePortalBackend::model_loading(
            emitter,
            request_handle,
            session_handle,
            "preparing model",
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }
}

impl SpokenLanguagePortalBackend {
    async fn emit_loading(
        &self,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        SpokenLanguagePortalBackend::model_loading(
            emitter,
            request_handle,
            session_handle,
            "preparing model",
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }
}

impl VisionPortalBackend {
    async fn emit_loading(
        &self,
        request_handle: &OwnedObjectPath,
        session_handle: &OwnedObjectPath,
        emitter: &SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        VisionPortalBackend::model_loading(
            emitter,
            request_handle,
            session_handle,
            "preparing model",
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }
}

impl ResponseOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::ResponseOptions {
        aileron_varlink::aileron_Inference::ResponseOptions {
            maximum_response_tokens: self.maximum_response_tokens,
            temperature: self.temperature,
            source_language_hint: self.source_language_hint,
            target_language_hint: self.target_language_hint,
            execution_mode: self.execution_mode,
        }
    }
}

impl GuidedOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::GuidedOptions {
        aileron_varlink::aileron_Inference::GuidedOptions {
            maximum_response_tokens: self.maximum_response_tokens,
            temperature: self.temperature,
            execution_mode: self.execution_mode,
        }
    }
}

impl EmbedOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::EmbedOptions {
        aileron_varlink::aileron_Inference::EmbedOptions {
            execution_mode: self.execution_mode,
        }
    }
}

impl SpeechOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::SpeechOptions {
        aileron_varlink::aileron_Inference::SpeechOptions {
            source_language_hint: self.source_language_hint,
            execution_mode: self.execution_mode,
        }
    }
}

impl SynthesisOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::SynthesisOptions {
        aileron_varlink::aileron_Inference::SynthesisOptions {
            voice_id: self.voice_id,
            language_hint: self.language_hint,
            execution_mode: self.execution_mode,
        }
    }
}

impl VisionOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::VisionOptions {
        aileron_varlink::aileron_Inference::VisionOptions {
            execution_mode: self.execution_mode,
        }
    }
}

impl VisionPointPromptDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::VisionPointPrompt {
        aileron_varlink::aileron_Inference::VisionPointPrompt {
            x: self.x,
            y: self.y,
            positive: self.positive,
        }
    }
}

impl VisionBoxPromptDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::VisionBoxPrompt {
        aileron_varlink::aileron_Inference::VisionBoxPrompt {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

impl VisionSegmentOptionsDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::VisionSegmentOptions {
        aileron_varlink::aileron_Inference::VisionSegmentOptions {
            execution_mode: self.execution_mode,
            points: self
                .points
                .into_iter()
                .map(VisionPointPromptDbus::into_varlink)
                .collect(),
            boxes: self
                .boxes
                .into_iter()
                .map(VisionBoxPromptDbus::into_varlink)
                .collect(),
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
    use std::sync::mpsc;
    use std::thread;

    static ADMISSION_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct TestDaemon {
        listener: std::os::unix::net::UnixListener,
        path: std::path::PathBuf,
        state: Arc<PortalState>,
    }

    impl TestDaemon {
        fn new() -> Self {
            static NEXT_SOCKET: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0);
            let suffix = NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aileron-bridge-{}-{suffix}.sock",
                std::process::id()
            ));
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let state = Arc::new(PortalState {
                daemon_address: Some(format!("unix:{}", path.display())),
                ..PortalState::default()
            });
            state.sessions.lock().unwrap().insert(
                "session".to_string(),
                SessionRecord {
                    interface: PortalInterface::Language,
                    use_case: "language.embed".to_string(),
                    daemon_session_id: "daemon-session".to_string(),
                    closing: false,
                },
            );
            state.requests.lock().unwrap().insert(
                "request".to_string(),
                RequestRecord {
                    session_handle: Some("session".to_string()),
                    daemon_session_id: None,
                    cancelled: false,
                    cancel_tx: tokio::sync::watch::channel(false).0,
                },
            );
            Self {
                listener,
                path,
                state,
            }
        }
    }

    impl Drop for TestDaemon {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn read_request(stream: &mut std::os::unix::net::UnixStream) -> serde_json::Value {
        use std::io::BufRead;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        std::io::BufReader::new(stream)
            .read_until(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes.pop(), Some(0));
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delayed_first_reply_keeps_executor_live_and_preserves_order_and_fds() {
        use std::io::Write;

        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        let fd = OwnedFd::from(std::os::fd::OwnedFd::from(
            std::fs::File::open("/dev/null").unwrap(),
        ));
        let path = fd_proc_path(&fd);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert_eq!(request["method"], "aileron.Inference.StreamEmbed");
            assert_eq!(request["more"], true);
            started_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("executor must release the first reply");
            assert!(
                std::fs::File::open(path).is_ok(),
                "worker must retain input fd"
            );
            for (value, continues) in [(1, true), (2, true), (3, false)] {
                let reply = serde_json::json!({"parameters": {"embedding": [value], "embedding_pipeline_id": "test"}, "continues": continues});
                write!(stream, "{reply}\0").unwrap();
            }
        });
        let mut replies =
            stream_replies(daemon.state.clone(), "request", vec![fd], false, |client| {
                Box::pin(async move {
                    client
                        .stream_embed(
                            "daemon-session".to_string(),
                            "text".to_string(),
                            EmbedOptionsDbus {
                                execution_mode: "interactive".to_string(),
                            }
                            .into_varlink(),
                        )
                        .await
                })
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), replies.recv())
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        for value in [1.0, 2.0, 3.0] {
            let reply = tokio::time::timeout(Duration::from_secs(1), replies.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(reply.embedding, [value]);
        }
        assert!(replies.recv().await.is_none());
        server.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_before_first_reply_reaches_daemon_with_data_workers_full() {
        use std::io::{Read, Write};

        let _admission_test = ADMISSION_TEST.lock().await;
        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            started_tx.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "Close must interrupt the first read"
            );
            let (mut control, _) = listener.accept().unwrap();
            let request = read_request(&mut control);
            assert_eq!(request["method"], "aileron.Inference.CancelActiveRequest");
            assert_eq!(request["parameters"]["session_id"], "daemon-session");
            control.write_all(b"{}\0").unwrap();
        });
        let mut replies =
            stream_replies(daemon.state.clone(), "request", vec![], false, |client| {
                Box::pin(async move {
                    client
                        .stream_embed(
                            "daemon-session".to_string(),
                            "text".to_string(),
                            EmbedOptionsDbus {
                                execution_mode: "interactive".to_string(),
                            }
                            .into_varlink(),
                        )
                        .await
                })
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();
        // Reserve the remaining capacity only after this request has started.
        let permits = TRANSPORT_WORKERS
            .acquire_many((MAX_TRANSPORT_WORKERS - 1) as u32)
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            cancel_request(&daemon.state, "request"),
        )
        .await
        .unwrap();
        let error = replies.recv().await.unwrap().err().unwrap();
        assert!(error.to_string().contains("RequestCancelled"));
        drop(permits);
        drop(replies);
        server.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_saturation_admits_interactive_and_general_work() {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};

        let _admission_test = ADMISSION_TEST.lock().await;
        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = thread::spawn(move || {
            let mut background_connections = Vec::new();
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        if request["parameters"]["options"]["execution_mode"] == "background" {
                            background_connections.push(stream);
                            let _ = started_tx.send(());
                        } else {
                            stream.write_all(b"{\"parameters\":{\"embedding\":[1],\"embedding_pipeline_id\":\"interactive\"}}\0").unwrap();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("mock daemon accept failed: {error}"),
                }
            }
        });
        let mut tasks = Vec::new();
        for index in 0..MAX_TRANSPORT_WORKERS {
            let request_id = format!("background-{index}");
            daemon.state.requests.lock().unwrap().insert(
                request_id.clone(),
                RequestRecord {
                    // Keep cleanup local to this admission test, which has no daemon sessions.
                    session_handle: None,
                    daemon_session_id: None,
                    cancelled: false,
                    cancel_tx: tokio::sync::watch::channel(false).0,
                },
            );
            let state = daemon.state.clone();
            tasks.push(tokio::spawn(async move {
                let mut replies = stream_replies(state, &request_id, vec![], true, |client| {
                    Box::pin(async move {
                        client
                            .stream_embed(
                                "session".to_string(),
                                "background".to_string(),
                                EmbedOptionsDbus {
                                    execution_mode: "background".to_string(),
                                }
                                .into_varlink(),
                            )
                            .await
                    })
                })
                .await
                .unwrap();
                let _ = replies.recv().await;
            }));
        }
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..MAX_BACKGROUND_WORKERS {
                started_rx.recv().await.unwrap();
            }
            // Give the remaining background tasks time to queue for admission.
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(
                started_rx.try_recv().is_err(),
                "background work exceeded its quota"
            );
            let mut replies =
                stream_replies(daemon.state.clone(), "request", vec![], false, |client| {
                    Box::pin(async move {
                        client
                            .stream_embed(
                                "daemon-session".to_string(),
                                "interactive".to_string(),
                                EmbedOptionsDbus {
                                    execution_mode: "interactive".to_string(),
                                }
                                .into_varlink(),
                            )
                            .await
                    })
                })
                .await
                .unwrap();
            assert_eq!(
                replies.recv().await.unwrap().unwrap().embedding_pipeline_id,
                "interactive"
            );
            assert!(replies.recv().await.is_none());
            assert_eq!(
                blocking(&TRANSPORT_WORKERS, None, || Ok(42)).await.unwrap(),
                42
            );
        })
        .await;
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        result.expect("background saturation must not block interactive or general work");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_worker_permit_outlives_cancelled_waiter() {
        static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task = tokio::spawn(blocking(&WORKERS, None, move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            WORKERS.available_permits(),
            0,
            "aborting the async waiter must not release a running worker's permit"
        );
        let (ran_tx, mut ran_rx) = tokio::sync::oneshot::channel();
        let queued = tokio::spawn(blocking(&WORKERS, None, move || {
            ran_tx.send(()).unwrap();
            Ok(())
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut ran_rx)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), queued)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_creation_and_prewarm_do_not_block_on_delayed_responses() {
        use std::io::Write;

        for create in [true, false] {
            let daemon = TestDaemon::new();
            if create {
                daemon
                    .state
                    .requests
                    .lock()
                    .unwrap()
                    .get_mut("request")
                    .unwrap()
                    .session_handle = None;
            }
            let listener = daemon.listener.try_clone().unwrap();
            let (release_tx, release_rx) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert_eq!(
                    request["method"],
                    if create {
                        "aileron.Inference.CreateSession"
                    } else {
                        "aileron.Inference.Prewarm"
                    }
                );
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("executor must release the unary reply");
                let reply = if create {
                    b"{\"parameters\":{\"session_id\":\"created\",\"profile_id\":\"test\"}}\0"
                        .as_slice()
                } else {
                    // Match the daemon's zlink unit reply, with no parameters.
                    b"{}\0".as_slice()
                };
                stream.write_all(reply).unwrap();
            });
            let operation = async {
                if create {
                    assert_eq!(
                        create_session_impl(
                            &daemon.state,
                            "request",
                            "app",
                            "",
                            "language.embed",
                            ""
                        )
                        .await?,
                        "created"
                    );
                } else {
                    prewarm_impl(
                        daemon.state.clone(),
                        "request".to_string(),
                        "session".to_string(),
                        "daemon-session".to_string(),
                        PortalInterface::Language,
                    )
                    .await?;
                }
                zbus::fdo::Result::Ok(())
            };
            tokio::pin!(operation);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut operation)
                    .await
                    .is_err()
            );
            release_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(1), operation)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*daemon.state.prewarm_workers.lock().unwrap(), 0);
            server.join().unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_deadline_releases_synthesis_slot_when_daemon_never_replies() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
        let daemon = TestDaemon::new();
        daemon.listener.set_nonblocking(true).unwrap();
        let listener =
            tokio::net::UnixListener::from_std(daemon.listener.try_clone().unwrap()).unwrap();
        attach_request_daemon_session(&daemon.state, "request", "daemon-session").unwrap();
        begin_synthesis_request(&daemon.state, "session", "request").unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = Vec::new();
            reader.read_until(0, &mut request).await.unwrap();
            let mut byte = [0];
            assert_eq!(reader.read(&mut byte).await.unwrap(), 0);
        });
        tokio::time::timeout(
            CANCEL_REQUEST_TIMEOUT + Duration::from_secs(1),
            cancel_request(&daemon.state, "request"),
        )
        .await
        .expect("Close must have a bounded deadline");
        assert!(request_is_cancelled(&daemon.state, "request"));
        assert!(
            daemon
                .state
                .active_synthesis_requests
                .lock()
                .unwrap()
                .is_empty()
        );
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_request_does_not_wait_for_worker_capacity() {
        static WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);
        let daemon = TestDaemon::new();
        let operation = blocking(
            &WORKERS,
            Some((&daemon.state, "request")),
            || -> zbus::fdo::Result<()> {
                panic!("cancelled queued request must not start a worker");
            },
        );
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut operation)
                .await
                .is_err()
        );
        cancel_request(&daemon.state, "request").await;
        let error = tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("RequestCancelled"));
    }

    #[test]
    fn request_close_after_session_cancellation_still_notifies_daemon_once() {
        let daemon = TestDaemon::new();
        attach_request_daemon_session(&daemon.state, "request", "daemon-session").unwrap();
        cancel_session_requests(&daemon.state, "session");
        let (session_id, _) = cancel_request_record(&daemon.state, "request").unwrap();
        assert_eq!(session_id.as_deref(), Some("daemon-session"));
        let (session_id, _) = cancel_request_record(&daemon.state, "request").unwrap();
        assert!(session_id.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_backpressured_stream_cancels_daemon_and_releases_worker() {
        use std::io::{Read, Write};

        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            for _ in 0..10 {
                stream.write_all(b"{\"parameters\":{\"embedding\":[1],\"embedding_pipeline_id\":\"test\"},\"continues\":true}\0").unwrap();
            }
            sent_tx.send(()).unwrap();
            let mut byte = [0];
            // Closing with unread replies can produce a reset rather than EOF.
            match stream.read(&mut byte) {
                Ok(count) => assert_eq!(count, 0),
                Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
            }
            let (mut control, _) = listener.accept().unwrap();
            assert_eq!(
                read_request(&mut control)["method"],
                "aileron.Inference.CancelActiveRequest"
            );
            control.write_all(b"{}\0").unwrap();
            cancelled_tx.send(()).unwrap();
        });
        let replies = stream_replies(daemon.state.clone(), "request", vec![], false, |client| {
            Box::pin(async move {
                client
                    .stream_embed(
                        "daemon-session".to_string(),
                        "text".to_string(),
                        EmbedOptionsDbus {
                            execution_mode: "interactive".to_string(),
                        }
                        .into_varlink(),
                    )
                    .await
            })
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), sent_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while replies.receiver.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(replies.receiver.max_capacity(), 1);
        assert!(!replies.worker.as_ref().unwrap().is_finished());
        let worker = replies.worker.as_ref().unwrap().abort_handle();
        drop(replies);
        assert!(request_is_cancelled(&daemon.state, "request"));
        tokio::time::timeout(Duration::from_secs(1), cancelled_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_completed_buffered_reply_does_not_cancel_new_session_work() {
        use std::io::Write;
        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            read_request(&mut first);
            first
                .write_all(
                    b"{\"parameters\":{\"embedding\":[1],\"embedding_pipeline_id\":\"first\"}}\0",
                )
                .unwrap();
            let (mut second, _) = listener.accept().unwrap();
            assert_eq!(
                read_request(&mut second)["method"],
                "aileron.Inference.StreamEmbed"
            );
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "dropping a completed stream must not send session-wide cancellation"
            );
            second
                .write_all(
                    b"{\"parameters\":{\"embedding\":[2],\"embedding_pipeline_id\":\"second\"}}\0",
                )
                .unwrap();
        });
        let make_call =
            |client: zlink::tokio::unix::Connection| -> StreamFuture<inference::StreamEmbed_Reply> {
                Box::pin(async move {
                    client
                        .stream_embed(
                            "daemon-session".into(),
                            "text".into(),
                            inference::EmbedOptions {
                                execution_mode: "interactive".into(),
                            },
                        )
                        .await
                })
            };
        let first = stream_replies(daemon.state.clone(), "request", vec![], false, make_call)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !first.worker.as_ref().unwrap().is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !first.receiver.is_empty(),
            "terminal reply must remain buffered"
        );
        daemon
            .state
            .requests
            .lock()
            .unwrap()
            .insert("second".into(), test_request_record(Some("session")));
        let mut second = stream_replies(daemon.state.clone(), "second", vec![], false, make_call)
            .await
            .unwrap();
        started_rx.await.unwrap();
        drop(first);
        assert!(!request_is_cancelled(&daemon.state, "request"));
        tokio::time::sleep(Duration::from_millis(30)).await;
        release_tx.send(()).unwrap();
        assert_eq!(
            second.recv().await.unwrap().unwrap().embedding_pipeline_id,
            "second"
        );
        assert!(second.recv().await.is_none());
        server.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_transport_error_is_not_successful_eof() {
        use std::io::Write;

        let daemon = TestDaemon::new();
        let listener = daemon.listener.try_clone().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            stream.write_all(b"{\"error\":\"aileron.Inference.GenerationFailed\",\"parameters\":{\"reason\":\"test failure\"}}\0").unwrap();
        });
        let mut replies =
            stream_replies(daemon.state.clone(), "request", vec![], false, |client| {
                Box::pin(async move {
                    client
                        .stream_embed(
                            "daemon-session".to_string(),
                            "text".to_string(),
                            EmbedOptionsDbus {
                                execution_mode: "interactive".to_string(),
                            }
                            .into_varlink(),
                        )
                        .await
                })
            })
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), replies.recv())
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert!(error.to_string().contains("GenerationFailed"));
        drop(replies);
        assert!(
            !request_is_cancelled(&daemon.state, "request"),
            "a terminal daemon error is completion, not abandonment"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        daemon.listener.set_nonblocking(true).unwrap();
        assert_eq!(
            daemon.listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "completed RPC must not send CancelActiveRequest"
        );
        server.join().unwrap();
    }

    #[test]
    fn depth_conversion_preserves_metric_values_and_unit() {
        let converted = depth_map_into_dbus(aileron_varlink::aileron_Inference::VisionDepthMap {
            width: 2,
            height: 1,
            values: vec![0.5, 12.0],
            unit: "meter".to_string(),
            minimum: 0.5,
            maximum: 12.0,
        })
        .expect("valid depth map");

        assert_eq!(converted.values, [0.5, 12.0]);
        assert_eq!(converted.unit, "meter");
        assert_eq!((converted.minimum, converted.maximum), (0.5, 12.0));
    }

    #[hegel::test]
    fn interface_use_case_accepts_supported_tokens(tc: TestCase) {
        let (use_case, interface) = tc.draw(gs::sampled_from(vec![
            ("language.summarize", PortalInterface::Language),
            ("language.embed", PortalInterface::Language),
            ("speech.transcribe", PortalInterface::SpokenLanguage),
            ("speech.translate", PortalInterface::SpokenLanguage),
            ("speech.synthesize", PortalInterface::SpokenLanguage),
            ("vision.describe", PortalInterface::Vision),
            ("vision.detect", PortalInterface::Vision),
            ("vision.segment", PortalInterface::Vision),
            ("vision.depth", PortalInterface::Vision),
        ]));

        assert!(ensure_interface_use_case(use_case, interface).is_ok());
    }

    #[hegel::test]
    fn interface_use_case_rejects_wrong_or_unknown_tokens(tc: TestCase) {
        let (use_case, interface) = tc.draw(gs::sampled_from(vec![
            ("language.generate", PortalInterface::Language),
            ("speech.transcribe", PortalInterface::Language),
            ("vision.describe", PortalInterface::SpokenLanguage),
            ("language.summarize", PortalInterface::Vision),
        ]));

        let err = ensure_interface_use_case(use_case, interface)
            .expect_err("unsupported use-case should fail");

        assert!(err.to_string().contains(interface.label()));
        assert!(err.to_string().contains(use_case));
        assert!(err.to_string().contains("supported use-cases"));
    }

    #[hegel::test]
    fn response_options_conversion_preserves_generated_fields(tc: TestCase) {
        let maximum_response_tokens = tc.draw(gs::integers::<i64>().min_value(1).max_value(4096));
        let temperature_tenths = tc.draw(gs::integers::<i64>().min_value(0).max_value(20));
        let options = ResponseOptionsDbus {
            maximum_response_tokens,
            temperature: temperature_tenths as f64 / 10.0,
            source_language_hint: "en".to_string(),
            target_language_hint: "es".to_string(),
            execution_mode: "background".to_string(),
        };

        let converted = options.into_varlink();

        assert_eq!(converted.maximum_response_tokens, maximum_response_tokens);
        assert_eq!(converted.temperature, temperature_tenths as f64 / 10.0);
        assert_eq!(converted.source_language_hint, "en");
        assert_eq!(converted.target_language_hint, "es");
        assert_eq!(converted.execution_mode, "background");
    }

    #[test]
    fn synthesis_options_conversion_preserves_generated_fields() {
        let converted = SynthesisOptionsDbus {
            voice_id: "default".to_string(),
            language_hint: "en".to_string(),
            execution_mode: "interactive".to_string(),
        }
        .into_varlink();

        assert_eq!(converted.voice_id, "default");
        assert_eq!(converted.language_hint, "en");
        assert_eq!(converted.execution_mode, "interactive");
    }

    #[test]
    fn synthesis_input_rejects_empty_oversized_and_unknown_mode() {
        let mut options = SynthesisOptionsDbus {
            voice_id: String::new(),
            language_hint: String::new(),
            execution_mode: "interactive".to_string(),
        };

        assert!(validate_synthesis_input("Hello", &options).is_ok());
        assert!(validate_synthesis_input("  ", &options).is_err());
        assert!(
            validate_synthesis_input(&"x".repeat(MAX_SYNTHESIS_TEXT_BYTES + 1), &options).is_err()
        );
        options.voice_id = "x".repeat(MAX_SYNTHESIS_VOICE_ID_BYTES + 1);
        assert!(validate_synthesis_input("Hello", &options).is_err());
        options.voice_id.clear();
        options.language_hint = "x".repeat(MAX_SYNTHESIS_LANGUAGE_HINT_BYTES + 1);
        assert!(validate_synthesis_input("Hello", &options).is_err());
        options.language_hint.clear();
        options.execution_mode = "urgent".to_string();
        assert!(validate_synthesis_input("Hello", &options).is_err());
    }

    #[test]
    fn audio_chunk_decode_enforces_framing_limits_and_stable_metadata() {
        let valid = aileron_varlink::aileron_Inference::AudioChunk {
            audio_base64: "AQACAA==".to_string(),
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".to_string(),
        };
        let decoded = decode_audio_chunk(valid, None).expect("valid PCM should decode");
        assert_eq!(decoded.audio, [1, 0, 2, 0]);

        let changed = aileron_varlink::aileron_Inference::AudioChunk {
            audio_base64: String::new(),
            sample_rate: 48_000,
            channels: 1,
            sample_format: "s16le".to_string(),
        };
        assert!(decode_audio_chunk(changed, Some(&decoded.metadata)).is_err());

        let partial_frame = aileron_varlink::aileron_Inference::AudioChunk {
            audio_base64: "AQ==".to_string(),
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".to_string(),
        };
        assert!(decode_audio_chunk(partial_frame, None).is_err());

        let malformed = aileron_varlink::aileron_Inference::AudioChunk {
            audio_base64: "not-base64".to_string(),
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".to_string(),
        };
        assert!(decode_audio_chunk(malformed, None).is_err());

        let oversized = aileron_varlink::aileron_Inference::AudioChunk {
            audio_base64: "A".repeat(MAX_AUDIO_CHUNK_BYTES.div_ceil(3) * 4 + 1),
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".to_string(),
        };
        assert!(decode_audio_chunk(oversized, None).is_err());
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
    fn permission_denied_maps_from_the_typed_inference_error() {
        let error = map_inference_error(inference::Error::PermissionDenied {
            app_id: "org.example.App".into(),
            use_case: "language.summarize".into(),
        });

        assert!(matches!(error, zbus::fdo::Error::AccessDenied(_)));
        assert!(error.to_string().contains("org.example.App"));
        assert!(error.to_string().contains("language.summarize"));
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
                interface: PortalInterface::Language,
                use_case: "language.summarize".to_string(),
                daemon_session_id: "daemon-session-1".to_string(),
                closing: false,
            },
        );

        assert!(
            ensure_known_session(&state, "session-1", PortalInterface::Language).is_ok(),
            "session should be valid on its owning interface"
        );
        let err = ensure_known_session(&state, "session-1", PortalInterface::SpokenLanguage)
            .expect_err("wrong interface should be rejected");

        assert!(err.to_string().contains("Language portal"));
        assert!(err.to_string().contains("Speech"));
    }

    #[test]
    fn ensure_known_session_rejects_closing_session() {
        let state = PortalState::default();
        state.sessions.lock().unwrap().insert(
            "session-1".to_string(),
            SessionRecord {
                interface: PortalInterface::Language,
                use_case: "language.summarize".to_string(),
                daemon_session_id: "daemon-session-1".to_string(),
                closing: true,
            },
        );

        let err = ensure_known_session(&state, "session-1", PortalInterface::Language)
            .expect_err("closing session should reject new work");

        assert!(err.to_string().contains("closing"));
    }

    #[test]
    fn language_generation_validator_rejects_specialized_sessions() {
        let use_case = "language.embed";
        let record = SessionRecord {
            interface: PortalInterface::Language,
            use_case: use_case.to_string(),
            daemon_session_id: "daemon-session-1".to_string(),
            closing: false,
        };

        let err = ensure_language_generation_session(&record)
            .expect_err("specialized language session should be rejected");

        assert!(err.to_string().contains("aileron.Inference.InvalidInput"));
        assert!(err.to_string().contains(use_case));
    }

    #[test]
    fn exact_session_use_case_validator_rejects_mismatch() {
        let record = SessionRecord {
            interface: PortalInterface::Vision,
            use_case: "vision.ocr".to_string(),
            daemon_session_id: "daemon-session-1".to_string(),
            closing: false,
        };

        let err = ensure_exact_session_use_case(&record, "vision.segment", "StreamSegment")
            .expect_err("mismatched use-case should fail");

        assert!(err.to_string().contains("aileron.Inference.InvalidInput"));
        assert!(err.to_string().contains("StreamSegment"));
        assert!(err.to_string().contains("vision.ocr"));
    }

    #[test]
    fn prewarm_worker_cap_rejects_when_full() {
        let state = PortalState::default();
        *state.prewarm_workers.lock().unwrap() = MAX_PREWARM_WORKERS;

        let err = acquire_prewarm_worker(&state).expect_err("full worker pool should fail");

        assert!(
            err.to_string()
                .contains("too many concurrent Prewarm operations")
        );
    }

    #[tokio::test]
    async fn request_cancellation_aborts_operation_and_drops_its_connection() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixListener;

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket_path = std::env::temp_dir().join(format!(
            "aileron-portal-cancel-{}-{suffix}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket_path).expect("test socket should bind");
        let reader = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("test socket should accept");
            let mut buf = [0; 1];
            stream
                .read(&mut buf)
                .await
                .expect("test socket read should finish")
        });
        let connection = zlink::tokio::unix::connect(&socket_path)
            .await
            .expect("test socket should connect");

        let state = Arc::new(PortalState::default());
        state
            .requests
            .lock()
            .unwrap()
            .insert("request-1".to_string(), test_request_record(None));
        let operation_state = state.clone();
        let operation = tokio::spawn(async move {
            await_request(&operation_state, "request-1", async move {
                let _connection = connection;
                std::future::pending::<()>().await;
            })
            .await
        });

        cancel_request(&state, "request-1").await;

        let error = operation
            .await
            .unwrap()
            .expect_err("operation should abort");
        assert!(error.to_string().contains("RequestCancelled"));
        assert_eq!(reader.await.unwrap(), 0, "server side should observe EOF");
        let _ = std::fs::remove_file(socket_path);
    }

    #[tokio::test]
    async fn request_cancellation_rejects_active_request() {
        let state = PortalState::default();
        state
            .requests
            .lock()
            .unwrap()
            .insert("request-1".to_string(), test_request_record(None));

        assert!(ensure_request_active(&state, "request-1").is_ok());
        cancel_request(&state, "request-1").await;

        let err =
            ensure_request_active(&state, "request-1").expect_err("cancelled request should fail");
        assert!(
            err.to_string()
                .contains("aileron.Inference.RequestCancelled")
        );
    }

    #[test]
    fn cancelled_synthesis_request_releases_session_slot() {
        let state = PortalState::default();
        let mut record = test_request_record(Some("session-1"));
        record.cancelled = true;
        state
            .requests
            .lock()
            .unwrap()
            .insert("request-1".to_string(), record);
        begin_synthesis_request(&state, "session-1", "request-1").unwrap();

        finish_synthesis_request(&state, "session-1", "request-1");

        assert!(state.active_synthesis_requests.lock().unwrap().is_empty());
        assert!(begin_synthesis_request(&state, "session-1", "request-2").is_ok());
    }

    #[test]
    fn session_cancellation_marks_only_matching_requests() {
        let state = PortalState::default();
        state.requests.lock().unwrap().insert(
            "request-1".to_string(),
            test_request_record(Some("session-1")),
        );
        state.requests.lock().unwrap().insert(
            "request-2".to_string(),
            test_request_record(Some("session-2")),
        );

        cancel_session_requests(&state, "session-1");

        assert!(ensure_request_active(&state, "request-1").is_err());
        assert!(ensure_request_active(&state, "request-2").is_ok());
    }

    fn test_request_record(session_handle: Option<&str>) -> RequestRecord {
        RequestRecord {
            session_handle: session_handle.map(str::to_string),
            daemon_session_id: None,
            cancelled: false,
            cancel_tx: tokio::sync::watch::channel(false).0,
        }
    }
}
