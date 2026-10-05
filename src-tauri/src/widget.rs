//! Opt-in compact local window. Reads existing cached observations and summary
//! pricing; it never starts a provider poller or reads transcript bodies.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tauri::{Emitter, Manager};

pub const WINDOW_LABEL: &str = "quota-widget";
const MAX_SUMMARIES: usize = 10_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetProvider {
    #[default]
    Codex,
    ClaudeCode,
    GeminiCli,
}
impl WidgetProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::GeminiCli => "gemini_cli",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetKind {
    #[default]
    Quota,
    Usage,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetPreferences {
    pub visible: bool,
    pub provider: WidgetProvider,
    pub kind: WidgetKind,
    pub always_on_top: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidgetSettings {
    version: u8,
    pub revision: u64,
    pub preferences: WidgetPreferences,
}
impl Default for WidgetSettings {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            preferences: WidgetPreferences::default(),
        }
    }
}
fn settings_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|p| p.join("agent-odometer/widget-v1.json"))
        .ok_or_else(|| "Widget settings are unavailable".into())
}
pub fn load() -> Result<WidgetSettings, String> {
    load_at(&settings_path()?)
}
fn load_at(path: &Path) -> Result<WidgetSettings, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WidgetSettings::default()),
        Err(_) => return Err("Widget settings are unreadable; existing file preserved".into()),
    };
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "Widget settings are unreadable")?;
    if bytes.len() > 4096 {
        return Err("Widget settings are too large; existing file preserved".into());
    }
    let settings: WidgetSettings = serde_json::from_slice(&bytes)
        .map_err(|_| "Widget settings are invalid; existing file preserved")?;
    if settings.version != 1 {
        return Err("Widget settings require another Odometer version".into());
    }
    Ok(settings)
}
struct SettingsLock(std::fs::File);
impl Drop for SettingsLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn save_at(
    path: &Path,
    revision: u64,
    preferences: WidgetPreferences,
) -> Result<WidgetSettings, String> {
    let parent = path.parent().ok_or("Widget settings are unavailable")?;
    std::fs::create_dir_all(parent).map_err(|_| "Widget settings could not be saved")?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|_| "Widget settings could not be locked")?;
    lock.try_lock()
        .map_err(|_| "Widget settings are changing in another app; retry")?;
    let _lock = SettingsLock(lock);
    let current = load_at(path)?;
    if current.revision != revision {
        return Err("Widget settings changed; reload before saving".into());
    }
    let next = WidgetSettings {
        version: 1,
        revision: revision.checked_add(1).ok_or("Widget revision exhausted")?,
        preferences,
    };
    let bytes = serde_json::to_vec(&next).map_err(|_| "Widget settings could not be saved")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Widget settings could not be saved")?;
    file.write_all(&bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| "Widget settings could not be saved")?;
    file.persist(path)
        .map_err(|_| "Widget settings could not be saved")?;
    Ok(next)
}
pub fn save(revision: u64, preferences: WidgetPreferences) -> Result<WidgetSettings, String> {
    save_at(&settings_path()?, revision, preferences)
}

