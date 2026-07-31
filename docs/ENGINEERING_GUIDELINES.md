# Agent Odometer engineering guidelines

These guidelines synthesize the project session archive and the current
repository architecture. They are intended for maintainers and coding agents
working on Odometer. The current checkout, tests, and source contracts take
precedence over historical branches, issue prose, memory summaries, and graph
indexes.

## 1. Establish the current truth before acting

- Start with `git status --short`, the active branch, the current commit, and
  the requested boundary. Preserve unrelated work.
- Treat live source, generated schemas, tests, and CI configuration as the
  authority. Use the knowledge graph for navigation, not for negative claims,
  when its index may be stale; reindex before relying on it structurally.
- Reconcile GitHub issues and prior reviews against merged code before calling
  work missing, duplicated, complete, or safe to close.
- If the request is for a review or plan, stop at that boundary. Do not turn a
  planning task into a broad implementation without approval.
- Keep implementation, commit/push, PR creation, release, and deployment as
  separate authorization gates.

## 2. Preserve the architecture seams

- Rust owns filesystem access, transcript parsing, persistence, scanning, and
  Tauri commands. Svelte receives typed data through IPC; it does not read
  session files directly.
- For a backend field, update the Rust model/parser, focused fixtures and
  tests, `src/lib/types.ts`, and every projection that consumes the field.
  Prefer Serde defaults or optional fields for historical data.
- For a command, implement it in `src-tauri/src/commands.rs`, register it in
  `src-tauri/src/lib.rs`, expose a typed wrapper in `src/lib/ipc.ts`, and add a
  capability only when the API truly requires one.
- Keep event names and payloads synchronized across producer, IPC listener,
  Rust serialization, TypeScript types, and UI state.
- Keep `SessionSummary` small and free of turns and token history. Fetch full
  session details only for the selected session.
- Reuse `sessions_in_ranges`, `SessionProjection`, and shared credit helpers
  for tables, analytics, comparisons, exports, tray totals, and correlation.
  Do not create a second frontend history scanner.
- Preserve the established Svelte 5 rune style and module-level state
  placement.

## 3. Treat parsing as a compatibility boundary

- Rollout files are append-only JSONL. Consume only newline-terminated
  records; retain a partial trailing record for the next watcher event.
- Malformed or unknown records must be isolated rather than aborting the file.
  Intentionally ignored records, such as `response_item` aggregate noise, must
  remain ignored while still advancing timestamp telemetry when required.
- A resumed file may repeat metadata. Refresh metadata without erasing token
  totals, history, or prior turn state.
- Keep cumulative totals, per-call deltas, model buckets, service tiers,
  rollback semantics, explicit start/completion timestamps, and event ordering
  distinct. Never use a resumed cumulative reconciliation delta as evidence for
  a single provider request.
- The `apply_line` fast path is allowed only for structurally unambiguous
  records. Otherwise fall through to the full parser.
- Normalize Codex and Claude tool observations once. Current Codex
  `custom_tool_call` records must be covered in addition to legacy call forms.
- In Claude streams, count repeated usage once per message ID; exclude
  synthetic, meta, sidechain-prompt, tool-result, command-echo, and interruption
  records from turn opening as defined by the parser contract. Key subagent
  files by file identity and link them to the parent.
- Add tests for incremental/full equivalence, partial lines, malformed input,
  resumes, model switches, rollbacks, timestamp edge cases, subagents, and
  duplicate streamed messages.

## 4. Separate durable truth from accelerators

- The scan cache is an optimization, never a source of truth. A fresh parse
  wins on size/mtime mismatch, read error, version mismatch, or uncertain
  cache state.
- Any parser, `Session`, token-evidence, or serialized-cache change requires a
  deliberate application/cache version bump and a regression test proving old
  entries cannot suppress new parsing.
- Durable history is independent of the disposable scan cache. Preserve
  sessions when sources disappear, move, or are temporarily unconfigured.
- Treat filesystem paths as availability observations, not logical identity.
  Normalize Windows paths consistently and test moves, collisions, returns,
  archive/source changes, and subagent identity.
