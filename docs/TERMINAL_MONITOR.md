# Local terminal monitor

Run `node packages/terminal-monitor/monitor.mjs --bin /path/to/agent-odometer` from this checkout. Use the matching `agent-odometer.exe` on Windows. Node 22 or later is required; the standalone package has no dependencies. If the executable is already on PATH, omit `--bin`. No desktop window is required, although the existing ledger must have been prepared by a matching desktop version.

The monitor invokes only the existing read-only `status`, `statusline`, `sessions --limit 25`, and `quota` JSON reports with export schema 2. Usage is the CLI's current-day window, with exact returned UTC bounds. Session rows show cumulative usage ordered by tokens, their recorded lifecycle and source availability, and explicit truncation. They are not a list of currently running processes. The CLI does not expose session activity timestamps or a supported running/completion signal, so execution state is unknown and activity age is unavailable. A fresh report fetch does not establish a fresh scan, active session, or complete accounting coverage.

Quota numbers retain their recorded source, observation/reset timestamps, and age. Expired resets, future/invalid timestamps, and stale flags keep values labeled stale; unavailable and unlimited are distinct from zero. The monitor never polls a provider, uses credentials, reads transcript bodies, writes harness settings, or calculates prices. It formats Rust-priced amounts separately by provider/currency and keeps partial or unpriced estimates labeled.

Keys:

- `1`, `2`, `3`, `Tab`, or left/right arrows: Usage, Sessions, Quota.
- Up/down arrows or `j`/`k`: select a session or scroll quota lines; PageUp/PageDown move five steps.
- `r`: request a bounded local refresh; bursts are coalesced and reads start at least five seconds apart.
- `Space`: pause/resume CLI refresh. Pausing cancels active readers. Host sampling has its own explicit toggle.
- `h`: enable/disable Host OS telemetry for this run.
- `q`, Escape, Ctrl-C, or Ctrl-D: cancel/reap readers, stop timers, restore the terminal, and exit.

`--interval 5` through `--interval 300` chooses the automatic refresh interval in seconds; the default is 30, scheduled after a completed cycle. At most two CLI subprocesses run at once. Each has a 12-second deadline, a 1 MiB stdout bound and an 8 KiB stderr bound; cancellation escalates to forced termination after 250 ms if needed. Child stderr is never echoed into the terminal. Failure preserves a previous successful report only with a stale label and fetch age; no fabricated empty or zero replaces it. Unsupported/malformed schemas fail closed. Terminal control and non-ASCII characters in report text are replaced before rendering, so source labels cannot emit ANSI or OSC commands. Resize redraws clip lines to the terminal width; very short terminals retain the quit hint.

Use `--once` for one plain snapshot without terminal control sequences, timers, or background processes. It exits 2 if a required report fails and includes a bounded, redacted failure summary for each report, including reports outside the Usage view. Unexpected interactive rendering failures also exit 2 after restoring terminal state and stopping readers. This also works with redirected output. `--help` lists options.

## Optional Host OS readings

Host collection is off by default. Pressing `h` or supplying `--host` explicitly enables local Node `os.cpus()`, `os.totalmem()`, and `os.freemem()` readings. Nothing is persisted. No disk or network telemetry is collected.

CPU utilization is the busy share of aggregate OS CPU-time deltas across two comparable samples, targeted every two seconds. The first sample, empty/invalid counters, a CPU-count change, clock regression, or counter regression remains unavailable and starts a new baseline. The panel shows actual delta-window duration and observation age. It is an average across host CPUs, not an agent's utilization.

Host memory is `totalmem - freemem`, with total and used MiB displayed. This is the OS total/free convention, not cache-adjusted available memory, Odometer RSS/heap, model memory, or token/context accounting. Missing/impossible readings are unavailable. Turning collection off discards values and stops its timer; exiting leaves no sampling process.

Tests: `node --test packages/terminal-monitor/monitor.test.mjs`. CI runs this separately from frontend tests. Native PTY verification covers keyboard navigation, narrow resize, opt-in telemetry, missing backend, empty ledger, refresh failure, pause/exit, terminal restoration, and no surviving child processes. No terminal framework or backend contract expansion is required.
