# Architecture

## System overview

Odometer is a local companion to agent CLI harnesses — the ChatGPT desktop app's Codex experience and Claude Code — with two halves:

- `src-tauri/`: Rust/Tauri backend for discovery, incremental JSONL parsing, filesystem watching, persistence, and native commands.
- `src/`: Svelte 5/TypeScript frontend for reactive state, scoped tabs (`all` plus each harness), filtering, projection/export, tables, details, settings, and credit calculations.

Every session carries a `harness` tag (`codex` | `claude_code`). All three scopes share one session store, `SessionsView`, detail pane, filter predicates, pricing projection, and model aggregate. The All scope never adds plan credits to USD.

The frontend starts at `src/main.ts` and `src/App.svelte`. The native process starts at `src-tauri/src/main.rs`. Normal launches call `src-tauri/src/lib.rs::run`; an explicitly installed harness hook exits through the headless `turn_receipts::try_run_cli` path before Tauri starts.

## Startup and live-update flow

1. `Config::load` reads the platform config file or creates defaults.
2. `watcher::start` begins watching all configured roots immediately, so changes during the initial scan are not missed.
3. `scanner::scan_all` bulk-loads existing sessions on a background thread, parsing files in parallel (rayon) and emitting a `session-updated` summary per file — the window is interactive immediately and the list populates progressively. A persistent SQLite scan cache (`scan_cache.rs`, stored under the OS cache directory and keyed by file size+mtime, versioned by app release) serves unchanged files without re-reading them. Each scan touches or replaces individual rows and prunes unseen generations, avoiding whole-corpus cache deserialization and rewrites. The previous JSON cache is imported on first use. Progress flows to the UI via throttled `scan-progress` events and the `get_scan_status` command.
4. `parser::parse_file` (Codex) or `claude_parser::parse_file` (Claude Code) builds a `Session` for each file. Before publication, `history_store.rs` reconciles the parsed source with the durable archive, assigns a path-independent `storage_id`, and records the source observation and normalized token-event suffix transactionally.
5. `AppState.sessions` stores the resulting projection by durable `storage_id`. Multiple paths on one event lineage converge on one session; reused provider IDs with divergent lineages remain separate collision records.
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

- Provider IDs are harness-namespaced: `codex:thread:<id>` and `claude_code:session:<id>`. Claude subagents use `claude_code:subagent:<parent-session-id>:<agent-id>`; only legacy subagents without a provider `agentId` may use a filename-stem fallback.
- A filesystem path is an availability observation, never logical identity. Stored path keys normalize separators, remove Windows verbatim prefixes, and fold case on Windows so scanner and watcher events address the same location.
- Provider identity, the first-event fingerprint, and an append-compatible token-event lineage reconcile copies and moves. Equal histories and prefix histories are one lineage. Divergence after a shared prefix creates a deterministic `:collision:<lineage-hash>` storage ID and marks every final session claiming that provider identity as a collision; neither transcript overwrites the other.
- A source observation can advance the materialized snapshot only when it has more token-history events, or the same event count with an equal-or-newer `last_event_at`. Metadata overlays such as session-index names advance the snapshot version and may update display metadata without creating or reassigning a source location; only the current full snapshot is retained.
- Token events are keyed deterministically within a durable session. Repeated scans and watcher passes are idempotent, while appends insert only the unseen suffix. Direct request-input evidence added by a later schema is backfilled without replaying or duplicating earlier events.

Availability publication is generation-safe. A completed scan marks unseen paths missing only when it is still the newest durable generation and the scan had zero parse failures; an incomplete scan retains the previous availability rather than inventing deletions. Per-path tombstones and the settings-transition lock prevent an older bulk-scan callback from resurrecting a watcher-removed or reconfigured source. If the history database cannot be opened or written, Odometer logs a warning and keeps live parsing available, but it makes no durability, move-reconciliation, or collision-preservation claim for that failed operation.

