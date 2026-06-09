/// Varlink interface definitions and generated type bindings for the Aileron project.
///
/// The four interfaces are:
/// - `aileron.Inference`   – create sessions, generate text, transcribe audio, describe images
/// - `aileron.Models`      – list, pull, delete, and assign OCI images
/// - `aileron.Permissions` – per-app, per-use-case permission records
/// - `aileron.Sessions`    – inspect and kill active inference sessions
///
/// Generated source files are produced by `varlink_generator::cargo_build` and
/// placed in `$OUT_DIR`.  The module name matches the file stem produced by the
/// generator (dots in the interface name are replaced with underscores).

// The varlink code generator produces non-standard names (e.g. `Call_Foo`,
// `Foo_Args`).  Suppress the relevant lints for these modules only.
#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Inference {
    include!(concat!(env!("OUT_DIR"), "/aileron.Inference.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Models {
    include!(concat!(env!("OUT_DIR"), "/aileron.Models.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Permissions {
    include!(concat!(env!("OUT_DIR"), "/aileron.Permissions.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Sessions {
    include!(concat!(env!("OUT_DIR"), "/aileron.Sessions.rs"));
}

// Convenience aliases that downstream crates use.
pub use aileron_Inference as inference;
pub use aileron_Models as models;
pub use aileron_Permissions as permissions;
pub use aileron_Sessions as sessions;
