use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::provider::{
    claude_code_provider_id, codex_provider_id, ProviderSource, ProviderSourceKind,
    ProviderSourceSet,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRoot {
    pub path: PathBuf,
    #[serde(default)]
    pub recursive: bool,
}

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
    /// Enables the read-only instruction inventory and its bounded file reads.
    #[serde(default)]
    pub instructions_enabled: bool,
    /// Keeps discovery enabled while allowing the navigation tab to be hidden.
    #[serde(default = "default_true")]
    pub instructions_tab_visible: bool,
    /// User-selected project or project-container roots for instruction discovery.
    #[serde(default)]
    pub instruction_roots: Vec<InstructionRoot>,
    /// Show a compact cost/usage receipt after completed harness turns.
    /// Default-off: when false the helper exits without reading transcripts.
    #[serde(default)]
    pub turn_receipts_enabled: bool,
    /// Install the receipt hook for Codex when the feature is enabled.
    #[serde(default = "default_true")]
    pub turn_receipts_codex: bool,
    /// Install the receipt hook for Claude Code when the feature is enabled.
    #[serde(default = "default_true")]
    pub turn_receipts_claude: bool,
}

fn default_true() -> bool {
    true
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

pub(crate) fn claude_config_dir() -> PathBuf {
    let configured = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    resolve_claude_config_dir(configured, dirs::home_dir())
}

fn resolve_claude_config_dir(configured: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from(".")).join(".claude"))
}

pub(crate) fn codex_home_dir() -> PathBuf {
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
            instructions_enabled: false,
            instructions_tab_visible: true,
            instruction_roots: Vec::new(),
            turn_receipts_enabled: false,
            turn_receipts_codex: true,
            turn_receipts_claude: true,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("config.json"))
}

impl Config {
    /// Projects the legacy provider-specific root fields into the shared
    /// adapter source model without changing the persisted config contract.
    pub fn provider_sources(&self) -> anyhow::Result<ProviderSourceSet> {
        let codex = codex_provider_id();
        let claude_code = claude_code_provider_id();
        let sources =
            self.session_roots
                .iter()
                .cloned()
                .map(|root| ProviderSource::new(codex.clone(), root, ProviderSourceKind::Live))
                .chain(self.archive_roots.iter().cloned().map(|root| {
                    ProviderSource::new(codex.clone(), root, ProviderSourceKind::Archived)
                }))
                .chain(self.claude_session_roots.iter().cloned().map(|root| {
                    ProviderSource::new(claude_code.clone(), root, ProviderSourceKind::Live)
                }));
        ProviderSourceSet::try_new(sources)
    }

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
            instructions_enabled: true,
            instructions_tab_visible: false,
            instruction_roots: vec![InstructionRoot {
                path: dir.path().join("projects"),
                recursive: true,
            }],
            turn_receipts_enabled: true,
            turn_receipts_codex: true,
            turn_receipts_claude: false,
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
        assert!(loaded.instructions_enabled);
        assert!(!loaded.instructions_tab_visible);
        assert_eq!(loaded.instruction_roots, cfg.instruction_roots);
        assert!(loaded.turn_receipts_enabled);
        assert!(loaded.turn_receipts_codex);
        assert!(!loaded.turn_receipts_claude);
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
        assert!(!cfg.instructions_enabled);
        assert!(cfg.instructions_tab_visible);
        assert!(cfg.instruction_roots.is_empty());
        assert!(!cfg.turn_receipts_enabled);
        assert!(cfg.turn_receipts_codex);
        assert!(cfg.turn_receipts_claude);
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
    fn receipt_changes_do_not_change_session_sources() {
        let first = Config::default();
        let mut second = first.clone();
        second.turn_receipts_enabled = true;
        second.turn_receipts_claude = false;
        assert!(first.session_sources_equal(&second));
    }

    #[test]
    fn legacy_roots_project_to_provider_sources_without_changing_config() {
        let dir = tempdir().unwrap();
        let config = Config {
            session_roots: vec![dir.path().join("codex-live")],
            archive_roots: vec![dir.path().join("codex-archive")],
            claude_session_roots: vec![dir.path().join("claude-live")],
            ..Config::default()
        };
        let original_json = serde_json::to_value(&config).unwrap();

        let sources = config.provider_sources().unwrap();
        let projected: Vec<_> = sources
            .iter()
            .map(|source| {
                (
                    source.provider_id().as_str(),
                    source.root().to_path_buf(),
                    source.kind(),
                )
            })
            .collect();

        assert_eq!(
            projected,
            vec![
                (
                    "codex",
                    dir.path().join("codex-live"),
                    ProviderSourceKind::Live
                ),
                (
                    "codex",
                    dir.path().join("codex-archive"),
                    ProviderSourceKind::Archived
                ),
                (
                    "claude_code",
                    dir.path().join("claude-live"),
                    ProviderSourceKind::Live
                ),
            ]
        );
        assert_eq!(serde_json::to_value(&config).unwrap(), original_json);
    }

    #[test]
    fn ambiguous_legacy_roots_fail_closed() {
        let dir = tempdir().unwrap();
        let shared = dir.path().join("shared");
        let config = Config {
            session_roots: vec![shared.clone()],
            archive_roots: Vec::new(),
            claude_session_roots: vec![shared],
            ..Config::default()
        };

        let error = config.provider_sources().unwrap_err().to_string();
        assert!(error.contains("ambiguous ownership"));
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
