use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub session_roots: Vec<PathBuf>,
    pub archive_roots: Vec<PathBuf>,
    #[serde(default = "default_session_index_path")]
    pub session_index_path: PathBuf,
}

fn default_session_index_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex/session_index.jsonl")
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            session_roots: vec![home.join(".codex/sessions")],
            archive_roots: vec![home.join(".codex/archived_sessions")],
            session_index_path: home.join(".codex/session_index.jsonl"),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("codex-data-viewer").join("config.json"))
}

impl Config {
    /// Loads config from `<config_dir>/codex-data-viewer/config.json`.
    /// If the file doesn't exist, writes and returns the default. If it is
    /// malformed, warns and returns the default.
    pub fn load() -> anyhow::Result<Self> {
        let path = match config_path() {
            Some(p) => p,
            None => {
                tracing::warn!("could not determine config directory; using defaults");
                return Ok(Self::default());
            }
        };

        if !path.exists() {
            let cfg = Self::default();
            cfg.save().unwrap_or_else(|e| {
                tracing::warn!("could not write initial config: {}", e);
            });
            return Ok(cfg);
        }

        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<Self>(&raw) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                tracing::warn!("malformed config at {:?}: {}; using defaults", path, e);
                Ok(Self::default())
            }
        }
    }

    /// Persists config to `<config_dir>/codex-data-viewer/config.json`.
    /// Uses a `.tmp` → rename dance for an atomic-ish write.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path()
            .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_config() {
        let dir = tempdir().unwrap();
        // Override config_path by writing/reading directly via serde to simulate the logic.
        let cfg = Config {
            session_roots: vec![dir.path().join("sessions")],
            archive_roots: vec![dir.path().join("archived")],
            session_index_path: dir.path().join("session_index.jsonl"),
        };

        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, &json).unwrap();

        let loaded: Config = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.session_roots, cfg.session_roots);
        assert_eq!(loaded.archive_roots, cfg.archive_roots);
    }

    #[test]
    fn legacy_config_without_session_index_path_loads_with_default() {
        // Pre-existing on-disk configs from before this field was added must still parse.
        let raw = r#"{"session_roots":["/x"],"archive_roots":["/y"]}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.session_roots, vec![PathBuf::from("/x")]);
        assert_eq!(cfg.archive_roots, vec![PathBuf::from("/y")]);
        // session_index_path should fall back to the home-dir default, never empty.
        assert!(cfg.session_index_path.ends_with("session_index.jsonl"));
    }

    #[test]
    fn malformed_config_falls_back_to_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"not valid json {{{{").unwrap();

        // Directly test the fallback branch.
        let raw = std::fs::read_to_string(&path).unwrap();
        let result = serde_json::from_str::<Config>(&raw);
        assert!(result.is_err(), "malformed JSON should fail to parse");

        let cfg = result.unwrap_or_else(|_| Config::default());
        // Falls back to default — session_roots should contain the .codex/sessions path.
        assert!(!cfg.session_roots.is_empty());
    }
}
