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
            zlink::Error::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
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
