//! Local-only prototype for issue #49 (opt-in sync design gate).
//!
//! This file backs the claims made in `docs/adr/0003-sync-protocol-schema.md`
//! and `docs/adr/0005-sync-identity-crypto-lifecycle-design.md`. It proves,
//! with runnable assertions, two structural properties of the proposed sync
//! design -- without shipping any of it:
//!
//!   1. An aggregate built from ledger-shaped facts is an *allowlist*
//!      projection: prompts, replies, raw working directories, and tool
//!      output structurally cannot reach the wire payload, because the
//!      projection function's output type never has a field for them.
//!   2. Encrypting the payload and merging it -- possibly more than once,
//!      possibly out of order -- into a receiving device's local mirror is
//!      idempotent and rollback-resistant (the "encryption boundary" and
//!      "merge idempotency" the issue asks for).
//!
//! Constraints this file honors (see the ADRs above for the full reasoning):
//!  - Local files only (a `tempfile::tempdir`), no network access anywhere.
//!  - Never touches the real `history-v1.sqlite3` ledger, its schema, or
//!    `odometer_lib::model::Session` -- the "ledger facts" here are a
//!    struct defined only in this file, so nothing about this prototype can
//!    accidentally exercise or depend on the production model.
//!  - `chacha20poly1305` is a `[dev-dependencies]`-only crate (see
//!    `src-tauri/Cargo.toml`); it is not part of the shipping binary's
//!    dependency graph.
//!  - This is a *design* prototype, not production crypto. The fixed demo
//!    key/nonce below exist to demonstrate the boundary's shape, not to be
//!    reused as-is -- the real design uses per-group random keys and
//!    X25519-negotiated key material (0005).

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;

/// Stand-in for the subset of ledger facts (`Session`/`TurnInfo`) a real
/// device would read locally, *including* every field this design commits
/// to keeping off the wire. Deliberately not `odometer_lib::model::Session`
/// -- this prototype must prove the projection discipline on its own terms,
/// not ride on the real struct happening to be shaped a particular way
/// today.
struct FakeLedgerFacts {
    device_id: &'static str,
    harness: &'static str,
    day: &'static str, // UTC day bucket, e.g. "2026-08-03"
    model: &'static str,
    project_working_directory: &'static str, // BANNED: raw path
    first_user_message: &'static str,        // BANNED: prompt content
    last_agent_message: &'static str,        // BANNED: reply content
    tool_output_sample: &'static str,        // BANNED: tool output
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    turns_count: u64,
    tool_calls: u64,
    revision: u64,
}

/// The only thing that ever leaves a device once sync is enabled: a
/// day-bucketed, allowlisted aggregate (see
/// `docs/adr/0003-sync-protocol-schema.md`). Every field is enumerated by
/// hand -- there is no `#[serde(flatten)]` and no blanket `From<Session>`
/// that could let a newly added ledger field slip onto the wire unreviewed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SyncDayAggregate {
    schema_version: u32,
    device_id: String,
    harness: String,
    day: String,
    model: String,
    /// A locally-salted handle, never the raw working directory. `None`
    /// means "folded into the unassigned bucket" -- the default, and the
    /// only state a project the user never explicitly shared can be in
    /// (see `docs/adr/0007-team-budgets-and-leaderboard-design.md` on why
    /// an unnamed-but-distinguishable row is still a leak).
    project_ref: Option<String>,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    turns_count: u64,
    tool_calls: u64,
    revision: u64,
    /// A deletion marker, applied through the same idempotent-merge path as
    /// ordinary data (see `as_tombstone` below and
    /// `docs/adr/0005-sync-identity-crypto-lifecycle-design.md`'s tombstone
    /// section). It never reaches the ledger -- only this file's
    /// `SyncedMirror` stand-in for the local synced-aggregates mirror.
    tombstone: bool,
}

/// Deterministic, non-reversible-without-the-salt project handle. Stands in
/// for the `HMAC-SHA256(local_group_salt, normalized_working_directory)`
/// construction in `docs/adr/0003-sync-protocol-schema.md`. This prototype
/// uses a dependency-free FNV-1a fold instead of keyed HMAC-SHA256 to avoid
/// pulling in a second crypto crate for a property (raw path in, opaque and
/// salt-stable token out) that a simple keyed fold already demonstrates;
/// the real implementation must use HMAC-SHA256, not this fold.
fn pseudonymous_project_ref(working_directory: &str, local_group_salt: &str) -> String {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for byte in local_group_salt.bytes().chain(working_directory.bytes()) {
        acc ^= u64::from(byte);
        acc = acc.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
    }
    format!("prj_{acc:016x}")
}

