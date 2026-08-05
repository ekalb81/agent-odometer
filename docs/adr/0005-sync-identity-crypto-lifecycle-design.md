# 0005. Device identity, encryption, and lifecycle design

- Status: Accepted as a discovery-gate decision record. No code ships.
- Date: 2026-08-04
- Related: [0002](0002-sync-threat-model.md), [0003](0003-sync-protocol-schema.md)
- Backing prototype: `src-tauri/tests/sync_design_prototype.rs`

## Hard constraint

The issue's acceptance criteria require: **encryption keys must not be
controlled solely by an Odometer-hosted service.** This design satisfies
that two ways at once — first structurally (0001/0004: the recommended
backends for the entire scope of this issue are local file and
user-controlled/self-hosted storage; there is no Odometer-hosted service in
this design at all), and second even if a hosted relay were ever added
later (below): keys are generated on-device and exchanged only through
device-to-device pairing, never issued, escrowed, or recoverable by a
server.

## Device identity

On first enabling sync, a device generates, entirely locally:

- An **Ed25519** signing keypair — proves "this aggregate came from this
  device" (0002, threats 3 and 6).
- An **X25519** key-agreement keypair — used only during pairing, to wrap a
  shared symmetric key for a new group member without a server ever seeing
  the shared key in the clear.

Both private keys live in OS-native secure storage (Keychain on macOS,
Credential Manager on Windows, Secret Service on Linux) — the same class of
storage already implied by "Odometer never stores backend account
credentials beyond what's needed" (0002, threat 2), extended to key
material. `device_id` is a random identifier with no relationship to
hardware serials, MAC addresses, or the harness's own session/thread ids —
it identifies "this Odometer installation," nothing else.

Device identity is **entirely separate from account identity**, because
there is no account (see "Account-free mode" below).

## Encryption and key ownership

- Payload encryption: **XChaCha20-Poly1305** (AEAD — confidentiality and
  integrity together, and a 24-byte nonce large enough to generate randomly
  per message without a meaningful collision risk, unlike the 12-byte
  nonce of plain ChaCha20-Poly1305/AES-GCM at sync-scale message counts).
- Key agreement for pairing: **X25519**. When device A pairs with device B,
  they exchange public keys out-of-band (QR code, short pairing code, or a
  small file — never through the sync backend and never through any
  Odometer-run service), then A wraps the group's shared symmetric key for
  B's public key. The backend, if it ever sees this exchange at all, sees
  only already-wrapped key material it cannot open.
- Signing: **Ed25519**, over the plaintext aggregate before encryption, so
  a receiving device verifies authorship immediately after decrypting.
- **Group key model:** one random 32-byte symmetric key per sync group
  (a personal multi-device group, or a team). Every aggregate is encrypted
  once, with the group key — not once per recipient — and every group
  member holds their own wrapped copy of that same key, wrapped for their
  own X25519 public key at pairing time. Revoking a device removes its
  wrapped copy from the roster and, for team groups where a stronger
  guarantee is wanted, rotates the group key and re-wraps for remaining
  members (personal-device groups may skip rotation, since a revoked
  personal device is the same user's own former device, not a third
  party — this is a deliberate cost/benefit choice to keep the common case
  cheap).

The prototype's fixed demo key/nonce
(`encryption_boundary_round_trips_and_rejects_tampering`) stands in for
this real key-agreement output — it proves the *boundary* (plaintext never
serialized to disk or network unencrypted; a single flipped ciphertext byte
is rejected, not silently misdecrypted) without implementing the full X25519
pairing flow, which is design, not yet code, per this issue's scope.
`chacha20poly1305` is added as a **dev-dependency only**
(`src-tauri/Cargo.toml`), specifically so this proof exists without adding
any weight to the shipping binary — see "Prototype dependency tradeoff"
below.

## Deduplication

Every aggregate is keyed by `(device_id, day, harness, model,
service_tier, project_ref)`. Two aggregates with the same key and the same
content are the same fact observed twice — applying the second is a no-op.
This is proved directly by
`merging_the_same_aggregate_twice_is_a_no_op` in the prototype: applying an
identical `SyncDayAggregate` three times leaves exactly one row.

## Conflict handling

