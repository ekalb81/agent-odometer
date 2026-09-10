# Headless reports and MCP

Odometer's command-line reports and stdio MCP server read the durable local ledger through the same Rust query service used by the desktop. They do not start the desktop, rescan transcripts, parse the full corpus, refresh network sources, or write accounting data. The executable is `agent-odometer` (`agent-odometer.exe` on Windows); examples assume it is on `PATH`.

## Reports

| Report | Result |
| --- | --- |
| `status` | Ledger availability, session count, database size, and rate-card metadata. |
| `report` | Token usage and prices grouped by model and currency. |
| `models` | Model usage and pricing from the same range report. |
| `sessions` | Bounded session usage rows, with pricing and explicit truncation metadata. |
| `projects` | Usage and prices grouped by project; path-bearing labels are redacted by default. |
| `activity` | Token activity by date and hour. |
| `metrics` | Workflow measurements with their denominators and availability. |
| `categories` | Task-category observations and priced usage for whole overlapping sessions. |
| `tools` | Aggregate normalized tool observations. |
| `context` | Aggregate context-source observations. |
| `findings` | Aggregate optimization findings without transcript evidence or raw paths. |
| `diagnostics` | Local provider and data-quality diagnostics, with sensitive paths redacted by default. |
| `quota` | Previously recorded quota state with freshness and availability; no provider polling. |
| `mirrors` | Recorded mirrored-session groups. |
| `statusline` | Compact current-day usage for a shell prompt under a shorter query budget. |
| `verify` | Read-only verification checks, including the MCP adapter. |

```powershell
agent-odometer --help
agent-odometer report --from 2026-09-01 --to 2026-09-07 --format json
agent-odometer categories --from 2026-09-01 --format markdown
agent-odometer sessions --limit 20 --schema-version 2 --format json
agent-odometer export --report models --from 2026-09-01 --format csv
agent-odometer statusline
```

`agent-odometer <command> --help` also displays usage. `export --report <command>` selects the report to export; without `--report`, it exports `report`. Reports default to text, while `export` defaults to JSON. `--format` accepts `text`, `json`, `csv`, or `markdown`. Unknown, duplicated, unsupported, or incomplete options fail before a report runs.

`sessions --limit N` defaults to 20 rows; `--limit 0` removes that output row limit while retaining all query safety limits. The largest explicit limit is 50,000. `--include-paths` is accepted only by `projects` and `diagnostics`, including when selected through `export`.

Windowed reports accept `--from YYYY-MM-DD` and `--to YYYY-MM-DD`. Dates are UTC: `from` starts at midnight, and `to` includes the named day through `23:59:59.999Z`. Either boundary may be omitted; a reversed or malformed window is an error. Status, diagnostics, quota, mirrors, statusline, and verify do not accept an arbitrary date window. Activity's hour labels and statusline's current-day boundary use the CLI process's current local UTC offset; there is no CLI timezone override.

Token and price reports use exact inclusive range accounting from ledger rollups and partial-hour edges. Category observations have a different scope: they include whole sessions whose activity interval overlaps the requested window. Their category totals must not be read as exact event totals for the window.

Tool and context reports keep provider capability and availability explicit; an unsupported dimension is not reported as an observed zero. Findings aggregate timestamped observations within the window. Diagnostics report facts available to the headless process and label desktop scan/cache state unavailable; they do not infer a successful scan from the presence of a ledger.

Prices retain currency and provenance. Codex credits and API currency amounts are not added together. Missing or unavailable prices remain distinct from zero, and totals that omit explicitly unpriced models remain partial. Headless adapters do not implement their own pricing formulas.

## Output compatibility

Existing command output remains compatible by default: legacy JSON objects or arrays and command-specific CSV headers retain their version-1 shapes. Every report accepts `--schema-version 1` or `--schema-version 2`; other versions are rejected. Version 2 explicitly selects the common export contract, and the `export` command defaults to it. New report JSON uses its own version-1 data schema. Categories, tools, context, findings, diagnostics, and statusline have no legacy CSV shape, so their CSV uses the typed version-2 field stream even when no export version was requested. Models reuses the existing range report's formats.