#[derive(Debug, Serialize)]
pub struct WidgetWindow {
    kind: crate::quota::QuotaWindowKind,
    unit: crate::quota::QuotaUnit,
    used: Option<f64>,
    remaining: Option<f64>,
    unlimited: bool,
    observed_at: DateTime<Utc>,
    resets_at: Option<DateTime<Utc>>,
    stale: bool,
    unavailable: Option<crate::quota::QuotaUnavailableReason>,
}
#[derive(Debug, Serialize)]
pub struct WidgetQuota {
    provenance: crate::quota::QuotaProvenance,
    unavailable: Option<crate::quota::QuotaUnavailableReason>,
    windows: Vec<WidgetWindow>,
    windows_omitted: usize,
}
pub fn project_quota(snapshot: crate::quota::QuotaSnapshot, now: DateTime<Utc>) -> WidgetQuota {
    let windows_omitted = snapshot.windows.len().saturating_sub(8);
    WidgetQuota {
        provenance: snapshot.provenance,
        unavailable: snapshot.unavailable,
        windows_omitted,
        windows: snapshot
            .windows
            .into_iter()
            .take(8)
            .map(|w| WidgetWindow {
                kind: w.kind,
                unit: w.unit,
                used: w.used,
                remaining: w.remaining,
                unlimited: w.unlimited,
                observed_at: w.observed_at,
                resets_at: w.resets_at,
                stale: w.stale
                    || w.observed_at > now
                    || w.resets_at.is_some_and(|reset| reset <= now),
                unavailable: w.unavailable,
            })
            .collect(),
    }
}
#[derive(Debug, Serialize)]
pub struct WidgetUsage {
    session_count: usize,
    total_tokens: u64,
    latest_activity_at: Option<DateTime<Utc>>,
    plan_amount: Option<f64>,
    plan_currency: String,
    api_amount_usd: Option<f64>,
    estimate_partial: bool,
    scan_complete: bool,
}
pub fn project_usage<'a>(
    summaries: impl Iterator<Item = &'a crate::model::SessionSummary>,
    provider: WidgetProvider,
    rates: &crate::rates::RateCard,
    now: DateTime<Utc>,
    scan_complete: bool,
) -> Result<WidgetUsage, String> {
    let mut out = WidgetUsage {
        session_count: 0,
        total_tokens: 0,
        latest_activity_at: None,
        plan_amount: None,
        plan_currency: rates
            .currencies
            .get(provider.id())
            .cloned()
            .unwrap_or_else(|| {
                if provider == WidgetProvider::Codex {
                    "credits"
                } else {
                    "USD"
                }
                .into()
            }),
        api_amount_usd: None,
        estimate_partial: false,
        scan_complete,
    };
    let mut plan = 0.0;
    let mut api = 0.0;
    let mut any_plan = false;
    let mut any_api = false;
    for (visited, summary) in summaries.enumerate() {
        if visited >= 100_000 {
            return Err(
                "Local index is too large for this compact usage surface; use the main dashboard"
                    .into(),
            );
        }
        if summary.harness.as_str() != provider.id()
            || summary.source_availability != crate::model::SourceAvailability::Present
            || summary.lifecycle != crate::model::SessionLifecycle::Present
        {
            continue;
        }
        out.session_count += 1;
        if out.session_count > MAX_SUMMARIES {
            return Err(
                "Usage widget supports up to 10000 local source sessions; use the main dashboard"
                    .into(),
            );
        }
        out.total_tokens = out
            .total_tokens
            .checked_add(summary.tokens_total.total_tokens)
            .ok_or("Usage total is unavailable")?;
        out.latest_activity_at = Some(
            out.latest_activity_at
                .map_or(summary.last_event_at, |prior| {
                    prior.max(summary.last_event_at)
                }),
        );
        let price = crate::query::price_surfaces(&summary.buckets, provider.id(), rates, now);
        plan += price.plan.total;
        any_plan |= price.plan.by_model.iter().any(|m| !m.unpriced);
        out.estimate_partial |=
            !price.plan.missing_models.is_empty() || !price.plan.unpriced_models.is_empty();
        if let Some(surface) = price.api {
            api += surface.total;
            any_api |= surface.by_model.iter().any(|m| !m.unpriced);
            out.estimate_partial |=
                !surface.missing_models.is_empty() || !surface.unpriced_models.is_empty();
        }
    }
    if !plan.is_finite() || !api.is_finite() {
        return Err("Usage estimates are unavailable".into());
    }
    out.plan_amount = any_plan.then_some(plan);
    out.api_amount_usd = any_api.then_some(api);
    Ok(out)
}
#[derive(Debug, Serialize)]
pub struct WidgetSnapshot {
    pub settings: WidgetSettings,
    pub computed_at: DateTime<Utc>,
    pub quota: Option<WidgetQuota>,
    pub usage: Option<WidgetUsage>,
}
pub fn snapshot(
    state: &crate::store::AppState,
    settings: WidgetSettings,
    rates: &crate::rates::RateCard,
    now: DateTime<Utc>,
) -> Result<WidgetSnapshot, String> {
    let generation = state.sessions_generation();
    let mut out = WidgetSnapshot {
        settings,
        computed_at: now,
        quota: None,
        usage: None,
    };
    match out.settings.preferences.kind {
        WidgetKind::Quota => {
            let age = chrono::Duration::seconds(state.quota_store().max_cache_age_secs);
            let snapshot = state
                .quota_snapshots(age, now)
                .into_iter()
                .find(|s| s.provider.as_str() == out.settings.preferences.provider.id())
                .ok_or("Quota snapshot unavailable")?;
            out.quota = Some(project_quota(snapshot, now));
        }
        WidgetKind::Usage => {
            // Resident summaries only: no session bodies, archive query or source I/O.
            let entries: Vec<_> = state
                .sessions
                .iter()
                .take(100_001)
                .map(|entry| entry.value().clone())
                .collect();
            out.usage = Some(project_usage(
                entries.iter().map(|entry| &entry.summary),
                out.settings.preferences.provider,
                rates,
                now,
                state.scanned.load(Ordering::Acquire),
            )?);
        }
    }
    if generation != state.sessions_generation() {
        return Err("Local data changed; refresh the widget again".into());
    }
    Ok(out)
}