The key design choice that avoids most conflict-resolution complexity:
**a device only ever writes rows under its own `device_id`.** Two devices
can never disagree about the same row, because there is no row two devices
both own. The only "conflict" that can occur is a device revising *its own*
past output — for example, re-deriving a corrected aggregate after a parser
bug fix changes historical token accounting. That case is resolved by the
monotonic `revision` counter (0003), not by field-level merging: a higher
revision for a key fully replaces the prior value; a revision at or below
the last-seen value for that key is rejected (0002, threat 6), proved by
`a_higher_revision_replaces_but_a_replayed_stale_revision_is_rejected`.

**Team totals are never stored as a pre-merged blob.** A viewing device
computes "the team's total tokens this week" by folding over the set of
per-device signed aggregates it has locally — client-side, recomputed
whenever new aggregates arrive. There is no cross-device merge logic to get
wrong, because there is no cross-device merge at all — only a read-time sum
over independently-owned, independently-signed rows. This also means a
malicious peer (0002, threat 3) can only ever inflate its own line item,
never silently overwrite someone else's.

## Offline operation

Every device keeps writing to its own local ledger exactly as it does
today, regardless of sync connectivity — sync never sits on that write
path. A device with sync enabled maintains:

- A local **outbox** of day-aggregates not yet successfully pushed,
  retried on the existing watcher/background-task cadence.
- A per-peer **high-water-mark** (last successfully pulled revision per
  device) so reconnecting after an arbitrary offline period only needs to
  fetch what's new, not replay everything.

This mirrors the existing "the scan cache is an optimization, never a
source of truth" discipline from `AGENTS.md`: sync is an optional
accelerator to a shared view, never a dependency for the app's core local
function.

## Tombstones

Deletion is represented as a signed aggregate with `tombstone: true` and an
advanced `revision` for its key (0003), applied through the exact same
idempotent-merge path as ordinary data. Backends are expected to retain
tombstones for at least as long as they might retain the data they shadow
— documented per-backend in 0004 (this is exactly where GitHub's
history-retention caveat and S3 bucket-versioning caveat come from).
Tombstones never reach the ledger; they only ever affect the local
synced-aggregates mirror (0002's trust-boundary table).

## Export

The synced-aggregates mirror gets its own export path, reusing the
existing backend-owned native-dialog pattern already used for the
performance-event exporter (`docs/ARCHITECTURE.md`: "can be exported
through backend-owned native dialogs as JSONL or CSV") rather than a new
one — the frontend supplies format/name, Rust validates the destination.
This keeps "what does my synced data actually contain" auditable at any
time, not just at the pre-enable preview moment (0003).

## Account-free mode

There is no Odometer account in this design, in any use case, including
team aggregation. "Identity" is a device keypair; "grouping" is a shared
secret exchanged directly between the user's own devices or a team's
devices via out-of-band pairing (QR code, short code, or a small exported
file) — never a central directory of users, never an email/password,
never an Odometer-run auth server. This is not a mode alongside an
account-based one; it is the only mode this design has, which is also
what makes "no account system ships" a structural property of the
recommendation rather than a promise about a future phase.

## Verifiable deletion

- **Local:** deleting the synced-aggregates mirror is an ordinary local
  delete; the export path (above) can confirm it's empty immediately.
- **Remote:** because the backend never holds plaintext or keys, "delete
  remote data" means removing the ciphertext objects via the same
  `SyncBackend::delete` used to write them (0004). Verification is a
  round-trip: after issuing the delete, Odometer re-lists/re-reads the
  backend and confirms zero matching objects remain, rather than treating
  "the delete call returned success" as sufficient proof.
- **What this cannot verify:** a backend's own retention outside Odometer's
  view — S3 object versioning, a self-hosted server's own backups, or
  GitHub's repo history/forks — may retain deleted ciphertext bytes the
  round-trip check cannot see (0002, threat 8; 0004's per-backend
  deletion-verifiability column). This is disclosed as a backend property
  to check before choosing it, not resolved by the app.

## Prototype dependency tradeoff

`chacha20poly1305` is a small, pure-Rust, actively maintained RustCrypto
crate with no OpenSSL/system-library dependency, added under
`[dev-dependencies]` in `src-tauri/Cargo.toml`. As a dev-dependency it is
compiled only for `cargo test`/`cargo bench`/example targets and is not
linked into the shipping `agent-odometer` binary or its `cdylib`/
`staticlib` outputs — `cargo tree --manifest-path src-tauri/Cargo.toml -e
normal` (the shipping dependency graph) does not include it. This was
chosen over a dependency-free toy cipher specifically because a security
prototype that claims to demonstrate an "encryption boundary" should use a
real, reviewed AEAD implementation rather than an ad hoc construction that
could itself be mistaken for a design recommendation.
