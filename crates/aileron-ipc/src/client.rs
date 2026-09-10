use crate::{IpcError, socket_path};

/// Open a Varlink connection to the aileron daemon.
///
/// zlink accepts the filesystem path directly; the `unix:` representation is
/// retained only for external tools through [`crate::varlink_address`].
pub async fn connect() -> Result<zlink::tokio::unix::Connection, IpcError> {
    let path = socket_path();
    connect_to(std::path::Path::new(&path)).await
}

async fn connect_to(path: &std::path::Path) -> Result<zlink::tokio::unix::Connection, IpcError> {
    zlink::tokio::unix::connect(path)
        .await
        .map_err(|error| match error {
            zlink::Error::Io(source)
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                IpcError::NotConnected {
                    path: path.to_string_lossy().into_owned(),
                }
            }
            other => IpcError::Zlink(other),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_socket_is_reported_as_not_connected() {
        let path =
            std::env::temp_dir().join(format!("aileron-ipc-stale-{}.socket", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(listener);
        let error = connect_to(&path).await.unwrap_err();
        assert!(
            matches!(error, IpcError::NotConnected { path: actual } if actual == path.to_string_lossy())
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn missing_socket_preserves_the_resolved_path() {
        let path = std::env::temp_dir().join(format!(
            "aileron-ipc-missing-{}-{}.socket",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&path);

        let error = connect_to(&path).await.unwrap_err();

        assert!(
            matches!(error, IpcError::NotConnected { path: actual } if actual == path.to_string_lossy())
        );
    }
}
