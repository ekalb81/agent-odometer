use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

impl RateCard {
    /// Loads the bundled rates.json. Unknown fields (e.g. _note) are silently ignored by serde.
    pub fn load_bundled() -> anyhow::Result<Self> {
        let raw = include_str!("../rates.json");
        let card: Self = serde_json::from_str(raw)?;
        Ok(card)
    }

    /// Persists an updated rate card. Phase 5 will implement writing to app-data directory.
    pub fn save(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
