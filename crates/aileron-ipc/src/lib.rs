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
/// Falls back to `/run/user/<uid>/aileron.socket` if `XDG_RUNTIME_DIR` is not set.
pub fn socket_path() -> String {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
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

extern "C" {
    fn getuid() -> u32;
}

unsafe fn libc_uid() -> u32 {
    getuid()
}