/// The allowlist projection: the *only* function in this file permitted to
/// construct a `SyncDayAggregate`. Its input type carries every banned
/// field, and its body never reads `first_user_message`,
/// `last_agent_message`, `tool_output_sample`, or the raw
/// `project_working_directory` -- only a salted handle derived from the
/// latter, and only when the caller opts the project into sharing.
fn to_sync_aggregate(
    facts: &FakeLedgerFacts,
    local_group_salt: &str,
    share_project: bool,
) -> SyncDayAggregate {
    SyncDayAggregate {
        schema_version: 1,
        device_id: facts.device_id.to_string(),
        harness: facts.harness.to_string(),
        day: facts.day.to_string(),
        model: facts.model.to_string(),
        project_ref: if share_project {
            Some(pseudonymous_project_ref(
                facts.project_working_directory,
                local_group_salt,
            ))
        } else {
            None
        },
        input_tokens: facts.input_tokens,
        cached_input_tokens: facts.cached_input_tokens,
        output_tokens: facts.output_tokens,
        reasoning_output_tokens: facts.reasoning_output_tokens,
        turns_count: facts.turns_count,
        tool_calls: facts.tool_calls,
        revision: facts.revision,
        tombstone: false,
    }
}

/// Turns a live aggregate into its deletion marker: same identity key, an
/// advanced revision, zeroed counts, and `tombstone: true`. This is the
/// *only* way a `SyncDayAggregate` becomes a tombstone in this file --
/// deliberately not a free-form constructor, so a tombstone can never
/// silently carry stale non-zero counts.
fn as_tombstone(live: &SyncDayAggregate, revision: u64) -> SyncDayAggregate {
    SyncDayAggregate {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        turns_count: 0,
        tool_calls: 0,
        revision,
        tombstone: true,
        ..live.clone()
    }
}

fn sample_facts() -> FakeLedgerFacts {
    FakeLedgerFacts {
        device_id: "device-a",
        harness: "claude_code",
        day: "2026-08-03",
        model: "claude-sonnet-4.5",
        project_working_directory: "/Users/synthetic-user/dev/definitely-not-a-real-project",
        first_user_message: "please refactor the synthetic payments module",
        last_agent_message: "done, synthetic tests pass",
        tool_output_sample: "synthetic stdout: 3 tests passed",
        input_tokens: 12_345,
        cached_input_tokens: 2_000,
        output_tokens: 6_789,
        reasoning_output_tokens: 512,
        turns_count: 4,
        tool_calls: 9,
        revision: 1,
    }
}

// ---------------------------------------------------------------------
// 1. Allowlist projection: banned content structurally cannot reach the
//    wire payload.
// ---------------------------------------------------------------------

#[test]
fn projection_excludes_every_banned_field_even_though_the_source_carries_them() {
    let facts = sample_facts();
    let aggregate = to_sync_aggregate(&facts, "device-a-local-salt", true);
    let json = serde_json::to_string(&aggregate).unwrap();

    for banned in [
        facts.project_working_directory,
        facts.first_user_message,
        facts.last_agent_message,
        facts.tool_output_sample,
    ] {
        assert!(
            !json.contains(banned),
            "banned content leaked into the sync payload: {banned}"
        );
    }

    // The struct's own field set is the real guarantee (there is no code
    // path that could serialize a field that does not exist); the string
    // scan above is defense-in-depth, not the whole proof. This count is a
    // deliberate tripwire: growing the schema must be a reviewed, counted
    // change, not a silent one.
    let field_count = serde_json::from_str::<serde_json::Value>(&json)
        .unwrap()
        .as_object()
        .unwrap()
        .len();
    assert_eq!(
        field_count, 14,
        "SyncDayAggregate grew or shrank a field -- update this count only \
         alongside a reviewed allowlist change"
    );
}

#[test]
fn project_ref_is_stable_for_the_same_salt_and_diverges_across_salts() {
    let facts = sample_facts();
    let a = to_sync_aggregate(&facts, "shared-device-group-salt", true);
    let b = to_sync_aggregate(&facts, "shared-device-group-salt", true);
    assert_eq!(
        a.project_ref, b.project_ref,
        "the same group salt must yield the same project handle"
    );

    let stranger = to_sync_aggregate(&facts, "a-different-users-salt", true);
    assert_ne!(
        a.project_ref, stranger.project_ref,
        "a different salt must not converge on the same handle"
    );

    let handle = a.project_ref.expect("project was shared");
    assert!(
        !handle.contains(facts.project_working_directory),
        "the handle must never contain the raw working directory"
    );
}

