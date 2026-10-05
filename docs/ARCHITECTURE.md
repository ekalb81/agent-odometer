# Architecture

## System overview

Odometer is a local companion to Codex (the ChatGPT desktop app), Claude Code, and Gemini CLI. It provides a Tauri desktop UI plus headless reports and a read-only stdio MCP server:

- `src-tauri/`: Rust/Tauri backend for discovery, incremental JSONL parsing, filesystem watching, persistence, and native commands.
- `src/`: Svelte 5/TypeScript frontend for reactive state, an All tab plus one tab per provider, filtering, projection/export, tables, details, settings, and presentation of backend-priced costs.

Each session carries a `harness` tag (`codex` | `claude_code` | `gemini_cli`). The All tab and all three provider tabs share one session store, `SessionsView`, detail pane, filter predicates, pricing projection, and model aggregate. The All tab never adds plan credits to USD.

The frontend starts at `src/main.ts` and `src/App.svelte`. The native process starts at `src-tauri/src/main.rs`. Normal launches call `src-tauri/src/lib.rs::run`; an explicitly installed harness hook exits through the headless `turn_receipts::try_run_cli` path before Tauri starts. The `agent-odometer` report CLI and stdio MCP server bypass Tauri and query the same Rust services through a read-only connection to the prepared durable-history database; they do not discover transcripts, migrate the database, or write accounting data.

## Startup and live-update flow

1. `Config::load` reads the platform config file or creates defaults.
2. `watcher::start` begins watching all configured roots immediately, so changes during the initial scan are not missed.
3. `scanner::scan_all` bulk-loads existing sessions on a background thread, parsing files in parallel (rayon) and emitting a `session-updated` summary per file — the window is interactive immediately and the list populates progressively. A persistent SQLite scan cache (`scan_cache.rs`, stored under the OS cache directory and keyed by file size+mtime, versioned by app release) serves unchanged files without re-reading them. Each scan touches or replaces individual rows and prunes unseen generations, avoiding whole-corpus cache deserialization and rewrites. The previous JSON cache is imported on first use. Progress flows to the UI via throttled `scan-progress` events and the `get_scan_status` command.
4. The provider registry dispatches each source to `parser.rs` (Codex), `claude_parser.rs` (Claude Code), or `gemini_parser.rs` (Gemini CLI JSONL, version 0.39 and later). Before publication, `history_store.rs` reconciles the parsed source with the durable archive, assigns a path-independent `storage_id`, and records the source observation and normalized token-event suffix transactionally.
5. `AppState.sessions` keeps the live session projection by durable `storage_id` for the desktop UI. Date-window accounting uses the durable ledger's event and rollup tables when available, with a bounded in-memory fallback for stale or unavailable session records. Multiple paths on one event lineage converge on one session; reused provider IDs with divergent lineages remain separate collision records.
6. `session_index::read` overlays current thread names from Codex's session index after the scan and advances the current materialized snapshot without changing source ownership.
7. `App.svelte` invokes `list_sessions`, `get_config`, and `get_rates`, then subscribes to update/removal events.
8. The watcher debounces filesystem activity, incrementally parses complete appended records, reconciles each result through the same durable-history boundary, updates the `DashMap`, and emits Tauri events.

Saving watched-root settings persists the new config, stops the old watcher, clears state, restarts the watcher, kicks off the same background rescan, and emits `config-updated`. Performance and turn-receipt settings apply without restarting watchers or rescanning the corpus; receipt setup additionally performs its bounded harness-config transaction.

## Durable-history contract

The durable history in `history_store.rs` is a source of retained session truth, not a parsing optimization. It is deliberately separate from the disposable, app-versioned scan cache:

- The archive lives under the platform local-data directory as `agent-odometer/history-v1.sqlite3`. It uses WAL mode, foreign keys, a five-second busy timeout, and forward-only `PRAGMA user_version` migrations. A schema newer than the running application is rejected instead of opened unsafely.
- A scan, watcher removal, moved transcript, missing configured root, or cache eviction never deletes a durable session, its current snapshot, an artifact, or a normalized token event. Source disappearance changes `source_availability` to `missing`; `archived` remains the provider's separate archive classification. Replacing a full materialized snapshot prunes its superseded copy; normalized events retain the append history without repeatedly storing the growing `Session` blob.
- The database contains materialized `Session` snapshots and normalized token events, so it is sensitive local application data. It inherits the same no-upload/no-log handling as the source transcripts and is not safe to treat as an anonymous metrics database.

Identity and reconciliation follow these invariants:

- Provider IDs are harness-namespaced: `codex:thread:<id>`, `claude_code:session:<id>`, and `gemini_cli:session:<id>`. Claude subagents use `claude_code:subagent:<parent-session-id>:<agent-id>`; only legacy subagents without a provider `agentId` may use a filename-stem fallback.
- A filesystem path is an availability observation, never logical identity. Stored path keys normalize separators, remove Windows verbatim prefixes, and fold case on Windows so scanner and watcher events address the same location.
- Provider identity, the first-event fingerprint, and an append-compatible token-event lineage reconcile copies and moves. Equal histories and prefix histories are one lineage. Divergence after a shared prefix creates a deterministic `:collision:<lineage-hash>` storage ID and marks every final session claiming that provider identity as a collision; neither transcript overwrites the other.
- A source observation can advance the materialized snapshot only when it has more token-history events, or the same event count with an equal-or-newer `last_event_at`. Metadata overlays such as session-index names advance the snapshot version and may update display metadata without creating or reassigning a source location; only the current full snapshot is retained.
- Token events are keyed deterministically within a durable session. Repeated scans and watcher passes are idempotent, while appends insert only the unseen suffix. Direct request-input evidence added by a later schema is backfilled without replaying or duplicating earlier events.

Availability publication is generation-safe. A completed scan marks unseen paths missing only when it is still the newest durable generation and the scan had zero parse failures; an incomplete scan retains the previous availability rather than inventing deletions. Per-path tombstones and the settings-transition lock prevent an older bulk-scan callback from resurrecting a watcher-removed or reconfigured source. If the history database cannot be opened or written, Odometer logs a warning and keeps live parsing available, but it makes no durability, move-reconciliation, or collision-preservation claim for that failed operation.

The durable archive now provides normalized session facts and hourly rollups for date-window accounting. Desktop range queries use the ledger when it is ready and return a retryable error while history is pending; they fall back to full session data only for stale or failed ledger reads. The desktop can explicitly rebuild history from source transcripts, with progress and cancellation. CLI/MCP reports and `verify` use a read-only connection and require an initialized database with the current schema; they do not perform that rebuild.

## Opt-in turn-receipt flow

Turn receipts are a separate, default-off freshness path; they do not replace the watcher:

