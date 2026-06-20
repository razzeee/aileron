/// aileron-ipc — Varlink client/server connection helpers.
///
/// Provides:
/// - `socket_path()` – resolve the socket path at runtime
/// - `varlink_address()` – Varlink address string for the daemon socket
/// - `client::connect()` – open a typed Varlink client connection
/// - `server::remove_stale_socket()` – clean up a leftover socket file
pub mod client;
pub mod error;
pub mod server;

pub use error::IpcError;

/// Resolve the Varlink socket path from the environment.
/// `AILERON_RUNTIME_DIR` overrides only Aileron's socket location; this is
/// useful for tests that must keep the desktop's real `XDG_RUNTIME_DIR`.
/// Falls back to `/run/user/<uid>/aileron.socket` if neither is set.
pub fn socket_path() -> String {
    let uid = unsafe { libc_uid() };
    socket_path_from(
        std::env::var("AILERON_RUNTIME_DIR").ok(),
        std::env::var("XDG_RUNTIME_DIR").ok(),
        uid,
    )
}

fn socket_path_from(
    aileron_runtime_dir: Option<String>,
    xdg_runtime_dir: Option<String>,
    uid: u32,
) -> String {
    let runtime_dir = aileron_runtime_dir
        .or(xdg_runtime_dir)
        .unwrap_or_else(|| format!("/run/user/{uid}"));
    format!("{}/aileron.socket", runtime_dir)
}

/// Return the Varlink address string for connecting to the daemon.
pub fn varlink_address() -> String {
    format!("unix:{}", socket_path())
}

unsafe extern "C" {
    fn getuid() -> u32;
}

unsafe fn libc_uid() -> u32 {
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn aileron_runtime_dir_overrides_xdg_runtime_dir(tc: TestCase) {
        let runtime_dir = tc.draw(gs::sampled_from(vec![
            "/tmp/aileron-runtime".to_string(),
            "/run/user/1000".to_string(),
            "relative-runtime".to_string(),
        ]));
        let xdg_runtime_dir = tc.draw(gs::sampled_from(vec![
            "/tmp/xdg-runtime".to_string(),
            "/run/user/2000".to_string(),
        ]));

        assert_eq!(
            socket_path_from(Some(runtime_dir.clone()), Some(xdg_runtime_dir), 1234),
            format!("{runtime_dir}/aileron.socket")
        );
    }

    #[hegel::test]
    fn xdg_runtime_dir_is_used_without_aileron_override(tc: TestCase) {
        let xdg_runtime_dir = tc.draw(gs::sampled_from(vec![
            "/tmp/xdg-runtime".to_string(),
            "/run/user/2000".to_string(),
            "relative-xdg".to_string(),
        ]));

        assert_eq!(
            socket_path_from(None, Some(xdg_runtime_dir.clone()), 1234),
            format!("{xdg_runtime_dir}/aileron.socket")
        );
    }

    #[hegel::test]
    fn uid_fallback_is_used_without_runtime_dirs(tc: TestCase) {
        let uid = tc.draw(gs::integers::<u32>());

        assert_eq!(
            socket_path_from(None, None, uid),
            format!("/run/user/{uid}/aileron.socket")
        );
    }
}
