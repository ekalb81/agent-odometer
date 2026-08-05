# 0003. Sync protocol and schema draft

- Status: Accepted as a discovery-gate decision record. No code ships;
  this is a draft schema, not a wire contract any running code implements.
- Date: 2026-08-04
- Related: [0002](0002-sync-threat-model.md),
  [0005](0005-sync-identity-crypto-lifecycle-design.md)
- Backing prototype: `src-tauri/tests/sync_design_prototype.rs`

## Design starting point: `RangeTotals` is already close to aggregate-only

`src-tauri/src/model.rs` already produces an aggregate-only projection of a
`Session` for a different reason — payload size, not privacy — via
`RangeTotals` (returned by the `sessions_in_ranges` command):

```rust
pub struct RangeTotals {
    pub tokens: TokenTotals,
    pub buckets: Vec<TierBucket>,             // (model, service_tier) -> tokens
    pub tool_metrics: ToolMetrics,             // counts only, no targets
    pub tool_metrics_by_model: BTreeMap<String, ToolMetrics>,
    pub optimization_findings_count: u64,
    pub optimization_summary: OptimizationSummary, // rule_id -> count, no evidence text
}
```

None of `RangeTotals`'s fields carry prompts, replies, raw paths, tool
output, or credentials — the same discipline that already keeps
`SessionSummary` free of `turns`/`tokens_history` for size reasons happens,
as a side effect, to keep it free of content too. The sync schema below is
deliberately shaped like `RangeTotals` plus a minimal identity envelope,
rather than a serialization of `Session` with fields removed after the
fact. **The projection is an allowlist, not a denylist**: `SyncDayAggregate`
enumerates every field it carries; there is no `#[serde(flatten)]` or
blanket conversion from `Session` that could let a newly added ledger field
reach the wire without a reviewer noticing.

## Wire schema v1

```rust
/// Everything that can ever leave a device once sync is enabled. One row
/// per (device, UTC day, harness, model, service tier, project_ref).
struct SyncDayAggregate {
    schema_version: u32,        // fail-closed: a client that does not recognize
                                 // this version refuses to merge the record.
    device_id: String,          // random, locally generated; not tied to any
                                 // account or hardware identifier.
    day: String,                // UTC calendar day, e.g. "2026-08-03" — not a
                                 // per-event timestamp.
    harness: String,            // "codex" | "claude_code"
    model: Option<String>,      // model id string; None only for the "no
                                 // model attributed yet" bucket, mirroring
                                 // RangeTotals' unattributed-usage handling.
    service_tier: Option<String>,
    project_ref: Option<String>, // HMAC-SHA256(normalized_working_dir, local_group_salt),
                                  // or None ("unassigned bucket"). See below.
    tokens: TokenTotals,         // input, cached_input, output, reasoning_output, total
    sessions_count: u64,
    turns_count: u64,
    tool_metrics: ToolMetrics,   // calls/reads/searches/mutations/commands/other/
                                  // successes/failures/duration_ms/output_bytes — all counts,
                                  // no target identity
    category_totals: BTreeMap<TaskCategory, CategoryMetric>, // enum keys only
    optimization_summary: OptimizationSummary, // rule_id -> count only, no evidence text
    revision: u64,               // monotonic per key, set by the originating device
    tombstone: bool,             // see 0005/0006
}
```

`content_hash` is deliberately *not* a wire field: dedup correctness comes
from equality of the row above, not from a hash. A hash is a useful index
optimization for a large corpus of rows, but making it load-bearing for
correctness would mean two structurally-different rows that happen to hash
the same are indistinguishable to the merge logic — equality is the
correctness primitive, hashing (if added later) is purely an index.

## Field-by-field allowlist mapping

Every ledger field that could plausibly matter to sync, and what happens to
it. "Excluded — structural" means the projection function's input type
never reads the field at all (not "reads it and drops it").

