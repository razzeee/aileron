//! Business operations for `aileron.Permissions`.

use crate::state::SharedState;
use crate::{observability, permissions::PermissionStore, request_execution};
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
        set_permission(
            &self.state,
            &app_id,
            &use_case,
            allowed,
            &PermissionStore::path(),
        )
        .await
    }
}

async fn set_permission(
    state: &SharedState,
    app_id: &str,
    use_case: &str,
    allowed: bool,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    set_permission_with_sync(
        state,
        app_id,
        use_case,
        allowed,
        path,
        std::fs::File::sync_all,
    )
    .await
}

async fn set_permission_with_sync(
    state: &SharedState,
    app_id: &str,
    use_case: &str,
    allowed: bool,
    path: &std::path::Path,
    sync_directory: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let (removed, result) = {
        let mut guard = state.0.lock().await;
        let result = guard.permissions.set_to_path_with_sync(
            app_id,
            use_case,
            allowed,
            path,
            sync_directory,
        );
        if let Err(err) = &result
            && !err.is::<crate::permissions::UncertainCommit>()
        {
            return result;
        }
        // A visible denial must revoke access even if directory sync failed.
        let mut removed = Vec::new();
        if !allowed {
            guard.sessions.retain(|session_id, session| {
                if session.app_id == app_id && session.use_case == use_case {
                    request_execution::mark_session_closed(state, session_id);
                    removed.push(session.clone());
                    false
                } else {
                    true
                }
            });
        }
        (removed, result)
    };
    // Termination can wait for runtime locks. Never hold the state lock here.
    for session in removed {
        request_execution::terminate_active_container_handles_for_session(
            state,
            &session.profile_id,
            &session.session_id,
        )
        .await;
        observability::log_session_ended(observability::SessionFields {
            session_id: &session.session_id,
            app_id: &session.app_id,
            use_case: &session.use_case,
            profile_id: &session.profile_id,
        });
    }
    result
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

    fn test_state() -> SharedState {
        use std::sync::{Arc, Mutex};
        let inner = crate::state::Inner {
            config: crate::config::Config {
                allow_all: false,
                auto_grant: false,
                idle_timeout_secs: 300,
                container_memory: "8g".into(),
                oci_store: None,
            },
            permissions: PermissionStore::default(),
            assignments: Default::default(),
            profiles: Default::default(),
            profile_epochs: Default::default(),
            runtimes: Default::default(),
            sessions: Default::default(),
            installing_profiles: Default::default(),
            runtime_downloads: Default::default(),
            runtime_download_owners: Default::default(),
            runtime_update_checks: Default::default(),
            recent_installs: Default::default(),
            recent_runtime_downloads: Default::default(),
            variant: crate::hardware::Variant::Cpu,
        };
        SharedState(
            Arc::new(tokio::sync::Mutex::new(inner)),
            Default::default(),
            Arc::new(tokio::sync::Mutex::new(
                crate::container::ContainerPool::new(),
            )),
            Arc::new(Mutex::new(Default::default())),
            Default::default(),
            Default::default(),
            Default::default(),
        )
    }

    async fn populate_sessions(state: &SharedState) {
        let mut guard = state.0.lock().await;
        for (id, app, case, profile) in [
            ("active", "app", "case", "shared"),
            ("queued", "app", "case", "shared"),
            ("cold", "app", "case", "cold-profile"),
            ("other-app", "other", "case", "shared"),
            ("other-case", "app", "other", "shared"),
        ] {
            guard.sessions.insert(
                id.into(),
                crate::state::Session {
                    session_id: id.into(),
                    app_id: app.into(),
                    use_case: case.into(),
                    profile_id: profile.into(),
                    instructions: String::new(),
                    started_at: chrono::Utc::now(),
                },
            );
        }
    }

    #[tokio::test]
    async fn denial_invalidates_matching_sessions_and_existing_cancellation_tokens() {
        use request_execution::RequestCancellation;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let state = test_state();
        populate_sessions(&state).await;
        let tokens =
            ["active", "queued", "cold"].map(|id| RequestCancellation::for_session(&state, id));
        for token in tokens {
            assert!(token.ensure_not_cancelled().is_ok());
        }

        set_permission(&state, "app", "case", false, &path)
            .await
            .unwrap();

        for token in tokens {
            assert_eq!(
                token.ensure_not_cancelled(),
                Err(request_execution::request_cancelled_reason())
            );
        }
        let guard = state.0.lock().await;
        assert_eq!(guard.sessions.len(), 2);
        for id in ["other-app", "other-case"] {
            assert!(guard.sessions.contains_key(id));
            assert!(!state.is_session_cancelled(id));
        }
        assert_eq!(guard.permissions.check("app", "case"), Some(false));
        drop(guard);
        let saved: PermissionStore =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.check("app", "case"), Some(false));

        // Repeated denial is harmless; regranting never revives retained session IDs.
        set_permission(&state, "app", "case", false, &path)
            .await
            .unwrap();
        set_permission(&state, "app", "case", true, &path)
            .await
            .unwrap();
        assert_eq!(state.0.lock().await.sessions.len(), 2);
        for token in tokens {
            assert!(token.is_cancelled());
        }
    }

    #[tokio::test]
    async fn uncertain_denial_invalidates_sessions_and_returns_error() {
        use request_execution::RequestCancellation;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let state = test_state();
        populate_sessions(&state).await;
        set_permission(&state, "app", "case", true, &path)
            .await
            .unwrap();
        let tokens =
            ["active", "queued", "cold"].map(|id| RequestCancellation::for_session(&state, id));

        let error = set_permission_with_sync(&state, "app", "case", false, &path, |directory| {
            assert!(directory.metadata()?.is_dir());
            let saved: PermissionStore = serde_json::from_slice(&std::fs::read(&path)?).unwrap();
            assert_eq!(saved.check("app", "case"), Some(false));
            Err(std::io::Error::other("injected directory sync failure"))
        })
        .await
        .unwrap_err();

        assert!(error.is::<crate::permissions::UncertainCommit>());
        for token in tokens {
            assert!(token.is_cancelled());
        }
        let guard = state.0.lock().await;
        assert_eq!(guard.permissions.check("app", "case"), Some(false));
        assert_eq!(guard.sessions.len(), 2);
        for id in ["other-app", "other-case"] {
            assert!(guard.sessions.contains_key(id));
            assert!(!state.is_session_cancelled(id));
        }
        let saved: PermissionStore =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.check("app", "case"), Some(false));
    }

    #[tokio::test]
    async fn denial_cancels_queued_work_before_it_can_cold_start() {
        use crate::profiles::RuntimeCandidate;
        use request_execution::RequestCancellation;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let state = test_state();
        populate_sessions(&state).await;
        let mut pool = state.2.lock().await;
        pool.oci_store = dir.path().join("oci");
        pool.system_oci_store = dir.path().join("system-oci");
        let queued_state = state.clone();
        let artifact = dir.path().join("model");
        let (waiting, ready) = tokio::sync::oneshot::channel();
        let queued = tokio::spawn(async move {
            let cancellation = RequestCancellation::for_session(&queued_state, "queued");
            assert!(cancellation.ensure_not_cancelled().is_ok());
            waiting.send(()).unwrap();
            queued_state
                .2
                .lock()
                .await
                .get_or_spawn_any_checked(
                    "shared",
                    0,
                    "fixture",
                    &[RuntimeCandidate {
                        variant: crate::hardware::Variant::Cpu,
                        image_ref: "fixture:latest".into(),
                    }],
                    &artifact,
                    &HashMap::new(),
                    |_| panic!("cancelled request must not start a runtime"),
                    || cancellation.ensure_not_cancelled(),
                )
                .err()
                .expect("queued request must be cancelled")
                .to_string()
        });
        ready.await.unwrap();

        // The pool is busy, but revocation must still publish cancellation.
        tokio::time::timeout(
            Duration::from_secs(2),
            set_permission(&state, "app", "case", false, &path),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!state.0.lock().await.sessions.contains_key("queued"));
        drop(pool);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), queued)
                .await
                .unwrap()
                .unwrap(),
            request_execution::request_cancelled_reason(),
        );
    }

    #[tokio::test]
    async fn grant_preserves_sessions_and_failed_denial_does_not_cancel_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let state = test_state();
        populate_sessions(&state).await;

        set_permission(&state, "app", "case", true, &path)
            .await
            .unwrap();
        assert!(
            set_permission(&state, "app", "case", false, &path.join("blocked"))
                .await
                .is_err()
        );

        let guard = state.0.lock().await;
        assert_eq!(guard.permissions.check("app", "case"), Some(true));
        assert_eq!(guard.sessions.len(), 5);
        for id in guard.sessions.keys() {
            assert!(!state.is_session_cancelled(id));
        }
        let saved: PermissionStore =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.check("app", "case"), Some(true));
    }

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