1. Settings transactionally reconciles one identifiable completion command per selected harness while
   retaining unrelated settings and handlers. Codex preserves an existing Odometer source; for a
   new integration it edits the `[[hooks.Stop]]` representation in inline `config.toml` when those
   hooks already exist, otherwise `hooks.json`. Other valid inline-array TOML shapes and symlinked
   config files fail closed rather than being rewritten or duplicated. Repair removes Odometer-owned
   duplicates across both sources. Claude Code uses a direct executable `command` plus explicit
   `args` in the user-level `settings.json`, shared by Claude Code 2.1.139+ CLI and local Desktop Code
   sessions; remote and SSH sessions use configuration on their host. Gemini CLI uses a named
   `AfterAgent` command in `~/.gemini/settings.json` with a 5000 ms timeout, following its
   [official hook contract](https://geminicli.com/docs/hooks/reference/). It has a separate
   default-off selection, including for already-enabled receipt setups. Only supported JSONL
   transcripts under its configured live roots are accepted; old JSON sessions remain unsupported.
   Its API estimate uses shared Rust pricing, and no quota is invented. `hooksConfig.enabled` and
   the disabled-handler list are preserved and reflected in status; project/system overrides may
   still prevent execution, so configuration is not reported as an observed receipt.
   For AppImage launches, the installed hook
   uses the absolute `APPIMAGE` path only when the running executable resolves inside the matching
   absolute `APPDIR`; otherwise status and setup both use the current executable. Disable owns only
   handlers whose parsed command or argument list contains the exact
   `--integration-id odometer-turn-receipts-v1` value (the `--integration-id=...` form is also
   recognized), never an arbitrary substring.

   Every planned source is checked against its original bytes immediately before replacement. New
   files use an atomic no-replace rename. Existing files use `ReplaceFileW` with no ignore flags on
   Windows, which makes ACL/attribute/stream merge failures fatal and leaves a random same-volume
   recovery file until commit. The documented `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2` partial state is
   repaired back to a canonical path before failure is returned. Linux uses `renameat2` exchange and
   macOS uses `renamex_np` swap; Linux fails closed for multiple hard links, extended attributes or
   ACLs, and owner/group/mode metadata it cannot reproduce, while macOS copies file metadata before
   swapping. Other Unix platforms fail closed rather than using a non-atomic fallback. Temporary and
   recovery names are random; Unix temporary files start mode `0600`. For an existing Windows file,
   the temporary copy is seeded with the original file's security resource properties before its
   contents are rewritten, and replacement merges the original security metadata again before the
   replacement becomes active. File contents are
   flushed before and after replacement, and containing directories are flushed where the platform
   supports it.

   A path-based byte or supported metadata edit observed before the swap aborts rather than being
   overwritten. Rollback swaps first and verifies the displaced file before deletion, so an edit
   ordered before that swap is restored instead of clobbered. Commit atomically detaches the recovery
   name before checking it; an already-open-handle edit visible in that check is preserved with an
   actionable error. This is not a claim that writes racing the final verification, or writes through
   a superseded handle afterward, can be detected forever; ordinary OS open-handle semantics apply.
   An incomplete multi-file write restores only files that still match Odometer's applied bytes, and
   cleanup or rollback failures are surfaced or logged with the retained recovery path rather than
   silently ignored. Config-save failure paths call the fallible rollback explicitly so incomplete
   restoration is included in the settings IPC error rather than only appearing in logs.
2. The selected harness passes bounded JSON on stdin, including the session and transcript path. The helper
   exits before Tauri startup, checks that the feature and harness remain enabled, and validates the
   canonical transcript path against the configured roots.
3. The helper parses the exact transcript synchronously, selects the supplied Codex `turn_id` or the
   latest completed Claude turn, and emits the common `{ "continue": true, "systemMessage": ... }`
   `Stop` response. Every failure is fail-open and never returns a continuation decision.
4. The ordinary 250 ms filesystem watcher still updates the dashboard and remains the universal
   path for users who never enable receipts. A later watcher pass is harmless because parser cursors
   and cumulative reconciliation remain idempotent.

Integration status separates request state, configuration presence, and receipt observation. The
wire field `receipt_observed` becomes true only after a successful receipt newer than the selected
user-level configuration is observed; merely finding the command never produces a green state. This
is historical evidence, not proof that higher-precedence managed or project policy still permits the
hook. The payload also names the active configuration source and carries explicit restart and Codex
trust-review recommendations.

Codex `rate_limits.primary` and `secondary` snapshots are retained only on full sessions. A receipt
compares the final snapshot for the turn with the prior turn's snapshot when the limit/reset identity
is stable. It reports the delta in percentage points as **observed**, `no measurable change` when the
provider value is unchanged, or `unavailable/window changed` rather than inventing precision. These
snapshots are account-wide and can include concurrent activity or rolling-window expiry.

The headless hook and desktop commands use the same pricing service in `query.rs`. Receipts price
the selected turn and cumulative session buckets through that service; `turn_receipts.rs` owns
receipt selection and formatting, not a separate token formula. Frozen pricing expectations and
focused receipt tests protect subset accounting, model resolution, and Fast-tier behavior.

## Backend modules

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/lib.rs` | Tauri setup, shared state, command registration, initial scan, watcher lifetime |
| `src-tauri/src/model.rs` | Serialized session, harness, turn-status, and token wire models |
| `src-tauri/src/provider.rs` | Built-in provider registry, parser adapters, and capability descriptors |
| `src-tauri/src/parser.rs` | Full and incremental Codex rollout JSONL parsing |
| `src-tauri/src/claude_parser.rs` | Full and incremental Claude Code session JSONL parsing |
| `src-tauri/src/gemini_parser.rs` | Full and incremental Gemini CLI session JSONL parsing |
| `src-tauri/src/scanner.rs` | Recursive JSONL discovery, cached parallel initial parse |
| `src-tauri/src/scan_cache.rs` | Incremental SQLite parsed-session cache keyed by file size+mtime |
| `src-tauri/src/history_store.rs` | Durable SQLite session archive, normalized events and rollups, source availability, identity reconciliation, and schema migration |
| `src-tauri/src/performance.rs` | Default-off, bounded local performance event writer and JSONL/CSV export |
| `src-tauri/src/watcher.rs` | Debounced file watching, per-harness parser dispatch, frontend events |
| `src-tauri/src/session_index.rs` | Thread-name overlay from `session_index.jsonl` |
| `src-tauri/src/commands.rs` | Tauri command boundary |
| `src-tauri/src/config.rs` | Session-root configuration and persistence |
| `src-tauri/src/rates.rs` | Bundled rate card and user override persistence |
| `src-tauri/src/query.rs` | Shared model, token, and bucket pricing; final range enrichment |
| `src-tauri/src/query_desktop.rs` | Cumulative summary/category, detail/turn, and dated-scenario pricing |
| `src-tauri/src/query_reports.rs` | Shared headless reports over ledger facts and lightweight category projections |
| `src-tauri/src/query_control.rs` | Request cancellation, deadlines, and materialization limits |
| `src-tauri/src/headless.rs` | Query selection and validation shared by CLI/MCP adapters |
| `src-tauri/src/report_cli.rs` / `report_output.rs` | Command-line adapter and versioned export formatting |
| `src-tauri/src/mcp_server.rs` | Bounded concurrent read-only stdio requests and cancellation |
| `src-tauri/src/integration_status.rs` | Shared status and bounded diagnostic vocabulary; headless scan readiness stays unknown |
| `src-tauri/src/mcp_integration.rs` | Explicit scoped MCP entry previews, race-safe apply, private backups and manual recovery |
| `src-tauri/src/integration_activity.rs` | Bounded allowlisted local call metadata, excluding all request/result bodies |
| `src-tauri/src/store.rs` | Concurrent in-memory session state and watcher handle |
| `src-tauri/src/telemetry.rs` | Cross-harness normalized tool metrics, classifier, and deterministic optimization findings |
| `src-tauri/src/tool_impact.rs` | Provider/tool target discovery, observed-use cohorts, and matched observational baselines |
| `src-tauri/src/correlation.rs` | Source-agnostic batched event/window attribution and metric observations |
| `src-tauri/src/config_events.rs` | Dedicated safe configuration resolver, snapshot, watcher, and versioned event log |
| `src-tauri/src/git_outcomes.rs` | Opt-in read-only local commit correlation through `gix` |
| `src-tauri/src/instructions.rs` | Opt-in bounded instruction discovery, hierarchy, warning signals, and allowlisted preview reads |
| `src-tauri/src/tray.rs` | Native tray lifecycle, menu events, and projected today labels |
| `src-tauri/src/harness_integration.rs` | Transactional install/remove/status for Odometer-owned harness hooks |
| `src-tauri/src/turn_receipts.rs` | Headless fail-open hook entry point, targeted parse, receipt/quota formatting |

## Parser model

Rollouts are append-only JSONL envelopes. Aggregate parsing currently cares about:

- `session_meta`: identity, timestamps, working directory, originator/source, CLI/provider metadata, forks, and subagent lineage.
- `turn_context`: active model, reasoning effort, collaboration mode, and turn identity.
- `event_msg`: first user message, task lifecycle (including abort/rollback), thread settings/service tier, token counts, context window, plan, and credit balance.

Irrelevant `response_item` records are deliberately skipped because they dominate rollout size. Legacy `function_call` and current `custom_tool_call` records selectively fall through to full parsing for normalized telemetry; only call identity, tool name/kind, bounded MCP provider tags, bounded effective tool names, hashed target identity, outcome, duration, and output byte count survive. Provider and effective-tool tags are inferred from direct names and executed orchestration payloads, but the payload itself is discarded. The structural fast path still skips every other `response_item`/`compacted` line while advancing `last_event_at`.

`SessionParser.byte_offset` advances only after a newline-terminated record. This is essential: the watcher may observe a file while its final JSON record is still being written.

Token accounting uses two views:

- Latest cumulative `total_token_usage` drives session totals.
- Per-call `last_token_usage` is attributed to the active model and appended to event history. Buckets are reconciled against the cumulative total so resumed sessions and early unassigned usage converge.

Cached input and cache-creation input are disjoint subsets of input; reasoning output is included within output. The shared Rust pricing service subtracts the subsets before applying the ordinary input/output rates, then prices each subset at its own rate.

Credit history also records `service_tier`. Pricing resolves canonical model aliases before applying supported speed rules. Unknown tiers and unsupported speed/model combinations are unavailable; a missing/default tier retains the configured Standard reference.

## Claude Code parser model

Claude Code sessions (`~/.claude/projects/<project>/<uuid>.jsonl`) have no `session_meta` envelope; every line is a self-describing record. The aggregate parser cares about:

- `user`: real human prompts open turns. Tool results, `isMeta` records, sidechain (subagent) prompts, `<command-…>` echoes, and interruption markers are excluded.
- `assistant`: carries the Anthropic API message with `message.usage` and `message.model`. Streamed messages repeat one `message.id` across several lines with identical usage, so usage is counted once per message ID. `<synthetic>` messages are skipped.
- `assistant` `tool_use` and user `tool_result` blocks are paired by tool id and deduplicated across streamed messages into the same normalized telemetry contract as Codex.
- `custom-title` / `summary`: thread-name sources (custom titles win).

Subagent transcripts (`.../<session>/subagents/agent-<id>.jsonl`) carry the parent session's `sessionId` on every record and mark everything `isSidechain`. They parse as their own sessions — identified by file stem, linked via `parent_thread_id`, tagged `source: subagent` — otherwise they would collide with and clobber the parent in the session map. Inside them the sidechain filter is waived so the subagent's task prompt forms its turn. Parent files in the current format do not duplicate this usage inline, so no double counting occurs.

Anthropic usage reports `input_tokens` excluding cache traffic, while the viewer's `TokenTotals` treats cached input and cache creation as two disjoint subsets of input. The mapping is `input = input + cache_read + cache_creation`, `cached = cache_read`, `cache_creation = cache_creation`, `reasoning = 0` (thinking is billed as ordinary output). `cache_creation_input_tokens` is its own field, distinct from `cached_input_tokens`, so credit calculation can price the cache-write premium at its own rate instead of folding it into the plain input rate (see Pricing-catalog contract below). There is no cumulative counter in the file; totals accumulate from per-message deltas, and sidechain usage counts toward the enclosing turn.

## Pricing-catalog contract

The rate card has two intentionally separate pricing layers:

- The legacy `models` map prices Codex plan credits and Claude API USD, with `currencies` and `fallback_models` keeping harness units and unknown-model fallbacks separate. `api_models` supplies the Codex tab's flat informational API-USD comparison. These maps remain authoritative for list summaries, range buckets, existing views, and editable user overrides. Every `ModelRate` carries `cache_creation_input` alongside `input`/`cached_input`/`output`/`reasoning` — a normalized dimension distinct from both, so cache-creation (write) tokens are priced at their own rate instead of the plain input rate. `cached_input_tokens` and `cache_creation_input_tokens` are disjoint subsets of `input_tokens`: `token_cost` in `src-tauri/src/query.rs` subtracts both subsets before pricing the remainder at the ordinary input rate, so neither subset is ever priced twice.
- `pricing_catalog` supplies opt-in, event-level scenarios. Every base period and conditional modifier has a stable ID, billing `surface`, exact model, half-open UTC interval `[from, to)`, label, and provenance (`evidence`, source URL, verification timestamp, and optional note). A rule never crosses from OpenAI API USD, Anthropic API USD, Gemini API USD, or Codex plan credits to another surface.

Catalog validation is fail-closed. Rule IDs, models, labels, evidence, and source URLs must be non-empty; IDs must be globally unique; interval ends must follow starts; cache-write and conditional multipliers must be positive; base periods for one `(surface, model)` cannot overlap; and modifiers with the same `(surface, model, condition)` cannot overlap. Adjacent periods are valid. A malformed or invalid on-disk override logs a warning and falls back to the bundled card.

Flat rates remain current Standard reference estimates, including for cumulative historical usage. `flat_rate_expires_at` is an exclusive UTC boundary that excludes a promotional flat reference rather than silently pricing it forever or applying a new price to history. Gemini 3.8 Flash's January 1, 2027 doubling is encoded in the existing dated catalog. After that boundary its flat reference is unavailable until a reviewed bundle/card replaces it; event-level Gemini scenarios continue to select the correct pre/post-boundary period. These scenarios assume paid text and do not claim coverage of modality, cache storage, tools, or other service tiers.

Time-aware pricing follows these rules:

- A full session is evaluated event by event at the event timestamp. Every priced event must identify a model and have a direct catalog period for its model and billing surface. Missing coverage, a known-unpriced model, or an unattributed nonzero event makes the time-aware scenario unavailable (`null`); it never substitutes a fallback model, the latest rate, or the flat reference retroactively.
- Conditional rules operate on one provider request. The current threshold condition applies only when observed `request_input_tokens` is strictly greater than the configured threshold. Missing request-level evidence never triggers a modifier speculatively; the result names the applicable rule under `conditional_evidence_missing`.
- Applicable input multipliers compose multiplicatively and cover ordinary, cached, and cache-creation input. Output multipliers likewise cover ordinary and reasoning output. The existing service-tier multiplier is then applied to the event.
- A period's declared cache-write multiplier (`cache_write_input_multiplier`) is provenance metadata; the dollar amount for cache-creation tokens comes from the period's own `rate.cache_creation_input`. `cache_write_pricing_unmodeled` is only `true` when a period declares a multiplier but no event in the session ever reported nonzero `cache_creation_input_tokens` under it — once cache-creation tokens are observed, the premium is priced directly rather than left as unobserved metadata.
- The scenario result carries the IDs of every applied period and modifier. The flat reference remains visible beside it so a dated or conditional scenario cannot silently redefine existing totals.

`unpriced_models` identifies known models without a published rate; flat calculations exclude and label their usage rather than applying a fallback. `free_local_models` identifies models that are explicitly zero-cost (free tier, local/self-hosted) — a distinct, deliberate declaration from `unpriced_models` (no published price) or an ordinary unresolved rate. Other unknown model IDs first resolve through `model_aliases` (raw provider id -> canonical rate-table key); alias chains may hop multiple entries, and a cycle is detected and terminated deterministically rather than looping. Unresolved model IDs then use the configured per-harness fallback only in the legacy flat calculation and remain named in the UI. Rust's `RateCard::resolve_model_pricing` is the shared model resolver used by query pricing; it records why a rate was selected: `direct`, `aliased`, `fallback`, `estimated`, `free_local`, `subscription`, `stale`, or `unavailable` (`PricingBasis`). The dashboard renders returned provenance as visually distinct badges (an amber ⚠ for fallback, an amber ◇ for unpriced, a blue ↝ for aliased) in the session detail pane and model-comparison table rather than resolving model rates itself or collapsing every non-exact price into one indicator.

When an older user rate card is upgraded, known older values are compared with their exact archived bundles in `src-tauri/rate-history/` (versions 11–13). Matching rows and floating aliases follow the revised bundle; differing values and user-chosen expiries survive. Equality is the recoverable signal, not proof of intent: an identical deliberate entry is indistinguishable from a former default. Unknown older bundles retain existing entries and mark them for review. New keys still inherit, while custom currencies, units, fallback choices, plans and separately identified catalog rules survive. Managed catalog rules refresh by stable ID unless they overlap a separately identified saved rule; the saved interval takes precedence so an upgrade cannot invalidate the entire card. `rate_provenance` belongs to a specific table/id row, `upgrade_review` names retained edits or unknown origins, and merging never advances the card reference date when such entries remain. The Settings editor preserves these fields and floating aliases, removes evidence from edited rows, displays retained-value warnings, and offers a confirmed whole-card reset. Editing a promotional flat row removes its old expiry and verification, marking the revised reference for review. Expired-only totals display unavailable; mixed totals label excluded usage while retaining all tokens. Saving validates the catalog before writing an adjacent temporary file and renaming it into place.

The archived-default comparison also applies to catalog periods and modifiers. A same-ID edited rule or a rule from an unknown older bundle remains intact and is listed for review; its ID alone does not establish that it is an unchanged default.

The remaining rate-card contracts are local and keep original accounting units intact:

- `subscription_plans` records user-declared plan names, prices, currencies, notes and optional baseline savings. It never infers plan-equivalent token allowances.
- `display_currency` carries `{ from_currency, target_currency, rate, as_of, source }`. Settings edits this offline input; Rust validates supported monetary currencies, a finite positive rate, timestamp and bounded source. Legacy settings infer a monetary card currency only. `convert_total` adds an optional backend `ConvertedTotal` to priced surfaces and reports without replacing originals. Credits, unrelated originals, unavailable-only totals and overflowing products are never restated. The UI displays the returned amount and source/timestamp; it does no exchange or token arithmetic.
- `refresh` describes explicit candidate validation age, separate from the catalog reference date. `apply_refresh_candidate` rejects invalid rates/catalog/FX metadata or dropped price coverage and retains the previous card with a failure reason. It is an offline candidate boundary, not a separate network fetch or evidence of an updater check.

New bundled cards are compiled into the app by `include_str!("../rates.json")` and delivered in full application updates through the existing updater endpoint. `tauri.conf.json` requires version-bound signatures; release packaging creates signed updater artifacts using the maintainer's signing secret. The installed updater verifies the downloaded artifact and signed version before installation. This is application delivery, not independent card hot-refresh; no host, capability or outbound request was added. Updating the app changes its embedded card, and normal startup merges only recognized unchanged saved defaults. The frozen version-13 bundle ensures a later release can distinguish those defaults from user edits. Row evidence and retained custom values are not falsely dated as newly verified.

Reads validate a bounded regular `rates.json`, then a bounded validated `rates.last-valid.json` if the active file is unreadable/invalid, then the embedded card. They create or repair nothing; an absent active override uses embedded defaults. Explicit saves validate before replacing the active file atomically and then attempt an atomic backup update. A backup failure leaves the committed valid active file intact and returns/logs a fixed warning; recovery may then use an older valid backup or the embedded card. Runtime-only `delivery` metadata identifies the selected source, app/card versions and recovery reason; persisted input cannot spoof it. Saving an invalid card changes neither file. This recovery does not roll back an installed application binary.

## IPC and frontend state

`src/lib/ipc.ts` is the only frontend Tauri boundary. It mirrors commands from `src-tauri/src/commands.rs` and listeners for these string contracts:

| Event | Payload | Purpose |
| --- | --- | --- |
| `session-updated` | `SessionSummary` | Insert or replace a session in the list |
| `session-removed` | session ID | Remove a session after its rollout disappears |
| `scan-progress` | `ScanStatus` | Bulk-scan progress for the startup indicator (throttled; final event has `complete: true`) |
| `config-updated` | `Config` | Refresh settings and replace the scanned session set |
| `rates-updated` | `RateCard` | Replace the rate card and refetch backend prices |
| `config-event` | `ExternalEvent` | Append a redacted local configuration-change marker |
| `open-settings` | none | Open Settings from the native tray menu |

The frontend batches incoming `session-updated` events into ~150ms flushes before touching the session store — during the initial scan they arrive by the hundred, and per-event map clones plus re-sorts would stall the UI.

Sessions cross the wire in two shapes. `SessionSummary` (list rows, live updates) carries metadata, cumulative totals, and per-(model, service_tier) `TierBucket`s — credit math is linear per (model, tier), so buckets price usage exactly without the event history. The full `Session` (turns + `tokens_history`) is fetched per-id via `get_session_details` when a session is selected. This matters at scale: a real 704-session corpus serializes to ~195 MB as full sessions but ~1 MB as summaries, and an active session's live update drops from ~2 MB to ~1 KB per emit.

Date-scoped numbers come from the batched `sessions_in_ranges` command. The frontend passes the filtered session IDs; the backend reads ledger rollups and exact partial-hour edges, with full-history fallback for unavailable or stale ledger entries. It returns per-session `RangeTotals` (tokens, tier buckets, and compact tool metrics). The table, analytics, model comparison, export, tray, and generic correlation engine reuse those maps rather than starting per-row scans.

Issue #248 keeps `plan` and `api` as legacy references for compatibility; their credit labels now say legacy. Additive `current` carries `as_of`, `purchased_credits`, `included_allowance` (Standard-credit equivalents, never a quota percentage), and `api_estimate`. Batch range and summary responses reprice their recorded buckets at the query date; they do not claim historical billed allocation. Session details additionally expose `dated_purchased_credits` and `dated_included_allowance` only when every event has covered dated rules. Unknown speed combinations are unavailable even in the legacy path; Standard direct/custom flat rates remain 1× references, and unknown Standard models may retain flagged fallback estimates. `included_allowance_basis` explicitly identifies the Standard purchased-credit-rate reference assumption: it is never provider-observed allowance usage or quota authority. Current references use the editable flat Standard rate; dated estimates require event-time catalog evidence.

The existing catalog represents service tiers with `PricingCondition::ServiceTier`. Half-open intervals, source evidence, uniform token multipliers, and no overlapping `(surface, model, tier)` rules are validated. The current rule lower bound is the October 4, 2026 verification date, not an inferred launch date. Alias resolution uses canonical identity for tiers. A fallback rate cannot prove speed support. `priority` is a verified persisted Codex spelling of Fast; the local bounded sample did not observe `fast` or `ultrafast`, so documented scenario tests are not presented as observed fixtures.

The four current OpenAI models have full-request API modifiers above 272,000 input tokens: input/cache ×2, output/reasoning ×1.5. Exactly 272,000 stays at the base rate. Missing full-request evidence is reported and leaves only a base-rate scenario. Codex credit/allowance surfaces reject API thresholds and charge zero cache writes; no parser adds Codex cache-write tokens. Claude's existing cache-creation dimension is five-minute writes. Source verification and frozen regression constants live in `billing_surfaces_regressions.rs`; legacy oracle files remain immutable with explicit correctness corrections in their Rust harnesses.

`RangeTotals.pricing` is optional: `{ plan: PricedSurface, api: PricedSurface | null }`. Each surface contains `total`, `by_model` (model, cost, basis, unpriced), `missing_models`, and `unpriced_models`. The shared Rust query service attaches this only after final range aggregation, using one loaded rate card and one timestamp per batch. Raw aggregates remain unpriced; this derived field is not persisted in the ledger or scan cache. Unknown provider identity leaves pricing absent, and an unsupported API surface is null rather than zero. Older payloads without the field remain valid.

These server estimates are authoritative for rendered costs, exports, and tray figures. All-time session and category prices come from the batched `get_session_pricing` command, which prices resident summary buckets and preserves their no-history fallback; an unbounded event window is not substituted for cumulative usage. `get_session_details` returns a response-only flattened session with plan, flat API, per-turn, and dated-scenario pricing, including the rule metadata from the same rate snapshot. Detail totals retain historical per-event subset clamping before costs are combined, even when corrected usage counters temporarily make an event's subset exceed its parent. Correlation responses carry before/after pricing per harness. None of these derived prices are persisted in session snapshots.

Table, analytics, tray, selected details, and correlation requests refresh when a saved rate-card object changes, including edits that retain the same card version. A rate-only refresh retains raw session data but marks outdated costs unavailable until replacement prices arrive. Request epochs prevent superseded successes or failures from updating replacement caches or tray output; ordinary session updates still fetch only changed IDs. Exports await matching backend prices and reject an incomplete response or changes to exported sessions or rates during that request. Model analytics sum server model costs once per session/model, independently of the number of service-tier token buckets.

The frontend formats money through `src/lib/currency.ts` and combines already-priced values; it contains no token-pricing engine. Frozen synthetic oracles under `tests/conformance/` preserve bucket and detailed pricing expectations, including no-history, turn, and dated-scenario behavior. Rust tests exercise the production service against those fixed expectations. Browser-only mocks consume Rust-generated synthetic prices; fixture-only range interpolation supports deterministic UI scenarios and is not a production pricing implementation.

`list_tool_impact_targets` discovers provider and individual-tool choices from the same filtered sessions and time window. `compare_tool_impact` then builds turn-level observed and not-observed cohorts for the selected target. When at least three comparisons are available, the UI uses nearest-in-time pairs with the same harness, model, and deterministic task category. This is observational: transcripts prove use, but cannot prove whether an unused target was installed or available, and whole overlapping turns are included because token events do not carry turn IDs.

Configuration tracking resolves global harness roots plus project scopes derived from session working directories (using the containing Git worktree when available). Only known settings/instruction files and bounded hook/skill trees are watched. Events retain hashes, sizes, a size-only safe diff, and a hashed path identity; they never persist config values or raw paths. The watcher is rebuilt after each session scan so newly discovered project scopes and settings-root changes share the same coverage. Config markers appear on the existing spend chart and the timeline reports before/after tokens, turns, active session duration, tool metrics, samples, and confounds through the source-agnostic correlation engine.

The optional Instructions view is deliberately separate from that redacted event pipeline. When enabled, `instructions.rs` inventories the known global harness folders, Git worktrees inferred from lightweight session directory/activity snapshots, and user-configured roots. Observed session directories are admitted only when Git discovery succeeds; a non-repository working directory never becomes an implicit recursive root and must be added explicitly if desired. Repeated working directories are resolved once per scan, and recursive ancestors cover nested roots without traversing those subtrees again. Each configured root is folder-only or recursive; recursive walks do not follow links, are bounded by depth, matching-file count, and total filesystem entries, and skip common VCS, dependency, and generated-output trees. The backend emits `instruction-scan-progress` while it prepares roots, walks entries, and analyzes matches; one shared frontend store feeds both the inline scan detail and the persistent bottom status bar without polling. Config capture and scan-generation allocation share the settings-transition lock, while generation checks and allowlist replacement share the allowlist lock. Instruction-source changes advance the generation and clear the allowlist atomically; direct cancellation and scan supersession advance it under the same publication lock. Cancelled, superseded, or reconfigured work therefore cannot publish an inventory, restore revoked paths, or leave stale progress visible. The wire model keeps harness ownership as a list of strings so future harness definitions do not require replacing the inventory contract, although only Codex `AGENTS.md` and Claude Code `CLAUDE.md` are admitted today.

Inventory reads are fail-closed: the UI must refresh discovery first, only discovered regular files with supported names enter the in-memory allowlist, symbolic links are rejected, and preview content is capped at 1 MiB. The frontend renders Markdown with Marked and sanitizes the HTML with DOMPurify; active links, remote media, embedded forms, scripts, styles, SVG, and other executable/remote surfaces are removed. Opening a file uses the platform's default application only after the same backend allowlist check.

Warnings are deterministic review signals rather than semantic verdicts. Duplicates share an exact normalized-content hash; possible conflicts are opposite `always`/`must` versus `never`/`must not`/`do not` directives within one effective ancestor chain; oversized files exceed 64 KiB or 800 lines; possibly stale files are unchanged for 180 days while their project has agent activity in the last 30 days. Selecting a file filters redacted configuration events by its normalized path identity and reuses the existing seven-day before/after correlation view.

Optimization findings are timestamped at the observation that triggered them. `RangeTotals.optimization_findings_count` therefore scopes precomputed findings by date without rerunning the analyzer for every analytics window. The analyzer keeps exact read-request identity separate from the private resource identity: ranges/pages are distinct requests, while a mutation of the same resource resets prior-read streaks. Volatile polling reads and neutral tool ratios are not optimization findings. Findings carry confidence, occurrence count, and a conservative likely-avoidable-call estimate; compact summaries expose severity and rule breakdowns without shipping full observations to the list view.

`src/lib/types.ts` manually mirrors Rust's serialized structs. Rust field or serialization changes therefore require an explicit TypeScript update.

`sessionsStore` is the canonical reactive session collection. `sessionProjection.ts` owns the pure selection, projection of backend prices, model aggregation, and export rows used by every scope. `SessionsView.svelte` derives ordering, day groups, analytics, comparison, export, event correlation, and selection from that projection; its fixed-height virtual list keeps DOM size bounded for large corpora. `DetailPane.svelte` fetches full details only on demand, including normalized observations, categories, findings, and prices.

## Local project assignments

The session detail pane can move one session into an existing project, split it into a
standalone project, or restore its detected project. Settings retains project alias and
merge management. These edits use the existing local override store and durable session
keys; they do not modify source transcripts or session summaries.

`resolve_projects` returns additive `overridden_session_keys` on each `ProjectInfo`,
covering explicitly reassigned sessions. The frontend joins this mapping before the
detected `project_key`; older responses without it remain valid. Every edit forces one
batched resolve, and request epochs reject obsolete successes and failures. Failed
refreshes retain the previous mapping with a retry message. Explicit reassignment affects
only the selected session, including when it belongs to a parent/subagent family.
Scan completion refreshes the shared mapping once so an early partial lookup cannot hide
saved assignments. Opening the editor refreshes destination choices and shares any active
lookup; transcript appends do not cause per-session project requests.
Headless project reports resolve labels and path provenance from the destination project,
including destinations whose original sessions are outside the report window or moved
elsewhere. Project token budgets join the same durable session assignments and canonical
merges; an unreadable override store makes the scoped value unavailable rather than
attributing usage to its detected project.

## Performance measurements

### Codex speed

`get_speed_report` defaults to whole-turn throughput from the existing parsed
Codex sessions. `speed.rs` uses completed turns, reconciled output tokens, and
explicit `duration_ms`, falling back to recorded start/end timestamps. Optional
time to first token comes only from the explicit recorded field. Full session
content resolves through `AppState` and durable history, in bounded batches;
preparation, read failures, and truncation are surfaced rather than hidden.
Timing never changes accounting, pricing, or persisted session contracts.

The separately selected response measurement reads local `logs_2.sqlite` with a
read-only connection, honoring `$CODEX_HOME`. Odometer does not install a collector,
change logging, or configure OpenTelemetry. This optional source depends on Codex's
version, settings, and retention. Response IDs deduplicate records internally;
responses under five seconds or 100 output tokens, invalid timestamps/usage, and
non-text or built-in-tool responses are excluded.

Only sanitized timing, token counts, model, effort, mode, and measurement provenance
cross IPC. Raw payloads, identifiers, prompts, credentials, tool output, and paths
are excluded. Turn tiers describe configured settings; response tiers describe
observed service. Unknown modes stay separate. Duration-weighted throughput divides
total output by total measured time, keeping turns and responses separate. Turn time
includes tools, waiting, and reasoning; response time includes reasoning and request
latency. Parent/subagent durations may overlap. These observations do not establish
pure decode speed, causal speedups, or accepted-task delivery performance.

`SpeedMonitor.svelte` polls only while its panel and Codex tab are active and the
document is visible. Today uses local midnight; rolling 7/14-day bounds cross IPC
as UTC timestamps. Request generations discard obsolete responses and failures
when either the window or measurement changes. CSV exports the displayed
model/reasoning projection with measurement provenance, without transcript identifiers.

### Application timings

Application performance tracking is local-only, explicitly opt-in, and disabled by default through `Config.performance_tracking_enabled`. `PerformanceRecorder` starts its bounded writer lazily when enabled, so the off path performs only an atomic flag check. Backend measurements cover setup, watcher/config discovery, bulk discovery and scanning, cache hit/miss/open time, aggregate parser time, incremental parsing, range rollups, correlations, Git evaluation, detail/list IPC, and exports. `src/lib/performance.ts` records frontend initialization, batched store updates and paints, virtual-list paints, range fetches, detail fetches, and export projection work.

Events use a versioned, redacted contract: timestamp, app/platform/process identity, operation name, duration, success, and bounded aggregate metadata. Prompts, tool arguments/output, session IDs, repository paths, and commands are forbidden. A bounded channel keeps measurements off hot paths; overflow increments a dropped counter instead of blocking work. JSONL data lives under the OS local-data directory, rotates between current and previous segments at the Settings-configured size, and can be exported through backend-owned native dialogs as JSONL or CSV.

The `open_task_in_chatgpt` command launches the supported `codex://threads/<id>` deep link. For a subagent rollout, the UI opens its parent task because subagents are not ordinary sidebar tasks. Claude Code sessions have no deep link; the button is hidden for them.

## Auto-update

The app registers `tauri-plugin-updater` and `tauri-plugin-process`. `App.svelte` checks for updates once at startup (silently tolerant of failure: offline, dev builds, or a not-yet-public endpoint) and shows a banner offering a one-click download-and-install with relaunch. Update packages are the platform installers themselves (`createUpdaterArtifacts`), minisign-signed in CI via `TAURI_SIGNING_PRIVATE_KEY`; the public key and the `releases/latest/download/latest.json` endpoint live in `tauri.conf.json`, and `tauri-apps/tauri-action` assembles and uploads `latest.json` per release. Note: the endpoint only resolves once the repository's releases are public and the release is published (drafts don't serve `latest/download` URLs). The private key lives outside the repo (`~/.tauri/`) and in GitHub secrets; losing it orphans every installed copy's update chain.

