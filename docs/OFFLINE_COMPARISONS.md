# Frozen offline comparisons

Open **Review local dataset → Compare frozen offline runs**. This is an imported,
local experiment record. It never executes a prompt or model, reads credentials,
or sends data over the network. A completed imported run is a user-reported
observation, not proof that Odometer executed or verified it.

Freeze the current dataset with two variants, a and b. Record an explicit prompt
identifier, version, prompt text, provider, model identifier, and service tier for
each variant, plus the human outcome rubric. The exact backend preview contains
every minimized input and prompt. Review it before storing. Optional exact
redaction phrases use the curated dataset's backend policy. Automatic redaction
does not find every secret; review remains necessary.

The immutable manifest records the dataset revision, membership, individual case
versions and content hashes, frozen input hashes, expected human outcomes,
prompts and their hashes, capture time, and frozen effective rate snapshot. The
snapshot records USD currency, selected provider/model/service tier, configuration
version, resolved model, quote time, direct/alias/fallback/stale/unavailable
provenance, and cache-creation provenance separately. Rust quotes one-million-token
basis vectors through the existing shared pricing service; imported token totals
use the same Rust cost function. The frontend never resolves rates or prices
tokens. These are API base estimates, not subscription payments or an exact
invoice; observed imported USD cost remains a separate dimension. Unsupported or
unavailable pricing stays null, while explicitly free pricing may be zero.

Changing a dataset case or rate card does not rewrite a frozen comparison. The
report identifies a changed current dataset or selected rate snapshot when
reloaded. User-provided run condition evidence is compared with the frozen input,
provider/model/tier, prompt identifier/version/hash, and rate hash. Missing
evidence is unverified; differences are visible. Even matching declarations do
not establish verified execution or controlled conditions. Never populate those
fields from the frozen manifest to imply evidence absent from the actual run.

Import one JSON array with up to 128 result rows and 1 MiB. The UI supplies a
template with empty condition evidence and null unreported measurements. Each row
selects a frozen `case_id`, variant a/b, and status completed/failed/missing.
Optional fields include output, elapsed milliseconds, observed USD cost, token
totals, an explicit human quality label, and observed condition identifiers.
Token subsets must reconcile with input/output totals. A missing run cannot
carry measurements or output. Failed runs may report genuinely consumed cost
and time; those observations remain in their numeric denominators. Human quality
requires a completed output and is never inferred by a model. Imported outputs
are redacted and limited to 3000 UTF-8 bytes before the exact review preview;
shortened outputs are marked. Preview commit stores only the reviewed token's
backend projection and uses the comparison's revision as a compare-and-swap.

Reports show completed, failed, missing, and completed-without-output counts
against the original membership denominator. Human accepted/rejected/unresolved/
not-rated/missing counts are separate. Observed cost, elapsed time, and frozen
estimated cost each show their own count, mean and observed range; an empty
measurement is unavailable. B-minus-A differences use only pairs reporting that
dimension, and report their own pair counts. There is no composite score or
automatic ranking. These user-selected, uncontrolled examples and small samples
do not support population confidence intervals, causal claims, or a best model.
Observed ranges are descriptive, not uncertainty bounds.

At most eight comparisons exist locally, each with the dataset's maximum of 64
cases. Manifest content is capped at 64 KiB, each frozen input at 16 KiB, and each
result at 8 KiB. Preview tokens are single-use, expire after five minutes, and at
most eight remain in memory. Import replaces only the current result for a pair;
old output versions are not retained. Comparison removal deletes all copied
inputs and results. Removing a curated case or purging its source cascades through
copied inputs and both variant results, advances the comparison revision, and
rejects late imports. The manifest retains IDs/hashes and original denominators
so the report honestly shows removed cases. Earlier explicit exports and recovery
backups remain separate files. Standard accounting, CLI/MCP, diagnostics and
session exports do not expose private experiment bodies. Export the reviewed
comparison explicitly through the native JSON save picker.

## Active replay decision: no-go

Issue #270's local imported phase does not authorize or implement active replay.
Active model execution remains a separate, unapproved milestone. A future go
decision needs an explicit product/security review and user consent contract for
credentials, per-provider destinations, frozen input minimization, spend/time
limits, cancellation, partial/failed runs, and immutable execution provenance.
Do not treat an imported comparison or this no-go decision as completion of live
execution. No execution implementation follow-up is authorized by this phase.

Native export is version-checked after the save picker closes, using the reviewed backend digest. A short immediate history transaction serializes final revalidation and atomic file publication with edits, removal and purge. No history lock spans the user-controlled picker. If content changed while choosing a file, export rejects stale bytes and leaves the destination unchanged; reload and review before retrying. A successfully completed explicit export remains an independent file if a later purge occurs.