- Persist normalized token events idempotently. Repeated scans and watcher
  passes must not duplicate events; use deterministic keys/upserts where a
  real corpus can contain repeated signatures.
- A failed or incomplete scan must not invent missing-source deletions. Older
  scan callbacks must not resurrect paths removed by a newer watcher or
  settings transition.
- Schema migrations must be forward-safe and tested against real historical
  shapes. Reject a database newer than the running application rather than
  opening it unsafely.

## 5. Make accounting evidence explicit

- Reconcile displayed numbers through the complete path: source events,
  parser totals, range rollups, `SessionProjection`, model aggregates, and
  rendered cards. A screenshot alone is not an accounting verification.
- Convert local `datetime-local` values to UTC before comparison. Session
  filters use interval overlap; date-scoped totals use the documented inclusive
  event bounds; all-time totals use cumulative summary buckets.
- Cached input is a subset of input and reasoning output is a subset of
  output. Subtract each subset before applying ordinary rates, then price the
  subset at its own rate exactly once.
- Keep Codex plan credits, Anthropic API USD, and OpenAI API-equivalent
  estimates separate in code, labels, and documentation. An API-equivalent
  number is not an invoice.
- Verify current provider pricing from authoritative dated sources before
  changing rates. Preserve provenance, effective intervals, billing surface,
  model identity, and evidence status.
- Apply conditional pricing only with request-level evidence. Missing evidence
  fails closed for the modifier; it must not trigger speculatively from a
  cumulative or resumed total.
- Use fallback rates only where the legacy flat calculation explicitly allows
  them, and label fallback, unpriced, unavailable, and scenario states
  distinctly.
- Add reconciliation tests for direct rates, fallback rates, unlimited rates,
  cached input, reasoning output, service tiers, model switches, date windows,
  unknown models, and collapsed model rows.

## 6. Keep privacy and authority boundaries fail-closed

- Process transcripts locally and never log or persist prompts, responses,
  tool arguments, tool output, commands, credentials, raw paths, or session
  identifiers in normalized telemetry or performance logs.
- Store only bounded provider/tool identifiers, aggregate measurements, hashes,
  sizes, safe diffs, and other explicitly documented metadata.
- Treat webview input as untrusted. Native save-dialog selection and final path
  validation belong in Rust; the frontend supplies format, default name, and
  content, not an arbitrary destination.
- Keep Tauri capabilities minimal. Do not add network, shell, remote content,
  or broader filesystem authority without an explicit requirement and security
  review.
- Instruction discovery is opt-in, read-only, bounded, and rooted in explicit
  user configuration or validated project scopes. Do not scan the whole
  machine by default, follow symlinks, traverse dependency/generated trees, or
  admit arbitrary files into an allowlist.
- Refresh discovery before reading or opening a file. Reject anything not in
  the current backend allowlist. Cap file size and preview length.
- Markdown is untrusted input. Rendering with Marked requires sanitization
  such as DOMPurify, or a restricted renderer; remove executable and remote
  surfaces.
- Keep optional features independently hideable and toggleable. Read-only
  inventory/preview is a valid first phase; editing, backups, and writes need a
  separate design and authorization.
- Parser fixtures, screenshots, exports, and examples must use synthetic or
  thoroughly redacted data.

## 7. Design watcher and cancellation paths as transactions

- Initial scan, incremental append, removal, archive roots, session-index
  overlays, and settings-triggered watcher restart are separate behaviors and
  need separate tests.
- Startup configuration snapshots must diff the union of old and new paths so
  changes made while the app was closed are visible.
- Couple configuration capture, scan-generation allocation, cancellation,
  allowlist clearing, and publication under one coherent synchronization
  strategy. A generation check followed by a separate publication is not
  atomic.
- A cancelled, superseded, or reconfigured operation must be unable to publish
  stale inventory, restore revoked paths, or leave stale progress visible.
- Test adversarial interleavings explicitly: settings change during discovery,
  cancellation after validation but before publication, watcher removal during
  scan, and an old callback completing after a newer generation.

## 8. Optimize from measured boundaries

- Batch parser telemetry refreshes after parse batches; do not recompute whole
  session analytics after every record.