## Dates and ranges

`CalendarActivity.svelte` requests daily `sessions_in_ranges` windows for the
current provider/session filters and optionally the shared project store's
effective merged or reassigned project. It batches at most 64 inclusive UTC
windows per call and reuses the existing mutation/range cache. Svelte sums raw
token and tool-call facts; it does not price tokens or persist a second history.
Local boundaries advance by calendar date, preserving 23/25-hour DST days; UTC
is a separate selectable calendar. The default shows the latest 90 calendar
days, while explicit scopes are bounded to 366 days with a narrowing message.

The calendar waits for scan/history readiness and requires the archive's
additive `coverage_complete` provenance. A ready but recovered archive shows
partial recorded totals; absent coverage or an unavailable archive prevents a
zero claim. A successful empty bucket means recorded zero only, and partial
history keeps that distinction visible. Bucket selection preserves inclusive
millisecond bounds and the actual event-session IDs when opening the existing
filtered session list. Daily active-session counts are not summed or presented
as a distinct range total. Price authorities and existing cost views are unchanged.

UI `datetime-local` values are local wall-clock values and must be converted to UTC ISO strings before comparison with rollout timestamps.

- A session matches a date filter when `[started_at, last_event_at]` overlaps the selected interval.
- In a filtered interval, displayed tokens and credits sum history events inside inclusive bounds.
- With no date bounds, cumulative session totals and per-model buckets remain the source of truth.

