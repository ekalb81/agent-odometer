# Transcript inspector

Select a session, then choose **Inspect transcript**. Opening the dialog is the only action that fetches source bodies. Records stay in source order and are collapsed initially. Expand records for provider-recorded messages, tool calls/results, replacements, or raw JSON. No HTML, scripts, images, or remote URLs in a payload are executed.

The reader resolves registered session identity in Rust and supplies at most 25 records and 128 KiB per requested page. The inspector retains one page of bodies, up to 100 navigation locations, and up to 500 tool-ID mappings. Closing or changing session discards it. It never changes token accounting, source files, summaries, exports, logs, or MCP responses.

Tool links require one uniquely identified matching call/result among pages visited. Unseen or ambiguous matches are labelled; the inspector does not invent a result. A recorded Edit replacement displays its explicit before/after strings, not a reconstructed full-file diff. Unknown source records remain available as escaped raw JSON. Provider parsing and extraction live in Rust; browser fixtures are synthetic typed pages.

**Select anchor** puts a stable source anchor in the navigation field. Copy it and use **Jump to record** to reopen that exact record. The component's `sessionId`/`recordId` props and DetailPane's `transcriptAnchor` prop are the navigation boundary for search and bookmarks. Replacement or mutation can invalidate an anchor; that state is visible and requires discovery again. Appending a source does not silently retarget an existing anchor.

Paging preserves selection and provides **Return to selected record**. Missing sources, malformed or oversized records, incomplete trailing JSONL and unavailable payloads remain explicit. Reaching the current source end does not erase omissions in earlier pages. Retained accounting history is not a retained copy of transcript content.

Choose **Bookmark record** on a readable record, then open **Record bookmarks** to return to its exact anchor. Bookmarks are private local annotations and survive restarts, paging, and incremental source appends. Adding one verifies the current record's source identity, offset, and hash in Rust; changing or removing its source never redirects it to another record. You can remove a bookmark even when its source is missing. Recovery leaves earlier bookmarks in the preserved backup and labels them unrestored; confirmed history purge removes the affected session's bookmarks.

The list retains at most 500 distinct record anchors per session, including removed-anchor revisions needed to reject stale edits. It stores no record bodies, titles, paths, or snippets. Bookmark metadata stays out of session summaries, ordinary exports, diagnostics, logs, and MCP responses.
