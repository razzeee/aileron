/// Shared mutable daemon state, behind a single `Arc<Mutex<…>>`.
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::assignments::Assignments;
use crate::config::Config;
use crate::container::ContainerPool;
use crate::permissions::PermissionStore;

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub app_id: String,
    pub use_case: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

pub struct Inner {
    pub config: Config,
    pub permissions: PermissionStore,
    pub assignments: Assignments,
    pub containers: ContainerPool,
    pub sessions: HashMap<String, Session>,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub async fn load(config: Config) -> anyhow::Result<Self> {
        let permissions = PermissionStore::load()?;
        let assignments = Assignments::load()?;
        let mut containers = ContainerPool::new();
        containers.idle_timeout_secs = config.idle_timeout_secs;
        Ok(Self(Arc::new(Mutex::new(Inner {
            config,
            permissions,
            assignments,
            containers,
            sessions: HashMap::new(),
        }))))
    }
}
