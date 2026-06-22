/// Varlink handler for `aileron.Sessions`.
use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects
use aileron_varlink::aileron_Sessions::{
    Call_KillSession, Call_ListActive, SessionInfo, VarlinkCallError, VarlinkInterface,
};

pub struct SessionsHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

impl SessionsHandler {
    pub fn new(state: SharedState, rt: tokio::runtime::Handle) -> Self {
        Self { state, rt }
    }
}

impl VarlinkInterface for SessionsHandler {
    fn list_active(&self, call: &mut dyn Call_ListActive) -> varlink::Result<()> {
        self.rt.block_on(async {
            let guard = self.state.0.lock().await;
            call.reply(active_sessions(&guard))
        })
    }

    fn kill_session(
        &self,
        call: &mut dyn Call_KillSession,
        session_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if !kill_session(&mut guard, &session_id) {
                return call.reply_session_not_found(session_id);
            }
            call.reply()
        })
    }
}

fn active_sessions(guard: &crate::state::Inner) -> Vec<SessionInfo> {
    guard
        .sessions
        .values()
        .map(|s| SessionInfo {
            session_id: s.session_id.clone(),
            app_id: s.app_id.clone(),
            use_case: s.use_case.clone(),
            profile_id: s.profile_id.clone(),
            started_at: s.started_at.to_rfc3339(),
        })
        .collect()
}

fn kill_session(guard: &mut crate::state::Inner, session_id: &str) -> bool {
    let session = match guard.sessions.remove(session_id) {
        Some(s) => s,
        None => return false,
    };
    let profile_still_used = guard
        .sessions
        .values()
        .any(|s| s.profile_id == session.profile_id);
    if !profile_still_used {
        guard.containers.kill(&session.profile_id);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignments::Assignments;
    use crate::config::Config;
    use crate::container::ContainerPool;
    use crate::hardware::Variant;
    use crate::manifests::RuntimeManifestStore;
    use crate::permissions::PermissionStore;
    use crate::profiles::ProfileStore;
    use crate::state::{Inner, InstallRecord, Session};
    use hegel::TestCase;
    use hegel::generators as gs;
    use std::collections::{HashMap, VecDeque};

    fn test_inner() -> Inner {
        Inner {
            config: Config {
                allow_all: false,
                auto_grant: false,
                idle_timeout_secs: 300,
                container_memory: "8g".to_string(),
                oci_store: None,
            },
            permissions: PermissionStore::default(),
            assignments: Assignments::default(),
            profiles: ProfileStore::default(),
            runtimes: RuntimeManifestStore::default(),
            containers: ContainerPool::new(),
            sessions: HashMap::new(),
            installing_profiles: HashMap::<String, InstallRecord>::new(),
            runtime_downloads: HashMap::<String, InstallRecord>::new(),
            runtime_download_owners: HashMap::new(),
            runtime_update_checks: HashMap::new(),
            recent_installs: VecDeque::new(),
            recent_runtime_downloads: VecDeque::new(),
            variant: Variant::Cpu,
        }
    }

    fn session(session_id: &str, profile_id: &str) -> Session {
        Session {
            session_id: session_id.to_string(),
            app_id: "org.aileron.Test".to_string(),
            use_case: "language.extract".to_string(),
            profile_id: profile_id.to_string(),
            instructions: "be concise".to_string(),
            started_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn active_sessions_reports_all_session_metadata() {
        let mut inner = test_inner();
        inner
            .sessions
            .insert("session-a".to_string(), session("session-a", "profile-a"));

        let sessions = active_sessions(&inner);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-a");
        assert_eq!(sessions[0].app_id, "org.aileron.Test");
        assert_eq!(sessions[0].use_case, "language.extract");
        assert_eq!(sessions[0].profile_id, "profile-a");
        assert!(!sessions[0].started_at.is_empty());
    }

    #[hegel::test]
    fn active_sessions_reports_generated_session_ids(tc: TestCase) {
        let ids = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                "session-a".to_string(),
                "session-b".to_string(),
                "session-c".to_string(),
                "session-d".to_string(),
            ]))
            .max_size(4),
        );
        let mut inner = test_inner();
        for id in &ids {
            inner
                .sessions
                .insert(id.clone(), session(id, &format!("profile-{id}")));
        }
        let mut expected = ids;
        expected.sort();
        expected.dedup();

        let mut actual = active_sessions(&inner)
            .into_iter()
            .map(|session| session.session_id)
            .collect::<Vec<_>>();
        actual.sort();

        assert_eq!(actual, expected);
    }

    #[test]
    fn kill_session_removes_only_requested_session() {
        let mut inner = test_inner();
        inner
            .sessions
            .insert("session-a".to_string(), session("session-a", "profile-a"));
        inner
            .sessions
            .insert("session-b".to_string(), session("session-b", "profile-a"));

        assert!(kill_session(&mut inner, "session-a"));

        assert!(!inner.sessions.contains_key("session-a"));
        assert!(inner.sessions.contains_key("session-b"));
    }

    #[hegel::test]
    fn kill_session_removes_generated_target_only(tc: TestCase) {
        let target = tc.draw(gs::sampled_from(vec![
            "session-a".to_string(),
            "session-b".to_string(),
        ]));
        let mut inner = test_inner();
        inner
            .sessions
            .insert("session-a".to_string(), session("session-a", "profile-a"));
        inner
            .sessions
            .insert("session-b".to_string(), session("session-b", "profile-a"));

        assert!(kill_session(&mut inner, &target));

        assert!(!inner.sessions.contains_key(&target));
        assert_eq!(inner.sessions.len(), 1);
    }

    #[test]
    fn kill_session_reports_missing_session() {
        let mut inner = test_inner();

        assert!(!kill_session(&mut inner, "missing"));
    }
}
