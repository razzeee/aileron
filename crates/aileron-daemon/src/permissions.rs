/// Per-app, per-use-case permission records.
///
/// Persisted at `$XDG_DATA_HOME/aileron/permissions.json`.
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The rename is visible, but its crash durability is unknown. Callers must
/// apply the new state (including revocation) without acknowledging success.
#[derive(Debug, thiserror::Error)]
#[error("permission rename is visible but directory sync failed: {0}")]
pub(crate) struct UncertainCommit(#[source] std::io::Error);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionEntry {
    pub allowed: bool,
    pub last_used: Option<String>,
}

/// Key = "<app_id>/<use_case>"
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionStore(pub HashMap<String, PermissionEntry>);

impl PermissionStore {
    pub(crate) fn path() -> PathBuf {
        let data_home = std::env::var("AILERON_DATA_HOME")
            .or_else(|_| std::env::var("XDG_DATA_HOME"))
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                format!("{}/.local/share", home)
            });
        PathBuf::from(data_home)
            .join("aileron")
            .join("permissions.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to_path(&Self::path())
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        self.save_to_path_with_sync(path, std::fs::File::sync_all)
    }

    fn save_to_path_with_sync(
        &self,
        path: &Path,
        sync_directory: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    ) -> Result<()> {
        let data = serde_json::to_vec_pretty(self)?;
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        let directory = std::fs::File::open(parent)?;
        let temporary = parent.join(format!(".permissions-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            sync_directory(&directory).map_err(UncertainCommit)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn key(app_id: &str, use_case: &str) -> String {
        format!("{}/{}", app_id, use_case)
    }

    /// Check if permission is explicitly granted.
    /// Returns `None` if no entry exists (i.e., permission prompt needed).
    pub fn check(&self, app_id: &str, use_case: &str) -> Option<bool> {
        self.0.get(&Self::key(app_id, use_case)).map(|e| e.allowed)
    }

    /// Record a denied permission entry when an app asks for a new use-case.
    /// This lets management UIs show first-use denials without granting access.
    pub fn deny_if_missing(&mut self, app_id: &str, use_case: &str) -> Result<()> {
        self.deny_if_missing_with_save(app_id, use_case, Self::save)
    }

    fn deny_if_missing_with_save(
        &mut self,
        app_id: &str,
        use_case: &str,
        save: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        let mut next = self.clone();
        if next.insert_denied_if_missing(app_id, use_case) {
            return self.commit_next(next, save);
        }
        Ok(())
    }

    fn insert_denied_if_missing(&mut self, app_id: &str, use_case: &str) -> bool {
        let key = Self::key(app_id, use_case);
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(
            key,
            PermissionEntry {
                allowed: false,
                last_used: None,
            },
        );
        true
    }

    /// Set a permission entry and persist.
    pub fn set(&mut self, app_id: String, use_case: String, allowed: bool) -> Result<()> {
        self.set_to_path(&app_id, &use_case, allowed, &Self::path())
    }

    pub(crate) fn set_to_path(
        &mut self,
        app_id: &str,
        use_case: &str,
        allowed: bool,
        path: &Path,
    ) -> Result<()> {
        self.set_to_path_with_sync(app_id, use_case, allowed, path, std::fs::File::sync_all)
    }

    pub(crate) fn set_to_path_with_sync(
        &mut self,
        app_id: &str,
        use_case: &str,
        allowed: bool,
        path: &Path,
        sync_directory: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    ) -> Result<()> {
        let mut next = self.clone();
        let key = Self::key(app_id, use_case);
        let entry = next.0.entry(key).or_insert(PermissionEntry {
            allowed,
            last_used: None,
        });
        entry.allowed = allowed;
        self.commit_next(next, |next| {
            next.save_to_path_with_sync(path, sync_directory)
        })
    }

    fn commit_next(&mut self, next: Self, save: impl FnOnce(&Self) -> Result<()>) -> Result<()> {
        let result = save(&next);
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|err| err.is::<UncertainCommit>())
        {
            *self = next;
        }
        result
    }

    /// Touch last-used timestamp for an entry.
    pub fn touch(&mut self, app_id: &str, use_case: &str) -> Result<()> {
        self.touch_with_save(app_id, use_case, Self::save)
    }

    fn touch_with_save(
        &mut self,
        app_id: &str,
        use_case: &str,
        save: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        let key = Self::key(app_id, use_case);
        let mut next = self.clone();
        if let Some(entry) = next.0.get_mut(&key) {
            entry.last_used = Some(chrono::Utc::now().to_rfc3339());
            return self.commit_next(next, save);
        }
        Ok(())
    }

    /// List all entries as a flat vec of (app_id, use_case, entry) tuples.
    pub fn list(&self) -> Vec<(String, String, &PermissionEntry)> {
        self.0
            .iter()
            .filter_map(|(k, v)| {
                let mut parts = k.splitn(2, '/');
                let app_id = parts.next()?.to_string();
                let use_case = parts.next()?.to_string();
                Some((app_id, use_case, v))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn permission_save_atomically_replaces_existing_file() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let mut store = PermissionStore::default();
        store.set_to_path("app", "case", true, &path).unwrap();
        let original = std::fs::File::open(&path).unwrap();
        store.0.get_mut("app/case").unwrap().last_used = Some("timestamp".into());
        store.set_to_path("app", "case", false, &path).unwrap();

        let saved: PermissionStore =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.check("app", "case"), Some(false));
        assert_eq!(saved.0["app/case"].last_used.as_deref(), Some("timestamp"));
        assert_ne!(
            original.metadata().unwrap().ino(),
            path.metadata().unwrap().ino()
        );
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn uncertain_permission_commit_updates_memory_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        for allowed in [false, true] {
            let mut store = PermissionStore::default();
            store.set_to_path("app", "case", !allowed, &path).unwrap();
            let error = store
                .set_to_path_with_sync("app", "case", allowed, &path, |directory| {
                    assert!(directory.metadata()?.is_dir());
                    let saved: PermissionStore =
                        serde_json::from_slice(&std::fs::read(&path)?).unwrap();
                    assert_eq!(saved.check("app", "case"), Some(allowed));
                    Err(std::io::Error::other("injected directory sync failure"))
                })
                .unwrap_err();

            assert!(error.is::<UncertainCommit>());
            assert_eq!(store.check("app", "case"), Some(allowed));
            assert_eq!(
                std::fs::read(&path).unwrap(),
                serde_json::to_vec_pretty(&store).unwrap()
            );
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn uncertain_first_denial_and_touch_keep_memory_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        let mut store = PermissionStore::default();
        let save = |next: &PermissionStore| {
            next.save_to_path_with_sync(&path, |_| {
                Err(std::io::Error::other("injected directory sync failure"))
            })
        };

        let error = store
            .deny_if_missing_with_save("app", "case", save)
            .unwrap_err();
        assert!(error.is::<UncertainCommit>());
        assert_eq!(store.check("app", "case"), Some(false));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            serde_json::to_vec_pretty(&store).unwrap()
        );

        let error = store.touch_with_save("app", "case", save).unwrap_err();
        assert!(error.is::<UncertainCommit>());
        assert!(store.0["app/case"].last_used.is_some());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            serde_json::to_vec_pretty(&store).unwrap()
        );
    }

    #[test]
    fn failed_permission_write_preserves_memory_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        for previous in [None, Some(false), Some(true)] {
            for allowed in [false, true] {
                let mut store = PermissionStore::default();
                if let Some(previous) = previous {
                    store.0.insert(
                        "app/case".into(),
                        PermissionEntry {
                            allowed: previous,
                            last_used: Some("timestamp".into()),
                        },
                    );
                }
                store.save_to_path(&path).unwrap();
                let before = std::fs::read(&path).unwrap();
                // A file cannot be the parent directory, even when running as root.
                assert!(
                    store
                        .set_to_path("app", "case", allowed, &path.join("blocked"))
                        .is_err()
                );
                assert_eq!(serde_json::to_vec_pretty(&store).unwrap(), before);
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
        }
    }

    #[test]
    fn failed_permission_rename_cleans_up_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("permissions.json");
        std::fs::create_dir(&path).unwrap();
        let mut store = PermissionStore::default();

        assert!(store.set_to_path("app", "case", true, &path).is_err());

        assert_eq!(store.check("app", "case"), None);
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn denied_first_use_is_listed_for_later_approval() {
        let mut store = PermissionStore::default();

        assert!(store.insert_denied_if_missing("org.aileron.Demo", "language.extract"));

        assert_eq!(
            store.check("org.aileron.Demo", "language.extract"),
            Some(false)
        );
    }

    #[hegel::test]
    fn denied_first_use_round_trips_generated_permission_key(tc: TestCase) {
        let app_id = tc.draw(gs::sampled_from(vec![
            "org.aileron.Demo".to_string(),
            "org.example.App".to_string(),
            "dev.test.Client".to_string(),
        ]));
        let use_case = tc.draw(gs::sampled_from(vec![
            "language.extract".to_string(),
            "speech.transcribe".to_string(),
            "vision.describe".to_string(),
        ]));
        let mut store = PermissionStore::default();

        assert!(store.insert_denied_if_missing(&app_id, &use_case));

        assert_eq!(store.check(&app_id, &use_case), Some(false));
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].0, app_id);
        assert_eq!(store.list()[0].1, use_case);
    }

    #[test]
    fn denied_first_use_does_not_override_existing_grant() {
        let mut store = PermissionStore::default();
        store.0.insert(
            PermissionStore::key("org.aileron.Demo", "language.extract"),
            PermissionEntry {
                allowed: true,
                last_used: None,
            },
        );

        assert!(!store.insert_denied_if_missing("org.aileron.Demo", "language.extract"));

        assert_eq!(
            store.check("org.aileron.Demo", "language.extract"),
            Some(true)
        );
    }

    #[hegel::test]
    fn denied_first_use_preserves_generated_existing_decision(tc: TestCase) {
        let allowed = tc.draw(gs::booleans());
        let mut store = PermissionStore::default();
        store.0.insert(
            PermissionStore::key("org.aileron.Demo", "language.extract"),
            PermissionEntry {
                allowed,
                last_used: None,
            },
        );

        assert!(!store.insert_denied_if_missing("org.aileron.Demo", "language.extract"));

        assert_eq!(
            store.check("org.aileron.Demo", "language.extract"),
            Some(allowed)
        );
    }
}