This is the first durable-history slice, not the complete normalized ledger tracked by issue #38. Existing desktop, tray, export, and range consumers still query the in-memory session projection; the archive currently retains the latest materialized session snapshot plus normalized token events rather than aggregate-only facts and exposes no user-controlled purge/rebuild or read-only recovery workflow. Those boundaries must be resolved before #38 can be considered complete.

## Opt-in turn-receipt flow

Turn receipts are a separate, default-off freshness path; they do not replace the watcher:

1. Settings transactionally reconciles one identifiable `Stop` command per selected harness while
   retaining unrelated settings and handlers. Codex preserves an existing Odometer source; for a
   new integration it edits the `[[hooks.Stop]]` representation in inline `config.toml` when those
   hooks already exist, otherwise `hooks.json`. Other valid inline-array TOML shapes and symlinked
   config files fail closed rather than being rewritten or duplicated. Repair removes Odometer-owned
   duplicates across both sources. Claude Code uses a direct executable `command` plus explicit
   `args` in the user-level `settings.json`, shared by Claude Code 2.1.139+ CLI and local Desktop Code
   sessions; remote and SSH sessions use configuration on their host. For AppImage launches, the installed hook
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
2. Codex/Claude passes bounded JSON on stdin, including the session and transcript path. The helper
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

The hook process cannot execute frontend TypeScript, so `turn_receipts.rs` contains a deliberately
narrow mirror of the existing subset-aware token formula and documented Fast-tier multipliers. Its
focused Rust tests are a parity gate; the dashboard remains the owner of interactive and date-scoped
pricing.

