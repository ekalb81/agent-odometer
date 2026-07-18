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

/// Rate card shipped with the binary or customized by the user.
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
    /// Per-harness currency labels (e.g. codex -> "credits", claude_code -> "USD").
    /// Falls back to `currency` when a harness is absent.
    #[serde(default)]
    pub currencies: HashMap<String, String>,
    /// Per-harness fallback models, so an unknown Claude model doesn't fall
    /// back to a Codex credit rate. Falls back to `fallback_model` when absent.
    #[serde(default)]
    pub fallback_models: HashMap<String, String>,
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
                Ok(card) => {
                    let bundled = Self::load_bundled()?;
                    Ok(merge_older_override(card, bundled))
                }
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

/// Add models introduced by a newer bundled card without overwriting any
/// user-edited model, currency, unit, or fallback choices.
fn merge_older_override(mut disk: RateCard, bundled: RateCard) -> RateCard {
    if disk.version >= bundled.version {
        return disk;
    }
    for (model, rate) in bundled.models {
        disk.models.entry(model).or_insert(rate);
    }
    for (harness, currency) in bundled.currencies {
        disk.currencies.entry(harness).or_insert(currency);
    }
    for (harness, model) in bundled.fallback_models {
        disk.fallback_models.entry(harness).or_insert(model);
    }
    disk.version = bundled.version;
    disk.source_url = bundled.source_url;
    disk.fetched_at = bundled.fetched_at;
    disk
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(value: f64) -> ModelRate {
        ModelRate {
            input: value,
            cached_input: value,
            output: value,
            reasoning: value,
        }
    }

    #[test]
    fn older_override_inherits_new_models_but_preserves_edits() {
        let disk = RateCard {
            version: 2,
            currency: "custom".into(),
            unit: "per_1m_tokens".into(),
            source_url: "old".into(),
            fetched_at: Some("old".into()),
            models: HashMap::from([("gpt-old".into(), rate(99.0))]),
            fallback_model: "gpt-old".into(),
            currencies: HashMap::new(),
            fallback_models: HashMap::new(),
        };
        let bundled = RateCard {
            version: 3,
            currency: "credits".into(),
            unit: "per_1m_tokens".into(),
            source_url: "current".into(),
            fetched_at: Some("current".into()),
            models: HashMap::from([("gpt-old".into(), rate(1.0)), ("gpt-new".into(), rate(2.0))]),
            fallback_model: "gpt-new".into(),
            currencies: HashMap::from([("claude_code".into(), "USD".into())]),
            fallback_models: HashMap::from([("claude_code".into(), "claude-new".into())]),
        };

        let merged = merge_older_override(disk, bundled);
        assert_eq!(merged.version, 3);
        assert_eq!(merged.currency, "custom");
        assert_eq!(merged.fallback_model, "gpt-old");
        assert_eq!(merged.models["gpt-old"].input, 99.0);
        assert_eq!(merged.models["gpt-new"].input, 2.0);
        assert_eq!(merged.source_url, "current");
        // Per-harness maps introduced by a newer bundled card are inherited.
        assert_eq!(merged.currencies["claude_code"], "USD");
        assert_eq!(merged.fallback_models["claude_code"], "claude-new");
    }
}