- Keep summary IPC payloads small, range rollups backend-owned, incoming UI
  updates batched, and large lists virtualized.
- Performance tracking is local-only, opt-in, default-off, bounded, and
  redact-only. Use non-blocking writers and flush barriers before status,
  disable, or export.
- Prefer a measured baseline and a focused probe over speculative optimization.
  Ignored performance probes are not correctness evidence.
- When a semantic search service is unavailable or rate-limited, pivot to
  local exact search, targeted file reads, graph navigation, and direct tests.
  Do not repeatedly block delivery on one auxiliary service.

## 9. Label analytics according to what the data proves

- Distinguish observed use from installation, availability, and causality.
  Transcript cohorts can prove that a tool was observed, not that it was
  installed when unused or that it caused a performance difference.
- For comparisons, match on harness, model, task category, and preferably
  nearby time. Report sample counts, minimum readiness, incomplete windows,
  and confounds.
- Do not show before/after outcome deltas until the after window is complete
  and both sides meet the minimum sample threshold.
- Present deterministic heuristics as heuristics. Keep confidence,
  occurrence, likely-avoidable estimates, and evidence visible rather than
  turning them into semantic verdicts.
- Range responses must retain tool-only activity even when token totals are
  zero.

## 10. Preserve usable, accessible UI behavior

- Extend existing filtering, sorting, virtualization, projection, and
  date-scoped-total seams. Avoid rewrites that fork behavior between views.
- Verify loading, empty, filtered-empty, error, archived/source-missing,
  pricing fallback/unpriced, narrow-window, disabled/busy, and validation-error
  states for each user-visible feature.
- Keyboard interaction is part of the contract: context menus, Escape,
  focus, accessible names, expand/collapse, and narrow-window behavior need
  component tests and a live UI check.
- Use source-backed visual scenarios and synthetic deterministic fixtures.
  Do not claim repository-wide coverage from a small tested slice; keep the
  explicit coverage manifest honest and expand it only with meaningful tests.
- Browser screenshots validate webview layout and projections, not native save
  dialogs, tray menus, updater behavior, permission prompts, or platform
  filesystem semantics. Smoke those native surfaces separately.

## 11. Use the full verification ladder

For normal code changes, match CI with:

```text
npm run check
npm run test:coverage
npm run check:updater-manifest
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Add `npm run visual:test` when the affected contracts require it. Run a native
`npm run tauri dev` smoke test for runtime, IPC, dialog, tray, updater,
filesystem, or capability changes. Inspect exit codes and test results rather
than treating a Windows incremental-cleanup warning as an application failure.
Always stop task-started processes and remove temporary artifacts before
handoff.

For accuracy work, run the smallest safe read-only reconciliation against the
real corpus in addition to unit tests. For parser, history, pricing, or wire
changes, include a synthetic regression fixture and a cross-layer assertion.
Do not claim completion when a required gate failed, was skipped, or only
received a superficial visual inspection.

## 12. Release and roadmap discipline

- Keep `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`
  versions synchronized.
- Require successful exact-SHA CI before creating a signed tag. Use the
  mutable draft release for artifact assembly and validation; publish only
  after all platform artifacts, signatures, hashes, and `latest.json` are
  verified. Published releases are immutable.
- Keep each delivery focused. Commit and push only when requested; open a PR
  or publish a release only when explicitly authorized.
- Preserve the dependency order of foundational roadmap work: provider/parser
  contracts and durable normalized history precede project dimensions, pricing,
  tool attribution, quotas, optimization, and adapter surfaces.
- Treat roadmap issue relationships as planning guidance until acceptance
  criteria are verified in code. Do not close child issues merely because
  related functionality exists.

## Short pre-handoff checklist

- Current branch, diff, and requested boundary are known.
- Rust, TypeScript, IPC, tests, cache/version, and capability surfaces agree.
- Privacy, authority, cancellation, and historical-data behavior are tested.
- Accounting claims state their evidence, currency/surface, date semantics,
  and uncertainty.
- Runtime/UI/native checks match the changed surfaces.
- Required processes and temporary artifacts are cleaned up.
- The final report names what was verified, what was not, and any remaining
  external gate.
