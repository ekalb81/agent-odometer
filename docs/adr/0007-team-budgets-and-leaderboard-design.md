# 0007. Team budgets and leaderboard: currency, pricing, aliases, and private projects

- Status: Accepted as a discovery-gate decision record. No code ships.
- Date: 2026-08-04
- Related: [0001](0001-opt-in-sync-use-cases-and-go-no-go.md) (both use
  cases are NO-GO for code in this issue),
  [0003](0003-sync-protocol-schema.md),
  depends on [#42](https://github.com/ekalb81/agent-odometer/issues/42)
  (pricing/model-alias unification)

This document designs the two highest-risk use cases from 0001 — private
team aggregation and the optional anonymous leaderboard — assuming their
unblock criteria are eventually met. It does not authorize building either.

## Currency

`SyncDayAggregate` (0003) carries **token counts, never priced dollar
figures.** `TokenTotals`/`TierBucket` are the synced unit; pricing is
applied client-side, at render time, by each viewing device's own rate
card — the same separation `docs/ARCHITECTURE.md` already documents for
Codex plan credits vs. Anthropic API USD vs. OpenAI API-equivalent
estimates ("Keep Codex plan credits, Anthropic API USD, and OpenAI
API-equivalent estimates separate in code, labels, and documentation. An
API-equivalent number is not an invoice").

This has a direct consequence for team totals: **two team members can see
different dollar totals for the same synced token data** if their local
rate cards differ (one has an edited override, or hasn't refreshed the
bundled catalog). This is intentional, not a bug to hide — baking a
point-in-time price into a synced record would silently misprice every
future re-render as rates change, and forcing one canonical "team rate
card" onto every viewer would either require a trusted rate-distribution
authority (reintroducing an account-like dependency this design avoids) or
silently override a user's own deliberate rate edits. A future, explicitly
opt-in "shared team rate-card override" — itself a signed, versioned
aggregate distributed alongside the roster — is a reasonable follow-up but
is not required for team aggregation's go decision.

## Pricing revisions

Because pricing is computed at render time rather than at sync time, a
rate correction (a new `pricing_catalog` period landing in `rates.json`,
per `docs/ARCHITECTURE.md`'s pricing-catalog contract) automatically
improves the accuracy of every already-synced day's totals for every
viewer, the next time they open the dashboard — no re-sync, no
re-derivation, no schema change. This is a direct benefit of keeping the
synced unit as tokens rather than dollars.

## Model aliases

Team members are not guaranteed to be on harness versions that report
identical model-id strings for what is conceptually the same model — this
gap is explicitly still open per `docs/ARCHITECTURE.md`'s pricing-catalog
contract note: "This catalog ... does not yet add model aliases." Because
`SyncDayAggregate.model` is keyed by the literal string reported by the
harness, an unresolved alias fragments a team total across near-duplicate
model rows (e.g., two spellings of the same underlying model showing as
two lines). This is a limitation **inherited from #42's still-open scope**,
not something team sync introduces or can independently fix.

**Decision:** private team aggregation's go-live is gated on #42's
model-alias unification having landed, *or*, if shipped before that lands,
the team-total UI must prominently disclose that near-duplicate model rows
may represent the same underlying model (0001, team-aggregation unblock
criterion 4) rather than silently presenting a fragmented total as
authoritative.

## Missing providers

A team member using a model with no rate-card entry contributes token
counts every synced device can still display, but only devices whose rate
card recognizes that model can price it — exactly the existing
`unpriced_models` behavior (`docs/ARCHITECTURE.md`: "flat calculations
exclude and label their usage rather than applying a fallback"). Team
totals inherit this per-viewer, per-model labeling rather than needing a
new mechanism: an unpriced model's tokens still sum into the team's token
total, and remain visibly unpriced in the team's dollar total, on whichever
viewer's rate card lacks the entry.

## Intentionally private projects — the sharp edge

The failure mode the issue specifically calls out: **a leaderboard (or a
team view) that leaks the *existence* of a project the user marked
private is a failure**, even if the project's name never appears. Seeing
"there is an unnamed project handle `prj_9e2c...` with N tokens that
nobody has claimed" is itself information — it proves a private project
exists and roughly how large it is.

The mitigation follows directly from 0003's `project_ref` design, and has
two layers:

1. **Opt-in per project, not per device.** A project only ever gets a
   `project_ref` if the user has explicitly marked *that specific working
   directory* as shared (mirroring `docs/ARCHITECTURE.md`'s existing rule
   for instruction discovery: "rooted in explicit user configuration or
   validated project scopes... a non-repository working directory never
   becomes an implicit recursive root"). Every project not explicitly
   marked shared — which is every project, by default — contributes to
   `project_ref = None`, the unassigned bucket.
2. **The unassigned bucket is not distinguishable per hidden project.**
   A private project's usage folds into the *same* `project_ref = None`
   row as every other unshared project on that device for that day/model —
   it is not given its own hashed-but-unnamed row. A hashed row, even one
   nobody can reverse to a path, is still a *distinguishable* row, and
   distinguishability is the actual leak the issue is warning about, not
   just reversibility. Folding removes the row entirely, not just its
   name.

### Leaderboard-specific mitigation

Per-project breakdown must **never** cross the leaderboard boundary at
all, in either direction — not named, not hashed, not bucketed. A
leaderboard, if ever built (0001: currently a near-permanent no-go),
should be scoped to per-user or per-team **total** volume only. This
sidesteps the private-project problem structurally for that specific
surface: there is nothing project-shaped in a leaderboard payload to leak
in the first place. Per-project breakdown remains available only inside
the *private* team-aggregation view, where the organization already has
out-of-band visibility into what projects exist among its own members —
and even there, an individual member's projects they have not personally
opted into sharing still fold into the unassigned bucket exactly as
described above; team membership does not imply project-level disclosure.

### Additional leaderboard-only requirements, if ever revisited

Beyond the private-project mitigation, a future leaderboard would need,
at minimum, before it is anything but a design document:

- **Per-submission opt-in**, not "opt in once, submit forever" — a
  standing subscription to a leaderboard is itself a durable per-identity
  series (0002, threat 4), which undermines "anonymous" over time via
  usage-pattern fingerprinting even without a name attached.
- **k-anonymity thresholds** — no bucket (e.g., "this model, this week")
  is shown until at least *k* independent participants have contributed to
  it, so a bucket of one can never be de-anonymized to "this is clearly
  that one person."
- **Coarse volume buckets**, not exact numbers, to blunt the same
  fingerprinting concern.
- A **hosted collection point**, which is exactly what this issue's
  acceptance criteria forbid building here, and which is a separate
  business/positioning decision (0001), not an engineering detail this
  ADR set can resolve unilaterally.
