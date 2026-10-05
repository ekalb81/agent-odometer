//! Opt-in transcript attention observations. No process discovery, raw bodies,
//! hooks, network access, or accounting writes. Missing evidence stays unknown.
use crate::model::{Session, SourceAvailability, ToolKind, ToolOutcome, TurnStatus};
use crate::provider::ProviderId;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_SEEN: usize = 512;
const MAX_SESSIONS: usize = 100;
const MAX_ALERTS: usize = 20;
const STORE_LIMIT: u64 = 64 * 1024;
const ERROR: &str =
    "Attention preferences or deduplication state are unavailable; alerts are suppressed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    TurnStarted,
    InputRequested,
    ToolCompleted,
    ToolFailed,
    TurnCompleted,
    TurnInterrupted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub revision: u64,
    pub categories: Vec<EventKind>,
    /// Empty means all registered providers, never an arbitrary text matcher.
    pub providers: Vec<ProviderId>,
    pub tool_kind: Option<ToolKind>,
    pub stale_after_seconds: u32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            revision: 0,
            categories: vec![],
            providers: vec![],
            tool_kind: None,
            stale_after_seconds: 300,
        }
    }
}
impl Preferences {
    fn validate(&self) -> Result<(), &'static str> {
        if self.categories.len() > 6
            || self.providers.len() > 16
            || self.providers.iter().any(|p| {
                p.as_str().len() > 64
                    || crate::provider::ProviderRegistry::builtin()
                        .adapter(p)
                        .is_none()
            })
            || !(30..=3600).contains(&self.stale_after_seconds)
        {
            return Err("Invalid attention preferences");
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    preferences: Preferences,
    seen: Vec<String>,
    watermarks: BTreeMap<String, DateTime<Utc>>,
    last_alert: Option<DateTime<Utc>>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            preferences: Preferences::default(),
            seen: vec![],
            watermarks: BTreeMap::new(),
            last_alert: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionState {
    Working,
    Waiting,
    Idle,
    Error,
    Unknown,
}

#[derive(Clone, Serialize)]
pub struct Observation {
    pub session_ref: String,
    pub provider: ProviderId,
    pub state: AttentionState,
    pub observed_state: AttentionState,
    pub observed_at: DateTime<Utc>,
    pub source: &'static str,
    pub stale: bool,
    pub partial: bool,
}
#[derive(Clone, Serialize)]
pub struct Alert {
    pub id: String,
    pub session_ref: String,
    /// Fixed provider label, never a session title or user-defined identifier.
    pub provider_label: &'static str,
    pub category: EventKind,
    pub observed_at: DateTime<Utc>,
    pub source: &'static str,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub preferences: Preferences,
    pub available: bool,
    pub observations: Vec<Observation>,
    pub alerts: Vec<Alert>,
}
struct Signal {
    id: String,
    at: DateTime<Utc>,
    category: EventKind,
    state: AttentionState,
    source: &'static str,
    tool_kind: Option<ToolKind>,
}
struct Inner {
    store: Store,
    available: bool,
    armed_at: DateTime<Utc>,
    observations: BTreeMap<String, Observation>,
    alerts: Vec<Alert>,
}
pub struct AttentionService {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

fn digest(value: &str) -> String {
    format!("{:016x}", crate::stable_hash::fnv1a64(value.as_bytes()))
}
fn load(path: &Path, now: DateTime<Utc>) -> Result<Store, &'static str> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(_) => return Err(ERROR),
    };
    let mut bytes = vec![];
    file.take(STORE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ERROR)?;
    if bytes.len() as u64 > STORE_LIMIT {
        return Err(ERROR);
    }
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| ERROR)?;
    if store.version != 1
        || store.seen.len() > MAX_SEEN
        || store.watermarks.len() > MAX_SESSIONS
        || store
            .seen
            .iter()
            .chain(store.watermarks.keys())
            .any(|id| id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ERROR);
    }
    store.preferences.validate()?;
    if store.last_alert.is_some_and(|at| at > now) || store.watermarks.values().any(|at| *at > now)
    {
        return Err(ERROR);
    }
    Ok(store)
}

