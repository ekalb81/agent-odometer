# Execution board

Open a session's context menu and choose **Compare execution**. Select up to eight sessions from the local index. Provider and effective project filters narrow the candidate list without silently removing an existing selection. Selection is ephemeral; no saved selection or transcript body is persisted.

The shared axis uses UTC bounds from the selected sessions. A session span is elapsed time between its recorded start and last event, including unknown gaps. Recorded activity points and tool intervals are separate from cumulative token usage and backend-priced estimates. Activity volume does not measure productivity.

Parent lanes require one explicit same-provider parent identity in the selection and recorded timestamps. Duplicate identities, cross-provider matches, cycles, missing parents, and missing timestamps remain unresolved. Tool intervals require one recorded call and result for a call ID with valid, ordered timestamps in visited pages. Tool IDs are scoped to one session; unrelated calls are never paired across sessions. The board does not infer nested tool calls, turn membership, or source-file token shares.

The desktop-only `get_execution_page` command reuses the authorized transcript reader. It caps each read to 25 records and a 128 KiB response budget; the underlying reader retains its existing per-record, scan, source-identity, and cursor limits. Rust projects only record anchors, timestamps, role, block kind, tool name/ID and source limitations. Raw JSON, presentation text, tool arguments/results, and file edits are discarded before IPC serialization. No new source parser, accounting path, schema, network request, or capability is introduced.

The board visits one page per selected session initially. More pages require an explicit action; each lane retains at most 100 records and renders at most 100 activity blocks. Source end, partial coverage, omitted activity, missing records, stale cursors and failures are visible. A replacement invalidates the cursor rather than combining different source generations.

Selecting an activity opens its exact record in the existing inspector. Selecting another session opens a second bounded inspector alongside it; narrow windows stack them vertically. Full source pages are loaded only after inspection is requested. Both panes retain the existing reader, exact-anchor behavior, keyboard handling, marked omissions and tool-pair navigation.

Usage remains the cumulative session total. Price displays format existing Rust `get_session_pricing` values in their provider currency without calculating rates. A rate-card replacement clears prices immediately and rejects superseded replies. These values are independent of the visited source pages and are not activity-block attribution.

Verification uses synthetic overlapping sessions, exact anchors, duplicate/cyclic/cross-provider parent links, missing and reversed tool timestamps, omitted records, delayed responses, and a large candidate list. No real session bodies or local paths belong in fixtures.