Version-2 JSON has this envelope:

```json
{
  "schema_version": 2,
  "report": "models",
  "data": {}
}
```

`data` contains the report's complete typed result, including any report-specific schema version. The export version describes the outer format; it does not replace the report's own version.

Version-2 CSV uses these stable columns:

```csv
schema_version,report,path,type,value
```

Each row identifies a leaf in the serialized document using a JSON Pointer. Explicit version-2 exports include the envelope, so report fields appear under `/data/...`, alongside `/report` and `/schema_version`. A new report's default CSV points directly into its report data. Object keys escape `~` as `~0` and `/` as `~1`; array indexes are numeric path segments. The root pointer is the empty string. `type` distinguishes `null`, `boolean`, `number`, `string`, `array`, and `object`; `value` is JSON-encoded before CSV quoting. Empty arrays and objects are emitted as values rather than discarded. This preserves unavailable prices, explicit zeroes, empty results, and pricing provenance without flattening them into ambiguous blank cells.

Markdown renders a field/value table and identifies the export schema version. Dynamic text is escaped for table cells. Structured consumers should select JSON or the typed version-2 CSV rather than parsing display text.

Results go to stdout. Invalid arguments, unavailable required data, cancellation, deadlines, and size limits produce an error on stderr with exit code 2 instead of a fabricated empty report or silently truncated document. A caller must check the process exit status before using output.

## Read-only storage and limits

Headless queries open the existing SQLite ledger with `SQLITE_OPEN_READ_ONLY` inside a read transaction and require the application's current schema. They do not create an application database or configuration, change stored records, migrate a schema, or rebuild rollups. SQLite may create or update its WAL coordination sidecars (`-wal` and `-shm`) while a read-only connection coordinates with other readers and writers. Missing, older, or newer schemas and dirty ledgers with stale rollups are rejected. Run the matching desktop version to initialize, upgrade, or recover the ledger before retrying.

The shared query control bounds work per request:

| Limit | Default |
| --- | --- |
| Deadline | 10 seconds |
| Selected sessions | 50,000 |
| Range windows | 32 |
| Serialized output | 8 MiB |
| Rows visited | 1,000,000 |
| Category snapshot read in isolation | 64 MiB per snapshot |
| Total full-snapshot materialization for quota | 64 MiB per request |

Cancellation and deadline checks run during query work, including SQLite execution. Exceeding a bound fails explicitly; limits are not permission to return a plausible partial total. Narrow the requested window or session result when a query is too large.

Statusline requests use a 250 ms query budget in both adapters. Their performance targets are below 200 ms cold and 50 ms warm; these are provisional targets, not measured guarantees or a substitute for measurements on the relevant corpus and machine. Statusline reads ledger aggregates and does not parse transcripts on each shell prompt. Query deadlines are fixed by the command; the CLI does not expose a timeout override.

A synthetic benchmark on Windows (2026-09-09) used 10,000 sessions, 480,000 historical token events and matching rollups, and a window spanning four hour buckets with partial edges. Invalid snapshot JSON verified that this path never parses snapshots. Both builds met the 250 ms deadline; the provisional 50 ms warm target remains unmet:

| Build | New SQLite connection | Five warm queries, mean | Warm maximum |
| --- | --- | --- | --- |
| Debug | 172.46 ms | 181.73 ms | 194.54 ms |
| Release | 141.84 ms | 164.27 ms | 193.15 ms |

These timings cover opening a query handle and token aggregation, not process startup, rate-card loading, pricing, or formatting. “New connection” does not imply a flushed operating-system disk cache. Workload size and selected window affect latency; larger requests can return a deadline error. Reproduce the synthetic release measurement with:

```powershell
cargo test --release --manifest-path src-tauri/Cargo.toml --locked --lib benchmark_statusline_cold_and_warm_large_ledger -- --ignored --nocapture
```