#[test]
fn a_project_never_explicitly_shared_defaults_to_the_unassigned_bucket() {
    let facts = sample_facts();
    let aggregate = to_sync_aggregate(&facts, "device-a-local-salt", false);
    assert_eq!(
        aggregate.project_ref, None,
        "a project must be explicitly marked shared before any handle for \
         it leaves the device -- see docs/adr/0007 on why an unnamed-but-\
         distinguishable row is still a leak"
    );
}

// ---------------------------------------------------------------------
// 2. Encryption boundary: plaintext never crosses it, and tampering with
//    the ciphertext is rejected rather than silently misdecrypted.
// ---------------------------------------------------------------------

/// XChaCha20-Poly1305 stands in for the real per-group AEAD key from
/// `docs/adr/0005-sync-identity-crypto-lifecycle-design.md`. The fixed
/// key/nonce here are for structural demonstration only: real key material
/// comes from device pairing (X25519 key agreement) and nonces must never
/// repeat for a given key -- neither property this prototype needs to
/// prove.
#[test]
fn encryption_boundary_round_trips_and_rejects_tampering() {
    let facts = sample_facts();
    let aggregate = to_sync_aggregate(&facts, "device-a-local-salt", true);
    let plaintext = serde_json::to_vec(&aggregate).unwrap();

    let key: Key = [0x11u8; 32].into();
    let cipher = XChaCha20Poly1305::new(&key);
    let nonce: XNonce = [0x22u8; 24].into();

    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .expect("seal the aggregate before it ever touches local storage");
    assert!(
        !ciphertext
            .windows(facts.first_user_message.len())
            .any(|window| window == facts.first_user_message.as_bytes()),
        "plaintext prompt content must not survive, even as a substring, in the sealed blob"
    );

    // "Local storage" standing in for a user-controlled backend: only
    // ciphertext ever touches disk here, and it is a scratch temp dir, not
    // any real Odometer data directory.
    let scratch = tempfile::tempdir().expect("local scratch dir for the prototype");
    let blob_path = scratch.path().join("device-a-2026-08-03.sealed");
    std::fs::File::create(&blob_path)
        .unwrap()
        .write_all(&ciphertext)
        .unwrap();

    let stored = std::fs::read(&blob_path).unwrap();
    let recovered_plaintext = cipher
        .decrypt(&nonce, stored.as_ref())
        .expect("open on the receiving device");
    let recovered: SyncDayAggregate = serde_json::from_slice(&recovered_plaintext).unwrap();
    assert_eq!(
        recovered, aggregate,
        "decrypted aggregate must round-trip exactly"
    );

    let mut tampered = stored.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert!(
        cipher.decrypt(&nonce, tampered.as_ref()).is_err(),
        "a single flipped ciphertext byte must fail authentication, not \
         decrypt to different-but-plausible data"
    );
}

// ---------------------------------------------------------------------
// 3. Merge idempotency and rollback resistance on the receiving side.
// ---------------------------------------------------------------------

/// A receiving device's local mirror of synced aggregates -- explicitly
/// **not** the durable ledger. Keyed by `(device_id, day, harness, model,
/// project_ref)`, never by wall-clock arrival time, matching
/// `docs/adr/0005`'s conflict-handling design: a device only ever owns
/// rows under its own `device_id`, so applying facts from other devices
/// can never conflict with -- or need to merge into -- this device's own
/// data.
#[derive(Default)]
struct SyncedMirror {
    rows: HashMap<(String, String, String, String, Option<String>), SyncDayAggregate>,
    rejected_rollbacks: u32,
}

impl SyncedMirror {
    fn key_for(aggregate: &SyncDayAggregate) -> (String, String, String, String, Option<String>) {
        (
            aggregate.device_id.clone(),
            aggregate.day.clone(),
            aggregate.harness.clone(),
            aggregate.model.clone(),
            aggregate.project_ref.clone(),
        )
    }

