use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::provider::{
    claude_code_provider_id, codex_provider_id, ProviderId, ProviderRegistry, ProviderSource,
    ProviderSourceKind, ProviderSourceSet,
};

pub const DEFENDER_EXCLUSION_RECEIPT_VERSION: u32 = 1;

/// Local evidence that Odometer verified the configured session roots as
/// effective Defender exclusions at a point in time. Windows or device policy
/// may change the effective exclusions later, so this is intentionally a
/// receipt rather than a continuously authoritative status flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefenderExclusionReceipt {
    pub version: u32,
    pub configured_roots: Vec<PathBuf>,
    pub verified_roots: Vec<PathBuf>,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRoot {
    pub path: PathBuf,
    #[serde(default)]
    pub recursive: bool,
}

/// Current layout version of the persisted config file. Version 1 makes the
/// provider-keyed `providers` map authoritative; the legacy flat root fields
/// are kept as a mirror of the two builtin providers so downgraded builds
/// still read the same roots.
pub const CONFIG_VERSION: u32 = 1;

/// Session sources for one provider. From `CONFIG_VERSION` 1 this is the
/// authoritative shape; unknown provider ids are preserved verbatim so a
/// config written by a newer build survives round-trips through this one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSourceConfig {
    #[serde(default)]
    pub live_roots: Vec<PathBuf>,
    #[serde(default)]
    pub archive_roots: Vec<PathBuf>,
    /// Provider-maintained session index, when the provider has one.
    #[serde(default)]
    pub session_index_path: Option<PathBuf>,
    /// Fields written by a newer schema. Preserved verbatim so saving from
    /// this build never strips them.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 0 (absent) = legacy flat-field layout; `CONFIG_VERSION` = `providers`
    /// is authoritative and the flat fields mirror its builtin entries.
    #[serde(default)]
    pub config_version: u32,
    /// Provider-keyed session sources, authoritative from `CONFIG_VERSION` 1.
    #[serde(default)]
    pub providers: BTreeMap<ProviderId, ProviderSourceConfig>,
    pub session_roots: Vec<PathBuf>,
    pub archive_roots: Vec<PathBuf>,
    #[serde(default = "default_session_index_path")]
    pub session_index_path: PathBuf,
    /// Roots containing Claude Code session JSONL files (~/.claude/projects).
    #[serde(default = "default_claude_session_roots")]
    pub claude_session_roots: Vec<PathBuf>,
    /// Backend-owned last-verification receipt for the opt-in Defender action.
    /// Ordinary config payloads preserve this field rather than overwriting it.
    #[serde(default)]
    pub defender_exclusion_receipt: Option<DefenderExclusionReceipt>,
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
    /// Top-level fields written by a newer schema. Preserved verbatim so a
    /// round-trip through this build never strips them.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
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
            config_version: 0,
            providers: BTreeMap::new(),
            session_roots: vec![codex_home.join("sessions")],
            archive_roots: vec![codex_home.join("archived_sessions")],
            session_index_path: codex_home.join("session_index.jsonl"),
            claude_session_roots: default_claude_session_roots(),
            defender_exclusion_receipt: None,
            performance_tracking_enabled: false,
            performance_log_max_mb: default_performance_log_max_mb(),
            instructions_enabled: false,
            instructions_tab_visible: true,
            instruction_roots: Vec::new(),
            turn_receipts_enabled: false,
            turn_receipts_codex: true,
            turn_receipts_claude: true,
            extra: BTreeMap::new(),
        }
        // Deliberately NOT normalized: struct-update construction
        // (`Config { custom_roots, ..Config::default() }`) must stay in the
        // legacy shape so a later normalization projects the custom flat
        // fields instead of overwriting them from the default's map.
    }
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("config.json"))
}

