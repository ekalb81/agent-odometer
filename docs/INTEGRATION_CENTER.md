# Integration Center

Settings → Integration Center configures **local stdio MCP** for Codex and Claude Code. It does not configure HTTP, poll accounts, copy authentication, install packages, or change turn-receipt hooks. Generic clients use the displayed manual server command.

Choose user scope or an existing absolute project directory. User scope honors `CODEX_HOME` and `CLAUDE_CONFIG_DIR`; project scope uses `.codex/config.toml` or `.mcp.json`. Codex only loads trusted project configuration; Claude Code project MCP can require approval. Restart the client and create a fresh task after changes. A saved entry does not prove the client loaded it.

Preview shows only Odometer's intended entry. Apply compares the original file again and refuses concurrent edits. Other servers and unrelated settings remain intact. A pre-existing `odometer` entry without Odometer's ownership marker is never taken over or removed. Repair preserves supported custom options. Managed entries with custom environment/CWD or disabled options require manual repair or explicit removal followed by setup; their preview and automated tests are refused rather than discarding settings or executing credentials.

Existing configurations receive private timestamped `.odometer-mcp-backup-*.bak` files beside the config. Preview Restore is available in the same app session, only while both installed config and original backup are unchanged. After restart, the backup remains available for manual recovery. Preview Removal removes only the managed entry. New config files remain valid empty containers after removal so Odometer does not delete unrelated additions.

## Evidence shown by the cards

Installation requires an executable and a successful bounded version check. Configuration requires a matching supported entry in the selected scope. **Test now** starts the Odometer server and verifies protocol `2025-06-18`, all tool names/input schemas, and an `odometer_status` canary with a readable ledger. These are direct server checks, not proof of actual agent-task use. Unknown or failed checks never become working states.

Start fresh-task verification records a local observation boundary. Restart the supported client, create a new task using the copyable instructions, then refresh activity. A successful reported call must follow a new MCP initialization to satisfy that observation boundary. Client name/version is **self-reported**, not authenticated identity. `odometer-verifier` calls are excluded. Supported-client agent-task proof remains a separate **not verified** state: no reported name, activity record or direct verifier can promote it. Actual task acceptance requires independent evidence from both supported clients.

The bounded activity store retains at most 100 local calls and 256 KiB. It records allowlisted client/tool names, time/duration, success, fixed error code, row/byte counts, schema/observation metadata and coverage flags. It never retains prompts, request arguments, session IDs, filesystem paths, tool results, credentials, or error bodies. Unavailable activity is reported separately from an empty history. Concurrent activity writes use nonblocking file locks; a dropped observation cannot prove no use.

## Status and analytics instructions

`odometer_status` and `agent-odometer integration-status --format json` share the same Rust status projection. It reports version, ledger/provider availability, recorded token-event date coverage, current pricing resolution and suggested next calls. The snapshot observation marker is diagnostic freshness metadata, not a transactional revision or resume cursor. Headless scan readiness is **unknown**: a readable retained ledger does not establish a complete live scan. The desktop additionally reports its own current scan state.

Keep observed tokens and plan credits separate from API USD estimates and as-of billing scenarios. Preserve fallback, stale and unavailable pricing. `quota_report` carries source and age; transcript observations do not become authoritative live quota. Status does not contact providers. Missing usage or quota is not zero.

Suggested instructions: use Odometer's read-only analytics for observed agent usage; call status first when uncertain; discover session keys through `session_report`; use the same inclusive UTC window for comparisons; treat cohort differences and findings as observational signals. The UI provides tailored Codex/Claude instructions and a copyable optional `SKILL.md`.

Sample fresh task: “Call odometer_status, then usage_report and workflow_metrics for the last complete UTC week. Explain missing coverage and propose one measurable experiment without claiming a demonstrated speedup.” Raw transcript content is intentionally outside the MCP contract.

Core diagnostics (`integration_not_configured`, `server_launch_failed`, `protocol_version_mismatch`, `tool_catalog_mismatch`, `ledger_not_ready`, `scan_in_progress`, `session_not_found`, `pricing_incomplete`, `query_too_broad`, `snapshot_expired`) pair bounded evidence with a safe next action. Generic/cancelled queries additionally use `query_failed` / `query_cancelled`; adapters do not copy underlying error bodies into activity.

## Verification boundary

Automated native checks use isolated synthetic homes, source fixtures and client configs. Neither a direct canary nor browser fixtures close the supported-client fresh-task requirement. Actual authenticated Codex and Claude tasks require credentials already present in that isolated environment; user credentials must never be copied in. When authentication is absent, record that barrier and leave issue #57 open until both fresh-task calls have been demonstrated.
