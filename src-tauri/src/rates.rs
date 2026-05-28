use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRate {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
    /// Typically the same as output for reasoning models.
    pub reasoning: f64,
}

/// Rate card shipped with the binary. Phase 5 will fetch live values from source_url.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateCard {
    pub version: u32,
    /// Three-letter currency code, e.g. "USD".
    pub currency: String,
    /// Token unit denomination, e.g. "per_1m_tokens".
    pub unit: String,
    /// Canonical URL for the live rate schedule.
    pub source_url: String,
    /// ISO8601 timestamp of the last successful fetch; null when using placeholder values.
    pub fetched_at: Option<String>,
    pub models: HashMap<String, ModelRate>,
    /// Model key to use when a session's model is not found in `models`.
    pub fallback_model: String,
}

fn rates_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("codex-data-viewer").join("rates.json"))
}

impl RateCard {
    /// Loads the bundled rates.json. Unknown fields (e.g. _note) are silently ignored by serde.
    pub fn load_bundled() -> anyhow::Result<Self> {
        let raw = include_str!("../rates.json");
        let card: Self = serde_json::from_str(raw)?;
        Ok(card)
    }

    /// Loads rates from <config_dir>/codex-data-viewer/rates.json.
    /// If the file is missing, returns load_bundled (and does NOT seed the disk file —
    /// users can edit the editor to materialize their own copy).
    /// If the file is present but malformed, logs a warn and returns load_bundled.
    pub fn load_from_disk() -> anyhow::Result<Self> {
        let Some(path) = rates_path() else {
            return Self::load_bundled();
        };
        if !path.exists() {
            return Self::load_bundled();
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(card) => Ok(card),
                Err(e) => {
                    tracing::warn!(
                        "rates.json at {:?} is malformed ({}); falling back to bundled",
                        path,
                        e
                    );
                    Self::load_bundled()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "could not read rates.json at {:?} ({}); falling back to bundled",
                    path,
                    e
                );
                Self::load_bundled()
            }
        }
    }

    /// Atomic-ish write to <config_dir>/codex-data-viewer/rates.json.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = rates_path().ok_or_else(|| anyhow::anyhow!("could not determine config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file alongside the target, then rename for atomicity.
        let tmp = path.with_extension("json.tmp");
        let serialized = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &serialized)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