This distinction matters for sessions that began before the requested range or resumed with cumulative carryover.

## Persistence and privacy

### Explicit transcript access (#250)

`get_transcript_page` / `getTranscriptPage` is an explicit read-only IPC request.
It returns complete, validated JSONL records as raw JSON strings plus Rust-extracted
provider, record kind, and message ID metadata. It does not run from session lists,
pricing, summaries, aggregate exports, diagnostics, performance recordings or MCP.
Accounting continues to use the existing provider parsers and Rust query service.

Requests identify a session storage ID, never a path. Registered ownership or a
bounded durable source-location lookup supplies candidate paths; configured provider
roots, opened-file identity, and a bounded provider identity/timestamp check gate
reads. Retained sources currently require the resident summary's hydrated start
timestamp and a registered observation to verify ownership; before hydration or
observation, access honestly reports unverified. Scanner/watcher observations bind
size/mtime/OS identity before and after the existing parse (or validated cache read).
A changed file is unreadable through this API until a stable observation publishes
it again. A replacement resets the incremental parser before a fresh parse. Known
durable collisions report ambiguous identity rather than guessing a lineage.
The disposable scan cache also binds hits to the OS identity observed before and
after parsing; legacy entries without that evidence miss once and then warm normally.
This metadata-only cache format change ships with the v0.8.21 version bump.
This is not an independent durable-session enumeration API.

