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
- Event names are contracts: `session-updated` (payload: `SessionSummary`), `session-removed`, `config-updated`, and `rates-updated`. Update every producer and listener together. Full sessions travel only through `get_session_details`; keep `SessionSummary` free of `turns`/`tokens_history` — the split exists because full sessions measured ~200 MB across a real corpus.
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
- Cached input is a subset of input, and reasoning output is a subset of output. Never add either subset twice when computing credits.
- All-time summaries use cumulative totals. Date-scoped summaries use event deltas inside inclusive UTC bounds. Session date filtering uses interval overlap.
- Unknown models use the configured fallback rate. Rates are expressed per one million tokens.
- `thread_settings_applied.service_tier` affects credit math. Fast GPT-5.5 uses 2.5x the standard rate and fast GPT-5.4 uses 2x; do not apply a multiplier to unsupported models.

Claude Code sessions (`claude_parser.rs`) have their own invariants:

- Streamed assistant messages repeat one `message.id` across lines with identical usage; count usage once per message ID.
- Anthropic `input_tokens` excludes cache traffic. Map to the viewer's subset convention: input = input + cache_read + cache_creation, cached = cache_read, reasoning = 0.
- Turns open on real human prompts only — never on tool results, `isMeta` records, sidechain prompts, `<command-…>` echoes, or interruption markers. Sidechain usage still counts toward the enclosing turn.
- Skip `<synthetic>` assistant messages. Records without timestamps (e.g. `custom-title`) must not move `last_event_at`.
- Subagent transcripts (`agent-*.jsonl` / under a `subagents` dir) reuse the parent's `sessionId`; they must be keyed by file stem with `parent_thread_id` set, never by the record `sessionId`, or they clobber the parent session.
- Sessions carry `harness: claude_code`; the per-harness `currencies`/`fallback_models` maps on the rate card keep Codex credits and Claude USD estimates separate.

Parser integration tests and fixtures live in `src-tauri/tests/`; small Rust unit tests live beside their modules. There is currently no frontend unit-test runner, so do not invent an `npm test` command.

## Validation

Match CI before handing off:

```powershell
npm run check
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

For runtime or UI changes, also exercise the affected flow with `npm run tauri dev`. If a failure predates your work, report it precisely and do not silently reformat or repair unrelated files.

## Change-specific checklist

- Parser/model change: update Rust structs, TypeScript mirrors, synthetic fixtures, parser tests, and credit/date rollups as applicable.
- IPC change: update command implementation, registration, TypeScript wrapper, payload type, capability (only if required), and event listeners.
- Watcher/config change: verify startup scan, incremental append, removal, archive status, session-index overlay, and watcher restart after settings changes.
- Default-path change: honor `$CODEX_HOME` before falling back to `~/.codex`, and `$CLAUDE_CONFIG_DIR` before falling back to `~/.claude` for Claude Code roots.
- UI change: verify empty/loading/error states, active and archived sessions, narrow-window behavior, keyboard behavior, and date/time-zone conversion.
- Rate change: update `src-tauri/rates.json` deliberately and test direct, fallback, unlimited, cached-input, and reasoning-output cases.