// The lock file is separate from the atomically replaced data file. Explicit
// unlock avoids inherited descriptors extending a lock's lifetime on Unix.
struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock(path: &Path) -> Result<Lock, &'static str> {
    let parent = path.parent().ok_or(ERROR)?;
    std::fs::create_dir_all(parent).map_err(|_| ERROR)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|_| ERROR)?;
    file.try_lock().map_err(|_| ERROR)?;
    Ok(Lock(file))
}
fn save(path: &Path, store: &Store) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec(store).map_err(|_| ERROR)?;
    if bytes.len() as u64 > STORE_LIMIT {
        return Err(ERROR);
    }
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or(ERROR)?).map_err(|_| ERROR)?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| ERROR)?;
    file.persist(path).map_err(|_| ERROR)?;
    Ok(())
}

impl Default for AttentionService {
    fn default() -> Self {
        Self::open(None, Utc::now())
    }
}
impl AttentionService {
    fn open(path: Option<PathBuf>, now: DateTime<Utc>) -> Self {
        let loaded = path
            .as_deref()
            .map(|path| load(path, now))
            .unwrap_or(Ok(Store::default()));
        let available = loaded.is_ok();
        Self {
            path,
            inner: Mutex::new(Inner {
                store: loaded.unwrap_or_default(),
                available,
                armed_at: now,
                observations: BTreeMap::new(),
                alerts: vec![],
            }),
        }
    }
    pub fn open_default() -> Self {
        Self::open(
            dirs::config_dir().map(|path| path.join("agent-odometer/attention-v1.json")),
            Utc::now(),
        )
    }
    pub fn preferences(
        &self,
        mut update: Preferences,
        now: DateTime<Utc>,
    ) -> Result<Preferences, &'static str> {
        update.validate()?;
        update.categories.sort();
        update.categories.dedup();
        update.providers.sort();
        update.providers.dedup();
        let mut inner = self.inner.lock().unwrap();
        let path = self.path.as_deref().ok_or(ERROR)?;
        let _lock = lock(path)?;
        let mut store = load(path, now)?;
        if update.revision != store.preferences.revision {
            return Err("Attention preferences changed; reload before saving");
        }
        update.revision = update.revision.checked_add(1).ok_or(ERROR)?;
        store.preferences = update.clone();
        save(path, &store)?;
        inner.store = store;
        inner.available = true;
        inner.armed_at = now;
        inner.observations.clear();
        inner.alerts.clear();
        Ok(update)
    }
    pub fn observe(&self, session: &Session, now: DateTime<Utc>, live_append: bool) {
        let mut inner = self.inner.lock().unwrap();
        let prefs = &inner.store.preferences;
        if !inner.available
            || prefs.categories.is_empty()
            || session.archived
            || session.source_availability != SourceAvailability::Present
            || (!prefs.providers.is_empty() && !prefs.providers.contains(&session.harness))
        {
            return;
        }
        let storage_id = session.effective_storage_id();
        let signals = signals(session);
        let Some(latest) = signals
            .iter()
            .filter(|signal| signal.at <= now)
            .max_by_key(|signal| (signal.at, signal.category))
        else {
            return;
        };
        let partial = session.turns.len() > 50 || session.tool_observations.len() > 200;
        let observation = Observation {
            session_ref: digest(&storage_id),
            provider: session.harness.clone(),
            state: latest.state,
            observed_state: latest.state,
            observed_at: latest.at,
            source: latest.source,
            stale: false,
            partial,
        };
        if inner
            .observations
            .get(&storage_id)
            .is_none_or(|old| old.observed_at <= latest.at)
        {
            inner.observations.insert(storage_id.clone(), observation);
        }
        if inner.observations.len() > MAX_SESSIONS {
            if let Some(oldest) = inner
                .observations
                .iter()
                .min_by_key(|(_, value)| value.observed_at)
                .map(|(key, _)| key.clone())
            {
                inner.observations.remove(&oldest);
            }
        }
        if !live_append {
            return;
        }
        let candidates: Vec<_> = signals
            .into_iter()
            .filter(|signal| {
                let prefs = &inner.store.preferences;
                signal.at >= inner.armed_at
                    && signal.at <= now
                    && now.signed_duration_since(signal.at)
                        <= Duration::seconds(i64::from(prefs.stale_after_seconds))
                    && prefs.categories.contains(&signal.category)
                    && (signal.tool_kind.is_none()
                        || prefs.tool_kind.is_none()
                        || prefs.tool_kind == signal.tool_kind)
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        if self
            .publish(&mut inner, &storage_id, &session.harness, candidates, now)
            .is_err()
        {
            inner.available = false;
            inner.alerts.clear();
        }
    }
    fn publish(
        &self,
        inner: &mut Inner,
        storage_id: &str,
        provider: &ProviderId,
        candidates: Vec<Signal>,
        now: DateTime<Utc>,
    ) -> Result<(), &'static str> {
        let path = self.path.as_deref().ok_or(ERROR)?;
        let _lock = lock(path)?;
        let mut store = load(path, now)?;
        if store.preferences != inner.store.preferences {
            return Err(ERROR);
        }
        let session_ref = digest(storage_id);
        store
            .watermarks
            .retain(|_, at| now.signed_duration_since(*at) <= Duration::hours(1));
        if !store.watermarks.contains_key(&session_ref) && store.watermarks.len() >= MAX_SESSIONS {
            return Ok(());
        }
        let watermark = store.watermarks.get(&session_ref).copied();
        let mut alert = None;
        let old_seen = store.seen.clone();
        for signal in candidates {
            // Older or same-time late arrivals cannot replay an earlier signal,
            // including after the bounded hash log evicts it. Signals sharing a
            // timestamp within this batch are still evaluated together.
            if watermark.is_some_and(|at| signal.at <= at) {
                continue;
            }
            let id = digest(&format!("{storage_id}\0{}", signal.id));
            if store.seen.contains(&id) {
                continue;
            }
            store.seen.push(id.clone());
            store.watermarks.insert(session_ref.clone(), signal.at);
            // Consume suppressed events too: no delayed replay after the limit.
            if alert.is_none()
                && store
                    .last_alert
                    .is_none_or(|at| now.signed_duration_since(at) >= Duration::seconds(30))
            {
                store.last_alert = Some(now);
                alert = Some(Alert {
                    id,
                    session_ref: digest(storage_id),
                    provider_label: match provider.as_str() {
                        "codex" => "Codex",
                        "claude_code" => "Claude Code",
                        "gemini_cli" => "Gemini CLI",
                        _ => "Other provider",
                    },
                    category: signal.category,
                    observed_at: signal.at,
                    source: signal.source,
                });
            }
        }
        if store.seen.len() > MAX_SEEN {
            store.seen.drain(..store.seen.len() - MAX_SEEN);
        }
        if store.seen != old_seen {
            save(path, &store)?;
        }
        inner.store = store;
        if let Some(alert) = alert {
            inner.alerts.push(alert);
            if inner.alerts.len() > MAX_ALERTS {
                inner.alerts.remove(0);
            }
        }
        Ok(())
    }
    pub fn snapshot(&self, now: DateTime<Utc>, valid: impl Fn(&str) -> bool) -> Snapshot {
        let mut inner = self.inner.lock().unwrap();
        // Also makes another running window/process's preferences authoritative.
        if let Some(path) = &self.path {
            match load(path, now) {
                Ok(store) => {
                    if store.preferences != inner.store.preferences {
                        inner.observations.clear();
                        inner.alerts.clear();
                        inner.armed_at = now;
                    }
                    inner.store = store;
                    // Readability does not prove writes recovered. A successful
                    // explicit preference save clears a persistence failure.
                }
                Err(_) => {
                    inner.available = false;
                    inner.alerts.clear();
                }
            }
        }
        let valid_refs: Vec<_> = inner
            .observations
            .iter()
            .filter(|(id, _)| valid(id))
            .map(|(_, value)| value.session_ref.clone())
            .collect();
        let observations = inner
            .observations
            .values()
            .filter(|value| valid_refs.contains(&value.session_ref))
            .map(|value| {
                let mut value = value.clone();
                value.stale = now < value.observed_at
                    || now.signed_duration_since(value.observed_at)
                        > Duration::seconds(i64::from(inner.store.preferences.stale_after_seconds));
                if value.stale || !inner.available {
                    value.state = AttentionState::Unknown;
                }
                value
            })
            .collect();
        let alerts = if inner.available {
            inner
                .alerts
                .iter()
                .filter(|alert| {
                    valid_refs.contains(&alert.session_ref)
                        && alert.observed_at <= now
                        && now.signed_duration_since(alert.observed_at)
                            <= Duration::seconds(i64::from(
                                inner.store.preferences.stale_after_seconds,
                            ))
                })
                .cloned()
                .collect()
        } else {
            vec![]
        };
        Snapshot {
            preferences: inner.store.preferences.clone(),
            available: inner.available,
            observations,
            alerts,
        }
    }
}