impl Config {
    /// Reconciles the provider map and the legacy flat mirror. Legacy
    /// payloads (version 0 — including everything the current frontend
    /// sends) project their flat fields into the map; versioned payloads
    /// rebuild the mirror from the map's builtin entries. Non-builtin map
    /// entries are never touched, so configs written by newer builds
    /// survive round-trips through set_config unchanged.
    pub fn normalized(mut self) -> Self {
        if self.config_version > CONFIG_VERSION {
            // Written by a newer build. Unknown fields are preserved via the
            // flatten catch-alls and the version marker is kept as-is so the
            // file is never falsely relabeled as this schema; the builtin
            // mirror is still refreshed from the map so this build works.
            tracing::warn!(
                "config was written by a newer schema (version {}); preserving its fields verbatim",
                self.config_version
            );
        }
        if self.config_version < CONFIG_VERSION {
            let codex = self.providers.entry(codex_provider_id()).or_default();
            codex.live_roots = self.session_roots.clone();
            codex.archive_roots = self.archive_roots.clone();
            codex.session_index_path = Some(self.session_index_path.clone());
            let claude = self.providers.entry(claude_code_provider_id()).or_default();
            claude.live_roots = self.claude_session_roots.clone();
            claude.archive_roots = Vec::new();
            self.config_version = CONFIG_VERSION;
        } else {
            let codex = self.providers.get(&codex_provider_id());
            self.session_roots = codex.map(|c| c.live_roots.clone()).unwrap_or_default();
            self.archive_roots = codex.map(|c| c.archive_roots.clone()).unwrap_or_default();
            self.session_index_path = codex
                .and_then(|c| c.session_index_path.clone())
                .unwrap_or_else(default_session_index_path);
            self.claude_session_roots = self
                .providers
                .get(&claude_code_provider_id())
                .map(|c| c.live_roots.clone())
                .unwrap_or_default();
        }
        self
    }

    /// Builds the validated source set from the provider map. Entries for
    /// providers with no registered adapter are skipped with a warning
    /// rather than failing the whole set: they stay dormant (and visible in
    /// the config file) until a build that knows the provider loads them.
    pub fn provider_sources(&self) -> anyhow::Result<ProviderSourceSet> {
        let registry = ProviderRegistry::builtin();
        let sources = self
            .clone()
            .normalized()
            .providers
            .into_iter()
            .filter(|(id, _)| {
                let known = registry.adapter(id).is_some();
                if !known {
                    tracing::warn!(
                        "config has session sources for unknown provider '{id}'; leaving them dormant"
                    );
                }
                known
            })
            .flat_map(|(id, provider_config)| {
                let archived_id = id.clone();
                let archived = provider_config
                    .archive_roots
                    .into_iter()
                    .map(move |root| {
                        ProviderSource::new(archived_id.clone(), root, ProviderSourceKind::Archived)
                    });
                provider_config
                    .live_roots
                    .into_iter()
                    .map(move |root| {
                        ProviderSource::new(id.clone(), root, ProviderSourceKind::Live)
                    })
                    .chain(archived)
            });
        ProviderSourceSet::try_new(sources)
    }