| Ledger field (`Session` / `TurnInfo` / `ToolObservation`) | Sync fate | Leak-prevention mechanism |
| --- | --- | --- |
| `id`, `storage_id` | Excluded | Not needed: `device_id` + revision key identifies rows. Reusing the ledger's own opaque id is unnecessary extra correlation surface. |
| `harness` | **Included** | Already a closed enum (`"codex"` \| `"claude_code"`); no content. |
| `first_user_message` | **Excluded — structural** | The projection's input struct/query never selects this column; there is no code path from ledger to wire that touches it. |
| `turns[].user_message`, `turns[].last_agent_message` | **Excluded — structural** | Sync operates on the `RangeTotals`-shaped aggregate, not on `turns` at all. `turns` is never part of the sync data model. |
| `working_directory` | **Excluded**, replaced by `project_ref` | Never serialized raw. See the pseudonymization construction below. |
| `file_path` | **Excluded — structural** | Never read by the projection. |
| `agent_path` | **Excluded** | Historically may carry a path-shaped value for subagents; treated as a path for this purpose and excluded pending a dedicated review if a future use case needs it. |
| `tool_observations[].target`, `.resource_id` | **Excluded** | These are already stable *hashes* in the ledger (never raw paths/args), but sync uses only `ToolMetrics` counts — even a hash of a private resource is a distinguishable, potentially-correlatable row, which the "existence leak" problem in 0007 says to avoid. |
| `tool_observations` (raw list) | **Excluded — structural** | Only the aggregated `ToolMetrics`/`ToolMetrics.by_model` counts sync, never the per-call list. |
| `optimization_findings[].evidence`, `.remediation` | **Excluded** | Free-text analyzer output; only `OptimizationSummary` (rule_id → count, avoidable-call estimate) syncs. |
| `credits_balance`, `credits_unlimited`, `plan_type` | **Excluded in v1** | Reveals subscription/account standing; not needed for token/turn/tool aggregation. Candidate for the *budgets/alerts* use case's own future design (0001), not this schema. |
| `cli_version`, `model_provider`, `originator`, `source` | **Excluded in v1** | Not needed for the aggregate use cases; low but non-zero fingerprinting value (client version, entry point) with no offsetting benefit yet. Can be added later behind an explicit allowlist review, same as any other field. |
| `tokens_total` / `tokens_history[].delta` | **Included, as day-bucketed `tokens`** | Numeric only; the schema buckets by day rather than shipping the event stream, which is also what keeps temporal fingerprinting coarse (0002, threat 4). |
| `total_turns` | **Included, as `turns_count`** | A count, not the prompts themselves. |
| `tool_metrics` (aggregate struct) | **Included** | Already count-only in the ledger's own type. |
| `category_totals` | **Included** | `TaskCategory` is a closed 7-value enum (`Planning`/`Exploration`/`Coding`/`Debugging`/`Testing`/`Review`/`Other`), assigned by a deterministic local classifier — no free text. |

## `project_ref`: how a raw path is prevented from leaking

`working_directory` is never serialized toward sync. Instead, a device that
has opted a project into sharing (opt-in per project, off by default — see
0007 for why the default matters) computes:

```
project_ref = "prj_" + hex(HMAC-SHA256(key = local_group_salt, message = normalized_working_directory))
```

- `normalized_working_directory` uses the same case/separator normalization
  as `src-tauri/src/paths.rs::normalized_path_key`, so the same project
  reached via different path spellings still yields one handle.
- `local_group_salt` is a random secret generated on first pairing and
  shared **only** with a device's own sync group during out-of-band pairing
  (0002's sequence diagram) — it is never itself a synced field and never
  reaches the backend.
- Same salt → same handle, so a user's own paired devices (or a team that
  shares a salt) can group usage by project consistently.
- Different salt → different handle for the same literal path, so two
  unrelated users' project names are not comparable even if they happen to
  use identical directory names, and the backend operator cannot recover
  the path from the handle without the salt.
- A project the user never explicitly marks as shared contributes to
  `project_ref = None` (the unassigned bucket) at exactly the same
  granularity as every other unassigned row — not as its own distinguishable
  hashed-but-unnamed row. See 0007 for why that distinction is the actual
  requirement, not a nice-to-have.

## Versioning and fail-closed migration

