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
    let runtime_dir = std::env::var("AILERON_RUNTIME_DIR")
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR"))
        .unwrap_or_else(|_| {
            // Best-effort fallback: use UID from the process.
            let uid = unsafe { libc_uid() };
            format!("/run/user/{}", uid)
        });
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
