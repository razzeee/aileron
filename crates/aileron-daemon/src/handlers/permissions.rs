//! Business operations for `aileron.Permissions`.

use crate::state::SharedState;
use aileron_varlink::permissions::{AppPermission, ListAppPermissions_Reply};

pub struct PermissionsHandler {
    state: SharedState,
}

impl PermissionsHandler {
    pub fn new(state: SharedState) -> Self {
        Self { state }
    }

    pub async fn list_app_permissions(&self) -> ListAppPermissions_Reply {
        let guard = self.state.0.lock().await;
        ListAppPermissions_Reply {
            permissions: app_permissions(&guard.permissions),
        }
    }

    pub async fn set_app_permission(
        &self,
        app_id: String,
        use_case: String,
        allowed: bool,
    ) -> anyhow::Result<()> {
        let mut guard = self.state.0.lock().await;
        guard.permissions.set(app_id, use_case, allowed)?;
        Ok(())
    }
}

fn app_permissions(store: &crate::permissions::PermissionStore) -> Vec<AppPermission> {
    store
        .list()
        .into_iter()
        .map(|(app_id, use_case, entry)| AppPermission {
            app_id,
            use_case,
            allowed: entry.allowed,
            last_used: entry.last_used.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{PermissionEntry, PermissionStore};
    use hegel::TestCase;
    use hegel::generators as gs;
    use std::collections::HashMap;

    #[hegel::test]
    fn app_permissions_preserve_generated_store_entries(tc: TestCase) {
        let allowed = tc.draw(gs::booleans());
        let last_used = tc.draw(gs::optional(gs::sampled_from(vec![
            "2026-06-20T00:00:00Z".to_string(),
            "2026-06-20T01:02:03Z".to_string(),
        ])));
        let store = PermissionStore(HashMap::from([(
            "org.aileron.Demo/language.extract".to_string(),
            PermissionEntry {
                allowed,
                last_used: last_used.clone(),
            },
        )]));

        let permissions = app_permissions(&store);

        assert_eq!(permissions.len(), 1);
        assert_eq!(permissions[0].app_id, "org.aileron.Demo");
        assert_eq!(permissions[0].use_case, "language.extract");
        assert_eq!(permissions[0].allowed, allowed);
        assert_eq!(permissions[0].last_used, last_used);
    }
}