fn signals(session: &Session) -> Vec<Signal> {
    let mut signals = vec![];
    for turn in session.turns.iter().rev().take(50) {
        if turn.status == TurnStatus::RolledBack {
            signals.push(Signal {
                id: format!("turn:{}:rollback:{}", turn.turn_id, session.last_event_at),
                at: session.last_event_at,
                category: EventKind::TurnInterrupted,
                state: AttentionState::Unknown,
                source: "retained rolled-back turn",
                tool_kind: None,
            });
            continue;
        }
        if let Some(at) = turn.started_at {
            signals.push(Signal {
                id: format!("turn:{}:started:{at}", turn.turn_id),
                at,
                category: EventKind::TurnStarted,
                state: AttentionState::Working,
                source: "retained turn start",
                tool_kind: None,
            });
        }
        if let Some(at) = turn.completed_at {
            let (category, state) = match turn.status {
                TurnStatus::Completed => (EventKind::TurnCompleted, AttentionState::Idle),
                TurnStatus::Aborted => (EventKind::TurnInterrupted, AttentionState::Unknown),
                _ => continue,
            };
            signals.push(Signal {
                id: format!("turn:{}:{category:?}:{at}", turn.turn_id),
                at,
                category,
                state,
                source: "retained turn completion",
                tool_kind: None,
            });
        }
    }
    for tool in session.tool_observations.iter().rev().take(200) {
        let (category, state) = match tool.outcome {
            ToolOutcome::Failure => (EventKind::ToolFailed, AttentionState::Error),
            ToolOutcome::Success => (EventKind::ToolCompleted, AttentionState::Working),
            ToolOutcome::Pending
                if matches!(
                    tool.name.as_str(),
                    "request_user_input" | "functions.request_user_input" | "AskUserQuestion"
                ) =>
            {
                (EventKind::InputRequested, AttentionState::Waiting)
            }
            _ => continue,
        };
        // Completion timestamps are derived only from a recorded duration;
        // otherwise the call time is conservative and expires sooner.
        let at = tool
            .duration_ms
            .and_then(|duration| i64::try_from(duration).ok())
            .and_then(|duration| {
                tool.timestamp
                    .checked_add_signed(Duration::milliseconds(duration))
            })
            .unwrap_or(tool.timestamp);
        signals.push(Signal {
            id: format!("tool:{}:{category:?}:{at}", tool.call_id),
            at,
            category,
            state,
            source: "retained tool observation",
            tool_kind: Some(tool.kind),
        });
    }
    signals.sort_by_key(|signal| (signal.at, signal.category));
    signals
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<Utc> {
        "2026-10-04T12:00:00Z".parse().unwrap()
    }
    fn session(at: DateTime<Utc>, id: &str) -> Session {
        let mut parser =
            crate::parser::SessionParser::new(PathBuf::from("/PRIVATE_PATH/session.jsonl"), false);
        for value in [
            serde_json::json!({"type":"session_meta","timestamp":at,"payload":{"id":"PRIVATE_SESSION","timestamp":at}}),
            serde_json::json!({"type":"event_msg","timestamp":at,"payload":{"type":"task_started","turn_id":id,"started_at":at}}),
            serde_json::json!({"type":"event_msg","timestamp":at,"payload":{"type":"user_message","message":"PRIVATE_PROMPT"}}),
        ] {
            parser.apply_line(&value.to_string()).unwrap();
        }
        parser.session.unwrap()
    }
    fn enable(service: &AttentionService, categories: Vec<EventKind>) {
        service
            .preferences(
                Preferences {
                    categories,
                    ..Default::default()
                },
                now(),
            )
            .unwrap();
    }
    #[test]
    fn off_by_default_and_explicit_preferences_start_without_replaying_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attention.json");
        let service = AttentionService::open(Some(path.clone()), now());
        service.observe(&session(now(), "before"), now(), true);
        assert!(!path.exists());
        assert!(service.snapshot(now(), |_| true).alerts.is_empty());
        enable(&service, vec![EventKind::TurnStarted]);
        service.observe(
            &session(now() - Duration::seconds(1), "before"),
            now(),
            true,
        );
        assert!(service.snapshot(now(), |_| true).alerts.is_empty());
        service.observe(
            &session(now() + Duration::seconds(1), "after"),
            now() + Duration::seconds(1),
            true,
        );
        let snapshot = service.snapshot(now() + Duration::seconds(1), |_| true);
        assert_eq!(snapshot.alerts.len(), 1);
        assert_eq!(snapshot.observations[0].state, AttentionState::Working);
        let serialized = serde_json::to_string(&snapshot).unwrap();
        for forbidden in ["PRIVATE_PATH", "PRIVATE_PROMPT", "PRIVATE_SESSION"] {
            assert!(!serialized.contains(forbidden));
        }
        let stored = std::fs::read_to_string(path).unwrap();
        assert!(!stored.contains("PRIVATE"));
    }
    #[test]
    fn repeats_reordering_rate_limits_and_restart_do_not_replay_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attention.json");
        let service = AttentionService::open(Some(path.clone()), now());
        enable(&service, vec![EventKind::TurnStarted]);
        for (second, id, delivery) in [
            (1, "a", 1),
            (2, "b", 2),
            (1, "a", 40),
            (2, "b", 40),
            (0, "late", 40),
            (41, "c", 41),
        ] {
            service.observe(
                &session(now() + Duration::seconds(second), id),
                now() + Duration::seconds(delivery),
                true,
            );
        }
        assert_eq!(
            service
                .snapshot(now() + Duration::seconds(41), |_| true)
                .alerts
                .len(),
            2
        );
        let resumed = AttentionService::open(Some(path), now() + Duration::seconds(42));
        resumed.observe(
            &session(now() + Duration::seconds(41), "c"),
            now() + Duration::seconds(43),
            true,
        );
        assert!(resumed
            .snapshot(now() + Duration::seconds(43), |_| true)
            .alerts
            .is_empty());
    }
    #[test]
    fn stale_missing_clock_reversal_and_rollback_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let service = AttentionService::open(Some(dir.path().join("a.json")), now());
        enable(&service, vec![EventKind::TurnStarted]);
        let mut row = session(now() + Duration::seconds(1), "a");
        service.observe(&row, now() + Duration::seconds(1), true);
        for time in [now(), now() + Duration::seconds(302)] {
            let snapshot = service.snapshot(time, |_| true);
            assert_eq!(snapshot.observations[0].state, AttentionState::Unknown);
            assert!(snapshot.alerts.is_empty());
        }
        assert!(service
            .snapshot(now() + Duration::seconds(1), |_| false)
            .observations
            .is_empty());
        assert!(service
            .snapshot(now() + Duration::seconds(1), |_| false)
            .alerts
            .is_empty());
        let prefs = service
            .snapshot(now() + Duration::seconds(302), |_| true)
            .preferences;
        service
            .preferences(prefs, now() + Duration::seconds(1))
            .unwrap();
        row.turns[0].status = TurnStatus::RolledBack;
        row.last_event_at = now() + Duration::seconds(2);
        service.observe(&row, now() + Duration::seconds(2), true);
        assert_eq!(
            service
                .snapshot(now() + Duration::seconds(2), |_| true)
                .observations[0]
                .state,
            AttentionState::Unknown
        );
    }
    #[test]
    fn explicit_question_failure_completion_and_matching_use_only_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let service = AttentionService::open(Some(dir.path().join("a.json")), now());
        enable(
            &service,
            vec![
                EventKind::InputRequested,
                EventKind::ToolFailed,
                EventKind::ToolCompleted,
                EventKind::TurnCompleted,
            ],
        );
        let mut row = session(now(), "a");
        row.tool_observations = vec![crate::model::ToolObservation {
            call_id: "SECRET_CALL".into(),
            turn_id: None,
            harness: row.harness.clone(),
            model: None,
            timestamp: now() + Duration::seconds(1),
            kind: ToolKind::Other,
            name: "request_user_input".into(),
            providers: vec![],
            effective_tools: vec![],
            target: None,
            resource_id: None,
            origin: Default::default(),
            shell_family: None,
            language: None,
            outcome: ToolOutcome::Pending,
            duration_ms: None,
            output_bytes: 0,
        }];
        service.observe(&row, now() + Duration::seconds(1), true);
        assert_eq!(
            service
                .snapshot(now() + Duration::seconds(1), |_| true)
                .observations[0]
                .state,
            AttentionState::Waiting
        );
        row.tool_observations[0].outcome = ToolOutcome::Failure;
        row.tool_observations[0].duration_ms = Some(31_000);
        service.observe(&row, now() + Duration::seconds(32), true);
        let snapshot = service.snapshot(now() + Duration::seconds(32), |_| true);
        assert_eq!(snapshot.observations[0].state, AttentionState::Error);
        assert_eq!(snapshot.alerts.len(), 2);
        row.turns[0].status = TurnStatus::Completed;
        row.turns[0].completed_at = Some(now() + Duration::seconds(64));
        service.observe(&row, now() + Duration::seconds(64), true);
        assert_eq!(
            service
                .snapshot(now() + Duration::seconds(64), |_| true)
                .observations[0]
                .state,
            AttentionState::Idle
        );
        let mut prefs = service
            .snapshot(now() + Duration::seconds(64), |_| true)
            .preferences;
        prefs.tool_kind = Some(ToolKind::Mutation);
        service
            .preferences(prefs, now() + Duration::seconds(65))
            .unwrap();
        row.tool_observations[0].timestamp = now() + Duration::seconds(100);
        row.tool_observations[0].duration_ms = Some(1);
        service.observe(&row, now() + Duration::seconds(101), true);
        assert!(service
            .snapshot(now() + Duration::seconds(101), |_| true)
            .alerts
            .is_empty());
    }
    #[test]
    fn persistence_errors_and_stale_preferences_fail_closed_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        let service = AttentionService::open(Some(path.clone()), now());
        enable(&service, vec![EventKind::TurnStarted]);
        assert!(service.preferences(Preferences::default(), now()).is_err());
        let before = std::fs::read(&path).unwrap();
        let held = lock(&path).unwrap();
        service.observe(
            &session(now() + Duration::seconds(1), "a"),
            now() + Duration::seconds(1),
            true,
        );
        assert!(service.inner.lock().unwrap().alerts.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(held);
        assert!(
            !service
                .snapshot(now() + Duration::seconds(1), |_| true)
                .available
        );
        std::fs::write(&path, b"malformed PRIVATE").unwrap();
        assert!(!service.snapshot(now(), |_| true).available);
        assert!(service.preferences(Preferences::default(), now()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"malformed PRIVATE");
    }

    #[test]
    fn unsupported_provider_and_future_dedup_state_are_explicitly_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        let service = AttentionService::open(Some(path.clone()), now());
        let invalid = Preferences {
            providers: vec![ProviderId::new("not_registered").unwrap()],
            ..Default::default()
        };
        assert!(service.preferences(invalid, now()).is_err());
        assert!(!path.exists());
        for future_watermark in [false, true] {
            let mut store = Store::default();
            store.preferences.categories = vec![EventKind::TurnStarted];
            if future_watermark {
                store
                    .watermarks
                    .insert(digest("session"), now() + Duration::days(10));
            } else {
                store.last_alert = Some(now() + Duration::days(10));
            }
            let bytes = serde_json::to_vec(&store).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let blocked = AttentionService::open(Some(path.clone()), now());
            assert!(!blocked.snapshot(now(), |_| true).available);
            blocked.observe(
                &session(now() + Duration::seconds(1), "new"),
                now() + Duration::seconds(1),
                true,
            );
            assert!(blocked
                .snapshot(now() + Duration::seconds(1), |_| true)
                .alerts
                .is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}
