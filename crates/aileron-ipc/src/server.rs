use anyhow::Result;

/// Helpers for setting up the server-side Varlink socket.
/// The daemon assembles the service; this module only provides path and socket
/// utilities used during startup.
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Return the filesystem path for the Unix socket.
pub fn socket_path() -> PathBuf {
    PathBuf::from(crate::socket_path())
}

/// Serialize daemon startup and remove a stale socket before binding.
/// Keep the returned lock alive for the lifetime of the listener. The lock file
/// must not be unlinked, because another process may already have it open.
pub fn prepare_socket() -> Result<std::fs::File> {
    let path = socket_path();
    prepare_socket_at(&path)
}

fn prepare_socket_at(path: &std::path::Path) -> Result<std::fs::File> {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock().map_err(|error| {
        anyhow::anyhow!(
            "another daemon holds the startup lock for {}: {error}",
            path.display()
        )
    })?;
    remove_stale_socket_at(path)?;
    Ok(lock)
}

fn remove_stale_socket_at(path: &std::path::Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to remove non-socket path at {}", path.display()),
        )
        .into());
    }

    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("a live Unix listener already exists at {}", path.display()),
        )
        .into()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error.into());
            }
            tracing::info!("removed stale socket at {:?}", path);
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn startup_lock_covers_cleanup_and_listener_lifetime() {
        let path = test_path("startup-lock");
        let lock = prepare_socket_at(&path).unwrap();
        assert!(
            prepare_socket_at(&path).is_err(),
            "second startup must not pass cleanup before bind"
        );
        let listener = UnixListener::bind(&path).unwrap();
        assert!(prepare_socket_at(&path).is_err());
        assert!(
            UnixStream::connect(&path).is_ok(),
            "losing startup must not unlink the listener"
        );
        drop(listener);
        drop(lock);
        let replacement = prepare_socket_at(&path).unwrap();
        assert!(
            !path.exists(),
            "stale socket is removed after the first daemon exits"
        );
        drop(replacement);
        let mut lock_path = path.into_os_string();
        lock_path.push(".lock");
        std::fs::remove_file(lock_path).unwrap();
    }

    #[test]
    fn remove_stale_socket_preserves_live_listener() {
        let path = test_path("live");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind live listener");

        let error = remove_stale_socket_at(&path).expect_err("live listener must be preserved");

        assert_eq!(
            io_error_kind(&error),
            Some(std::io::ErrorKind::AlreadyExists)
        );
        assert!(error.to_string().contains("live Unix listener"));
        assert!(path.exists());
        drop(listener);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remove_stale_socket_removes_unbound_socket() {
        let path = test_path("stale");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind stale listener fixture");
        drop(listener);

        remove_stale_socket_at(&path).expect("remove stale socket");

        assert!(!path.exists());
    }

    #[test]
    fn remove_stale_socket_preserves_non_socket_file() {
        let path = test_path("placeholder");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "placeholder").expect("write fixture");

        let error = remove_stale_socket_at(&path).expect_err("placeholder must be preserved");

        assert_eq!(
            io_error_kind(&error),
            Some(std::io::ErrorKind::AlreadyExists)
        );
        assert!(error.to_string().contains("non-socket path"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "placeholder");
        let _ = std::fs::remove_file(path);
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

    fn io_error_kind(error: &anyhow::Error) -> Option<std::io::ErrorKind> {
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
    }
}
