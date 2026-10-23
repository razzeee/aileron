//! Daemon request execution lifecycle helpers.
//!
//! This module owns cancellation vocabulary for inference requests. Session
//! closure and per-session request epochs stop both active and still-loading
//! inference without cancelling later requests in the same session.

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use crate::container::ContainerHandle;
use crate::state::SharedState;

pub(crate) struct ActiveContainerRequest<'a> {
    state: &'a SharedState,
    session_id: &'a str,
    registration: u64,
}

impl<'a> ActiveContainerRequest<'a> {
    pub(crate) fn new(
        state: &'a SharedState,
        profile_id: &'a str,
        session_id: &'a str,
        handle: ContainerHandle,
    ) -> Self {
        let registration = state.begin_container_request(profile_id, session_id, handle);
        Self {
            state,
            session_id,
            registration,
        }
    }
}

impl Drop for ActiveContainerRequest<'_> {
    fn drop(&mut self) {
        self.state
            .end_container_request(self.session_id, self.registration);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RequestCancellation<'a> {
    state: &'a SharedState,
    session_id: &'a str,
    epoch: u64,
}

impl<'a> RequestCancellation<'a> {
    #[cfg(test)]
    pub(crate) fn for_session(state: &'a SharedState, session_id: &'a str) -> Self {
        Self::for_epoch(state, session_id, state.request_epoch(session_id))
    }

    pub(crate) fn for_epoch(state: &'a SharedState, session_id: &'a str, epoch: u64) -> Self {
        Self {
            state,
            session_id,
            epoch,
        }
    }

    pub(crate) fn is_cancelled(self) -> bool {
        self.state.is_request_cancelled(self.session_id, self.epoch)
    }

