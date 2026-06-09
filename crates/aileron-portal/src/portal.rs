/// D-Bus portal backend for `org.freedesktop.impl.portal.AI`.
use anyhow::Result;
use tracing::info;
use zbus::{connection, interface};

pub async fn run() -> Result<()> {
    info!("registering D-Bus portal backend");

    let portal = AiPortalBackend;
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

struct AiPortalBackend;

#[interface(name = "org.freedesktop.impl.portal.AI")]
impl AiPortalBackend {
    async fn create_session(&self, app_id: &str, use_case: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .create_session(app_id.to_string(), use_case.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(reply.session_id)
    }

    async fn generate(&self, session_id: &str, prompt: &str) -> zbus::fdo::Result<String> {
        use aileron_varlink::aileron_Inference::VarlinkClientInterface;

        let conn =
            aileron_ipc::client::connect().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
        let reply = client
            .generate(session_id.to_string(), prompt.to_string())
            .call()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(reply.token)
    }

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
