//! Aileron's Varlink contracts, generated from the checked-in IDLs at build time.
//! See build.rs for the small set of zlink-codegen 0.7 adaptations.

macro_rules! contract {
    ($name:ident) => {
        #[allow(non_snake_case, non_camel_case_types, clippy::too_many_arguments)]
        pub mod $name {
            include!(concat!(env!("OUT_DIR"), "/", stringify!($name), ".rs"));
        }
    };
}

contract!(aileron_Inference);
contract!(aileron_Models);
contract!(aileron_Permissions);
contract!(aileron_Sessions);

pub use aileron_Inference as inference;
pub use aileron_Models as models;
pub use aileron_Permissions as permissions;
pub use aileron_Sessions as sessions;
pub mod service;
mod stream;

/// Published IDLs, also used to verify the daemon's introspection output.
pub const INTERFACES: [(&str, &str); 4] = [
    (
        "aileron.Inference",
        include_str!("../varlink/aileron.Inference.varlink"),
    ),
    (
        "aileron.Models",
        include_str!("../varlink/aileron.Models.varlink"),
    ),
    (
        "aileron.Permissions",
        include_str!("../varlink/aileron.Permissions.varlink"),
    ),
    (
        "aileron.Sessions",
        include_str!("../varlink/aileron.Sessions.varlink"),
    ),
];
