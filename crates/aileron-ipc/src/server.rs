use anyhow::Result;

/// Helpers for setting up the server-side Varlink socket.
/// The actual `varlink::VarlinkService` is assembled by the daemon; this module
/// only provides path / socket utilities used during startup.

use std::path::PathBuf;

/// Return the filesystem path for the Unix socket.
pub fn socket_path() -> PathBuf {
    PathBuf::from(crate::socket_path())
}

/// Remove a stale socket file if it exists (called before binding).
pub fn remove_stale_socket() -> Result<()> {
    let path = socket_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        tracing::info!("removed stale socket at {:?}", path);
    }
    Ok(())
}