Pages clamp to 1–100 records and 4–256 KiB of serialized JSON, with a 1 MiB scan
budget and 64 KiB per-record buffer. Oversized records produce omission markers and
may require multiple continuations; malformed records and incomplete trailing lines
are explicit partial states. Only newline-terminated records are returned.
Eight source locations are inspected at most; further candidates are an explicit
coverage limitation. A valid empty source page differs from missing, unreadable,
unsupported, unknown-session and invalid-continuation responses.

`next_cursor` binds the session/source and opened file identity, length and modified
time. Any observed append, replacement, truncation or rewrite invalidates it; restart
from the first page rather than combining generations. Record anchors separately
include source/artifact identity, OS file identity, byte offset and a content-change
fingerprint, so unchanged records retain IDs on append and application restart.
Moves preserving file identity retain anchors; copies across filesystems do not.
`record_id` seeks directly to a returned anchor and validates the boundary and exact
anchor before returning content. It cannot be combined with a cursor. Seeking into
the middle reports earlier records as uninspected. Metadata IDs/kinds are bounded to
64 characters; full original fields remain in the validated raw record.

Follow continuations and combine their issues. `source_complete` is true only when
the selected generation reaches its end without known omissions or uninspected
earlier records. Source availability is separate from #38 lifecycle: a missing source
can still have retained summaries and historical usage, and never implies purge.
No transcript index, raw-body persistence, retention policy or purge is added here;
source deletion removes access to those bodies without changing historical totals.