    pub fn session_sources_equal(&self, other: &Self) -> bool {
        let this = self.clone().normalized();
        let other = other.clone().normalized();
        this.providers == other.providers
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
            Ok(cfg) => Ok(cfg.normalized()),
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

        // Persist normalized so the versioned map and the downgrade mirror
        // can never drift on disk, whatever shape the caller assembled.
        let json = serde_json::to_string_pretty(&self.clone().normalized())?;
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
            config_version: 0,
            providers: BTreeMap::new(),
            extra: BTreeMap::new(),
            session_roots: vec![dir.path().join("sessions")],
            archive_roots: vec![dir.path().join("archived")],
            session_index_path: dir.path().join("session_index.jsonl"),
            claude_session_roots: vec![dir.path().join("claude-projects")],
            defender_exclusion_receipt: Some(DefenderExclusionReceipt {
                version: DEFENDER_EXCLUSION_RECEIPT_VERSION,
                configured_roots: vec![dir.path().join("sessions")],
                verified_roots: vec![dir.path().join("sessions")],
                verified_at: DateTime::parse_from_rfc3339("2026-07-29T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            }),
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
        assert_eq!(
            loaded.defender_exclusion_receipt,
            cfg.defender_exclusion_receipt
        );
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
        assert!(cfg.defender_exclusion_receipt.is_none());
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
    fn legacy_payload_projects_into_versioned_provider_map() {
        let raw = r#"{"session_roots":["/codex-live"],"archive_roots":["/codex-arch"],"claude_session_roots":["/claude-live"]}"#;
        let cfg = serde_json::from_str::<Config>(raw).unwrap().normalized();
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        let codex = &cfg.providers[&codex_provider_id()];
        assert_eq!(codex.live_roots, vec![PathBuf::from("/codex-live")]);
        assert_eq!(codex.archive_roots, vec![PathBuf::from("/codex-arch")]);
        assert!(codex.session_index_path.is_some());
        let claude = &cfg.providers[&claude_code_provider_id()];
        assert_eq!(claude.live_roots, vec![PathBuf::from("/claude-live")]);
    }

    #[test]
    fn versioned_payload_rebuilds_mirror_and_preserves_unknown_providers() {
        let raw = r#"{
            "config_version": 1,
            "providers": {
                "codex": {"live_roots": ["/map-live"], "archive_roots": ["/map-arch"]},
                "claude_code": {"live_roots": ["/map-claude"]},
                "cursor": {"live_roots": ["/cursor-db"]}
            },
            "session_roots": ["/stale-mirror"],
            "archive_roots": []
        }"#;
        let cfg = serde_json::from_str::<Config>(raw).unwrap().normalized();
        // The map is authoritative: the stale flat mirror is rebuilt from it.
        assert_eq!(cfg.session_roots, vec![PathBuf::from("/map-live")]);
        assert_eq!(cfg.archive_roots, vec![PathBuf::from("/map-arch")]);
        assert_eq!(cfg.claude_session_roots, vec![PathBuf::from("/map-claude")]);
        // Unknown providers survive verbatim and stay out of the source set.
        let cursor = ProviderId::new("cursor").unwrap();
        assert_eq!(
            cfg.providers[&cursor].live_roots,
            vec![PathBuf::from("/cursor-db")]
        );
        let sources = cfg.provider_sources().unwrap();
        assert!(sources.iter().all(|s| s.provider_id() != &cursor));
        assert_eq!(sources.len(), 3);
    }

    #[test]
    fn newer_schema_round_trips_without_stripping_fields() {
        let raw = r#"{
            "config_version": 2,
            "providers": {
                "codex": {"live_roots": ["/live"], "quota_source": "api"}
            },
            "session_roots": [],
            "archive_roots": [],
            "team_sync": {"enabled": true}
        }"#;
        let cfg = serde_json::from_str::<Config>(raw).unwrap().normalized();
        // Never relabeled as the current schema.
        assert_eq!(cfg.config_version, 2);
        // The builtin mirror still works for this build.
        assert_eq!(cfg.session_roots, vec![PathBuf::from("/live")]);
        // Unknown top-level and per-provider fields survive a save cycle.
        let rewritten = serde_json::to_value(cfg.clone().normalized()).unwrap();
        assert_eq!(rewritten["team_sync"]["enabled"], serde_json::json!(true));
        assert_eq!(
            rewritten["providers"]["codex"]["quota_source"],
            serde_json::json!("api")
        );
        assert_eq!(rewritten["config_version"], serde_json::json!(2));
    }

    #[test]
    fn session_sources_equal_compares_the_authoritative_map() {
        let base = Config {
            session_roots: vec![PathBuf::from("/a")],
            archive_roots: Vec::new(),
            claude_session_roots: Vec::new(),
            ..Config::default()
        };
        let same_after_normalization = base.clone().normalized();
        assert!(base.session_sources_equal(&same_after_normalization));
        let mut different = base.clone();
        different.session_roots = vec![PathBuf::from("/b")];
        assert!(!base.session_sources_equal(&different));
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
        // Order-insensitive: the map-driven build iterates providers
        // alphabetically, and source-set semantics are order-independent
        // (ownership validation makes overlaps unambiguous).
        let mut projected: Vec<_> = sources
            .iter()
            .map(|source| {
                (
                    source.provider_id().as_str(),
                    source.root().to_path_buf(),
                    source.kind(),
                )
            })
            .collect();
        projected.sort();

        let mut expected = vec![
            (
                "codex",
                dir.path().join("codex-live"),
                ProviderSourceKind::Live,
            ),
            (
                "codex",
                dir.path().join("codex-archive"),
                ProviderSourceKind::Archived,
            ),
            (
                "claude_code",
                dir.path().join("claude-live"),
                ProviderSourceKind::Live,
            ),
        ];
        expected.sort();
        assert_eq!(projected, expected);
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
    fn defender_receipt_changes_do_not_change_session_sources() {
        let first = Config::default();
        let mut second = first.clone();
        second.defender_exclusion_receipt = Some(DefenderExclusionReceipt {
            version: DEFENDER_EXCLUSION_RECEIPT_VERSION,
            configured_roots: first.session_roots.clone(),
            verified_roots: first.session_roots.clone(),
            verified_at: DateTime::parse_from_rfc3339("2026-07-29T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        });
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