pub fn apply_window(app: &tauri::AppHandle, preferences: &WidgetPreferences) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window
            .set_always_on_top(preferences.always_on_top)
            .map_err(|_| "Widget window unavailable")?;
        if preferences.visible {
            window.show()
        } else {
            window.hide()
        }
        .map_err(|_| "Widget window unavailable")?;
        return Ok(());
    }
    if !preferences.visible {
        return Ok(());
    }
    let window = tauri::WebviewWindowBuilder::new(
        app,
        WINDOW_LABEL,
        tauri::WebviewUrl::App("index.html?surface=widget".into()),
    )
    .title("Odometer local widget")
    .inner_size(360.0, 440.0)
    .min_inner_size(300.0, 220.0)
    .max_inner_size(720.0, 900.0)
    .always_on_top(preferences.always_on_top)
    .build()
    .map_err(|_| "Widget window could not be opened")?;
    let handle = app.clone();
    let closing_window = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = closing_window.hide();
            let result = load().and_then(|current| {
                let mut preferences = current.preferences;
                preferences.visible = false;
                save(current.revision, preferences)
            });
            match result {
                Ok(settings) => {
                    let _ = handle.emit("widget-settings-updated", settings);
                }
                Err(_) => {
                    let _ = handle.emit(
                        "widget-settings-error",
                        "Visibility could not be saved; widget hidden for this run",
                    );
                }
            }
        }
    });
    Ok(())
}
pub fn restore_window(app: &tauri::AppHandle) {
    if let Ok(settings) = load() {
        if apply_window(app, &settings.preferences).is_err() {
            tracing::warn!("configured local widget could not be opened");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_projection_keeps_recorded_values_but_marks_expiry_and_clock_skew_stale() {
        use crate::quota::{
            QuotaConfidence, QuotaProvenance, QuotaSnapshot, QuotaUnit, QuotaWindow,
            QuotaWindowKind,
        };
        let now: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let window = QuotaWindow {
            kind: QuotaWindowKind::Burst,
            unit: QuotaUnit::Percent,
            window_minutes: None,
            used: Some(63.0),
            remaining: Some(37.0),
            limit: Some(100.0),
            unlimited: false,
            resets_at: Some(now),
            window_started_at: None,
            window_started_at_estimated: false,
            observed_at: now,
            confidence: QuotaConfidence::Medium,
            stale: false,
            unavailable: None,
            forecast: None,
        };
        let mut skew = window.clone();
        skew.resets_at = None;
        skew.observed_at = now + chrono::Duration::seconds(1);
        let mut unlimited = window.clone();
        unlimited.unlimited = true;
        unlimited.used = None;
        unlimited.remaining = None;
        let out = project_quota(
            QuotaSnapshot {
                provider: crate::provider::codex_provider_id(),
                provenance: QuotaProvenance::TranscriptDerived,
                unavailable: None,
                windows: [window, skew, unlimited]
                    .into_iter()
                    .cycle()
                    .take(10)
                    .collect(),
            },
            now,
        );
        assert_eq!(out.windows.len(), 8);
        assert_eq!(out.windows_omitted, 2);
        assert!(out.windows[0].stale);
        assert!(out.windows[1].stale);
        assert_eq!(out.windows[0].remaining, Some(37.0));
        assert!(out.windows[2].unlimited);
        assert_eq!(out.windows[2].remaining, None);
        let wire = serde_json::to_string(&out).unwrap();
        assert!(!wire.contains("forecast"));
        assert!(!wire.contains("provider"));
    }
    #[test]
    fn preferences_default_off_cas_restart_and_invalid_file_preservation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("widget.json");
        assert!(!load_at(&path).unwrap().preferences.visible);
        let enabled = WidgetPreferences {
            visible: true,
            provider: WidgetProvider::ClaudeCode,
            kind: WidgetKind::Usage,
            always_on_top: false,
        };
        let saved = save_at(&path, 0, enabled).unwrap();
        assert_eq!(load_at(&path).unwrap(), saved);
        assert!(save_at(&path, 0, WidgetPreferences::default())
            .unwrap_err()
            .contains("changed"));
        let raw = b"{invalid synthetic configuration}";
        std::fs::write(&path, raw).unwrap();
        assert!(save_at(&path, saved.revision, WidgetPreferences::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), raw);
    }
    #[test]
    fn usage_scope_and_prices_reuse_shared_projection_without_private_fields() {
        let now: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut parser = crate::parser::SessionParser::new("synthetic-private-path".into(), false);
        parser.apply_line(r#"{"type":"session_meta","timestamp":"2026-01-01T00:00:00Z","payload":{"id":"synthetic-private-id","timestamp":"2026-01-01T00:00:00Z"}}"#).unwrap();
        let mut session = parser.session.unwrap();
        session.first_user_message = Some("synthetic-private-prompt".into());
        session.model = Some("gpt-5.4".into());
        session.tokens_total = crate::model::TokenTotals {
            input_tokens: 1_000_000,
            total_tokens: 1_000_000,
            ..Default::default()
        };
        let mut summary = crate::model::SessionSummary::of(&session);
        summary.buckets = vec![crate::model::TierBucket {
            model: "gpt-5.4".into(),
            service_tier: None,
            tokens: session.tokens_total.clone(),
        }];
        let mut other = summary.clone();
        other.harness = crate::provider::claude_code_provider_id();
        let mut missing = summary.clone();
        missing.source_availability = crate::model::SourceAvailability::Missing;
        let mut superseded = summary.clone();
        superseded.lifecycle = crate::model::SessionLifecycle::Superseded;
        let mut purged = summary.clone();
        purged.lifecycle = crate::model::SessionLifecycle::Purged;
        let rates = crate::rates::RateCard::load_bundled().unwrap();
        let expected = crate::query::price_surfaces(&summary.buckets, "codex", &rates, now);
        let result = project_usage(
            [&summary, &other, &missing, &superseded, &purged].into_iter(),
            WidgetProvider::Codex,
            &rates,
            now,
            false,
        )
        .unwrap();
        assert_eq!(result.session_count, 1);
        assert_eq!(result.total_tokens, 1_000_000);
        assert_eq!(result.plan_amount, Some(expected.plan.total));
        assert!(!result.scan_complete);
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("synthetic-private"));
    }
}
