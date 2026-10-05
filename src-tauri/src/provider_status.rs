//! Opt-in public status observations, isolated from all accounting authorities.
//! Only fixed HTTPS endpoints are requested. Bodies never leave this module;
//! snapshots contain allowlisted states/timestamps and live only in memory.
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

const INTERVAL: i64 = 300;
const FRESH_SECONDS: i64 = 900;
const MAX_BODY: usize = 32 * 1024;
const SOURCES: [(&str, &str); 2] = [
    ("codex", "https://status.openai.com/api/v2/status.json"),
    (
        "claude_code",
        "https://status.claude.com/api/v2/status.json",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Indicator {
    Operational,
    Minor,
    Major,
    Critical,
    Maintenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    OfflineOrTimeout,
    RateLimited,
    HttpError,
    InvalidResponse,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderStatus {
    pub provider: String,
    pub source_url: Option<String>,
    pub state: &'static str,
    pub current_indicator: Option<Indicator>,
    pub last_known_indicator: Option<Indicator>,
    pub checked_at: Option<DateTime<Utc>>,
    pub source_updated_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub failure: Option<Failure>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusSnapshot {
    pub enabled: bool,
    pub providers: Vec<ProviderStatus>,
}

#[derive(Clone, Default)]
struct Observation {
    indicator: Option<Indicator>,
    checked: Option<DateTime<Utc>>,
    updated: Option<DateTime<Utc>>,
    attempted: Option<DateTime<Utc>>,
    next: Option<DateTime<Utc>>,
    failure: Option<Failure>,
    failures: u32,
    in_flight: bool,
}

#[derive(Default)]
struct State {
    enabled: bool,
    generation: u64,
    observations: [Observation; 2],
}

#[derive(Default)]
pub struct ProviderStatusService(Mutex<State>);

type FetchResult = Result<(Indicator, DateTime<Utc>), (Failure, Option<i64>)>;

impl ProviderStatusService {
    /// Called only after the explicit setting has been saved successfully.
    pub fn configure(&self, enabled: bool) {
        let mut state = self.0.lock().unwrap();
        if state.enabled != enabled {
            state.enabled = enabled;
            state.generation = state.generation.wrapping_add(1);
            state.observations = Default::default();
        }
    }

    fn begin(&self, index: usize, now: DateTime<Utc>) -> Option<u64> {
        let mut state = self.0.lock().unwrap();
        if !state.enabled {
            return None;
        }
        let row = &mut state.observations[index];
        if row.in_flight || row.next.is_some_and(|next| now < next) {
            return None;
        }
        row.in_flight = true;
        row.attempted = Some(now);
        row.next = Some(now + Duration::seconds(INTERVAL));
        Some(state.generation)
    }

    fn finish(&self, index: usize, generation: u64, now: DateTime<Utc>, result: FetchResult) {
        let mut state = self.0.lock().unwrap();
        if !state.enabled || state.generation != generation {
            return;
        }
        let row = &mut state.observations[index];
        row.in_flight = false;
        match result {
            Ok((indicator, updated)) => {
                row.indicator = Some(indicator);
                row.updated = Some(updated);
                row.checked = Some(now);
                row.failure = None;
                row.failures = 0;
                row.next = Some(now + Duration::seconds(INTERVAL));
            }
            Err((failure, retry_after)) => {
                row.failure = Some(failure);
                row.failures = row.failures.saturating_add(1);
                let backoff = (INTERVAL * (1_i64 << row.failures.min(4))).min(3600);
                let delay = backoff.max(retry_after.unwrap_or(0).clamp(0, 86400));
                row.next = Some(now + Duration::seconds(delay));
            }
        }
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> StatusSnapshot {
        let state = self.0.lock().unwrap();
        let mut providers = Vec::with_capacity(3);
        for (index, (provider, source)) in SOURCES.iter().enumerate() {
            let row = &state.observations[index];
            let fresh = row.checked.is_some_and(|at| {
                let age = now.signed_duration_since(at).num_seconds();
                (0..=FRESH_SECONDS).contains(&age)
            });
            let availability = if !state.enabled {
                "disabled"
            } else if row.failure.is_some() {
                "unavailable"
            } else if fresh {
                "current"
            } else if row.checked.is_some() {
                "stale"
            } else {
                "pending"
            };
            providers.push(ProviderStatus {
                provider: (*provider).into(),
                source_url: Some((*source).into()),
                state: availability,
                current_indicator: (availability == "current")
                    .then_some(row.indicator)
                    .flatten(),
                last_known_indicator: row.indicator,
                checked_at: row.checked,
                source_updated_at: row.updated,
                last_attempt_at: row.attempted,
                next_attempt_at: row.next,
                failure: row.failure,
            });
        }
        providers.push(ProviderStatus {
            provider: "gemini_cli".into(),
            source_url: None,
            state: "unsupported",
            current_indicator: None,
            last_known_indicator: None,
            checked_at: None,
            source_updated_at: None,
            last_attempt_at: None,
            next_attempt_at: None,
            failure: None,
        });
        StatusSnapshot {
            enabled: state.enabled,
            providers,
        }
    }

    /// UI refresh only schedules due requests. Frequent IPC calls cannot bypass
    /// cadence/backoff, and closing the status surface stops future polling.
    pub fn refresh_due(self: &Arc<Self>) {
        for index in 0..SOURCES.len() {
            let Some(generation) = self.begin(index, Utc::now()) else {
                continue;
            };
            let service = self.clone();
            tauri::async_runtime::spawn(async move {
                // A queued task must not start a request after opt-out.
                {
                    let state = service.0.lock().unwrap();
                    if !state.enabled || state.generation != generation {
                        return;
                    }
                }
                let result = fetch(index).await;
                service.finish(index, generation, Utc::now(), result);
            });
        }
    }
}

#[derive(Deserialize)]
struct PublicResponse {
    page: PublicPage,
    status: PublicIndicator,
}
#[derive(Deserialize)]
struct PublicPage {
    url: String,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize)]
struct PublicIndicator {
    indicator: String,
}

fn parse(index: usize, body: &[u8], now: DateTime<Utc>) -> FetchResult {
    let invalid = (Failure::InvalidResponse, None);
    if body.len() > MAX_BODY {
        return Err(invalid);
    }
    let response: PublicResponse = serde_json::from_slice(body).map_err(|_| invalid)?;
    let origin = SOURCES[index].1.trim_end_matches("/api/v2/status.json");
    if response.page.url.trim_end_matches('/') != origin
        || response.page.updated_at > now + Duration::minutes(5)
    {
        return Err(invalid);
    }
    let indicator = match response.status.indicator.as_str() {
        "none" => Indicator::Operational,
        "minor" => Indicator::Minor,
        "major" => Indicator::Major,
        "critical" => Indicator::Critical,
        "maintenance" => Indicator::Maintenance,
        _ => return Err(invalid),
    };
    Ok((indicator, response.page.updated_at))
}

fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    // The updater installs this provider lazily during its own check. Public
    // status can run first, so it must initialize the same existing provider.
    // A concurrent initializer may win; never replace an installed provider.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    // Reuse the updater's existing HTTP dependency. No cookie store, account
    // session, auth header, proxy credentials, redirects, or caller-provided URL.
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(8))
        .connect_timeout(std::time::Duration::from_secs(4))
        .user_agent(concat!("Odometer/", env!("CARGO_PKG_VERSION")))
        .build()
}

async fn fetch(index: usize) -> FetchResult {
    let offline = (Failure::OfflineOrTimeout, None);
    let client = http_client().map_err(|_| offline)?;
    let mut response = client
        .get(SOURCES[index].1)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| offline)?;
    if response.status().as_u16() == 429 {
        let now = Utc::now();
        let delay = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| {
                text.parse::<i64>().ok().or_else(|| {
                    DateTime::parse_from_rfc2822(text)
                        .ok()
                        .map(|time| time.signed_duration_since(now).num_seconds())
                })
            });
        return Err((Failure::RateLimited, delay));
    }
    if !response.status().is_success() {
        return Err((Failure::HttpError, None));
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BODY as u64)
    {
        return Err((Failure::InvalidResponse, None));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| offline)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err((Failure::InvalidResponse, None));
        }
        body.extend_from_slice(&chunk);
    }
    parse(index, &body, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_client_can_be_built_before_the_updater_runs() {
        // No requests or account access: client construction must work in a
        // fresh process, without depending on updater startup side effects.
        tauri::async_runtime::block_on(async {
            http_client().expect("public status TLS client must initialize independently");
        });
    }

    fn now() -> DateTime<Utc> {
        "2026-10-04T12:00:00Z".parse().unwrap()
    }
    fn body(indicator: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"page":{"url":"https://status.openai.com/","updated_at":"2026-10-01T12:00:00Z"},"status":{"indicator":indicator,"description":"PRIVATE_UNTRUSTED_PAYLOAD"}})).unwrap()
    }
    #[test]
    fn parses_only_allowlisted_states_and_never_projects_response_bodies() {
        let service = ProviderStatusService::default();
        service.configure(true);
        for (raw, expected) in [
            ("none", Indicator::Operational),
            ("minor", Indicator::Minor),
            ("major", Indicator::Major),
            ("critical", Indicator::Critical),
            ("maintenance", Indicator::Maintenance),
        ] {
            let result = parse(0, &body(raw), now()).unwrap();
            assert_eq!(result.0, expected);
            service.finish(0, 1, now(), Ok(result));
            assert!(!serde_json::to_string(&service.snapshot(now()))
                .unwrap()
                .contains("PRIVATE_UNTRUSTED_PAYLOAD"));
        }
        assert!(parse(0, &body("unknown"), now()).is_err());
        assert!(parse(1, &body("none"), now()).is_err());
        assert!(parse(0, b"<html>offline</html>", now()).is_err());
        assert!(parse(0, &vec![b' '; MAX_BODY + 1], now()).is_err());
        assert!(parse(0, &body("none"), now() - Duration::days(10)).is_err());
    }
    #[test]
    fn default_off_cadence_backoff_and_disable_discard_in_flight_results() {
        let service = ProviderStatusService::default();
        assert!(service.begin(0, now()).is_none());
        assert_eq!(service.snapshot(now()).providers[0].state, "disabled");
        service.configure(true);
        let generation = service.begin(0, now()).unwrap();
        assert!(service.begin(0, now() + Duration::days(1)).is_none());
        service.finish(0, generation, now(), Err((Failure::OfflineOrTimeout, None)));
        assert!(service.begin(0, now() + Duration::seconds(599)).is_none());
        assert_eq!(service.snapshot(now()).providers[0].current_indicator, None);
        let generation = service.begin(0, now() + Duration::seconds(600)).unwrap();
        service.finish(
            0,
            generation,
            now(),
            Err((Failure::RateLimited, Some(7200))),
        );
        assert_eq!(
            service.snapshot(now()).providers[0].next_attempt_at,
            Some(now() + Duration::seconds(7200))
        );
        service.configure(false);
        service.configure(true);
        service.finish(0, generation, now(), parse(0, &body("none"), now()));
        assert_eq!(service.snapshot(now()).providers[0].current_indicator, None);
    }
    #[test]
    fn stale_offline_and_clock_reversal_cannot_report_old_state_as_current() {
        let service = ProviderStatusService::default();
        service.configure(true);
        let generation = service.begin(0, now()).unwrap();
        service.finish(0, generation, now(), parse(0, &body("major"), now()));
        assert_eq!(
            service.snapshot(now()).providers[0].current_indicator,
            Some(Indicator::Major)
        );
        assert!(service.begin(0, now() + Duration::seconds(299)).is_none());
        for at in [now() + Duration::seconds(901), now() - Duration::seconds(1)] {
            let snapshot = service.snapshot(at);
            assert_eq!(snapshot.providers[0].state, "stale");
            assert_eq!(snapshot.providers[0].current_indicator, None);
        }
        service.finish(0, generation, now(), Err((Failure::InvalidResponse, None)));
        let snapshot = service.snapshot(now());
        assert_eq!(snapshot.providers[0].state, "unavailable");
        assert_eq!(snapshot.providers[0].current_indicator, None);
        assert_eq!(
            snapshot.providers[0].last_known_indicator,
            Some(Indicator::Major)
        );
        assert_eq!(snapshot.providers[2].state, "unsupported");
    }
}
