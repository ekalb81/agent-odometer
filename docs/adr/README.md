# Architecture decision records

ADRs in this directory record decisions Odometer has actually made. Unlike
the rest of `docs/`, this directory is allowed to accumulate historical
documents that describe a point-in-time decision rather than the current
state of the code — see each ADR's `Status` line before treating it as
current.

## Sync and team-aggregation design gate (issue [#49](https://github.com/ekalb81/agent-odometer/issues/49))

Odometer is local-only today: it reads transcripts from disk, keeps its
durable ledger on disk, and uploads nothing. Issue #49 is a **discovery and
security gate**, not a feature. It asks for a decision record before any
sync code exists, so that identity, encryption, conflict resolution,
retention, deletion, and possible team/leaderboard aggregation are chosen
deliberately instead of being implied by whatever the first PR happens to
do. No production upload, hosted service, account system, telemetry, or
public leaderboard ships as part of these documents.

Read them in this order:

1. [0001-opt-in-sync-use-cases-and-go-no-go.md](0001-opt-in-sync-use-cases-and-go-no-go.md) —
   the five use cases evaluated separately, and the three go/no-go
   decisions (personal sync, teams, leaderboard).
2. [0002-sync-threat-model.md](0002-sync-threat-model.md) — data-flow and
   trust-boundary diagrams, and the eight required threats with mitigation
   and residual risk.
3. [0003-sync-protocol-schema.md](0003-sync-protocol-schema.md) — the
   versioned, aggregate-only wire schema derived from `history_store.rs`
   and `model.rs`, the banned-field mapping, and how a user inspects what
   would leave their device.
4. [0004-sync-backend-evaluation.md](0004-sync-backend-evaluation.md) —
   local file, GitHub, S3-compatible, R2/MinIO, and self-hosted service,
   evaluated and ranked without coupling the ledger to one vendor.
5. [0005-sync-identity-crypto-lifecycle-design.md](0005-sync-identity-crypto-lifecycle-design.md) —
   device identity, key ownership, deduplication, conflict handling,
   offline operation, tombstones, export, account-free mode, verifiable
   deletion.
6. [0006-sync-compatibility-migration-backup-restore-deletion.md](0006-sync-compatibility-migration-backup-restore-deletion.md) —
   versioning, migration, backup/restore expectations, and the
   disable-vs-delete policy.
7. [0007-team-budgets-and-leaderboard-design.md](0007-team-budgets-and-leaderboard-design.md) —
   currency, pricing revisions, model aliases, missing providers, and the
   intentionally-private-project problem for team and leaderboard scopes.

A runnable prototype backing the encryption-boundary and merge-idempotency
claims in 0003 and 0005 lives at
[`src-tauri/tests/sync_design_prototype.rs`](../../src-tauri/tests/sync_design_prototype.rs)
and runs under the ordinary `cargo test` suite. It is local-only, adds no
runtime dependency to the shipping app, and never touches the real
`history-v1.sqlite3` ledger.