    /// Applies a decrypted, signature-verified aggregate. A revision at or
    /// below what's already recorded for this key is rejected as a
    /// (possibly malicious) rollback rather than accepted; an identical
    /// replay of the current revision is silently idempotent.
    fn apply(&mut self, incoming: SyncDayAggregate) {
        let key = Self::key_for(&incoming);
        match self.rows.get(&key) {
            Some(existing) if existing.revision >= incoming.revision => {
                if existing != &incoming {
                    self.rejected_rollbacks += 1;
                }
            }
            _ => {
                self.rows.insert(key, incoming);
            }
        }
    }
}

#[test]
fn merging_the_same_aggregate_twice_is_a_no_op() {
    let facts = sample_facts();
    let aggregate = to_sync_aggregate(&facts, "device-a-local-salt", true);

    let mut mirror = SyncedMirror::default();
    mirror.apply(aggregate.clone());
    mirror.apply(aggregate.clone());
    mirror.apply(aggregate);

    assert_eq!(
        mirror.rows.len(),
        1,
        "one logical row regardless of how many times the same payload is applied"
    );
    assert_eq!(mirror.rejected_rollbacks, 0);
}

#[test]
fn a_higher_revision_replaces_but_a_replayed_stale_revision_is_rejected() {
    let mut facts = sample_facts();
    let mut mirror = SyncedMirror::default();

    let v1 = to_sync_aggregate(&facts, "device-a-local-salt", true);
    mirror.apply(v1.clone());

    // e.g. a corrected re-derivation from the ledger after a parser fix.
    facts.output_tokens += 500;
    facts.revision = 2;
    let v2 = to_sync_aggregate(&facts, "device-a-local-salt", true);
    mirror.apply(v2.clone());

    let key = SyncedMirror::key_for(&v2);
    assert_eq!(mirror.rows[&key].output_tokens, v2.output_tokens);

    // A backend/attacker replays the stale v1 blob after v2 is already applied.
    mirror.apply(v1);
    assert_eq!(
        mirror.rows[&key].output_tokens, v2.output_tokens,
        "a stale replayed revision must not roll the mirror back"
    );
    assert_eq!(mirror.rejected_rollbacks, 1);
}

#[test]
fn out_of_order_delivery_still_converges_on_the_highest_revision() {
    let mut facts = sample_facts();
    let v1 = to_sync_aggregate(&facts, "device-a-local-salt", true);
    facts.revision = 2;
    facts.turns_count += 3;
    let v2 = to_sync_aggregate(&facts, "device-a-local-salt", true);
    facts.revision = 3;
    facts.turns_count += 1;
    let v3 = to_sync_aggregate(&facts, "device-a-local-salt", true);

    // Peers can deliver in any order (retries, differing network paths).
    let mut mirror = SyncedMirror::default();
    mirror.apply(v2.clone());
    mirror.apply(v1);
    mirror.apply(v3.clone());

    let key = SyncedMirror::key_for(&v3);
    assert_eq!(mirror.rows.len(), 1);
    assert_eq!(
        mirror.rows[&key], v3,
        "the mirror must converge on the highest revision seen"
    );
    assert_eq!(
        mirror.rejected_rollbacks, 1,
        "v1 arriving after v2/v3 is a rejected rollback"
    );
}

// ---------------------------------------------------------------------
// 4. Deletion propagation: a tombstone converges the same way ordinary
//    data does, and never touches anything outside this local mirror.
// ---------------------------------------------------------------------

#[test]
fn a_tombstone_is_just_a_higher_revision_row_and_converges_like_any_other_update() {
    let facts = sample_facts();
    let mut mirror = SyncedMirror::default();

    let live = to_sync_aggregate(&facts, "device-a-local-salt", true);
    mirror.apply(live.clone());

    let tombstone = as_tombstone(&live, 2);
    mirror.apply(tombstone.clone());

    let key = SyncedMirror::key_for(&tombstone);
    assert!(mirror.rows[&key].tombstone);
    assert_eq!(mirror.rows[&key].input_tokens, 0);
    assert_eq!(
        mirror.rows.len(),
        1,
        "a tombstone updates the existing key in place; it does not create a second row"
    );

    // A peer that was offline for the live row and only ever sees the
    // tombstone converges to the same deleted state directly.
    let mut late_peer = SyncedMirror::default();
    late_peer.apply(tombstone.clone());
    assert_eq!(late_peer.rows[&key], tombstone);

    // A stale replay of the pre-deletion live row must not resurrect it.
    mirror.apply(live);
    assert!(
        mirror.rows[&key].tombstone,
        "a replayed lower-revision live row must not undo the tombstone"
    );
}
