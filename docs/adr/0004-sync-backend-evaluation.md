# 0004. Sync backend evaluation

- Status: Accepted as a discovery-gate decision record. No backend
  integration ships.
- Date: 2026-08-04
- Related: [0002](0002-sync-threat-model.md), [0003](0003-sync-protocol-schema.md)

## Evaluation criteria

Because the payload is always end-to-end encrypted before it reaches any
backend (0002, 0005), a backend's job is reduced to durable, addressable
blob storage plus a way to list/read/write/delete. That is deliberate: it
means the choice of backend is a storage-and-operations decision, not a
trust decision, and the core ledger and sync logic must not be coupled to
any one backend's API shape.

| Criterion | What it measures |
| --- | --- |
| Key ownership | Who can end up holding decryption keys, even accidentally |
| Metadata leakage to operator | What the backend operator learns without decrypting anything |
| Cost | To the user, at realistic sync volumes (small — day-bucketed aggregates, not transcripts) |
| Offline behavior | What happens when the backend is unreachable |
| Deletion verifiability | Whether "delete" can be confirmed by reading the backend back, and whether the backend itself retains deleted bytes elsewhere (versioning, history, backups) |
| Vendor coupling | How much backend-specific code/API surface the core ledger would need to know about |

## Backends

| Backend | Key ownership | Metadata leakage to operator | Cost | Offline behavior | Deletion verifiability | Vendor coupling |
| --- | --- | --- | --- | --- | --- | --- |
| **Local file** (a folder the user already syncs with their own tool — Syncthing, Dropbox, iCloud Drive, a mounted network share) | Entirely user/device; Odometer never talks to a "vendor" at all | None beyond whatever the user's own sync tool already sees (outside Odometer's control, already true today) | Free (reuses infrastructure the user already has) | Read/write local files always succeeds; propagation to other devices is however fast the user's own sync tool is | High — Odometer deletes the local file, and can immediately re-read the directory to confirm; propagation delay is the user's sync tool's problem, not verifiability | None — plain file I/O |
| **GitHub** (private repo, one blob per device/day, or packed) | User's GitHub account/PAT; Odometer stores no GitHub credential beyond a narrowly-scoped token in OS-native secure storage | GitHub sees repo size, commit cadence, requester IP; commit metadata (author, timestamp) if not carefully minimized | Free for private repos at this data volume | Requires network; git operations queue locally and retry, per **offline operation** in 0005 | **Weak** — git history retains old blob contents by default; "deleting" a file is a new commit, not a removal of history. Real deletion requires history rewrite (`git filter-repo`/force-push), which is destructive to a shared repo and must be disclosed as a caveat, not silently promised | Moderate — needs a git client/API integration, but the payload format itself stays backend-agnostic |
| **S3-compatible** (AWS S3) | User's AWS account/credentials | AWS sees bucket access patterns, object sizes, requester IP; CloudTrail-style logging is the user's own account, not Odometer's | Small (near-free at this volume, but a real recurring line item on the user's own bill, unlike local file) | Requires network; standard retry/backoff | **Good** if bucket versioning is off (delete removes the current object and a listing confirms it); **weak** if versioning is on, since S3 retains prior versions by default — recommend the app document (not silently assume) that the sync bucket should have versioning disabled | Moderate — S3 API is a de facto standard other providers mimic, keeping this close to "S3-compatible" broadly |
| **R2 / MinIO** (S3-compatible: Cloudflare R2 or self-hosted MinIO) | User's Cloudflare account, or fully self-hosted (user's own server/keys end-to-end) | R2: Cloudflare sees the same class of metadata as AWS S3. MinIO self-hosted: only the user's own infrastructure sees anything | R2: free tier covers this volume comfortably (no egress fee is the specific reason to prefer R2 over raw S3 for a bandwidth-sensitive open-source tool's users). MinIO: free (self-hosted) | Same as S3-compatible | Same considerations as S3-compatible; self-hosted MinIO gives the user full control over retention/versioning policy | Low — same S3-compatible API surface as above, so one adapter serves S3, R2, and MinIO |
| **Self-hosted service** (a small Odometer-authored sync server) | Whoever runs the server; if that's the user, equivalent to local file/self-hosted backup. If it's a service *Odometer* would operate, this becomes a hosted service — explicitly out of scope for this issue and gated behind its own product decision (0001) | If self-run by the user: none beyond local file. If Odometer-operated: the operator sees everything a backend operator sees under "compromised storage" (0002) by default, all the time, not just under compromise | Self-run: user's own hosting cost. Odometer-operated: ongoing operational cost and liability, not evaluated here | Requires network to the server; otherwise same retry pattern | Depends entirely on the server's own implementation — this is the backend with the most that could go right *or* wrong, because Odometer would own the delete semantics end to end | Highest — a bespoke server means a bespoke API, unless it deliberately speaks the same blob-storage contract as everything else above |

## Avoiding vendor coupling

The core ledger and the sync projection (0003) must depend only on a small
trait, not on any backend's SDK:

```rust
trait SyncBackend {
    fn put(&self, key: BlobKey, ciphertext: &[u8]) -> Result<()>;
    fn get(&self, key: BlobKey) -> Result<Option<Vec<u8>>>;
    fn list_since(&self, watermark: Watermark) -> Result<Vec<BlobKey>>;
    fn delete(&self, key: BlobKey) -> Result<()>;
}
```

Every backend in the table above implements the same four operations. This
is the same seam the codebase already uses elsewhere to keep a boundary
thin and swappable — compare how `history_store.rs` isolates all SQLite
specifics behind a small set of functions the rest of the app calls,
without the caller knowing it's SQLite. Nothing about `SyncDayAggregate`,
the encryption boundary, or the merge logic changes based on which
`SyncBackend` is configured.

## Recommendation

Ship backends in this order, and no faster than demonstrated need justifies:

1. **Local file** first. It requires no new Tauri network capability (it's
   still local filesystem I/O — the user's own sync tool, not Odometer,
   does the networking), has the best deletion verifiability, and fully
   validates the projection/encryption/merge design end-to-end before any
   network code exists at all. This is the natural MVP for personal
   multi-device sync and self-hosted backup (0001, use cases 1–2).
2. **R2/MinIO** (one S3-compatible adapter) second, specifically because
   R2's no-egress-fee model fits an open-source tool whose users are not
   paying Odometer for anything, and MinIO gives fully self-hosted users
   the same adapter. This is the first backend that requires an explicit,
   reviewed Tauri network capability.
3. **GitHub** third, with the history-rewrite deletion caveat documented
   prominently in-product wherever a user picks it, not just in this ADR.
4. **Self-hosted service** last, and only ever as "a server the user runs,"
   never as an Odometer-operated hosted service — the latter is a different
   product decision this ADR set does not make (0001).

Raw S3 is intentionally not separately ranked above R2/MinIO: it shares an
adapter and a risk profile with them, and a user who wants "real AWS S3"
specifically can already point the same adapter at it.
