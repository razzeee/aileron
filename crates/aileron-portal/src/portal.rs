/// D-Bus portal backend for `org.freedesktop.impl.portal.AI`.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::info;
use zbus::zvariant::Type;
use zbus::{connection, interface, object_server::SignalContext};

pub async fn run() -> Result<()> {
    info!("registering D-Bus portal backend");

    let portal = AiPortalBackend::default();
    let conn = connection::Builder::session()?
        .name("org.freedesktop.impl.portal.desktop.aileron")?
        .serve_at("/org/freedesktop/portal/desktop", portal)?
        .build()
        .await?;

    info!("D-Bus connection established; serving portal interface");
    let _ = conn;
    std::future::pending::<()>().await;
    Ok(())
}

#[derive(Default)]
struct AiPortalBackend {
    warm: Mutex<HashSet<String>>,
    session_use_cases: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct ModelAvailabilityDbus {
    is_available: bool,
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
struct ChatMessageDbus {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
struct GuidedFieldDbus {
    name: String,
    kind: String,
    description: String,
    required: bool,
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

#[interface(name = "org.freedesktop.impl.portal.AI")]
impl AiPortalBackend {
    async fn get_use_case_availability(
        &self,
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
            reason: reply.availability.reason,
        })
    }

    async fn create_session(
        &self,
        app_id: &str,
        use_case: &str,
        instructions: &str,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .create_session(
                app_id.to_string(),
                use_case.to_string(),
                instructions.to_string(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        self.session_use_cases
            .lock()
            .unwrap()
            .insert(reply.session_id.clone(), use_case.to_string());
        Ok(reply.session_id)
    }

    async fn prewarm(
        &self,
        session_id: &str,
        prompt_prefix: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        AiPortalBackend::model_loading(&ctxt, "starting model")
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        client
            .prewarm(session_id.to_string(), prompt_prefix.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        if let Some(use_case) = self
            .session_use_cases
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
        {
            self.warm.lock().unwrap().insert(use_case);
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn model_loading(ctxt: &SignalContext<'_>, message: &str) -> zbus::Result<()>;

    async fn respond(
        &self,
        session_id: &str,
        prompt: &str,
        options: GenerationOptionsDbus,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &ctxt).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .respond(
                session_id.to_string(),
                prompt.to_string(),
                options.into_varlink(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.mark_warm(session_id);
        Ok(reply.content)
    }

    async fn stream_response(
        &self,
        session_id: &str,
        prompt: &str,
        options: GenerationOptionsDbus,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &ctxt).await?;
        let session_id = session_id.to_string();
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
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
            let token = reply
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                .token;

            if let Some(previous) = pending_token.replace(token) {
                AiPortalBackend::token_received(&ctxt, &session_id, &previous, false)
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }
        }

        if let Some(token) = pending_token {
            AiPortalBackend::token_received(&ctxt, &session_id, &token, true)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }

        self.mark_warm(&session_id);
        Ok(())
    }

    async fn chat(
        &self,
        session_id: &str,
        messages: Vec<ChatMessageDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &ctxt).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .chat(
                session_id.to_string(),
                messages
                    .into_iter()
                    .map(ChatMessageDbus::into_varlink)
                    .collect(),
                options.into_varlink(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.mark_warm(session_id);
        Ok(reply.content)
    }

    async fn stream_chat(
        &self,
        session_id: &str,
        messages: Vec<ChatMessageDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &ctxt).await?;
        let session_id = session_id.to_string();
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.stream_chat(
            session_id.clone(),
            messages
                .into_iter()
                .map(ChatMessageDbus::into_varlink)
                .collect(),
            options.into_varlink(),
        );
        let iter = call
            .more()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        let mut pending_token: Option<String> = None;
        for reply in iter {
            let token = reply
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
                .token;

            if let Some(previous) = pending_token.replace(token) {
                AiPortalBackend::token_received(&ctxt, &session_id, &previous, false)
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            }
        }

        if let Some(token) = pending_token {
            AiPortalBackend::token_received(&ctxt, &session_id, &token, true)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }

        self.mark_warm(&session_id);
        Ok(())
    }

    #[zbus(signal)]
    async fn token_received(
        ctxt: &SignalContext<'_>,
        session_id: &str,
        token: &str,
        done: bool,
    ) -> zbus::Result<()>;

    async fn respond_guided(
        &self,
        session_id: &str,
        prompt: &str,
        fields: Vec<GuidedFieldDbus>,
        options: GenerationOptionsDbus,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        self.emit_loading_if_cold(session_id, &ctxt).await?;
        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .respond_guided(
                session_id.to_string(),
                prompt.to_string(),
                fields
                    .into_iter()
                    .map(GuidedFieldDbus::into_varlink)
                    .collect(),
                options.into_varlink(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.mark_warm(session_id);
        Ok(reply.content)
    }

    async fn transcribe(
        &self,
        session_id: &str,
        audio_b64: &str,
        language_hint: &str,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .transcribe(
                session_id.to_string(),
                audio_b64.to_string(),
                language_hint.to_string(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(reply.text)
    }

    async fn describe(&self, session_id: &str, image_b64: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .describe(session_id.to_string(), image_b64.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
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
        let reply = client
            .segment(session_id.to_string(), image_b64.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
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
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        client
            .end_session(session_id.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.session_use_cases.lock().unwrap().remove(session_id);
        Ok(())
    }
}

impl AiPortalBackend {
    async fn emit_loading_if_cold(
        &self,
        session_id: &str,
        ctxt: &SignalContext<'_>,
    ) -> zbus::fdo::Result<()> {
        let use_case = self
            .session_use_cases
            .lock()
            .unwrap()
            .get(session_id)
            .cloned();
        let is_warm = use_case
            .as_ref()
            .is_some_and(|u| self.warm.lock().unwrap().contains(u));
        if !is_warm {
            AiPortalBackend::model_loading(ctxt, "starting model")
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        Ok(())
    }

    fn mark_warm(&self, session_id: &str) {
        if let Some(use_case) = self
            .session_use_cases
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
        {
            self.warm.lock().unwrap().insert(use_case);
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

impl ChatMessageDbus {
    fn into_varlink(self) -> aileron_varlink::aileron_Inference::ChatMessage {
        aileron_varlink::aileron_Inference::ChatMessage {
            role: self.role,
            content: self.content,
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
