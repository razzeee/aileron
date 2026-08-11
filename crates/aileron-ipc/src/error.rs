use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("varlink transport error: {0}")]
    Zlink(#[from] zlink::Error),

    #[error("connection refused: socket not found at {path}")]
    NotConnected { path: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