Codex sessions are resolved below `$CODEX_HOME`, falling back to `~/.codex`:

- `$CODEX_HOME/sessions`
- `$CODEX_HOME/archived_sessions`
- `$CODEX_HOME/session_index.jsonl`

Claude Code sessions are resolved below `$CLAUDE_CONFIG_DIR`, falling back to `~/.claude`:

- `$CLAUDE_CONFIG_DIR/projects`

Gemini CLI JSONL sessions are resolved below `~/.gemini/tmp`; Gemini CLI does not document an environment override for this root.

User-owned app data is stored under the platform configuration directory in `agent-odometer/config.json` and, after rate edits, `agent-odometer/rates.json`. The fallback rate card is compiled from `src-tauri/rates.json`. Durable parsed session history lives separately under the platform local-data directory in `agent-odometer/history-v1.sqlite3`; it is retained independently of transcript availability and scan-cache eviction. Enabled turn receipts also keep independent bounded `turn-receipt-status-codex.json` and `turn-receipt-status-claude-code.json`, and `turn-receipt-status-gemini_cli.json` health records under the OS local-data directory; they contain no session IDs or paths. Reads retain compatibility with the earlier shared development-format file.

Session files can contain full prompts, responses, system/developer instructions, local paths, and tool output. Keep processing local, avoid logging message bodies, and use synthetic/redacted test data. Tauri capabilities in `src-tauri/capabilities/default.json` should remain narrowly scoped.

