# Architecture

## System overview

Odometer is a local companion to agent CLI harnesses — the ChatGPT desktop app's Codex experience and Claude Code — with two halves:

- `src-tauri/`: Rust/Tauri backend for discovery, incremental JSONL parsing, filesystem watching, persistence, and native commands.
- `src/`: Svelte 5/TypeScript frontend for reactive state, scoped tabs (`all` plus each harness), filtering, projection/export, tables, details, settings, and credit calculations.

Every session carries a `harness` tag (`codex` | `claude_code`). All three scopes share one session store, `SessionsView`, detail pane, filter predicates, pricing projection, and model aggregate. The All scope never adds plan credits to USD.

The frontend starts at `src/main.ts` and `src/App.svelte`. The native process starts at `src-tauri/src/main.rs`, which calls `src-tauri/src/lib.rs::run`.

## Startup and live-update flow

1. `Config::load` reads the platform config file or creates defaults.
2. `watcher::start` begins watching all configured roots immediately, so changes during the initial scan are not missed.
3. `scanner::scan_all` bulk-loads existing sessions on a background thread, parsing files in parallel (rayon) and emitting a `session-updated` summary per file — the window is interactive immediately and the list populates progressively. A persistent SQLite scan cache (`scan_cache.rs`, stored under the OS cache directory and keyed by file size+mtime, versioned by app release) serves unchanged files without re-reading them. Each scan touches or replaces individual rows and prunes unseen generations, avoiding whole-corpus cache deserialization and rewrites. The previous JSON cache is imported on first use. Progress flows to the UI via throttled `scan-progress` events and the `get_scan_status` command.
4. `parser::parse_file` (Codex) or `claude_parser::parse_file` (Claude Code) builds a `Session` for each file, stored in `AppState.sessions` keyed by session ID. When duplicate IDs exist under multiple roots, the parallel scan's winner is nondeterministic.
5. `session_index::read` overlays current thread names from Codex's session index after the scan.
6. `App.svelte` invokes `list_sessions`, `get_config`, and `get_rates`, then subscribes to update/removal events.
7. The watcher debounces filesystem activity, incrementally parses complete appended records, updates the `DashMap`, and emits Tauri events.

Saving watched-root settings persists the new config, stops the old watcher, clears state, restarts the watcher, kicks off the same background rescan, and emits `config-updated`. Performance-only settings are applied live and deliberately skip the restart/rescan path.

