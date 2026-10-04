# Local history lifecycle

Schema 11 separates source availability from retained history. A `present`
session has at least one currently observed source. `retained` history keeps
its current local snapshot and accounting after all sources disappear.
`superseded` history keeps accounting after another fingerprint takes over its
last source path. Provider archival status is independent of all three states.

The default retention policy keeps everything. A configured duration selects
retained or superseded activity before a UTC calendar-day cutoff for an explicit
purge preview. It does not schedule deletion. Present sessions and unreconciled
usage are ineligible. Sessions sharing a provider identity and first-event
fingerprint form one exclusion group; any present or newer sibling excludes the
whole group from the preview. Confirmation rechecks the exact candidate set in
an immediate SQLite transaction. A resumed source or any saved policy change
invalidates the preview, even if the former policy is later restored.

Purge deletes local snapshots, bounded summaries, source metadata, normalized
facts, rollups, and session-specific project assignments together. Project-wide
aliases and merges remain independent. It never deletes provider transcript
files. Minimal tombstones contain only storage identity, provider identity,
first-event fingerprint, and the confirmation timestamp. They exclude copies,
moves, and resumes of that fingerprint from future ingestion. A genuinely new
fingerprint can be imported and can later be purged independently. Purged
sessions have no content-bearing list row; an explicit detail lookup reports
that history was purged.

The independent `.exclusions.jsonl` sidecar records the same minimal confirmed
identities before SQL deletion commits. It contains no usage, paths, titles, or
message text. If SQL rolls back, existing history remains readable. If the
database subsequently becomes unreadable, recovery conservatively honors the
confirmed exclusion intent. A complete malformed journal record makes recovery
unavailable; it never silently permits reimport. A partial trailing record
cannot have preceded a committed purge and is excluded from recovery. This is
an application retention policy, not a claim of physical secure disk erasure.
Previously preserved recovery backups and provider files remain separate local
copies and are not removed by purge.

Explicit unavailable-database recovery preserves the database, WAL, and SHM in
a new sibling backup directory before preparing a replacement. A durable
recovery marker precedes those moves, so a process interruption cannot make a replacement
claim complete coverage. This does not claim power-loss durability of directory
entries. The replacement restores verified exclusions and
rebuilds only readable configured sources. Its persistent coverage remains
incomplete: missing-source history preserved in the damaged backup is not
claimed as recovered. Token and money budgets must consult
`HistoryStore::has_complete_coverage()` and remain unavailable for incomplete or
unreadable coverage. Provider quota observations are independent.

Durable listing reads `session_summaries`, never full Session blobs. Accepted
snapshot writes materialize summaries from the existing parsed Session. The
schema 10 migration creates them entirely with SQLite JSON aggregation; no
Rust Session hydration is used for migration. Summary previews are bounded to
1,024 message characters and 512 title characters while full local snapshots
retain the original text. Pricing continues to use shared Rust buckets and
range accounting continues to use normalized facts plus matching hour rollups.

Future local annotations, bookmarks, search indexes, or training datasets must
declare whether they reference a session or own independent data. Any
session-owned derived records must join the same purge transaction, and any
independent copies need an explicit retention policy and confirmation scope.
Those features do not exist in this delivery; purge does not imply otherwise.
# Policy and coverage boundaries

Purge previews bind the saved policy revision. Confirmation rechecks that revision,
the candidate identities and their facts inside the deletion transaction. A supplied
cutoff newer than the current policy allows is rejected. An older reviewed cutoff
remains conservative across a UTC midnight; review again to include newly eligible
history. Changing and restoring the policy still invalidates the prior preview.

Integration Center and headless `odometer_status` expose `coverage_complete` and
diagnostics for recovered partial history or unverified coverage. These are recorded
totals; a readable ledger, completed scan, or successful MCP call does not establish
that missing historical sources were recovered.
