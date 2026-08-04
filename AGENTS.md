# AGENTS.md

These instructions apply to the entire repository.

## Project purpose

Odometer is a local Tauri companion to agent CLI harnesses: the ChatGPT desktop app's Codex experience and Claude Code. It reads each harness's session JSONL files and presents searchable task, turn, token, subagent, and estimated cost data in per-harness tabs. Rust owns filesystem access, parsing, persistence, and Tauri IPC; Svelte owns filtering, presentation, and credit calculations.

Start with [README.md](README.md) for commands and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the data flow, contracts, invariants, and known limitations.

## Before changing code

- Run `git status --short` and preserve unrelated work. Do not normalize or rewrite existing files as a side effect.
- Treat `src-tauri/gen/schemas/` as generated. Do not hand-edit it; commit generated changes only when an intentional Tauri capability/configuration change requires them.
- Both lockfiles are authoritative and should stay committed. Use `npm ci` and Cargo's `--locked` flag for reproducible validation.
- Never commit real Codex sessions, prompts, local paths, credentials, or platform config. Parser fixtures must be synthetic or thoroughly redacted.

## Architecture boundaries

- Keep filesystem access and JSONL parsing in Rust. The frontend should receive typed data through Tauri commands/events, not read session files directly.
- Put Rust commands in `src-tauri/src/commands.rs`, register them in `src-tauri/src/lib.rs`, and expose typed frontend wrappers in `src/lib/ipc.ts`.
- Keep Rust serialized structs and `src/lib/types.ts` synchronized. Add backward-compatible Serde defaults when persisted or historical data may omit a new field.
- Event names are contracts: `session-updated` (payload: `SessionSummary`), `session-removed`, `scan-progress` (payload: `ScanStatus`), `config-updated`, and `rates-updated`. Update every producer and listener together. Full sessions travel only through `get_session_details`; keep `SessionSummary` free of `turns`/`tokens_history` — the split exists because full sessions measured ~200 MB across a real corpus.
- Use the established Svelte 5 rune style (`$state`, `$derived`, `$effect`). Module-level rune state belongs in `*.svelte.ts` files.
- Keep Tauri capabilities minimal. Do not add remote content, network access, shell execution, or broader capabilities without an explicit requirement and a security review. Current exceptions: `updater:default` and `process:allow-restart` exist solely for the in-app auto-updater.

## Parser and accounting invariants

Parser and credit changes are high risk. Preserve these behaviors and add focused tests:

- Rollout files are append-only JSONL. Parse only newline-terminated records and leave a partial trailing record for the next watcher event.
- Malformed or unknown records must not abort the entire file. `response_item` records are intentionally ignored for aggregate parsing.
- A repeated `session_meta` in a resumed rollout must refresh metadata without erasing accumulated tokens or history.
- Current ChatGPT app rollouts can encode `source` as an object for subagents and expose `parent_thread_id`, `agent_path`, `agent_nickname`, `forked_from_id`, `history_mode`, and `memory_mode` directly.
- Preserve explicit `task_started.started_at`, `task_complete.completed_at`, `turn_aborted`, and `thread_rolled_back` semantics. Rollback changes turn state but does not erase already-consumed token usage.
- `total_token_usage` is cumulative; `last_token_usage` is the per-call delta. Per-model buckets must reconcile to the latest cumulative total, including resumes and model switches.
- The `apply_line` fast path may skip full JSON parsing only for structurally unambiguous `response_item`/`compacted` lines, and must still advance `last_event_at` from their timestamps. When in doubt it must fall through to the full parse.
- The scan cache (`scan_cache.rs`) is an optimization, never a source of truth: it must lose to a fresh parse on any size/mtime mismatch, version mismatch, or read error. It is versioned by `CARGO_PKG_VERSION`, so parser or `Session` changes are invalidated by any release — never ship a cache format change without a version bump.
- Cached input and cache-creation input are two disjoint subsets of input, and reasoning output is a subset of output. Never add any subset twice when computing credits, and never price one subset at another subset's rate.
- All-time summaries use cumulative totals. Date-scoped summaries use event deltas inside inclusive UTC bounds. Session date filtering uses interval overlap.
- Adding a token or tool dimension means changing the ledger **twice**. `range_totals_multi` serves whole-hour buckets from `rollup_*` tables and only the partial edge buckets from raw `durable_*_events`. A new field added to the fact table but not to the matching rollup table returns the correct value for the edges and zero for every whole hour — a partial, silent under-count that still looks like a plausible number. Extend the fact table, the rollup table, the migration backfill, and both the rollup write and read paths together. Prove it with a golden ledger-vs-in-memory test whose fixture has non-zero values for the new dimension in at least two hour buckets, and whose window starts and ends inside buckets so one call exercises both the rollup and edge paths. Confirm the test fails before the rollup half of the change.
- Migration backfills run as SQL `INSERT ... SELECT ... GROUP BY` inside SQLite. Never deserialize a `Session` or materialize events in Rust to backfill: a real ledger is multiple GB with millions of token events, and an in-Rust backfill exhausts memory.
- Unknown models use the configured fallback rate. Rates are expressed per one million tokens.
- `thread_settings_applied.service_tier` affects credit math. Fast GPT-5.5 uses 2.5x the standard rate and fast GPT-5.4 uses 2x; do not apply a multiplier to unsupported models.