### Explicit session content search (#251)

`search_session_content` / `searchSessionContent` is private desktop IPC using
the existing bounded transcript reader. Rust matches escaped Unicode literals
against typed message blocks, with conversation-only defaults and independent
tool-call/result consent flags. One source page returns at most one bounded
snippet per matching record and pins continuation to the query, scope, session
and existing source cursor. Unknown and omitted content remains explicitly
unsearched.

Incomplete source coverage offers a separate retained phase. `history_search.rs`
reads at most 25 turns from a snapshot capped at 8 MiB using SQLite JSON field
projection, without decoding a whole Session or building an index. Prompt and
final-reply fields retain the parser's 500-character limits; other messages and
tools are absent. Retained targets bind opaque lineage, current snapshot revision,
turn and field. `resolve_retained_search_target` fails closed after changes,
purge or ambiguity. Query/snippet/body data never enters summaries, telemetry,
accounting, exports or MCP. See [Session content search](TRANSCRIPT_SEARCH.md).

## Known limitations

Durable enumeration, lifecycle states, confirmed local-history purge and preserved
corrupt-database recovery are specified in [History lifecycle](HISTORY_LIFECYCLE.md).
`HistoryStatus` and Integration Center status carry `coverage_complete`: true for
an intact archive, false after nonempty purge or readable-source recovery, and null when unverified.
A ready/readable archive alone does not establish complete historical coverage.

