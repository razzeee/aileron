//! Daemon request execution lifecycle helpers.
//!
//! This module owns cancellation vocabulary for inference requests. Session
//! closure is the real cancellation seam today; the `RequestCancellation` shape
//! leaves room for per-request cancellation if another adapter makes it real.

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use crate::container::ContainerHandle;
use crate::state::SharedState;

pub(crate) struct ActiveContainerRequest<'a> {
    state: &'a SharedState,
    profile_id: &'a str,
    session_id: &'a str,
    handle: ContainerHandle,
}

impl<'a> ActiveContainerRequest<'a> {
    pub(crate) fn new(
        state: &'a SharedState,
        profile_id: &'a str,
        session_id: &'a str,
        handle: ContainerHandle,
    ) -> Self {
        state.begin_container_request(profile_id, session_id, handle.clone());
        Self {
            state,
            profile_id,
            session_id,
            handle,
        }
    }
}

impl Drop for ActiveContainerRequest<'_> {
    fn drop(&mut self) {
        self.state
            .end_container_request(self.profile_id, self.session_id, &self.handle);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RequestCancellation<'a> {
    state: &'a SharedState,
    session_id: &'a str,
}

impl<'a> RequestCancellation<'a> {
    pub(crate) fn for_session(state: &'a SharedState, session_id: &'a str) -> Self {
        Self { state, session_id }
    }

    pub(crate) fn is_cancelled(self) -> bool {
        self.state.is_session_cancelled(self.session_id)
    }

    pub(crate) fn ensure_not_cancelled(self) -> Result<(), String> {
        if self.is_cancelled() {
            Err(request_cancelled_reason())
        } else {
            Ok(())
        }
    }

    pub(crate) fn ensure_not_cancelled_or_terminate_spawned(
        self,
        handle: &ContainerHandle,
        spawned: bool,
    ) -> Result<(), String> {
        if let Err(reason) = self.ensure_not_cancelled() {
            if spawned {
                handle.terminate();
            }
            Err(reason)
        } else {
            Ok(())
        }
    }

    pub(crate) fn spawn_watcher(self, handle: &ContainerHandle) -> CancelWatcher {
        spawn_cancel_watcher(self.state, self.session_id, handle)
    }
}

pub(crate) fn mark_session_closed(state: &SharedState, session_id: &str) {
    state.cancel_session_requests(session_id);
}

pub(crate) async fn terminate_active_container_handles_for_session(
    state: &SharedState,
    profile_id: &str,
    session_id: &str,
) {
    let handles = state.terminate_active_container_handles(profile_id, session_id);
    if handles.is_empty() {
        return;
    }
    let mut containers = state.2.lock().await;
    for handle in handles {
        containers.kill_handle(profile_id, &handle);
    }
}

pub(crate) fn request_cancelled_reason() -> String {
    "container returned error request_cancelled: session was closed".to_string()
}

pub(crate) fn background_preempted_reason() -> String {
    "container returned error request_cancelled: background request was preempted by interactive request"
        .to_string()
}

pub(crate) fn predict_next_superseded_reason() -> String {
    "container returned error request_cancelled: superseded by newer StreamPredictNext request"
        .to_string()
}

pub(crate) fn is_request_cancelled_failure(reason: &str) -> bool {
    crate::observability::runtime_error_code(reason) == Some("request_cancelled")
}

pub(crate) struct CancelWatcher {
    stop: Arc<(StdMutex<bool>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl CancelWatcher {
    pub(crate) fn stop(mut self) {
        notify_cancel_watcher_stop(&self.stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CancelWatcher {
    fn drop(&mut self) {
        notify_cancel_watcher_stop(&self.stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_cancel_watcher(
    state: &SharedState,
    session_id: &str,
    handle: &ContainerHandle,
) -> CancelWatcher {
    let state = state.clone();
    let session_id = session_id.to_string();
    let handle = handle.clone();
    let stop = Arc::new((StdMutex::new(false), Condvar::new()));
    let thread_stop = stop.clone();
    let thread = thread::spawn(move || {
        loop {
            if state.is_session_cancelled(&session_id) {
                handle.terminate();
                break;
            }
            let (lock, wake) = &*thread_stop;
            let Ok(stopped) = lock.lock() else {
                break;
            };
            let Ok((stopped, _)) =
                wake.wait_timeout_while(stopped, Duration::from_millis(20), |stopped| !*stopped)
            else {
                break;
            };
            if *stopped {
                break;
            }
        }
    });

    CancelWatcher {
        stop,
        thread: Some(thread),
    }
}

fn notify_cancel_watcher_stop(stop: &Arc<(StdMutex<bool>, Condvar)>) {
    let (lock, wake) = &**stop;
    if let Ok(mut stopped) = lock.lock() {
        *stopped = true;
        wake.notify_one();
    }
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
    use crate::state::{Inner, InstallRecord};
    use std::collections::{HashMap, HashSet, VecDeque};
    use tokio::sync::Mutex;

    fn shared_state() -> SharedState {
        SharedState(
            Arc::new(Mutex::new(Inner {
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
                profile_epochs: HashMap::new(),
                runtimes: RuntimeManifestStore::default(),
                sessions: HashMap::new(),
                installing_profiles: HashMap::<String, InstallRecord>::new(),
                runtime_downloads: HashMap::<String, InstallRecord>::new(),
                runtime_download_owners: HashMap::new(),
                runtime_update_checks: HashMap::new(),
                recent_installs: VecDeque::new(),
                recent_runtime_downloads: VecDeque::new(),
                variant: Variant::Cpu,
            })),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(Mutex::new(ContainerPool::new())),
            Arc::new(StdMutex::new(HashSet::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
        )
    }

    #[test]
    fn session_closure_is_the_current_cancellation_seam() {
        let state = shared_state();
        let cancellation = RequestCancellation::for_session(&state, "session-a");

        assert!(cancellation.ensure_not_cancelled().is_ok());

        mark_session_closed(&state, "session-a");

        assert_eq!(
            cancellation.ensure_not_cancelled(),
            Err(request_cancelled_reason())
        );
    }

    #[test]
    fn request_cancelled_failures_are_classified_for_adapters() {
        assert!(is_request_cancelled_failure(&request_cancelled_reason()));
        assert!(is_request_cancelled_failure(&background_preempted_reason()));
        assert!(is_request_cancelled_failure(
            &predict_next_superseded_reason()
        ));
        assert!(!is_request_cancelled_failure(
            "container returned error invalid_input: bad"
        ));
    }
}
