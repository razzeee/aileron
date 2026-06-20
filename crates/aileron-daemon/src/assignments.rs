/// Use-case -> installed profile assignments.
///
/// Persisted at `$XDG_DATA_HOME/aileron/assignments.json`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assignments(pub HashMap<String, String>);

impl Assignments {
    fn path() -> PathBuf {
        let data_home = std::env::var("AILERON_DATA_HOME")
            .or_else(|_| std::env::var("XDG_DATA_HOME"))
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                format!("{}/.local/share", home)
            });
        PathBuf::from(data_home)
            .join("aileron")
            .join("assignments.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, serde_json::to_string_pretty(&self)?)?;
        Ok(())
    }

    /// Return the profile assigned to a use-case, if any.
    pub fn get(&self, use_case: &str) -> Option<&str> {
        self.0.get(use_case).map(|s| s.as_str())
    }

    /// Assign (or replace) the profile for a use-case.
    pub fn assign(&mut self, use_case: String, profile_id: String) -> Result<()> {
        self.assign_in_memory(use_case, profile_id);
        self.save()
    }

    fn assign_in_memory(&mut self, use_case: String, profile_id: String) {
        self.0.insert(use_case, profile_id);
    }

    /// Return all assignments.
    pub fn all(&self) -> &HashMap<String, String> {
        &self.0
    }

    /// Remove all assignments for a given profile.
    pub fn remove_profile(&mut self, profile_id: &str) -> Result<()> {
        self.remove_profile_in_memory(profile_id);
        self.save()
    }

    fn remove_profile_in_memory(&mut self, profile_id: &str) {
        self.0.retain(|_, v| v != profile_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn assign_replaces_generated_use_case_mapping(tc: TestCase) {
        let use_case = tc.draw(gs::sampled_from(vec![
            "language.summarize".to_string(),
            "speech.transcribe".to_string(),
            "vision.describe".to_string(),
        ]));
        let first = tc.draw(gs::sampled_from(vec![
            "profile-a".to_string(),
            "profile-b".to_string(),
        ]));
        let second = tc.draw(gs::sampled_from(vec![
            "profile-c".to_string(),
            "profile-d".to_string(),
        ]));
        let mut assignments = Assignments::default();

        assignments.assign_in_memory(use_case.clone(), first);
        assignments.assign_in_memory(use_case.clone(), second.clone());

        assert_eq!(assignments.get(&use_case), Some(second.as_str()));
        assert_eq!(assignments.all().len(), 1);
    }

    #[hegel::test]
    fn remove_profile_removes_only_generated_matching_profile(tc: TestCase) {
        let removed_profile = tc.draw(gs::sampled_from(vec![
            "profile-a".to_string(),
            "profile-b".to_string(),
        ]));
        let kept_profile = tc.draw(gs::sampled_from(vec![
            "profile-c".to_string(),
            "profile-d".to_string(),
        ]));
        let mut assignments = Assignments(HashMap::from([
            ("language.summarize".to_string(), removed_profile.clone()),
            ("speech.transcribe".to_string(), kept_profile.clone()),
            ("vision.describe".to_string(), removed_profile.clone()),
        ]));

        assignments.remove_profile_in_memory(&removed_profile);

        assert_eq!(
            assignments.get("speech.transcribe"),
            Some(kept_profile.as_str())
        );
        assert!(
            !assignments
                .all()
                .values()
                .any(|profile_id| profile_id == &removed_profile)
        );
    }

    #[test]
    fn save_and_load_round_trips_assignments_file() {
        let dir = test_dir("assignments-roundtrip");
        let path = dir.join("aileron").join("assignments.json");
        let assignments = Assignments(HashMap::from([(
            "language.summarize".to_string(),
            "profile-a".to_string(),
        )]));

        assignments.save_to_path(&path).expect("save assignments");
        let loaded = Assignments::load_from_path(&path).expect("load assignments");

        assert_eq!(loaded.get("language.summarize"), Some("profile-a"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aileron-{name}-{}", std::process::id()))
    }
}
