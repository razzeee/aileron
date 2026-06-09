/// Shared mutable daemon state, behind a single `Arc<Mutex<…>>`.
///
/// All Varlink handler methods receive a clone of this handle.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::assignments::Assignments;
use crate::container::ContainerPool;
use crate::permissions::PermissionStore;

/// A live inference session created via `Inference.CreateSession`.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub app_id: String,
    pub use_case: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

pub struct Inner {
    pub permissions: PermissionStore,
    pub assignments: Assignments,
    pub containers: ContainerPool,
    pub sessions: HashMap<String, Session>,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Mutex<Inner>>);

impl SharedState {
    pub async fn load() -> anyhow::Result<Self> {
        let permissions = PermissionStore::load()?;
        let assignments = Assignments::load()?;
        let containers = ContainerPool::new();
        Ok(Self(Arc::new(Mutex::new(Inner {
            permissions,
            assignments,
            containers,
            sessions: HashMap::new(),
        }))))
    }
}
