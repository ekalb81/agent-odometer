# Explicit human outcomes

Select a session, choose **Edit organization**, and enter an outcome: **Not rated**, **Accepted**, **Rejected**, or **Unresolved**. These are user assertions, not automated evaluations. Git retained/reverted/abandoned states, tokens, cost, timing, and repair effort never set an outcome. You can edit or clear a label later.

Optional repair minutes are whole, user-reported minutes from 0 through 525600. Empty means unknown; an explicit zero remains zero. First-pass acceptance is a separate optional human report. Yes requires Accepted; No is explicit negative first-pass evidence for a rated task. Neither field is inferred from the other. Optional repair details use the existing private note, bounded to 32 KiB.

## Measurement contract

A task is one root session and its linked subagent work. The root label represents the user's assessment of that task. Subagent sessions may have their own labels for local inspection; they are excluded from the root-task summary and export denominator. Sessions marked as subagents without an available parent are also excluded. Unknown or absent parent metadata cannot establish a complete task tree, and this contract does not claim otherwise.

**Analytics → Outcomes → Human outcomes** uses root sessions in the current effective provider, project, text, model, organization, and date filter. Ratings apply to the whole task even when a date filter selects only part of its activity. The report separately shows labelled tasks, Not rated tasks, unavailable rating metadata, and excluded subagent sessions. First-pass acceptance reports accepted-first-pass / explicitly reported first-pass assessments among labelled root tasks, plus the count without first-pass evidence. Repair totals disclose how many root tasks have user-reported time; no reports means unavailable, never zero.

User-reported repair minutes are separate from observed session duration. Accepted end-to-end delivery time is not collected or calculated. Neither token speed nor any timing, cost, or Git measurement establishes acceptance, correctness, productivity, or causation. This feature generates no productivity score.

Reports and exports retain at most the first 10000 root tasks in the effective filter. A larger scope explicitly shows its omitted task count; those tasks are outside every displayed denominator. Narrow the filters for complete counts. This bounded prefix is not a representative statistical sample.

## Persistence, privacy, and export

Structured fields use the existing private annotation identity `(session_key, first_event_fingerprint)` and shared revision checks. App restart, source relinking, and display/project/tag renaming preserve the identity-bound label. Fingerprint promotion preserves it in place; stale edits require reload. Confirmed history purge removes the session's annotation, including label, effort, and private note. A reused identity cannot inherit it.

Ordinary session exports, accounting/query reports, diagnostics, performance logs, and MCP omit human outcomes and private notes. **Export structured human outcomes JSON** is an explicit local export of only session keys, labels, optional effort, first-pass reports, availability, aggregate coverage counts, and the measurement contract. It never includes note text, tag labels, source bodies, or local paths. Unavailable rows contain null fields and an availability flag, distinct from Not rated rows.

Preservation-first corrupt-history recovery cannot reconstruct human input from source transcripts. Earlier labels and repair notes remain in the preserved database backup. Until a new explicit outcome edit exists in the rebuilt database, the report treats a rating as unavailable rather than Not rated, even if a pin or private note was subsequently edited. Backup restoration remains a separate operation.
