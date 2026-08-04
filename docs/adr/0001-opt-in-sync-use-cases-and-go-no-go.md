# 0001. Sync use cases and go/no-go decisions

- Status: Accepted as a discovery-gate decision record. No code from this
  ADR ships. Design only.
- Date: 2026-08-04
- Related: [issue #49](https://github.com/ekalb81/agent-odometer/issues/49),
  parent roadmap [#36](https://github.com/ekalb81/agent-odometer/issues/36),
  depends on [#38](https://github.com/ekalb81/agent-odometer/issues/38)
  (durable ledger), [#41](https://github.com/ekalb81/agent-odometer/issues/41)
  (project/repo analytics), [#42](https://github.com/ekalb81/agent-odometer/issues/42)
  (pricing/model-alias unification).

## Context

Odometer's current promise is "local processing, no prompt upload." The
durable ledger (`src-tauri/src/history_store.rs`) already retains full
parsed `Session` records — including `first_user_message`,
`turns[].user_message`, `turns[].last_agent_message`, `working_directory`,
and `file_path` — as local, on-disk application data. `docs/ARCHITECTURE.md`
is explicit that this database "inherits the same no-upload/no-log
handling as the source transcripts."

Any sync feature therefore has to answer, before a single network call
exists: what leaves the device, under whose key, to which backend, merged
how, revocable how. The issue asks for five sync use cases to be evaluated
**separately**, because they do not share a risk profile:

| # | Use case | Parties who see data | Network required |
| - | -------- | --------------------- | ----------------- |
| 1 | Personal multi-device history | The one user, across their own devices | Yes |
| 2 | Self-hosted backup | The one user, to storage they run/own | Yes |
| 3 | Private team aggregation | A closed set of team members the user chooses | Yes |
| 4 | Budgets/alerts | The one user (local), or a team (shared) | Local: no. Shared: yes, plus notification delivery |
| 5 | Optional anonymous leaderboard | Unknown third parties | Yes, and a hosted collection point |

## Decision

Evaluate and gate each use case independently. A "go" below means **go for
continued design and prototyping in a future issue**, not "ship." Nothing
in this issue enables a Tauri network capability, an account system, or a
production backend.

### 1. Personal multi-device history

**Risk profile:** single trust domain (one person, their own devices). The
attacker who matters most is someone with access to the storage backend or
a lost/stolen device, not another user.

**Decision: GO for design.** This is the least risky use case and the one
the rest of this design optimizes for first. See 0003–0006 for the schema,
backend choice, and lifecycle design. Concrete unblock criteria before code
ships are listed in [Go/no-go criteria](#go-no-go-criteria) below.

### 2. Self-hosted backup

**Risk profile:** identical trust domain to (1) — still one person — but
the destination is explicitly user-run infrastructure rather than a
third-party service. This removes "vendor account takeover" as a novel
threat (it reduces to "the user's own infrastructure is compromised," which
is already outside Odometer's control) but does not remove backend-storage
compromise, device loss, or deletion-verifiability concerns.

**Decision: GO for design**, effectively as a special case of (1) with a
different backend (see 0004). No separate protocol is needed — a
self-hosted target is one more `SyncBackend` implementation, not a new
data model.

### 3. Private team aggregation

**Risk profile:** multi-party trust. Every device in a team's sync group
can, by construction, submit numbers that the rest of the team will
display. This is qualitatively different from (1)/(2): a malicious or
compromised peer is now a real actor, not a hypothetical.

**Decision: GO for design, NO-GO for code in this issue**, and additionally
**gated on personal sync having already shipped and run without incident**
(see criteria below). Team aggregation inherits every personal-sync risk
and adds device-revocation, roster-management, and the intentionally-private-project
problem (0007). It should not be the first sync surface Odometer ships.

### 4. Budgets/alerts

This use case splits into two, and they are not one decision:

- **Local budgets/alerts** (a personal monthly cap compared against the
  user's own ledger, with an in-app warning) require **no sync at all** —
  the ledger and rate card are already local. **Decision: this is not a
  sync feature and is out of scope for this gate entirely.** It can be
  designed and shipped independently, with its own (much smaller) review.
- **Shared/team budget alerts** (a team-wide spend cap with a notification
  when exceeded) is an application of private team aggregation (3) plus a
  **new capability this ADR set does not evaluate: outbound notification
  delivery** (email, webhook, or push all imply a new recipient, a new
  network destination, and typically a stored contact address). **Decision:
  NO-GO**, and explicitly do not assume it is included when/if team
  aggregation goes green — it needs its own threat model.

### 5. Optional anonymous leaderboard

**Risk profile:** the highest. "Anonymous" and "leaderboard" are in
tension — a leaderboard implies a durable, rankable, per-participant
series, and a durable per-participant series is re-identifiable over time
through usage-pattern fingerprinting even with no name attached. It also
requires a hosted aggregation point, which contradicts Odometer's current
positioning and this issue's own acceptance criteria ("no ... public
leaderboard ships in this issue").

**Decision: NO-GO**, treated as **near-permanent** pending a separate,
explicit product decision made outside engineering (this is a business/
positioning call, not an implementation detail — see 0007 for what would
have to be true for it to be revisited).

## Go/no-go criteria

### Personal sync / self-hosted backup — unblock criteria

All of the following must be true before any personal-sync code ships,
beyond what this issue delivers:

1. A backend-adapter trait exists with at least the local-file adapter
   implemented and reviewed (0004).
2. Device pairing and end-to-end encryption are implemented and pass the
   merge-idempotency and plaintext-never-crosses-the-boundary properties
   demonstrated in `src-tauri/tests/sync_design_prototype.rs` (0005).
3. The in-app "what will leave your device" preview (0003) ships **before**
   the first opt-in toggle, not after.
4. Disable and delete flows are tested to leave the local ledger
   (`history-v1.sqlite3`) untouched (0006).
5. A new, narrowly-scoped Tauri network capability is added deliberately,
   with its own security review — not folded into an unrelated capability
   bump.

### Private team aggregation — unblock criteria

Everything above, plus:

1. Personal sync has shipped and run at least one full release cycle
   without a known incident.
2. Device revocation and a team roster are implemented and tested
   (0005).
3. The private-project folding behavior (0007) is implemented and has a
   test proving a private project's existence cannot be inferred from any
   synced payload.
4. Model-alias fragmentation from #42 is either resolved or prominently
   disclosed in the team-total UI before go-live.
5. A basic legal/consent review has happened for aggregating a second
   person's derived usage data, even in aggregate form.

### Optional anonymous leaderboard — unblock criteria

Not scheduled. If ever revisited, it requires, at minimum: per-submission
(not persistent) opt-in, k-anonymity thresholds before any bucket is shown,
coarse volume buckets instead of exact numbers, no per-project data ever,
and — because it requires a hosted collection point — a separate business
decision to operate a hosted service at all, which is outside this issue's
and this ADR's authority.

## Consequences

- Building work should start with personal sync / self-hosted backup only;
  team aggregation and leaderboard remain design documents until their own
  criteria are met.
- Local budgets/alerts can proceed independently of all of this, on its own
  timeline, with its own lightweight review.
- Every backend, schema, and lifecycle decision in 0003–0006 is written to
  serve personal sync first and to make team aggregation an additive layer,
  not a redesign.
