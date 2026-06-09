/// D-Bus portal backend for `org.freedesktop.impl.portal.AI`.
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Mutex;
use tracing::info;
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
    let _ = conn; // keep alive
    std::future::pending::<()>().await;
    Ok(())
}

/// Use-cases whose containers are already warm (model fully loaded).
#[derive(Default)]
struct AiPortalBackend {
    warm: Mutex<HashSet<String>>,
}

#[interface(name = "org.freedesktop.impl.portal.AI")]
impl AiPortalBackend {
    async fn create_session(
        &self,
        app_id: &str,
        use_case: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        // If the container is already warm, skip the more/loading dance and
        // call directly — CreateSession returns immediately.
        let already_warm = self.warm.lock().unwrap().contains(use_case);

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);

        if already_warm {
            let reply = client
                .create_session(app_id.to_string(), use_case.to_string())
                .call()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            return Ok(reply.session_id);
        }

        // Cold start — use more() to receive status lines during model load.
        let mut call = client.create_session(app_id.to_string(), use_case.to_string());
        let iter = call.more().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        let mut session_id = String::new();
        for reply in iter {
            let r = reply.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            if r.session_id.starts_with("status:") {
                let msg = r.session_id.trim_start_matches("status:").to_string();
                AiPortalBackend::model_loading(&ctxt, &msg)
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            } else {
                session_id = r.session_id;
            }
        }

        // Mark this use-case as warm for future calls.
        self.warm.lock().unwrap().insert(use_case.to_string());

        Ok(session_id)
    }

    /// Fired during `CreateSession` while the model container is loading.
    /// `message` is a human-readable status line from the container (e.g.
    /// "[aileron-llm] loading /model/model.gguf").
    #[zbus(signal)]
    async fn model_loading(ctxt: &SignalContext<'_>, message: &str) -> zbus::Result<()>;

    /// Non-streaming Generate — returns the full concatenated text.
    async fn generate(&self, session_id: &str, prompt: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.generate(session_id.to_string(), prompt.to_string());
        let iter = call.more().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut result = String::new();
        for reply in iter {
            result.push_str(&reply.map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?.token);
        }
        Ok(result)
    }

    /// Streaming Generate — returns immediately, then fires `TokenReceived`
    /// signals as tokens arrive from the daemon.
    async fn generate_stream(
        &self,
        session_id: &str,
        prompt: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let session_id = session_id.to_string();
        let prompt = prompt.to_string();

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let mut call = client.generate(session_id.clone(), prompt);
        let iter = call.more().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        // Collect all tokens (varlink iterator is synchronous/blocking).
        let tokens: Vec<String> = iter
            .map(|r| r.map(|v| v.token))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e: aileron_varlink::aileron_Inference::Error| {
                zbus::fdo::Error::Failed(e.to_string())
            })?;

        let total = tokens.len();
        for (i, token) in tokens.into_iter().enumerate() {
            let done = i + 1 == total;
            AiPortalBackend::token_received(&ctxt, &session_id, &token, done)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }

        Ok(())
    }

    /// Signal emitted once per token during `GenerateStream`.
    #[zbus(signal)]
    async fn token_received(
        ctxt: &SignalContext<'_>,
        session_id: &str,
        token: &str,
        done: bool,
    ) -> zbus::Result<()>;

    /// `audio_b64`: raw PCM bytes encoded as base64 (16 kHz mono f32le).
    async fn transcribe(&self, session_id: &str, audio_b64: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .transcribe(session_id.to_string(), audio_b64.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(reply.text)
    }

    /// `image_b64`: PNG or JPEG bytes encoded as base64.
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

    async fn end_session(&self, session_id: &str) -> zbus::fdo::Result<()> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        client
            .end_session(session_id.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }

    /// `schema`: JSON Schema object serialised as a string.
    /// Returns a JSON string that validates against the schema.
    async fn generate_structured(
        &self,
        session_id: &str,
        prompt: &str,
        schema: &str,
    ) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .generate_structured(
                session_id.to_string(),
                prompt.to_string(),
                schema.to_string(),
            )
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(reply.result)
    }
}