## Backend modules

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/lib.rs` | Tauri setup, shared state, command registration, initial scan, watcher lifetime |
| `src-tauri/src/model.rs` | Serialized session, harness, turn-status, and token wire models |
| `src-tauri/src/parser.rs` | Full and incremental Codex rollout JSONL parsing |
| `src-tauri/src/claude_parser.rs` | Full and incremental Claude Code session JSONL parsing |
| `src-tauri/src/scanner.rs` | Recursive JSONL discovery, cached parallel initial parse |
| `src-tauri/src/scan_cache.rs` | Incremental SQLite parsed-session cache keyed by file size+mtime |
| `src-tauri/src/history_store.rs` | Durable SQLite session archive, source availability, identity reconciliation, and schema migration |
| `src-tauri/src/performance.rs` | Default-off, bounded local performance event writer and JSONL/CSV export |
| `src-tauri/src/watcher.rs` | Debounced file watching, per-harness parser dispatch, frontend events |
| `src-tauri/src/session_index.rs` | Thread-name overlay from `session_index.jsonl` |
| `src-tauri/src/commands.rs` | Tauri command boundary |
| `src-tauri/src/config.rs` | Session-root configuration and persistence |
| `src-tauri/src/rates.rs` | Bundled rate card and user override persistence |
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

Cached input and reasoning output are included within input and output respectively. Credit calculation in `src/lib/credits.ts` subtracts the subsets before applying the ordinary input/output rates, then prices the subsets at their own rates.

Credit history also records `service_tier`. Current documented Fast mode multipliers are applied event-by-event for GPT-5.5 and GPT-5.4; models without a documented Fast rate remain at the standard multiplier.

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

- The legacy `models` map prices Codex plan credits and Claude API USD, with `currencies` and `fallback_models` keeping harness units and unknown-model fallbacks separate. `api_models` supplies the Codex tab's flat informational API-USD comparison. These maps remain authoritative for list summaries, range buckets, existing views, and editable user overrides. Every `ModelRate` carries `cache_creation_input` alongside `input`/`cached_input`/`output`/`reasoning` — a normalized dimension distinct from both, so cache-creation (write) tokens are priced at their own rate instead of the plain input rate. `cached_input_tokens` and `cache_creation_input_tokens` are disjoint subsets of `input_tokens`: `eventCost` (`src/lib/credits.ts`) and `token_cost` (`src-tauri/src/turn_receipts.rs`) both subtract both subsets before pricing the remainder at the ordinary input rate, so neither subset is ever priced twice.
- `pricing_catalog` supplies opt-in, event-level scenarios. Every base period and conditional modifier has a stable ID, billing `surface`, exact model, half-open UTC interval `[from, to)`, label, and provenance (`evidence`, source URL, verification timestamp, and optional note). A rule never crosses from OpenAI API USD, Anthropic API USD, or Codex plan credits to another surface.

Catalog validation is fail-closed. Rule IDs, models, labels, evidence, and source URLs must be non-empty; IDs must be globally unique; interval ends must follow starts; cache-write and conditional multipliers must be positive; base periods for one `(surface, model)` cannot overlap; and modifiers with the same `(surface, model, condition)` cannot overlap. Adjacent periods are valid. A malformed or invalid on-disk override logs a warning and falls back to the bundled card.

Time-aware pricing follows these rules:

- A full session is evaluated event by event at the event timestamp. Every priced event must identify a model and have a direct catalog period for its model and billing surface. Missing coverage, a known-unpriced model, or an unattributed nonzero event makes the time-aware scenario unavailable (`null`); it never substitutes a fallback model, the latest rate, or the flat reference retroactively.
- Conditional rules operate on one provider request. The current threshold condition applies only when observed `request_input_tokens` is strictly greater than the configured threshold. Missing request-level evidence never triggers a modifier speculatively; the result names the applicable rule under `conditionalEvidenceMissing`.
- Applicable input multipliers compose multiplicatively and cover ordinary, cached, and cache-creation input. Output multipliers likewise cover ordinary and reasoning output. The existing service-tier multiplier is then applied to the event.
- A period's declared cache-write multiplier (`cache_write_input_multiplier`) is provenance metadata; the dollar amount for cache-creation tokens comes from the period's own `rate.cache_creation_input`. `cacheWritePricingUnmodeled` is only `true` when a period declares a multiplier but no event in the session ever reported nonzero `cache_creation_input_tokens` under it — once cache-creation tokens are observed, the premium is priced directly rather than left as unobserved metadata.
- The scenario result carries the IDs of every applied period and modifier. The flat reference remains visible beside it so a dated or conditional scenario cannot silently redefine existing totals.

`unpriced_models` identifies known models without a published rate; flat calculations exclude and label their usage rather than applying a fallback. `free_local_models` identifies models that are explicitly zero-cost (free tier, local/self-hosted) — a distinct, deliberate declaration from `unpriced_models` (no published price) or an ordinary unresolved rate. Other unknown model IDs first resolve through `model_aliases` (raw provider id -> canonical rate-table key); alias chains may hop multiple entries, and a cycle is detected and terminated deterministically rather than looping. Unresolved model IDs then use the configured per-harness fallback only in the legacy flat calculation and remain named in the UI. `RateCard::resolve_model_pricing` (mirrored by `resolveModelPricing` in `credits.ts` — keep both in sync) is the one place every surface resolves a raw model id and records why: `direct`, `aliased`, `fallback`, `estimated`, `free_local`, `subscription`, `stale`, or `unavailable` (`PricingBasis`). The dashboard renders this provenance as visually distinct badges (an amber ⚠ for fallback, an amber ◇ for unpriced, a blue ↝ for aliased) in the session detail pane and model-comparison table rather than collapsing every non-exact price into one indicator.

When an older user rate card is upgraded, user-edited legacy rates, currencies, units, and fallback choices are preserved; new bundled legacy entries, aliases, free/local declarations, and subscription plans are added only where the user has no existing entry for that key; bundled catalog rules replace or append by stable ID; separately identified custom rules remain; and bundled notes are unioned. A model a user already customized before `cache_creation_input` existed has that one field backfilled from the bundled card (its `input`/`cached_input`/`output`/`reasoning` edits are untouched) so upgrading a pre-#42 override doesn't silently zero-price cache-creation tokens forever. Saving validates the catalog before writing an adjacent temporary file and renaming it into place.

Three further pieces of the rate-card contract exist as offline-complete data structures with a deliberately unimplemented network seam, per issue #42's scope (`AGENTS.md` forbids adding outbound network access without an explicit requirement and a security review):

- `subscription_plans` (per harness) records a user-declared plan name, monthly price, currency, notes, and an optional local/proxy baseline savings figure. It never claims or infers a plan-equivalent token allowance — only what the user enters.
- `display_currency` is an optional user-supplied `{ target_currency, rate, as_of, source }`. Odometer performs no FX fetch; a display conversion is applied only when present, and the original-currency amount, its currency, and the converted amount are always retained separately (a converted total never replaces or is combined with a different original currency or with harness credits).
- `refresh` (`RateRefreshState`: `last_success_at`, `last_attempt_at`, `last_failure_reason`, `max_cache_age_secs`) is bounded-cache-age bookkeeping for a future refresh flow. `RateCard::apply_refresh_candidate` (Rust) is the validation/rollback contract that flow must use: a candidate that fails catalog validation, or that comes back with an empty/partial price table where the previous card had one, is rejected and the previous valid (or bundled) card is retained with the failure recorded — it never partially applies an invalid candidate. Nothing in this codebase calls it with a network-fetched candidate yet; that transport is out of scope for this change and is called out explicitly in the module's SEAM comment.

This catalog is a bounded advance on issue #42, not the complete pricing authority. Signed network price refresh and currency exchange-rate fetching remain out of scope — see the SEAM notes above.

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

`list_tool_impact_targets` discovers provider and individual-tool choices from the same filtered sessions and time window. `compare_tool_impact` then builds turn-level observed and not-observed cohorts for the selected target. When at least three comparisons are available, the UI uses nearest-in-time pairs with the same harness, model, and deterministic task category. This is observational: transcripts prove use, but cannot prove whether an unused target was installed or available, and whole overlapping turns are included because token events do not carry turn IDs.

Configuration tracking resolves global harness roots plus project scopes derived from session working directories (using the containing Git worktree when available). Only known settings/instruction files and bounded hook/skill trees are watched. Events retain hashes, sizes, a size-only safe diff, and a hashed path identity; they never persist config values or raw paths. The watcher is rebuilt after each session scan so newly discovered project scopes and settings-root changes share the same coverage. Config markers appear on the existing spend chart and the timeline reports before/after tokens, turns, active session duration, tool metrics, samples, and confounds through the source-agnostic correlation engine.

The optional Instructions view is deliberately separate from that redacted event pipeline. When enabled, `instructions.rs` inventories the known global harness folders, Git worktrees inferred from lightweight session directory/activity snapshots, and user-configured roots. Observed session directories are admitted only when Git discovery succeeds; a non-repository working directory never becomes an implicit recursive root and must be added explicitly if desired. Repeated working directories are resolved once per scan, and recursive ancestors cover nested roots without traversing those subtrees again. Each configured root is folder-only or recursive; recursive walks do not follow links, are bounded by depth, matching-file count, and total filesystem entries, and skip common VCS, dependency, and generated-output trees. The backend emits `instruction-scan-progress` while it prepares roots, walks entries, and analyzes matches; one shared frontend store feeds both the inline scan detail and the persistent bottom status bar without polling. Config capture and scan-generation allocation share the settings-transition lock, while generation checks and allowlist replacement share the allowlist lock. Instruction-source changes advance the generation and clear the allowlist atomically; direct cancellation and scan supersession advance it under the same publication lock. Cancelled, superseded, or reconfigured work therefore cannot publish an inventory, restore revoked paths, or leave stale progress visible. The wire model keeps harness ownership as a list of strings so future harness definitions do not require replacing the inventory contract, although only Codex `AGENTS.md` and Claude Code `CLAUDE.md` are admitted today.

Inventory reads are fail-closed: the UI must refresh discovery first, only discovered regular files with supported names enter the in-memory allowlist, symbolic links are rejected, and preview content is capped at 1 MiB. The frontend renders Markdown with Marked and sanitizes the HTML with DOMPurify; active links, remote media, embedded forms, scripts, styles, SVG, and other executable/remote surfaces are removed. Opening a file uses the platform's default application only after the same backend allowlist check.

Warnings are deterministic review signals rather than semantic verdicts. Duplicates share an exact normalized-content hash; possible conflicts are opposite `always`/`must` versus `never`/`must not`/`do not` directives within one effective ancestor chain; oversized files exceed 64 KiB or 800 lines; possibly stale files are unchanged for 180 days while their project has agent activity in the last 30 days. Selecting a file filters redacted configuration events by its normalized path identity and reuses the existing seven-day before/after correlation view.

Optimization findings are timestamped at the observation that triggered them. `RangeTotals.optimization_findings_count` therefore scopes precomputed findings by date without rerunning the analyzer for every analytics window. The analyzer keeps exact read-request identity separate from the private resource identity: ranges/pages are distinct requests, while a mutation of the same resource resets prior-read streaks. Volatile polling reads and neutral tool ratios are not optimization findings. Findings carry confidence, occurrence count, and a conservative likely-avoidable-call estimate; compact summaries expose severity and rule breakdowns without shipping full observations to the list view.

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

User-owned app data is stored under the platform configuration directory in `agent-odometer/config.json` and, after rate edits, `agent-odometer/rates.json`. The fallback rate card is compiled from `src-tauri/rates.json`. Durable parsed session history lives separately under the platform local-data directory in `agent-odometer/history-v1.sqlite3`; it is retained independently of transcript availability and scan-cache eviction. Enabled turn receipts also keep independent bounded `turn-receipt-status-codex.json` and `turn-receipt-status-claude-code.json` health records under the OS local-data directory; they contain no session IDs or paths. Reads retain compatibility with the earlier shared development-format file.

Session files can contain full prompts, responses, system/developer instructions, local paths, and tool output. Keep processing local, avoid logging message bodies, and use synthetic/redacted test data. Tauri capabilities in `src-tauri/capabilities/default.json` should remain narrowly scoped.

## Known limitations

- A configured root that does not exist when the watcher starts is skipped; creating it later requires saving settings or restarting the app to establish the watch.
- If the durable history database is unavailable, live sessions still work for readable sources, but disappeared-source retention and collision-safe reconciliation are unavailable until persistence succeeds again.
- An invalid envelope timestamp falls back to the current time and can affect ordering.
- `forked_from_id` is represented in the model/UI but may be absent when the source rollout does not provide or the parser does not extract it.
- Claude Code's `Stop` payload does not include subscription rate-limit windows, so its first receipt version shows tokens and API-rate estimates but no per-turn subscription delta. Codex quota values come from its transcript snapshots.
- Hook commands parse one transcript from disk in a short-lived process. Very large transcripts can make a receipt late or unavailable, but the harness turn still completes and the dashboard watcher remains unaffected.
- Frontend behavior is checked by TypeScript/Svelte validation and manual Tauri runs; no frontend unit-test framework is configured.

## Safe extension patterns

For a new backend field, update the Rust model/parser, add parser coverage, update `src/lib/types.ts`, and then consume it in Svelte. Prefer optional fields or Serde defaults for historical rollout compatibility.

For a new command, implement it in `commands.rs`, register it in `lib.rs`, add a typed wrapper in `ipc.ts`, and expand capabilities only when the API actually requires it.

For watcher changes, test initial files, incremental appends, partial trailing lines, removal, archive roots, session-index updates, and config-triggered restart separately.
