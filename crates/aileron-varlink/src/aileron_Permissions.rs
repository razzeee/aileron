#![allow(non_camel_case_types)]

use serde::{Deserialize, Serialize};
use zlink::introspect::{CustomType, Type};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, CustomType)]
pub struct AppPermission {
    pub app_id: String,
    pub use_case: String,
    pub allowed: bool,
    pub last_used: Option<String>,
}

pub const CUSTOM_TYPES: &[&zlink::idl::CustomType<'static>] = &[AppPermission::CUSTOM_TYPE];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct ListAppPermissions_Reply {
    pub permissions: Vec<AppPermission>,
}

/// This interface currently declares no application errors.
#[derive(Clone, Debug, PartialEq, zlink::ReplyError, zlink::introspect::ReplyError)]
#[zlink(interface = "aileron.Permissions")]
pub enum Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[zlink::proxy("aileron.Permissions")]
pub trait VarlinkClientInterface {
    async fn list_app_permissions(
        &mut self,
    ) -> zlink::Result<std::result::Result<ListAppPermissions_Reply, Error>>;
    async fn set_app_permission(
        &mut self,
        app_id: String,
        use_case: String,
        allowed: bool,
    ) -> zlink::Result<std::result::Result<(), Error>>;
}
