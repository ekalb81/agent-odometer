# Session content search

Select a session, choose **Search content**, enter literal text, and search from
the start. Search is local and case-insensitive. Conversation messages are
selected by default; tool call arguments and tool results/errors each require
their own explicit checkbox. This search is separate from the session list's
summary filter.

Each request searches one bounded source page: up to 25 records, 128 KiB of
returned records, and the existing transcript reader's 1 MiB scan limit. Choose
**Search next page** to continue. Source results count matching records, with one
snippet per record; they do not count every occurrence. Opening a source result
loads its exact record anchor and focuses the matching content block in the
transcript inspector. Changed or missing records are unavailable, never replaced
by nearby text.

When source coverage is incomplete, **Search retained messages** can inspect the
existing retained snapshot. These results are separately labeled and can overlap
source results. The snapshot retains only each turn's first 500 characters of
the user prompt and final agent reply, not intermediate messages or tool bodies.
One page reads at most 25 turns. Snapshots larger than 8 MiB or with unsupported
formats are explicitly unsearched. A retained result opens the exact field only
while its opaque session lineage, snapshot revision and turn identity still
match; purge, replacement, revision changes and ambiguous turns invalidate it.

Empty results describe the examined page. Partial, omitted, oversized or
unavailable content can contain additional matches. Search query changes,
scope changes, session changes and closing the dialog discard pending responses.
Ordinary queries, snippets and opened content are ephemeral desktop data. No
content index, diagnostic payload, analytics field, export or MCP command is
added, and accounting/pricing are unchanged.

Unit tests cover scope privacy, Unicode/literal matching and bounded retained
targets. Component tests cover stale results, exact landing and honest coverage.
Browser visuals use synthetic typed fixtures and do not prove the Rust search
path; native validation exercises synthetic source files through the real IPC.
