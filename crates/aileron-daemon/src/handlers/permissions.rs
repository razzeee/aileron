/// Varlink handler for `aileron.Permissions`.
use crate::state::SharedState;
use aileron_varlink::aileron_Permissions::{
    AppPermission, Call_ListAppPermissions, Call_SetAppPermission, VarlinkInterface,
};

fn io_err(_msg: impl std::fmt::Display) -> varlink::Error {
    varlink::Error::from(varlink::ErrorKind::Io(std::io::ErrorKind::Other))
}

pub struct PermissionsHandler {
    state: SharedState,
}

impl PermissionsHandler {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }
}

impl VarlinkInterface for PermissionsHandler {
    fn list_app_permissions(&self, call: &mut dyn Call_ListAppPermissions) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let guard = self.state.0.lock().await;
            let result: Vec<AppPermission> = guard
                .permissions
                .list()
                .into_iter()
                .map(|(app_id, use_case, entry)| AppPermission {
                    app_id,
                    use_case,
                    allowed: entry.allowed,
                    last_used: entry.last_used.clone(),
                })
                .collect();
            call.reply(result)
        })
    }

    fn set_app_permission(
        &self,
        call: &mut dyn Call_SetAppPermission,
        app_id: String,
        use_case: String,
        allowed: bool,
    ) -> varlink::Result<()> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            guard
                .permissions
                .set(app_id, use_case, allowed)
                .map_err(io_err)?;
            call.reply()
        })
    }
}