- A configured root that does not exist when the watcher starts is skipped; creating it later requires saving settings or restarting the app to establish the watch.
- If the durable history database is unavailable, live sessions still work for readable sources, but disappeared-source retention and collision-safe reconciliation are unavailable until persistence succeeds again.
- An invalid envelope timestamp falls back to the current time and can affect ordering.
- `forked_from_id` is represented in the model/UI but may be absent when the source rollout does not provide or the parser does not extract it.
- Claude Code's `Stop` payload does not include subscription rate-limit windows, so its first receipt version shows tokens and API-rate estimates but no per-turn subscription delta. Codex quota values come from its transcript snapshots.
- Hook commands parse one transcript from disk in a short-lived process. Very large transcripts can make a receipt late or unavailable, but the harness turn still completes and the dashboard watcher remains unaffected.
- Frontend checks include Svelte/TypeScript validation, Vitest unit and component tests, source-backed coverage enforcement, and Playwright visual regression against committed fixture baselines. Runtime changes still require an affected-flow Tauri run.

## Safe extension patterns

For a new backend field, update the Rust model/parser, add parser coverage, update `src/lib/types.ts`, and then consume it in Svelte. Prefer optional fields or Serde defaults for historical rollout compatibility.

For a new command, implement it in `commands.rs`, register it in `lib.rs`, add a typed wrapper in `ipc.ts`, and expand capabilities only when the API actually requires it.

For watcher changes, test initial files, incremental appends, partial trailing lines, removal, archive roots, session-index updates, and config-triggered restart separately.

## Private organization

Ledger schema 12 adds `session_annotations`, `annotation_tags`, `organization_tags`, and `saved_searches`. These tables are accessed only by explicit private desktop commands; they never join `SessionSummary`, normalized facts, query reports, MCP, diagnostics, or ordinary exports. Bulk list projections return identity/revision, pin state, tag labels, and a note-presence flag, without note text or full snapshots. Note reads and edits are separate bounded IPC operations.

An annotation binds `(session_key, first_event_fingerprint)` and an optional opaque source-record anchor. The composite foreign key cascades fingerprint promotion in place and deletes session-owned rows in the same confirmed purge transaction. Identity and revision checks reject stale editors after promotion, purge/reused keys, or tag changes. Current session edits accept only the empty anchor; record bookmark UI and source-anchor validation belong to #272, which reuses this storage and typed identity seam. No raw source record is duplicated into annotations.

Private edits increment an organization revision included in purge previews, so edits after review require a fresh review. Tag rename/delete invalidates affected editor revisions; saved searches retain their explicit tag choices and report unavailable until reviewed when a tag disappears. Searches store only the explicit user-saved query/filter definition and shared content-class choices. Ordinary conversation-search requests, hits, snippets, and bodies are never saved automatically.

Saved UTC bounds remain authoritative across timezone changes and repeated DST hours. Display inputs preserve seconds and milliseconds; editing one bound replaces only that instant, and calendar selection replaces both. Restoring a content-scope search never silently falls back to summary search.

Ordinary source rebuild/refresh preserves identity-bound organization. Corrupt-database recovery preserves private data only in the original backup; it cannot reconstruct notes, pins, tags, or saved searches from source transcripts. Private editors and search management disclose this limitation, and dependent pin/tag result sets remain unavailable rather than presenting absent annotations as a complete empty result. New private edits apply to the rebuilt database. Backup restoration is a separate future operation.

Schema 14 follows the schema-13 workflow lifecycle migration and adds three nullable structured human-outcome columns to `session_annotations`. Explicit desktop annotation edits validate labels, whole user-reported repair minutes, and optional first-pass evidence under the existing identity/revision transaction. An omitted outcome preserves earlier labels. A null label distinguishes absent human edits from an explicit Not rated edit, so a post-recovery pin/note change cannot make an unrestored rating look known. Root-task reports and explicit structured outcome exports disclose coverage and exclude separately labelled subagents; ordinary accounting, exports, diagnostics, logs, MCP, and source/cache snapshots remain unchanged. Repair notes reuse the private note body. See [human outcome measurement](HUMAN_OUTCOMES.md).


Schema 15 adds private `curated_cases` and the content-free `curated_changes` journal. One Rust projection supplies short-lived previews and persisted minimized conversation blocks. Commit rechecks exact source anchors, human-outcome revision and dataset revision; clients cannot submit arbitrary case bodies. Composite foreign keys cascade session removal/replacement. Membership triggers advance both dataset and organization revisions, invalidating earlier purge reviews. Bounds are 64 cases of 16 KiB and 4,096 ID/hash changes. Explicit JSON export is separate from accounting, ordinary reports and transcript exports. See [curation and deletion](CURATED_DATASETS.md).

### Account-scoped live quota and soft budgets (#43)

`quota_accounts.rs` owns consent metadata and the desktop polling timer; `quota_live.rs` owns bounded Codex app-server stdio reads. `quota.rs` converts both live buckets and transcript evidence through the shared window service. Live account views never overwrite transcript observations or durable usage. The payload-free `live-quota-updated` event invalidates the tray, and the dashboard reads the same in-memory service. Monetary/token budget values use the ledger range/pricing authority. The sole `check_ambient_alerts` collector evaluates transcript budgets and exact consent-bound cached live account thresholds with one serialized notification-log write; presentation reads are non-consuming. `quota-v3.json` protects the combined policy, rules and dedup metadata from older releases. See [QUOTAS.md](QUOTAS.md) for consent, persistence, unsupported-source, and verification boundaries.

Schema 16 adds private `offline_experiments` immutable manifests and bounded `offline_cases` input/result copies. Frozen Rust price probes share the existing query authority, preserve explicit USD/tier/modifier/fallback/stale provenance, and never write accounting. Reviewed single-use previews and immediate revision checks govern freeze/import/remove. Case/source deletion cascades copied bodies and advances comparison revision; manifests retain original membership denominators. Reports describe separate raw dimensions and unverified imported conditions without model execution. See [offline comparisons](OFFLINE_COMPARISONS.md).