`schema_version` follows the same fail-closed precedent already established
by `history_store.rs`'s `PRAGMA user_version` gate ("a schema newer than the
running application is rejected instead of opened unsafely"): a client that
receives a `SyncDayAggregate` with a `schema_version` it does not recognize
refuses to merge that row and surfaces an "update Odometer" state, rather
than guessing at a forward-compatible interpretation. New fields are
additive with Serde defaults, mirroring the existing `Session`/
`SessionSummary` compatibility discipline in `AGENTS.md`. See 0006 for the
full migration policy, including why a breaking schema change here is
cheaper than a typical remote-data migration (every row is re-derivable
from the local ledger, so "fix and re-push" replaces "write a migration for
data you no longer have the source of").

## Sample payloads

**Plaintext aggregate (what the preview shows the user, and what gets
encrypted before it ever reaches a backend):**

```json
{
  "schema_version": 1,
  "device_id": "dev_3f7a9c2e1b6d4a58",
  "day": "2026-08-03",
  "harness": "claude_code",
  "model": "claude-sonnet-4.5",
  "service_tier": null,
  "project_ref": "prj_9e2c7a1f4b8d0361",
  "tokens": {
    "input_tokens": 148230,
    "cached_input_tokens": 62110,
    "output_tokens": 39871,
    "reasoning_output_tokens": 0,
    "total_tokens": 188101
  },
  "sessions_count": 3,
  "turns_count": 21,
  "tool_metrics": {
    "calls": 87, "reads": 41, "searches": 12, "mutations": 19,
    "commands": 15, "other": 0, "successes": 79, "failures": 5,
    "unknown": 3, "mutation_targets": 14, "one_shot_mutations": 11,
    "retry_count": 4, "duration_ms": 96210, "output_bytes": 214003
  },
  "category_totals": {
    "coding": { "turns": 12, "tokens": { "input_tokens": 90210, "cached_input_tokens": 40000, "output_tokens": 21000, "reasoning_output_tokens": 0, "total_tokens": 111210 }, "tool_calls": 55, "buckets": [] },
    "debugging": { "turns": 6, "tokens": { "input_tokens": 40020, "cached_input_tokens": 15000, "output_tokens": 12000, "reasoning_output_tokens": 0, "total_tokens": 52020 }, "tool_calls": 28, "buckets": [] }
  },
  "optimization_summary": {
    "findings": 2, "warnings": 1, "likely_avoidable_calls": 6,
    "by_rule": { "repeated-read": 2 }
  },
  "revision": 1,
  "tombstone": false
}
```

**Deletion (tombstone) for the same key:**

```json
{
  "schema_version": 1,
  "device_id": "dev_3f7a9c2e1b6d4a58",
  "day": "2026-08-03",
  "harness": "claude_code",
  "model": "claude-sonnet-4.5",
  "service_tier": null,
  "project_ref": "prj_9e2c7a1f4b8d0361",
  "tokens": { "input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": 0 },
  "sessions_count": 0,
  "turns_count": 0,
  "tool_metrics": { "calls": 0, "reads": 0, "searches": 0, "mutations": 0, "commands": 0, "other": 0, "successes": 0, "failures": 0, "unknown": 0, "mutation_targets": 0, "one_shot_mutations": 0, "retry_count": 0, "duration_ms": 0, "output_bytes": 0 },
  "category_totals": {},
  "optimization_summary": { "findings": 0, "warnings": 0, "likely_avoidable_calls": 0, "by_rule": {} },
  "revision": 2,
  "tombstone": true
}
```

**On the wire (what actually reaches a backend):** the JSON body above,
serialized, then sealed as `XChaCha20-Poly1305(key, nonce, plaintext)` under
the device/group key from 0005, plus the unavoidable envelope
(`device_id`, `day`, `harness`, ciphertext length) needed to route and
dedup it without decrypting — no field beyond that envelope is visible to
the backend. This boundary is exercised directly by
`encryption_boundary_round_trips_and_rejects_tampering` in the prototype.

## Worked example: inspecting exactly what would leave the device

Before the opt-in toggle can be enabled (0001, unblock criterion 3), the
app runs the identical allowlist projection used for real sync against the
user's own recent ledger data and writes the plaintext result — the exact
JSON shown above, not a sanitized summary of it — to a local file the user
can open in any text editor, with no network call involved. This is the
same "preview before you trust it" pattern already established for the
`Instructions` view's redacted-event pipeline in `docs/ARCHITECTURE.md`
("Config capture ... never persist config values or raw paths").

The prototype demonstrates the mechanism this preview relies on:
`projection_excludes_every_banned_field_even_though_the_source_carries_them`
builds a `SyncDayAggregate` from a fact set that *does* contain a prompt, a
reply, a real-looking path, and sample tool output, serializes it, and
asserts none of that content appears in the resulting JSON string — the
same check a user could perform manually on the preview file with
`grep`/`Select-String` for their own prompt text before ever enabling sync.
