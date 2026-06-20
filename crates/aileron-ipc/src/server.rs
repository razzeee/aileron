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
    remove_stale_socket_at(&path)
}

fn remove_stale_socket_at(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
        tracing::info!("removed stale socket at {:?}", path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_stale_socket_removes_existing_file() {
        let path = test_path("existing");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "stale socket placeholder").expect("write fixture");

        remove_stale_socket_at(&path).expect("remove stale socket");

        assert!(!path.exists());
    }

    #[test]
    fn remove_stale_socket_accepts_missing_file() {
        let path = test_path("missing");
        let _ = std::fs::remove_file(&path);

        remove_stale_socket_at(&path).expect("missing socket is ok");
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-ipc-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }
}