## Backend modules

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/lib.rs` | Tauri setup, shared state, command registration, initial scan, watcher lifetime |
| `src-tauri/src/model.rs` | Serialized session, harness, turn-status, and token wire models |
| `src-tauri/src/parser.rs` | Full and incremental Codex rollout JSONL parsing |
| `src-tauri/src/claude_parser.rs` | Full and incremental Claude Code session JSONL parsing |
| `src-tauri/src/scanner.rs` | Recursive JSONL discovery, cached parallel initial parse |
| `src-tauri/src/scan_cache.rs` | Incremental SQLite parsed-session cache keyed by file size+mtime |
| `src-tauri/src/performance.rs` | Default-off, bounded local performance event writer and JSONL/CSV export |
| `src-tauri/src/watcher.rs` | Debounced file watching, per-harness parser dispatch, frontend events |
| `src-tauri/src/session_index.rs` | Thread-name overlay from `session_index.jsonl` |
| `src-tauri/src/commands.rs` | Tauri command boundary |
| `src-tauri/src/config.rs` | Session-root configuration and persistence |
| `src-tauri/src/rates.rs` | Bundled rate card and user override persistence |
| `src-tauri/src/store.rs` | Concurrent in-memory session state and watcher handle |
| `src-tauri/src/telemetry.rs` | Cross-harness normalized tool metrics, classifier, and deterministic optimization findings |
| `src-tauri/src/correlation.rs` | Source-agnostic batched event/window attribution and metric observations |
| `src-tauri/src/config_events.rs` | Dedicated safe configuration resolver, snapshot, watcher, and versioned event log |
| `src-tauri/src/git_outcomes.rs` | Opt-in read-only local commit correlation through `gix` |
| `src-tauri/src/tray.rs` | Native tray lifecycle, menu events, and projected today labels |

## Parser model

Rollouts are append-only JSONL envelopes. Aggregate parsing currently cares about:

- `session_meta`: identity, timestamps, working directory, originator/source, CLI/provider metadata, forks, and subagent lineage.
- `turn_context`: active model, reasoning effort, collaboration mode, and turn identity.
- `event_msg`: first user message, task lifecycle (including abort/rollback), thread settings/service tier, token counts, context window, plan, and credit balance.

Irrelevant `response_item` records are deliberately skipped because they dominate rollout size. Function calls/results selectively fall through to full parsing for normalized telemetry; only call identity, tool name/kind, hashed target identity, outcome, duration, and output byte count survive. The structural fast path still skips every other `response_item`/`compacted` line while advancing `last_event_at`.

`SessionParser.byte_offset` advances only after a newline-terminated record. This is essential: the watcher may observe a file while its final JSON record is still being written.

Token accounting uses two views:

- Latest cumulative `total_token_usage` drives session totals.
- Per-call `last_token_usage` is attributed to the active model and appended to event history. Buckets are reconciled against the cumulative total so resumed sessions and early unassigned usage converge.

Cached input and reasoning output are included within input and output respectively. Credit calculation in `src/lib/credits.ts` subtracts the subsets before applying the ordinary input/output rates, then prices the subsets at their own rates.

Credit history also records `service_tier`. Current documented Fast mode multipliers are applied event-by-event for GPT-5.5 and GPT-5.4; models without a documented Fast rate remain at the standard multiplier.

## Claude Code parser model

Claude Code sessions (`~/.claude/projects/<project>/<uuid>.jsonl`) have no `session_meta` envelope; every line is a self-describing record. The aggregate parser cares about:

- `user`: real human prompts open turns. Tool results, `isMeta` records, sidechain (subagent) prompts, `<command-…>` echoes, and interruption markers are excluded.
- `assistant`: carries the Anthropic API message with `message.usage` and `message.model`. Streamed messages repeat one `message.id` across several lines with identical usage, so usage is counted once per message ID. `<synthetic>` messages are skipped.
- `assistant` `tool_use` and user `tool_result` blocks are paired by tool id and deduplicated across streamed messages into the same normalized telemetry contract as Codex.
- `custom-title` / `summary`: thread-name sources (custom titles win).

Subagent transcripts (`.../<session>/subagents/agent-<id>.jsonl`) carry the parent session's `sessionId` on every record and mark everything `isSidechain`. They parse as their own sessions — identified by file stem, linked via `parent_thread_id`, tagged `source: subagent` — otherwise they would collide with and clobber the parent in the session map. Inside them the sidechain filter is waived so the subagent's task prompt forms its turn. Parent files in the current format do not duplicate this usage inline, so no double counting occurs.

Anthropic usage reports `input_tokens` excluding cache traffic, while the viewer's `TokenTotals` treats cached input as a subset of input. The mapping is `input = input + cache_read + cache_creation`, `cached = cache_read`, `reasoning = 0` (thinking is billed as ordinary output). Cache writes are priced at the plain input rate, a slight underestimate of the 1.25x write premium. There is no cumulative counter in the file; totals accumulate from per-message deltas, and sidechain usage counts toward the enclosing turn.

The rate card prices Codex models in credits and Claude models in USD; `currencies` and `fallback_models` on the card map each harness to its display currency and fallback rate so the two never mix. A third table, `api_models`, holds the same Codex models at OpenAI API USD prices — it powers the Codex tab's informational "Est. $" column and the detail pane's est.-API-cost figures, priced from the same (model, tier) buckets as the credit math.

## IPC and frontend state

`src/lib/ipc.ts` is the only frontend Tauri boundary. It mirrors commands from `src-tauri/src/commands.rs` and listeners for these string contracts:

| Event | Payload | Purpose |
| --- | --- | --- |
| `session-updated` | `SessionSummary` | Insert or replace a session in the list |
| `session-removed` | session ID | Remove a session after its rollout disappears |
| `scan-progress` | `ScanStatus` | Bulk-scan progress for the startup indicator (throttled; final event has `complete: true`) |
| `config-updated` | `Config` | Refresh settings and replace the scanned session set |
| `rates-updated` | `RateCard` | Recompute displayed credit estimates |
| `config-event` | `ExternalEvent` | Append a redacted local configuration-change marker |
| `open-settings` | none | Open Settings from the native tray menu |

The frontend batches incoming `session-updated` events into ~150ms flushes before touching the session store — during the initial scan they arrive by the hundred, and per-event map clones plus re-sorts would stall the UI.

Sessions cross the wire in two shapes. `SessionSummary` (list rows, live updates) carries metadata, cumulative totals, and per-(model, service_tier) `TierBucket`s — credit math is linear per (model, tier), so buckets price usage exactly without the event history. The full `Session` (turns + `tokens_history`) is fetched per-id via `get_session_details` when a session is selected. This matters at scale: a real 704-session corpus serializes to ~195 MB as full sessions but ~1 MB as summaries, and an active session's live update drops from ~2 MB to ~1 KB per emit.

Date-scoped numbers come from the batched `sessions_in_ranges` command. The frontend passes the filtered session IDs, and chronological histories use binary partitioning to visit only each window's relevant slice. It returns per-session `RangeTotals` (tokens, tier buckets, and compact tool metrics). The table, analytics, model comparison, export, tray, and generic correlation engine reuse those maps rather than starting per-row scans.

`src/lib/types.ts` manually mirrors Rust's serialized structs. Rust field or serialization changes therefore require an explicit TypeScript update.

`sessionsStore` is the canonical reactive session collection. `sessionProjection.ts` owns the pure selection, date-scoped pricing, model aggregation, and export rows used by every scope. `SessionsView.svelte` derives ordering, day groups, analytics, comparison, export, event correlation, and selection from that projection; its fixed-height virtual list keeps DOM size bounded for large corpora. `DetailPane.svelte` fetches full details only on demand, including normalized observations, categories, and findings.

## Performance measurements

Application performance tracking is local-only, explicitly opt-in, and disabled by default through `Config.performance_tracking_enabled`. `PerformanceRecorder` starts its bounded writer lazily when enabled, so the off path performs only an atomic flag check. Backend measurements cover setup, watcher/config discovery, bulk discovery and scanning, cache hit/miss/open time, aggregate parser time, incremental parsing, range rollups, correlations, Git evaluation, detail/list IPC, and exports. `src/lib/performance.ts` records frontend initialization, batched store updates and paints, virtual-list paints, range fetches, detail fetches, and export projection work.

Events use a versioned, redacted contract: timestamp, app/platform/process identity, operation name, duration, success, and bounded aggregate metadata. Prompts, tool arguments/output, session IDs, repository paths, and commands are forbidden. A bounded channel keeps measurements off hot paths; overflow increments a dropped counter instead of blocking work. JSONL data lives under the OS local-data directory, rotates between current and previous segments at the Settings-configured size, and can be exported through backend-owned native dialogs as JSONL or CSV.

The `open_task_in_chatgpt` command launches the supported `codex://threads/<id>` deep link. For a subagent rollout, the UI opens its parent task because subagents are not ordinary sidebar tasks. Claude Code sessions have no deep link; the button is hidden for them.

