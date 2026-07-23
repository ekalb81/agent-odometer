use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub session_roots: Vec<PathBuf>,
    pub archive_roots: Vec<PathBuf>,
    #[serde(default = "default_session_index_path")]
    pub session_index_path: PathBuf,
    /// Roots containing Claude Code session JSONL files (~/.claude/projects).
    #[serde(default = "default_claude_session_roots")]
    pub claude_session_roots: Vec<PathBuf>,
    /// Local app performance measurements. Disabled unless explicitly enabled.
    #[serde(default)]
    pub performance_tracking_enabled: bool,
    /// Per-segment limit; the recorder keeps the current and previous segment.
    #[serde(default = "default_performance_log_max_mb")]
    pub performance_log_max_mb: u64,
}

fn default_performance_log_max_mb() -> u64 {
    64
}

fn default_session_index_path() -> PathBuf {
    codex_home_dir().join("session_index.jsonl")
}

fn default_claude_session_roots() -> Vec<PathBuf> {
    vec![claude_config_dir().join("projects")]
}

fn claude_config_dir() -> PathBuf {
    let configured = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    resolve_claude_config_dir(configured, dirs::home_dir())
}

fn resolve_claude_config_dir(configured: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from(".")).join(".claude"))
}

fn codex_home_dir() -> PathBuf {
    let configured = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    resolve_codex_home(configured, dirs::home_dir())
}

fn resolve_codex_home(configured: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from(".")).join(".codex"))
}

impl Default for Config {
    fn default() -> Self {
        let codex_home = codex_home_dir();
        Self {
            session_roots: vec![codex_home.join("sessions")],
            archive_roots: vec![codex_home.join("archived_sessions")],
            session_index_path: codex_home.join("session_index.jsonl"),
            claude_session_roots: default_claude_session_roots(),
            performance_tracking_enabled: false,
            performance_log_max_mb: default_performance_log_max_mb(),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("config.json"))
}

impl Config {
    pub fn session_sources_equal(&self, other: &Self) -> bool {
        self.session_roots == other.session_roots
            && self.archive_roots == other.archive_roots
            && self.session_index_path == other.session_index_path
            && self.claude_session_roots == other.claude_session_roots
    }

    /// Loads config from `<config_dir>/agent-odometer/config.json`.
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

    /// Persists config to `<config_dir>/agent-odometer/config.json`.
    /// Uses a `.tmp` → rename dance for an atomic-ish write.
    pub fn save(&self) -> anyhow::Result<()> {
        let path =
            config_path().ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        if std::fs::rename(&tmp, &path).is_err() {
            // Windows cannot rename over an existing destination.
            std::fs::write(&path, json)?;
            let _ = std::fs::remove_file(tmp);
        }
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
            claude_session_roots: vec![dir.path().join("claude-projects")],
            performance_tracking_enabled: true,
            performance_log_max_mb: 32,
        };

        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, &json).unwrap();

        let loaded: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.session_roots, cfg.session_roots);
        assert_eq!(loaded.archive_roots, cfg.archive_roots);
        assert_eq!(loaded.claude_session_roots, cfg.claude_session_roots);
        assert!(loaded.performance_tracking_enabled);
        assert_eq!(loaded.performance_log_max_mb, 32);
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
        // claude_session_roots should fall back to <claude config dir>/projects.
        assert_eq!(cfg.claude_session_roots.len(), 1);
        assert!(cfg.claude_session_roots[0].ends_with("projects"));
        assert!(!cfg.performance_tracking_enabled);
        assert_eq!(cfg.performance_log_max_mb, 64);
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

    #[test]
    fn performance_changes_do_not_change_session_sources() {
        let first = Config::default();
        let mut second = first.clone();
        second.performance_tracking_enabled = true;
        second.performance_log_max_mb = 16;
        assert!(first.session_sources_equal(&second));
    }

    #[test]
    fn codex_home_override_takes_precedence() {
        let resolved = resolve_codex_home(
            Some(PathBuf::from("/custom/codex")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(resolved, PathBuf::from("/custom/codex"));
    }

    #[test]
    fn codex_home_defaults_below_user_home() {
        let resolved = resolve_codex_home(None, Some(PathBuf::from("/home/user")));
        assert_eq!(resolved, PathBuf::from("/home/user/.codex"));
    }

    #[test]
    fn claude_config_dir_override_takes_precedence() {
        let resolved = resolve_claude_config_dir(
            Some(PathBuf::from("/custom/claude")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(resolved, PathBuf::from("/custom/claude"));
    }

    #[test]
    fn claude_config_dir_defaults_below_user_home() {
        let resolved = resolve_claude_config_dir(None, Some(PathBuf::from("/home/user")));
        assert_eq!(resolved, PathBuf::from("/home/user/.claude"));
    }
}
