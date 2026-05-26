use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub session_roots: Vec<PathBuf>,
    pub archive_roots: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            session_roots: vec![home.join(".codex/sessions")],
            archive_roots: vec![home.join(".codex/archived_sessions")],
        }
    }
}

impl Config {
    /// Loads config from the Tauri app-data directory. Phase 3 will implement persistence.
    pub fn load() -> anyhow::Result<Self> {
        Ok(Self::default())
    }

    /// Persists config to the Tauri app-data directory. Phase 3 will implement persistence.
    pub fn save(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
