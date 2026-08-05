# 0006. Compatibility, migration, backup, restore, and deletion policy

- Status: Accepted as a discovery-gate decision record. No code ships.
- Date: 2026-08-04
- Related: [0003](0003-sync-protocol-schema.md), [0005](0005-sync-identity-crypto-lifecycle-design.md)

## Compatibility and versioning

`SyncDayAggregate.schema_version` (0003) governs the wire format the same
way `PRAGMA user_version` governs `history-v1.sqlite3`: forward-only, and
fail-closed on a version newer than the running app understands. New
fields must be additive with Serde defaults, matching the existing
`Session`/`SessionSummary` compatibility rule in `AGENTS.md` ("Add
backward-compatible Serde defaults when persisted or historical data may
omit a new field"). A field can be *added* to a new schema version without
breaking older clients reading it (they ignore fields they don't recognize
via ordinary Serde behavior); a field can never be *repurposed* — that
requires a new `schema_version`.

## Migration

A traditional remote-data migration writes a script that transforms
already-stored data in place, because the source of truth for that data no
longer exists anywhere else. That is not true here: **every
`SyncDayAggregate` is fully re-derivable from the originating device's
local ledger.** When a schema change is needed, the originating device
re-runs the (updated) allowlist projection over its own ledger and re-pushes
under the new `schema_version` with an advanced `revision` — "recompute and
re-push" replaces "migrate what's already out there." This is a direct
consequence of the aggregate-only design (0003): nothing was ever shipped
that only exists in the synced copy.

The one thing this does *not* solve: a device that is offline, decommissioned,
or has had sync disabled cannot re-push under the new schema. Its last-pushed
rows remain valid under their original `schema_version` until that device
returns — which is exactly why `schema_version` must be checked per-row,
not once per sync session, and why a receiving device must be able to hold
mixed schema versions from different peers simultaneously rather than
assuming the whole group upgraded together.

## Backup

**Sync is not a backup of the ledger, and must never be presented as one.**
Because the sync schema is aggregate-only by design (0003), a synced blob
cannot reconstruct the transcripts, turns, prompts, or tool history that
produced it — only the numeric rollups. The ledger's own backup story (a
file copy of `history-v1.sqlite3` under the documented local-data
directory, already user-accessible today) is the actual backup mechanism
for full-fidelity data, and this design does not change or replace it.
Self-hosted backup (0001, use case 2) is a backup of the *aggregate view*,
useful for continuity of dashboards/budgets across a lost device, not a
substitute for backing up the ledger file itself.

This distinction must be stated plainly wherever "backup" appears in
product copy: syncing to a self-hosted target backs up what you'd see on
the dashboard, not what you'd need to recover a transcript.

## Restore

A fresh device with no local ledger of its own can pull existing peer
aggregates and reconstruct **aggregate-level** history — charts, rollups,
tool-metric summaries — for any day another device already pushed. It can
never recover the original per-turn transcripts, because those were never
uploaded by any device (0003). Restore is explicitly partial, and the UI
must disclose this at the moment of restore (e.g., "this device now shows
synced totals from your other devices; opening a specific session's detail
view requires that session's own transcript to still exist on the device
that produced it"), not bury it in documentation.

## Deletion policy

Two distinct actions, with distinct blast radii, must never be conflated in
the UI or the implementation:

| Action | Effect | Effect on local ledger |
| --- | --- | --- |
| **Disable sync** | Stops future pushes/pulls. Local synced-aggregates mirror is, by user choice, either kept as a read-only last-known view or cleared — the user picks, defaulting to "keep." | **Untouched.** Disabling sync cannot reach the ledger by construction (0002's trust-boundary table: there is no edge from the synced mirror to the ledger). |
| **Delete my synced data** | Removes remote ciphertext objects (via `SyncBackend::delete`, verified by round-trip re-read per 0005), the local synced-aggregates mirror, and this device's sync key material for that group. A separate, more destructive, explicitly-confirmed action from "disable" — not its default consequence. | **Untouched**, for the same structural reason. |

This directly targets the issue's acceptance criterion — "Disable/delete
flows are tested and do not erase the local ledger unintentionally" — by
making it a property of the data-flow topology (there is no code path from
either action to the ledger) rather than a behavior that has to be
independently re-verified every time the sync code changes. The test
surface for this, once code exists, is: assert the ledger's row count and
content hash are identical before and after both `disable_sync()` and
`delete_synced_data()`, for a ledger that had data before sync was ever
enabled.

Backend-level deletion caveats (GitHub history retention, S3 versioning,
self-hosted server backups — 0004) apply on top of this: Odometer's
deletion action is complete and verifiable *within what the backend
exposes to it*, but cannot force a backend operator's own out-of-band
retention to also purge. This must be disclosed per backend at the point a
user chooses one (0004), not only in this document.
