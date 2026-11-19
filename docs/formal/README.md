# Odometer formal model

This TLA+ specification models the behavior of Odometer v0.8.31 at its accounting, concurrency, persistence, and trust boundaries. Each TLC configuration has a finite set of inputs so the checker can explore every reachable state within those bounds. [Architecture](../ARCHITECTURE.md)

## Run the checks

Use Java 11 or later, the project's Node.js runtime, and `tla2tools.jar` from the [TLA+ tools 1.7.4 release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4). That distribution runs TLC 2.19.

The GitHub Actions Formal model job runs the same command with Java 21. It checks the jar against the release's pinned SHA-256 before execution.

```sh
node docs/formal/check.mjs path/to/tla2tools.jar
node docs/formal/check.mjs path/to/tla2tools.jar Watcher Ledger
```

With no configuration names, the runner checks every `.cfg` alongside the specification. It then runs [the fault injections and reachability checks](controls.mjs). Each injection must violate the named property, and each reachability check must find its required state. A failed check exits nonzero and prints the TLC error and any counterexample. The runner uses temporary copies of the models so TLC's state files and trace artifacts stay outside the repository.

## Model coverage

| Module | Contract | Implementation to compare |
| --- | --- | --- |
| [Parser](Parser.tla) | Complete-record boundaries, token attribution, and turn state across provider record shapes | [Parser model](../ARCHITECTURE.md#parser-model), [Claude Code parser model](../ARCHITECTURE.md#claude-code-parser-model) |
| [ScanCache](ScanCache.tla) | Cache trust by parser version and file metadata, and fresh parsing on a cache miss | [SQLite scan cache](../../src-tauri/src/scan_cache.rs) |
| [CodexDelta](CodexDelta.tla) | Early unlabelled usage, last-only records, and replayed cumulative records | [Codex parser](../../src-tauri/src/parser.rs) |
| [Reconciliation](Reconciliation.tla) | Disjoint cache-write accounting when cumulative Codex evidence arrives late | [Codex parser](../../src-tauri/src/parser.rs) |
| [Ingestion](Ingestion.tla) | Provider-scoped identity, compatible copies and replays, and divergent lineages | [Durable-history contract](../ARCHITECTURE.md#durable-history-contract) |
| [Watcher](Watcher.tla) | Bulk scans, live observations, path removal, root changes, and stale callbacks | [Startup and live-update flow](../ARCHITECTURE.md#startup-and-live-update-flow) |
| [Hydration](Hydration.tla) | Startup snapshot delivery, buffered events, and terminal scan status | [Desktop startup](../../src/App.svelte) |
| [Ledger](Ledger.tla) | Durable facts, transactional changes, hourly rollups and partial-hour edges, and genuine unavailability | [SQLite history](../../src-tauri/src/history_store.rs), [IPC and frontend state](../ARCHITECTURE.md#ipc-and-frontend-state) |
| [DurableWrites](DurableWrites.tla) | Rollups reflect inserted facts, an upgrade repairs stale totals, and a provisional session can keep its key only with a compatible prefix | [SQLite history](../../src-tauri/src/history_store.rs) |
| [RetainedRepair](RetainedRepair.tla) | Repair from retained evidence with a missing transcript, covering snapshots, turns, rollups, and legacy keys | [SQLite history](../../src-tauri/src/history_store.rs) |
| [Pricing](Pricing.tla) | Price evidence, token subsets, effective rates, billing surfaces, and card refresh | [Pricing catalog](../ARCHITECTURE.md#pricing-catalog-contract) |
| [Access](Access.tla) | Revision-bound transcript pages, record anchors, and retained search targets | [Persistence and privacy](../ARCHITECTURE.md#persistence-and-privacy) |
| [Private](Private.tla) | Revision-bound organization edits, reviewed curation and freezes, and deletion cascades | [Curated datasets](../CURATED_DATASETS.md), [private organization](../ARCHITECTURE.md#private-organization) |
| [External](External.tla) | Provider-status opt-in, account consent, and stale evidence | [Quotas](../QUOTAS.md) |
| [Alerts](Alerts.tla) | Ambient alert and live-account eligibility, quiet hours, startup, and unknown evidence | [Shared alerts and provider tray](../AMBIENT_CONTROLS.md) |
| [Boundary](Boundary.tla) | Local integration authority, export privacy, and signed update trust | [Integration Center](../INTEGRATION_CENTER.md), [auto-update](../ARCHITECTURE.md#auto-update) |
| [Odometer](Odometer.tla) | End-to-end interleavings across source discovery, ledger authority, and published frontend results | [Startup and live-update flow](../ARCHITECTURE.md#startup-and-live-update-flow), [IPC and frontend state](../ARCHITECTURE.md#ipc-and-frontend-state) |

The model uses synthetic records and small domains for paths, versions, identities, and token values. It treats transaction commit, OS calls, and signature verification as abstract operations. Rust/SQLite and browser tests still exercise the actual parser, SQL statements, numeric representation, platform behavior, and frontend. Replay any useful TLC counterexample as a regression at the relevant production boundary.

The watcher model assumes weak fairness for the scan worker's inspection, write, and repair steps. It permits a temporary stale durable row until repair while requiring the live projection to honor the removal. Ledger configurations exercise multiple durable scan generations, including in the same watched-root configuration.

Hydration and Odometer share the [startup gate](StartupGate.tla). They distinguish a failed session-list attempt from proof that a complete snapshot was installed. The watcher token abstracts the native handle identity and lock. Receipt revisions abstract file timestamps. Parser checks use representative record sequences, and ScanCache treats the reported filesystem stamps as evidence about the contents. The alert properties cover the shared ambient queue and live-account conditions within one deduplication retention interval. Legacy budget alerts are covered by application tests.

## Implementation regressions

| Failure | Production test boundary |
| --- | --- |
| Pre-model, last-only, or replayed Codex usage changes date-scoped totals | [Codex parser integration tests](../../src-tauri/tests/parser_tests.rs) |
| Duplicate inserts inflate hourly totals or old rollups survive an upgrade | [SQLite store tests](../../src-tauri/src/history_store.rs) |
| Upgrading with a missing transcript leaves a provably repairable session flagged and blocks the aggregate | [SQLite migration tests](../../src-tauri/src/history_store.rs) |
| A third first-turn fingerprint collides, or a file replacement reuses the old last-only prefix | [SQLite identity tests](../../src-tauri/src/history_store.rs) |
| A warm cache hides newer parser output | [Scanner upgrade test](../../src-tauri/tests/scan_cache_tests.rs) |
| A failed session list lets empty totals reach the UI or tray | [App startup tests](../../src/App.pricingRefresh.test.ts) |
| A callback from a replaced watcher writes to the current ledger | [Native watcher tests](../../src-tauri/src/watcher.rs) |
| An old receipt is accepted after hook configuration changes | [Integration status tests](../../src-tauri/src/harness_integration.rs) |
| Missing, zero, and threshold request sizes are treated as equivalent | [Bundled rate-card pricing test](../../src-tauri/tests/billing_surfaces_regressions.rs) |

## What the first model missed

Several safety properties described what a result must look like if one appeared. The checker could satisfy those properties when a query never started or a successful result was never delivered. The end-to-end model also supplied a successful snapshot by construction, while the startup model allowed a failed list to open the gate.

The initial ledger model built rollups from an already deduplicated event sequence. It skipped the distinction between an attempted insert and an inserted row, and its lineage examples varied token history when production can distinguish sessions by first-turn evidence alone. The cache model followed the architecture guide's old application-version rule. The code had moved to an independent parser version, so an app release did not invalidate cached parse output.

Release review found an interaction between two fixes. A last-only Codex record now supplies a real token fact before the session has a cumulative fingerprint. When a different file replaces that path, the new record must prove it extends the old prefix before it can keep the old key. An already-bound shorter copy can still resolve to the same session after another copy supplies that fingerprint. The regressions cover replacement, normal append, and moved or replayed sources.

For important guarantees, check a healthy trace that reaches the consumer, an adverse trace that exercises the guard, and a production regression that replays the same scenario with synthetic inputs. Record the true origin of a callback separately from the dispatcher's label, so a relabelled callback cannot pass its own check. Verify compatibility and identity fields against the source. Check handoffs for the same failure states before treating separate model results as end-to-end evidence. When a model uses synthetic billing values, pair it with the Rust pricing tests that exercise the bundled rate card.

## Verification record

TLC 2.19 on Java 21 completed all 17 configurations. The runs explored 465,634 distinct states across their separate state spaces. Ledger was the largest configuration at 410,121 distinct states. TLC detected all 29 injected faults and found all 12 required reachable states.
