/// Shared mutable daemon state, behind a single `Arc<Mutex<…>>`.
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::assignments::Assignments;
use crate::config::Config;
use crate::container::ContainerPool;
use crate::hardware::Variant;
use crate::manifests::RuntimeManifestStore;
use crate::permissions::PermissionStore;
use crate::profiles::ProfileStore;

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
    pub runtimes: RuntimeManifestStore,
    pub containers: ContainerPool,
    pub sessions: HashMap<String, Session>,
    pub installing_profiles: HashMap<String, InstallRecord>,
    pub recent_installs: VecDeque<(String, InstallRecord)>,
    /// Best available hardware variant, detected once at startup.
    pub variant: Variant,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub async fn load(config: Config) -> anyhow::Result<Self> {
        let permissions = PermissionStore::load()?;
        let assignments = Assignments::load()?;
        let profiles = ProfileStore::load()?;
        let runtimes = RuntimeManifestStore::load()?;
        let mut containers = ContainerPool::new();
        containers.idle_timeout_secs = config.idle_timeout_secs;
        containers.memory_limit = config.container_memory.clone();
        let variant = crate::hardware::detect();
        Ok(Self(Arc::new(Mutex::new(Inner {
            config,
            permissions,
            assignments,
            profiles,
            runtimes,
            containers,
            sessions: HashMap::new(),
            installing_profiles: HashMap::new(),
            recent_installs: VecDeque::new(),
            variant,
        }))))
    }
}