## MCP transport

Launch the local server with:

```powershell
agent-odometer mcp
```

The server uses JSON-RPC 2.0 over stdio, with one JSON message per line, and advertises MCP protocol version `2025-06-18`. It supports `initialize`, `ping`, `tools/list`, and `tools/call`. Stdout is reserved for protocol responses. It does not listen on a local HTTP port or expose a network service.

| MCP tool | Corresponding report |
| --- | --- |
| `usage_report` | report |
| `model_report` | models |
| `project_report` | projects |
| `workflow_metrics` | metrics |
| `session_report` | sessions |
| `activity_report` | activity |
| `category_report` | categories |
| `tools_report` | tools |
| `context_report` | context |
| `findings_report` | findings |
| `diagnostics_report` | diagnostics |
| `quota_status` | legacy quota array |
| `quota_report` | versioned quota report |
| `mirrored_sessions` | versioned mirror report |
| `ledger_status` | status |
| `statusline` | statusline |

Windowed tools accept optional string arguments `from` and `to`, with the same strict inclusive UTC dates as the CLI. `session_report` additionally accepts an integer `limit` from 1 to 1,000, defaulting to 20; unlike the CLI, zero is not accepted. Diagnostics, quota, mirrors, ledger status, and statusline accept no date window. Activity and statusline use UTC rather than the CLI's local offset. Extra arguments are rejected, including `include_paths`: MCP project and diagnostic paths are always redacted.

Tool results contain a JSON report encoded in the MCP text-content item. A client can parse that text as JSON after checking `isError`. For example:

`ledger_status` and `diagnostics_report` return `ledger_available: false` when the ledger is missing, outdated, dirty, or unreadable. Unavailable ledger counts remain null; usage reports still return an explicit error. Availability reports retain cancellation, deadline, and row limits and do not create or repair the ledger.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"local-report-client","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"usage_report","arguments":{"from":"2026-09-01","to":"2026-09-07"}}}
```

At most two query workers run concurrently, with no waiting queue. An additional call receives error `-32001` and may be retried after an active query completes. Input lines are limited to 64 KiB; an oversized line is discarded through its newline so a following request can still be read. Each serialized response is limited to 8 MiB. Clients must match responses by request ID rather than assuming query completion order. Reusing an ID while its query is active is rejected. Accepted work drains on stdin EOF under its query controls.

Send cancellation as a notification whose `requestId` matches the active request:

```json
{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}
```

The input reader remains available while queries run. Cancellation reaches the shared query control; valid notifications themselves receive no response. Invalid JSON returns `-32700`, a malformed JSON-RPC request returns `-32600`, an unknown method returns `-32601`, and an unknown tool or invalid arguments return `-32602`. Query failures return an MCP tool result with `isError: true`. Invalid tool calls are rejected before opening user data files.

Clients must continue draining stdout. Standard blocking pipe backpressure can delay replies and cancellation processing when a client stops reading. The `verify` command gives its MCP subprocess a 20-second deadline and terminates and reaps it on completion or failure.

The existing `quota_status` tool preserves its raw-array result. `quota_report` returns `{ "schema_version": 1, "snapshots": [...] }`; `mirrored_sessions` returns `{ "schema_version": 1, "groups": [...] }`. These are report contracts, independent of the CLI's version-2 export envelope. Quota reports read stored observations only; they do not contact providers or reuse credentials.

## Privacy boundary

Aggregate reports expose normalized measurements and bounded metadata. They do not return prompts, responses, tool arguments, tool output, or raw finding evidence. Project and diagnostic paths are redacted by default; opting into a path-bearing report makes its output private local data. Session and mirror reports may contain durable session identifiers, so even a prompt-free report should be reviewed before sharing.

The headless surface adds no network refresh, credential use, transcript export, configuration write, or local HTTP server. Those capabilities require separate design and authorization; they are not implied by access to the read-only CLI or MCP tools.