## Auto-update

The app registers `tauri-plugin-updater` and `tauri-plugin-process`. `App.svelte` checks for updates once at startup (silently tolerant of failure: offline, dev builds, or a not-yet-public endpoint) and shows a banner offering a one-click download-and-install with relaunch. Update packages are the platform installers themselves (`createUpdaterArtifacts`), minisign-signed in CI via `TAURI_SIGNING_PRIVATE_KEY`; the public key and the `releases/latest/download/latest.json` endpoint live in `tauri.conf.json`, and `tauri-apps/tauri-action` assembles and uploads `latest.json` per release. Note: the endpoint only resolves once the repository's releases are public and the release is published (drafts don't serve `latest/download` URLs). The private key lives outside the repo (`~/.tauri/`) and in GitHub secrets; losing it orphans every installed copy's update chain.

## Dates and ranges

UI `datetime-local` values are local wall-clock values and must be converted to UTC ISO strings before comparison with rollout timestamps.

- A session matches a date filter when `[started_at, last_event_at]` overlaps the selected interval.
- In a filtered interval, displayed tokens and credits sum history events inside inclusive bounds.
- With no date bounds, cumulative session totals and per-model buckets remain the source of truth.

This distinction matters for sessions that began before the requested range or resumed with cumulative carryover.

## Persistence and privacy

Default inputs are resolved below `$CODEX_HOME`, falling back to `~/.codex`:

- `$CODEX_HOME/sessions`
- `$CODEX_HOME/archived_sessions`
- `$CODEX_HOME/session_index.jsonl`

Claude Code sessions are resolved below `$CLAUDE_CONFIG_DIR`, falling back to `~/.claude`:

- `$CLAUDE_CONFIG_DIR/projects`

User-owned app data is stored under the platform configuration directory in `agent-odometer/config.json` and, after rate edits, `agent-odometer/rates.json`. The fallback rate card is compiled from `src-tauri/rates.json`.

Session files can contain full prompts, responses, system/developer instructions, local paths, and tool output. Keep processing local, avoid logging message bodies, and use synthetic/redacted test data. Tauri capabilities in `src-tauri/capabilities/default.json` should remain narrowly scoped.

## Known limitations

- Watcher parser state is not seeded from the initial scan. Removing a startup-existing file before the watcher has observed a create/modify event may leave its session in memory until a rescan.
- A configured root that does not exist when the watcher starts is skipped; creating it later requires saving settings or restarting the app to establish the watch.
- Sessions are keyed only by ID. Duplicate IDs found under multiple roots overwrite according to scan traversal order.
- An invalid envelope timestamp falls back to the current time and can affect ordering.
- `forked_from_id` is represented in the model/UI but may be absent when the source rollout does not provide or the parser does not extract it.
- Frontend behavior is checked by TypeScript/Svelte validation and manual Tauri runs; no frontend unit-test framework is configured.

## Safe extension patterns

For a new backend field, update the Rust model/parser, add parser coverage, update `src/lib/types.ts`, and then consume it in Svelte. Prefer optional fields or Serde defaults for historical rollout compatibility.

For a new command, implement it in `commands.rs`, register it in `lib.rs`, add a typed wrapper in `ipc.ts`, and expand capabilities only when the API actually requires it.

For watcher changes, test initial files, incremental appends, partial trailing lines, removal, archive roots, session-index updates, and config-triggered restart separately.
