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
            let sessions: Vec<SessionInfo> = guard
                .sessions
                .values()
                .map(|s| SessionInfo {
                    session_id: s.session_id.clone(),
                    app_id: s.app_id.clone(),
                    use_case: s.use_case.clone(),
                    profile_id: s.profile_id.clone(),
                    started_at: s.started_at.to_rfc3339(),
                })
                .collect();
            call.reply(sessions)
        })
    }

    fn kill_session(
        &self,
        call: &mut dyn Call_KillSession,
        session_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            let session = match guard.sessions.remove(&session_id) {
                Some(s) => s,
                None => return call.reply_session_not_found(session_id),
            };
            let profile_still_used = guard
                .sessions
                .values()
                .any(|s| s.profile_id == session.profile_id);
            if !profile_still_used {
                guard.containers.kill(&session.profile_id);
            }
            call.reply()
        })
    }
}
