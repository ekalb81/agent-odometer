# Architecture

## System overview

Codex Activity Viewer is a local companion to agent CLI harnesses — the ChatGPT desktop app's Codex experience and Claude Code — with two halves:

- `src-tauri/`: Rust/Tauri backend for discovery, incremental JSONL parsing, filesystem watching, persistence, and native commands.
- `src/`: Svelte 5/TypeScript frontend for reactive state, per-harness tabs, filtering, tables, details, settings, and credit calculations.

Every session carries a `harness` tag (`codex` | `claude_code`). The UI shows one tab per harness; both tabs share the same session store, table, drawer, and filter components.

The frontend starts at `src/main.ts` and `src/App.svelte`. The native process starts at `src-tauri/src/main.rs`, which calls `src-tauri/src/lib.rs::run`.

## Startup and live-update flow

1. `Config::load` reads the platform config file or creates defaults.
2. `scanner::initial_scan` recursively finds JSONL files under active/archive Codex roots and Claude Code roots.
3. `parser::parse_file` (Codex) or `claude_parser::parse_file` (Claude Code) builds a `Session` for each file and stores it in `AppState.sessions`, keyed by session ID.
4. `session_index::read` overlays current thread names from Codex's session index.
5. `watcher::start` watches all configured roots and the session-index parent directory; each changed file gets the parser matching the root it lives under.
6. `App.svelte` invokes `list_sessions`, `get_config`, and `get_rates`, then subscribes to update/removal events.
7. The watcher debounces filesystem activity, incrementally parses complete appended records, updates the `DashMap`, and emits Tauri events.

Saving settings persists the new config, stops the old watcher, clears and rescans state, reapplies thread names, starts a new watcher, and emits `config-updated`.

## Backend modules

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/lib.rs` | Tauri setup, shared state, command registration, initial scan, watcher lifetime |
| `src-tauri/src/model.rs` | Serialized session, harness, turn-status, and token wire models |
| `src-tauri/src/parser.rs` | Full and incremental Codex rollout JSONL parsing |
| `src-tauri/src/claude_parser.rs` | Full and incremental Claude Code session JSONL parsing |
| `src-tauri/src/scanner.rs` | Recursive JSONL discovery and initial parse |
| `src-tauri/src/watcher.rs` | Debounced file watching, per-harness parser dispatch, frontend events |
| `src-tauri/src/session_index.rs` | Thread-name overlay from `session_index.jsonl` |
| `src-tauri/src/commands.rs` | Tauri command boundary |
| `src-tauri/src/config.rs` | Session-root configuration and persistence |
| `src-tauri/src/rates.rs` | Bundled rate card and user override persistence |
| `src-tauri/src/store.rs` | Concurrent in-memory session state and watcher handle |

## Parser model

Rollouts are append-only JSONL envelopes. Aggregate parsing currently cares about:

- `session_meta`: identity, timestamps, working directory, originator/source, CLI/provider metadata, forks, and subagent lineage.
- `turn_context`: active model, reasoning effort, collaboration mode, and turn identity.
- `event_msg`: first user message, task lifecycle (including abort/rollback), thread settings/service tier, token counts, context window, plan, and credit balance.

`response_item` is deliberately skipped by the aggregate parser because it is large and is not needed for session totals. Unknown record types are ignored. Invalid individual records are logged and skipped so one bad line does not hide the rest of a rollout.

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
- `custom-title` / `summary`: thread-name sources (custom titles win).

Anthropic usage reports `input_tokens` excluding cache traffic, while the viewer's `TokenTotals` treats cached input as a subset of input. The mapping is `input = input + cache_read + cache_creation`, `cached = cache_read`, `reasoning = 0` (thinking is billed as ordinary output). Cache writes are priced at the plain input rate, a slight underestimate of the 1.25x write premium. There is no cumulative counter in the file; totals accumulate from per-message deltas, and sidechain usage counts toward the enclosing turn.

The rate card prices Codex models in credits and Claude models in USD; `currencies` and `fallback_models` on the card map each harness to its display currency and fallback rate so the two never mix.

## IPC and frontend state

`src/lib/ipc.ts` is the only frontend Tauri boundary. It mirrors commands from `src-tauri/src/commands.rs` and listeners for these string contracts:

| Event | Payload | Purpose |
| --- | --- | --- |
| `session-updated` | `Session` | Insert or replace a parsed session |
| `session-removed` | session ID | Remove a session after its rollout disappears |
| `config-updated` | `Config` | Refresh settings and replace the scanned session set |
| `rates-updated` | `RateCard` | Recompute displayed credit estimates |

`src/lib/types.ts` manually mirrors Rust's serialized structs. Rust field or serialization changes therefore require an explicit TypeScript update.

`sessionsStore` is the canonical reactive session collection. `SessionsView.svelte` (one instance per harness tab) derives filters, ordering, range-scoped totals, and the open drawer. `SettingsView.svelte` edits roots and rate cards; `RowDrawer.svelte` presents one session; `Sparkline.svelte` is presentation-only.

The `open_task_in_chatgpt` command launches the supported `codex://threads/<id>` deep link. For a subagent rollout, the UI opens its parent task because subagents are not ordinary sidebar tasks. Claude Code sessions have no deep link; the button is hidden for them.

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

User-owned app data is stored under the platform configuration directory in `codex-data-viewer/config.json` and, after rate edits, `codex-data-viewer/rates.json`. The fallback rate card is compiled from `src-tauri/rates.json`.

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