    pub(crate) fn ensure_not_cancelled(self) -> Result<(), String> {
        if self.is_cancelled() {
            Err(if self.state.is_session_cancelled(self.session_id) {
                request_cancelled_reason()
            } else {
                "container returned error request_cancelled: active request was stopped".to_owned()
            })
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
        spawn_cancel_watcher(self.state, self.session_id, self.epoch, handle)
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
    finish_terminating_container_handles(state, profile_id, handles).await;
}

pub(crate) async fn finish_terminating_container_handles(
    state: &SharedState,
    profile_id: &str,
    handles: Vec<ContainerHandle>,
) {
    if handles.is_empty() {
        return;
    }
    {
        let mut containers = state.2.lock().await;
        for handle in &handles {
            containers.kill_handle(profile_id, handle);
        }
    }
    for handle in handles {
        handle.wait_terminated().await;
    }
}

pub(crate) fn request_cancelled_reason() -> String {
    "container returned error request_cancelled: session was closed".to_string()
}

pub(crate) fn background_preempted_reason() -> String {
    "container returned error request_cancelled: background request was preempted by interactive request"
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
    epoch: u64,
    handle: &ContainerHandle,
) -> CancelWatcher {
    let state = state.clone();
    let session_id = session_id.to_string();
    let handle = handle.clone();
    let stop = Arc::new((StdMutex::new(false), Condvar::new()));
    let thread_stop = stop.clone();
    let thread = thread::spawn(move || {
        loop {
            if state.is_request_cancelled(&session_id, epoch) {
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
        assert!(!is_request_cancelled_failure(
            "container returned error invalid_input: bad"
        ));
    }

    #[test]
    fn active_cancellation_stops_existing_work_but_not_a_later_request() {
        let state = shared_state();
        let first = RequestCancellation::for_session(&state, "session-a");
        let epoch = state.request_epoch("session-a");
        let unrelated = RequestCancellation::for_session(&state, "session-b");
        state.cancel_active_requests("session-a");
        assert!(first.is_cancelled());
        // Legacy callbacks create their guard after receiving a token, which
        // can happen after cancellation has advanced the session epoch.
        assert!(RequestCancellation::for_epoch(&state, "session-a", epoch).is_cancelled());
        assert!(!unrelated.is_cancelled());
        assert!(!RequestCancellation::for_session(&state, "session-a").is_cancelled());
        assert!(
            first
                .ensure_not_cancelled()
                .unwrap_err()
                .contains("request_cancelled")
        );
    }

    #[test]
    fn epoch_exhaustion_is_permanent_and_session_local() {
        let state = shared_state();
        state.7.lock().unwrap().insert("a".into(), u64::MAX - 1);
        let old = RequestCancellation::for_session(&state, "a");
        state.cancel_active_requests("a");
        assert!(old.is_cancelled());
        assert!(RequestCancellation::for_session(&state, "a").is_cancelled());
        state.cancel_active_requests("a");
        mark_session_closed(&state, "a");
        state.clear_session_cancelled("a");
        assert_eq!(state.request_epoch("a"), u64::MAX);
        assert!(RequestCancellation::for_session(&state, "a").is_cancelled());
        assert!(!RequestCancellation::for_session(&state, "b").is_cancelled());
    }

    #[test]
    fn close_reopen_never_revives_a_captured_epoch() {
        let state = shared_state();
        let old = RequestCancellation::for_session(&state, "a");
        mark_session_closed(&state, "a");
        state.clear_session_cancelled("a");
        assert!(old.is_cancelled());
        assert!(!RequestCancellation::for_session(&state, "a").is_cancelled());
    }

    #[test]
    fn stale_guard_drop_preserves_new_registration_of_same_handle() {
        let state = shared_state();
        let handle = ContainerHandle::for_lifecycle_test();
        let old = ActiveContainerRequest::new(&state, "profile", "a", handle.clone());
        let newer = ActiveContainerRequest::new(&state, "profile", "a", handle.clone());
        let other = ActiveContainerRequest::new(&state, "profile", "b", handle.clone());
        thread::scope(|scope| {
            // Move the actual RAII guard across threads; join fixes the interleaving.
            scope.spawn(move || drop(old)).join().unwrap();
        });
        assert_eq!(state.active_container_handles("profile", "a").len(), 1);
        assert_eq!(state.active_container_handles("profile", "b").len(), 1);
        drop(newer);
        assert!(state.active_container_handles("profile", "a").is_empty());
        assert_eq!(state.active_container_handles("profile", "b").len(), 1);
        drop(other);
        assert!(state.4.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancellation_snapshot_excludes_later_and_other_session_handles() {
        let state = shared_state();
        let old = ContainerHandle::for_lifecycle_test();
        let later = ContainerHandle::for_lifecycle_test();
        let other = ContainerHandle::for_lifecycle_test();
        let _old = ActiveContainerRequest::new(&state, "profile", "a", old.clone());
        let _other = ActiveContainerRequest::new(&state, "profile", "b", other.clone());
        let captured = state.request_epoch("a");
        let worker_state = state.clone();
        let snapshot = thread::spawn(move || worker_state.cancel_active_requests("a"))
            .join()
            .unwrap();
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot[0].ptr_eq(&old));
        assert!(old.is_terminating());
        let _later = ActiveContainerRequest::new(&state, "profile", "a", later.clone());
        finish_terminating_container_handles(&state, "profile", snapshot).await;
        assert!(state.is_request_cancelled("a", captured));
        assert!(!RequestCancellation::for_session(&state, "a").is_cancelled());
        assert!(!later.is_terminating());
        assert!(!other.is_terminating());
    }

    #[test]
    fn racing_epoch_capture_has_a_single_cancellation_order() {
        use std::sync::Barrier;
        let state = shared_state();
        for _ in 0..32 {
            let before = state.request_epoch("a");
            let barrier = Barrier::new(2);
            thread::scope(|scope| {
                let captured = scope.spawn(|| {
                    barrier.wait();
                    state.request_epoch("a")
                });
                barrier.wait();
                state.cancel_active_requests("a");
                let captured = captured.join().unwrap();
                assert!(captured == before || captured == before + 1);
                assert_eq!(
                    state.is_request_cancelled("a", captured),
                    captured == before
                );
                assert_eq!(state.request_epoch("b"), 0);
            });
        }
    }

    #[tokio::test]
    async fn watcher_cancels_late_registration_but_not_a_new_epoch() {
        let state = shared_state();
        let old_epoch = state.request_epoch("a");
        // Nothing was registered when cancellation took its snapshot.
        assert!(state.cancel_active_requests("a").is_empty());
        let old_handle = ContainerHandle::for_lifecycle_test();
        let new_handle = ContainerHandle::for_lifecycle_test();
        let _old = ActiveContainerRequest::new(&state, "profile", "a", old_handle.clone());
        let _new = ActiveContainerRequest::new(&state, "profile", "a", new_handle.clone());
        let old_watcher =
            RequestCancellation::for_epoch(&state, "a", old_epoch).spawn_watcher(&old_handle);
        let new_watcher = RequestCancellation::for_session(&state, "a").spawn_watcher(&new_handle);
        tokio::time::timeout(Duration::from_secs(2), old_handle.wait_terminated())
            .await
            .expect("old-token watcher must queue termination");
        old_watcher.stop();
        new_watcher.stop();
        assert!(old_handle.is_terminating());
        assert!(!new_handle.is_terminating());
    }
}
