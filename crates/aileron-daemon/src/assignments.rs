/// Use-case → OCI image reference assignments.
///
/// Persisted at `$XDG_DATA_HOME/aileron/assignments.json`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assignments(pub HashMap<String, String>);

impl Assignments {
    fn path() -> PathBuf {
        let data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                format!("{}/.local/share", home)
            });
        PathBuf::from(data_home).join("aileron").join("assignments.json")
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
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_string_pretty(&self)?)?;
        Ok(())
    }

    /// Return the OCI image ref assigned to a use-case, if any.
    pub fn get(&self, use_case: &str) -> Option<&str> {
        self.0.get(use_case).map(|s| s.as_str())
    }

    /// Assign (or replace) the OCI image ref for a use-case.
    pub fn assign(&mut self, use_case: String, image_ref: String) -> Result<()> {
        self.0.insert(use_case, image_ref);
        self.save()
    }

    /// Return all assignments.
    pub fn all(&self) -> &HashMap<String, String> {
        &self.0
    }

    /// Remove all assignments for a given image ref (called on image delete).
    pub fn remove_image(&mut self, image_ref: &str) -> Result<()> {
        self.0.retain(|_, v| v != image_ref);
        self.save()
    }
}
