# Context explanation

Choose **Explain context** in session details. It reads the same bounded, authorized source pages as the transcript inspector, only while open. **Inspect source record** opens the exact source anchor. Pagination replaces the previous page; no derived archive, network request, or background scan is created.

Recorded message roles and tool block types identify evidence categories (user/assistant messages, instructions, tool input/output, and explicit Claude `Skill` invocations). These are source records, not unique messages or proof of model context membership. References or instructions not explicitly identified by the source remain unavailable. Unsupported record types and omitted payloads remain labeled unavailable.

Codex `compacted` and Claude `system/compact_boundary` records mark recorded compaction boundaries. Claude `compactMetadata.preTokens` is shown only when recorded as a nonnegative integer. Post-compaction size is unavailable; adjacent usage observations never imply an exact before/after reduction. Codex token-count records can show recorded last-call input/output and model context window; cumulative session totals are not substituted for per-call indicators.

Coverage is always the current bounded page. Reaching the current source end does not mean complete context reconstruction. Source gaps, malformed/oversized records, incomplete tails, unsupported providers, and missing transcripts retain the transcript reader's explicit limitation states. The absence of a compaction marker in a page never proves compaction did not occur.

No token-share estimates or per-file token attribution are calculated. Existing coarse context-source telemetry continues to describe recorded cache/input buckets separately. Provider usage totals, ledger accounting, rate cards, and pricing oracles remain authoritative and unchanged.
