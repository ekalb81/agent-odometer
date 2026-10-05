# Shared alerts and provider tray

The shared alert policy lives in the existing private `quota-v2.json` settings
and notification dedup log. Existing budget notification opt-in is preserved.
Transcript attention, cached public provider incidents, expired transcript quota
observations, and missing retained-history source transcripts start disabled.
Attention users must select transcript categories and explicitly enable shared
alerts plus its transcript category in Settings.

Local quiet hours apply to every category and budget, for both in-app and desktop
delivery. The interval includes its start and excludes its end; a later start
wraps across midnight, while equal hours disable the interval. Disabled, quiet,
expired, startup, policy-change baseline, resume, and persistence-failure observations are consumed without a
later backlog. Unknown evidence does not resolve or rearm an active condition.
An observed resolution does. A failed save suppresses delivery; recovery first
persists a suppressed baseline before delivery resumes.

One main-window adapter evaluates delivery every 15 seconds, independently of
the selected tab. Ledger budget queries and retention counts run at most once a
minute. Dashboard budget and Settings history reads never consume notification edges. The main collector serializes overlapping refreshes; committed policy events invalidate prior approvals. Desktop
permission is separate from opt-in; unsupported or denied desktop notification
delivery leaves in-app notices and evidence available. Recent delivered metadata
is bounded to 20 displayed notices, 500 total dedup entries, and 30 days. It holds
fixed labels/codes, hashes, timestamps, and typed evidence routes; source bodies,
project paths, account labels, and session titles are not copied into it.

Provider incident alerts use only cached public service observations from the
existing explicitly enabled status panel. This feature starts no background
public polling and accesses no provider credentials. Closing that panel may mean
no new status observations. Service outages cannot change accounting, rates,
live-quota consent, or historical usage. Retention notices mean stored sessions
have missing source transcripts; they do not claim an automatic deletion or an
unobserved future loss.

The native tray selects All providers, Codex, Claude Code, or Gemini CLI. Today's
tokens and returned Rust prices use that provider scope. Gemini prices remain
separate from Claude estimates. Quota uses the same backend windows, reset time,
pace, and observation timestamp as the dashboard/widget. Transcript quota remains
unattributed. All providers requires a provider selection for quota. Codex live
quota requires exactly one enabled approved account; multiple enabled accounts
are explicitly unavailable rather than selecting an identity or summing limits.
Unavailable live data never falls back to a transcript reading. Future/expired
observations mark the result stale and suppress a current pace claim. Native tray
availability remains platform dependent; the main application stays functional.

Issue #48's optional gamification is a no-go for this delivery: no additional
usage history, inferred productivity score, streak, or competitive ranking is
introduced.

Validation used synthetic local profiles with no credentials or public polling.
The native Tauri flow exercised the actual D-Bus tray menu, provider selection,
quiet suppression, resume without replay, fresh shared delivery, evidence
navigation, and deduplication. Positive token, Rust pricing, and quota values
were identical before and after. A test-only StatusNotifierWatcher supplied the
desktop panel protocol missing from Xvfb; this proves native menu contents and
events, rather than the appearance of a particular desktop shell's panel.
