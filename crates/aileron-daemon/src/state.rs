/// Shared mutable daemon state.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

use crate::assignments::Assignments;
use crate::config::Config;
use crate::container::{ContainerHandle, ContainerPool};
use crate::hardware::Variant;
use crate::manifests::RuntimeManifestStore;
use crate::permissions::PermissionStore;
use crate::profiles::ProfileStore;

pub type ActiveContainerEntry = (String, ContainerHandle);
pub type ActiveContainerRequests = HashMap<String, Vec<ActiveContainerEntry>>;
pub type SharedActiveContainerRequests = Arc<StdMutex<ActiveContainerRequests>>;

#[derive(Default)]
pub struct ProfileExecutionState {
    pub pending_interactive: u64,
    pub active_interactive: u64,
    pub active_background: Vec<BackgroundExecutionEntry>,
}

pub struct BackgroundExecutionEntry {
    pub handle: ContainerHandle,
    pub preempted: Arc<AtomicBool>,
}

pub type ProfileExecutionStates = HashMap<String, ProfileExecutionState>;
pub type SharedProfileExecutionStates = Arc<StdMutex<ProfileExecutionStates>>;

#[derive(Debug, Clone)]
pub struct InstallRecord {
    pub bytes_pulled: u64,
    pub total_bytes: u64,
    pub status: String,
    pub cancel_requested: bool,
    pub samples: VecDeque<InstallSample>,
}

#[derive(Debug, Clone)]
pub struct InstallSample {
    pub at: chrono::DateTime<chrono::Utc>,
    pub bytes_pulled: u64,
}

