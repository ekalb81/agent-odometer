# Codex speed

Open **Codex → Analytics & exports → Codex speed**. The report supports
Today, Last 7 days, Last 14 days, model/reasoning filters, Fast/Standard
groups, and CSV export of the selected measurement. It refreshes every 15 seconds
while the panel, tab, and document are visible. Today starts at local midnight;
the longer windows use the same local wall-clock time 7 or 14 days earlier.
Session-list filters do not scope this separate report.

The feature is inspired by [Codex Speed Monitor](https://github.com/petergpt/codex-speed-monitor).
Turn throughput is the default. It uses the Codex JSONL sessions already parsed
by Odometer and retained in durable history. It does not require completed-response
SQLite telemetry. The optional response view reads `$CODEX_HOME/logs_2.sqlite`
(otherwise `~/.codex/logs_2.sqlite`) read-only. Neither view changes Codex
configuration or installs an OpenTelemetry collector.

## What the measurement means

| Measurement | Numerator | Denominator | Mode evidence |
| --- | --- | --- | --- |
| Turn throughput (default) | Reconciled output tokens across the completed turn | Explicit turn duration, otherwise recorded start/end timestamps | Configured Fast/Standard setting |
| Response throughput (optional) | Completed response's output tokens | Server-reported response duration | Observed response service tier |

Turn time includes tools, reasoning, queueing, and waiting. It is useful for
comparing agent work under similar conditions, but does not isolate model generation.
Recorded time to first token is shown where available; it is never inferred from
the first assistant message. Running, aborted, rolled-back, zero-output, and
invalid-duration turns do not enter completed-turn rates. The completion instant
determines the report window; the row contains the whole turn's tokens and duration.
Parent and subagent turns can overlap, so summed durations describe worker time,
not elapsed task delivery. Mixed-model turns must not attribute all output to the
last configured model.
The turn effort filter describes its final configured effort; existing parsed
history does not retain a separate effort setting for each model call.

Response time includes reasoning and request latency. Its timestamps have second
precision. Only completed text responses with at least 100 output tokens and
five seconds are included; non-text/tool responses, invalid records, and duplicate
response IDs are excluded.

Group averages divide total output by total measured duration, rather than
averaging rates from differently sized samples. The measurements remain separate.
Neither is pure decode speed, accepted-task delivery speed, or evidence of a causal
speedup.

Configured turn settings do not prove which tier served a response. Unknown modes
stay separate. Missing reasoning evidence leaves reasoning/visible-token metrics
unavailable. Explicit timings and timestamp fallbacks carry separate provenance
in exports, alongside the selected measurement and source.

Availability depends on recorded evidence and local retention. Preparing history
or incomplete scans are surfaced rather than presented as a complete zero.
Turn reports load at most 500 candidate sessions in batches of eight, examine
up to 10,000 turns, and return up to 5,000 samples. A five-second processing
budget is checked while selecting candidates, between store batches, and between
turns; it cannot interrupt an
individual existing store read. Response
scans cover up to 100,000 matching log rows and return up to 5,000 samples;
a 1 MiB per-record byte limit, a 256-byte response ID limit, and a three-second
query budget bound work.
Truncated results are labeled explicitly.
Prompts, response text, identifiers, credentials, and paths are excluded from IPC
and CSV. Speed observations never change the usage ledger or pricing.

## Validation on 2026-10-04

All six required checks passed: frontend type checking (zero errors/warnings),
273 frontend tests plus 31 script tests, frontend build, Rust formatting,
Clippy with warnings denied, and 684 Rust tests (19 existing ignored tests).
All 43 browser/visual checks passed in the pinned Playwright container; the
focused speed flow passed again after final UI changes. Existing screenshot
baselines were unchanged. Focused coverage includes timing precedence/fallbacks, window bounds,
token attribution, missing data, measurement changes, filtering/export provenance,
and superseded requests. The original response source additionally has synthetic
SQLite tests for deduplication, byte limits, and text versus reasoning/tool output.
The turn source also covers completion-second precision, ambiguous adjacent
token events, configured-tier coverage, and invalid time-to-first-token values.

A read-only sample of eight recent real Codex JSONL files contained 143 completed
turns, all with duration, start/end, and time-to-first-token fields. Only aggregate
field-presence counts were inspected; no real transcripts became fixtures.
The real local SQLite source contained no eligible response timings. That absence
does not imply that session timing is unavailable.

The updated native flow was exercised with `npm run tauri dev` against real
session history. The Today turn report showed populated Fast/Standard/unknown
groups, model/final-effort groups, recorded TTFT, and enabled CSV export.
Filtering, measurement switching, and CSV contents were verified with synthetic
component/browser fixtures; no real session became a test fixture.

Two existing issues were observed without changing unrelated code: the header
extends to roughly 960 px at an 800 px viewport (the speed panel fits and does not
widen it), and `history_store::tests::query_sql_progress_interrupts_cancellation_and_deadline`
timed out on the first Rust run and passed on the full rerun.