Claude Code sessions (`claude_parser.rs`) have their own invariants:

- Streamed assistant messages repeat one `message.id` across lines with identical usage; count usage once per message ID.
- Anthropic `input_tokens` excludes cache traffic. Map to the viewer's subset convention: input = input + cache_read + cache_creation, cached = cache_read, cache_creation = cache_creation, reasoning = 0. `cached_input_tokens` and `cache_creation_input_tokens` are disjoint subsets of `input_tokens` and each has its own rate (`ModelRate.cached_input` / `ModelRate.cache_creation_input`); never add either subset twice, and never price one at the other's rate.
- Turns open on real human prompts only — never on tool results, `isMeta` records, sidechain prompts, `<command-…>` echoes, or interruption markers. Sidechain usage still counts toward the enclosing turn.
- Skip `<synthetic>` assistant messages. Records without timestamps (e.g. `custom-title`) must not move `last_event_at`.
- Subagent transcripts (`agent-*.jsonl` / under a `subagents` dir) reuse the parent's `sessionId`; they must be keyed by file stem with `parent_thread_id` set, never by the record `sessionId`, or they clobber the parent session.
- Sessions carry `harness: claude_code`; the per-harness `currencies`/`fallback_models` maps on the rate card keep Codex credits and Claude USD estimates separate.

Frontend unit and component tests use Vitest with jsdom and Svelte Testing Library; run them with `npm test`. `npm run test:coverage` enforces the explicit source-backed coverage slice in `scripts/validate-frontend-coverage.mjs`; expand its manifest only alongside meaningful tests for the added files. Parser integration tests and fixtures live in `src-tauri/tests/`; small Rust unit tests live beside their modules.

## Validation

These six commands are necessary but **not sufficient** — run them before handing off:

```powershell
npm run check
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

CI additionally runs Playwright visual regression against committed baselines, and it has caught real bugs — including a rendered-dollar-figure accounting bug — that all six commands above passed cleanly. Green on these six is not the same as green in CI; check the `Visual regression` job too.

The visual suite runs in a pinned container and baselines are platform-specific: `npm run visual:update` refuses to run unless `ODOMETER_VISUAL_BASELINE_ENV=playwright-v1.62.0-jammy` is set (see `scripts/assert-visual-baseline-platform.mjs` for the exact check and the `Visual regression` job in `.github/workflows/ci.yml` for the pinned image digest and invocation). Workflow: run `npm run visual:test` against the committed baselines first, inspect the diff images for every failure, confirm each changed pixel is intended, and only then regenerate with `npm run visual:update` inside the matching container. Never hand-edit baseline PNGs, and never weaken the comparison threshold to make a diff pass.

Important limitation: visual regression only covers frontend-computed values. The suite renders `src/dev-mock.ts` fixtures in a browser with no Rust backend, so `scripts/visual-impact.mjs` correctly treats `src-tauri/src/**` as non-impacting and skips the job for Rust-only changes. It is not a safety net for Rust-side accounting — pricing and parser changes need their own equivalence tests.

For runtime or UI changes, also exercise the affected flow with `npm run tauri dev`. If a failure predates your work, report it precisely and do not silently reformat or repair unrelated files.

## Change-specific checklist

- Parser/model change: update Rust structs, TypeScript mirrors, synthetic fixtures, parser tests, and credit/date rollups as applicable.
- IPC change: update command implementation, registration, TypeScript wrapper, payload type, capability (only if required), and event listeners.
- Watcher/config change: verify startup scan, incremental append, removal, archive status, session-index overlay, and watcher restart after settings changes.
- Default-path change: honor `$CODEX_HOME` before falling back to `~/.codex`, and `$CLAUDE_CONFIG_DIR` before falling back to `~/.claude` for Claude Code roots.
- UI change: verify empty/loading/error states, active and archived sessions, narrow-window behavior, keyboard behavior, and date/time-zone conversion.
- Rate change: update `src-tauri/rates.json` deliberately and test direct, aliased, fallback, unlimited, cached-input, cache-creation-input, and reasoning-output cases.