#[derive(Debug, Clone)]
pub struct RuntimeUpdateCheck {
    pub local_digest: String,
    pub available: bool,
    pub status: String,
    pub checking: bool,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub app_id: String,
    pub use_case: String,
    pub profile_id: String,
    pub instructions: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

pub struct Inner {
    pub config: Config,
    pub permissions: PermissionStore,
    pub assignments: Assignments,
    pub profiles: ProfileStore,
    pub profile_epochs: HashMap<String, u64>,
    pub runtimes: RuntimeManifestStore,
    pub sessions: HashMap<String, Session>,
    pub installing_profiles: HashMap<String, InstallRecord>,
    pub runtime_downloads: HashMap<String, InstallRecord>,
    pub runtime_download_owners: HashMap<String, String>,
    pub runtime_update_checks: HashMap<String, RuntimeUpdateCheck>,
    pub recent_installs: VecDeque<(String, InstallRecord)>,
    pub recent_runtime_downloads: VecDeque<(String, InstallRecord)>,
    /// Best available hardware variant, detected once at startup.
    pub variant: Variant,
}

#[derive(Clone)]
pub struct SharedState(
    pub Arc<Mutex<Inner>>,
    pub Arc<StdMutex<HashMap<String, u64>>>,
    pub Arc<Mutex<ContainerPool>>,
    pub Arc<StdMutex<HashSet<String>>>,
    pub SharedActiveContainerRequests,
    pub Arc<StdMutex<HashMap<String, u64>>>,
    pub SharedProfileExecutionStates,
);

impl SharedState {
    pub async fn load(config: Config) -> anyhow::Result<Self> {
        let permissions = PermissionStore::load()?;
        let assignments = Assignments::load()?;
        let profiles = ProfileStore::load()?;
        let runtimes = RuntimeManifestStore::load()?;
        let mut containers = ContainerPool::new();
        containers.idle_timeout_secs = config.idle_timeout_secs;
        containers.memory_limit = config.container_memory.clone();
        containers.oci_store = config
            .oci_store
            .clone()
            .unwrap_or_else(crate::container::default_oci_store);
        let variant = crate::hardware::detect();
        Ok(Self(
            Arc::new(Mutex::new(Inner {
                config,
                permissions,
                assignments,
                profiles,
                profile_epochs: HashMap::new(),
                runtimes,
                sessions: HashMap::new(),
                installing_profiles: HashMap::new(),
                runtime_downloads: HashMap::new(),
                runtime_download_owners: HashMap::new(),
                runtime_update_checks: HashMap::new(),
                recent_installs: VecDeque::new(),
                recent_runtime_downloads: VecDeque::new(),
                variant,
            })),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(Mutex::new(containers)),
            Arc::new(StdMutex::new(HashSet::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
        ))
    }

    pub fn begin_predict_next(&self, session_id: &str) -> u64 {
        let mut generations = self.1.lock().expect("prediction generation mutex poisoned");
        let generation = generations
            .get(session_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        generations.insert(session_id.to_string(), generation);
        generation
    }

    pub fn is_current_predict_next(&self, session_id: &str, generation: u64) -> bool {
        self.1
            .lock()
            .expect("prediction generation mutex poisoned")
            .get(session_id)
            .is_some_and(|current| *current == generation)
    }

    pub fn clear_predict_next(&self, session_id: &str) {
        self.1
            .lock()
            .expect("prediction generation mutex poisoned")
            .remove(session_id);
    }

    pub fn cancel_session_requests(&self, session_id: &str) {
        self.3
            .lock()
            .expect("session cancellation mutex poisoned")
            .insert(session_id.to_string());
    }

    pub fn is_session_cancelled(&self, session_id: &str) -> bool {
        self.3
            .lock()
            .expect("session cancellation mutex poisoned")
            .contains(session_id)
    }

    pub fn clear_session_cancelled(&self, session_id: &str) {
        self.3
            .lock()
            .expect("session cancellation mutex poisoned")
            .remove(session_id);
    }

    pub fn set_profile_epoch(&self, profile_id: &str, epoch: u64) {
        self.5
            .lock()
            .expect("profile epoch mutex poisoned")
            .insert(profile_id.to_string(), epoch);
    }

    pub fn current_profile_epoch(&self, profile_id: &str) -> u64 {
        self.5
            .lock()
            .expect("profile epoch mutex poisoned")
            .get(profile_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn begin_container_request(
        &self,
        profile_id: &str,
        session_id: &str,
        handle: ContainerHandle,
    ) {
        self.4
            .lock()
            .expect("active request mutex poisoned")
            .entry(session_id.to_string())
            .or_default()
            .push((profile_id.to_string(), handle));
    }

    pub fn end_container_request(
        &self,
        profile_id: &str,
        session_id: &str,
        handle: &ContainerHandle,
    ) {
        let mut active = self.4.lock().expect("active request mutex poisoned");
        let Some(handles) = active.get_mut(session_id) else {
            return;
        };
        handles.retain(|(active_profile, active_handle)| {
            active_profile != profile_id || !active_handle.ptr_eq(handle)
        });
        if handles.is_empty() {
            active.remove(session_id);
        }
    }

    pub fn active_container_handles(
        &self,
        profile_id: &str,
        session_id: &str,
    ) -> Vec<ContainerHandle> {
        self.4
            .lock()
            .expect("active request mutex poisoned")
            .get(session_id)
            .map(|handles| {
                handles
                    .iter()
                    .filter(|(active_profile, _)| active_profile == profile_id)
                    .map(|(_, handle)| handle.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn terminate_active_container_handles(
        &self,
        profile_id: &str,
        session_id: &str,
    ) -> Vec<ContainerHandle> {
        let active = self.4.lock().expect("active request mutex poisoned");
        let handles: Vec<ContainerHandle> = active
            .get(session_id)
            .map(|handles| {
                handles
                    .iter()
                    .filter(|(active_profile, _)| active_profile == profile_id)
                    .map(|(_, handle)| handle.clone())
                    .collect()
            })
            .unwrap_or_default();
        for handle in &handles {
            handle.terminate();
        }
        handles
    }

    pub fn begin_interactive_execution(&self, profile_id: &str) {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let state = states.entry(profile_id.to_string()).or_default();
        state.pending_interactive = state.pending_interactive.saturating_add(1);
        for background in &state.active_background {
            background.preempted.store(true, Ordering::SeqCst);
            background.handle.terminate();
        }
    }

    pub fn activate_interactive_execution(&self, profile_id: &str) {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let state = states.entry(profile_id.to_string()).or_default();
        state.pending_interactive = state.pending_interactive.saturating_sub(1);
        state.active_interactive = state.active_interactive.saturating_add(1);
    }

    pub fn cancel_pending_interactive_execution(&self, profile_id: &str) {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let Some(state) = states.get_mut(profile_id) else {
            return;
        };
        state.pending_interactive = state.pending_interactive.saturating_sub(1);
        if state.pending_interactive == 0
            && state.active_interactive == 0
            && state.active_background.is_empty()
        {
            states.remove(profile_id);
        }
    }

    pub fn end_interactive_execution(&self, profile_id: &str) {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let Some(state) = states.get_mut(profile_id) else {
            return;
        };
        state.active_interactive = state.active_interactive.saturating_sub(1);
        if state.pending_interactive == 0
            && state.active_interactive == 0
            && state.active_background.is_empty()
        {
            states.remove(profile_id);
        }
    }

    pub fn background_execution_can_start(&self, profile_id: &str) -> bool {
        self.6
            .lock()
            .expect("execution state mutex poisoned")
            .get(profile_id)
            .is_none_or(|state| state.pending_interactive == 0 && state.active_interactive == 0)
    }

    pub fn begin_background_execution(
        &self,
        profile_id: &str,
        handle: ContainerHandle,
    ) -> Option<Arc<AtomicBool>> {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let state = states.entry(profile_id.to_string()).or_default();
        if state.pending_interactive > 0 || state.active_interactive > 0 {
            return None;
        }
        let preempted = Arc::new(AtomicBool::new(false));
        state.active_background.push(BackgroundExecutionEntry {
            handle,
            preempted: preempted.clone(),
        });
        Some(preempted)
    }

    pub fn end_background_execution(&self, profile_id: &str, handle: &ContainerHandle) {
        let mut states = self.6.lock().expect("execution state mutex poisoned");
        let Some(state) = states.get_mut(profile_id) else {
            return;
        };
        state
            .active_background
            .retain(|active| !active.handle.ptr_eq(handle));
        if state.pending_interactive == 0
            && state.active_interactive == 0
            && state.active_background.is_empty()
        {
            states.remove(profile_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignments::Assignments;
    use crate::config::Config;
    use crate::hardware::Variant;
    use crate::manifests::RuntimeManifestStore;
    use crate::permissions::PermissionStore;
    use crate::profiles::ProfileStore;

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
                installing_profiles: HashMap::new(),
                runtime_downloads: HashMap::new(),
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
    fn newer_predict_next_generation_supersedes_older_generation() {
        let state = shared_state();

        let first = state.begin_predict_next("session-a");
        let second = state.begin_predict_next("session-a");

        assert!(!state.is_current_predict_next("session-a", first));
        assert!(state.is_current_predict_next("session-a", second));
    }

    #[test]
    fn predict_next_generations_are_scoped_to_session() {
        let state = shared_state();

        let first = state.begin_predict_next("session-a");
        let other = state.begin_predict_next("session-b");

        assert!(state.is_current_predict_next("session-a", first));
        assert!(state.is_current_predict_next("session-b", other));
    }

    #[test]
    fn predict_next_generation_can_be_cleared() {
        let state = shared_state();

        let generation = state.begin_predict_next("session-a");
        state.clear_predict_next("session-a");

        assert!(!state.is_current_predict_next("session-a", generation));
    }
}
