#![allow(non_camel_case_types)]

use serde::{Deserialize, Serialize};
use zlink::introspect::{CustomType, Type};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, CustomType)]
pub struct SessionInfo {
    pub session_id: String,
    pub app_id: String,
    pub use_case: String,
    pub profile_id: String,
    pub started_at: String,
}

pub const CUSTOM_TYPES: &[&zlink::idl::CustomType<'static>] = &[SessionInfo::CUSTOM_TYPE];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct ListActive_Reply {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Clone, Debug, PartialEq, zlink::ReplyError, zlink::introspect::ReplyError)]
#[zlink(interface = "aileron.Sessions")]
pub enum Error {
    SessionNotFound { session_id: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[zlink::proxy("aileron.Sessions")]
pub trait VarlinkClientInterface {
    async fn list_active(&mut self) -> zlink::Result<std::result::Result<ListActive_Reply, Error>>;
    async fn kill_session(
        &mut self,
        session_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_error_round_trips_with_qualified_wire_name() {
        let json = serde_json::json!({
            "error": "aileron.Sessions.SessionNotFound",
            "parameters": { "session_id": "missing" }
        });
        assert_eq!(
            serde_json::from_value::<Error>(json).unwrap(),
            Error::SessionNotFound {
                session_id: "missing".into()
            }
        );
    }
}
