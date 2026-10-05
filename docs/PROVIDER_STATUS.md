# Public provider service status

Settings → Provider service status is off by default. Enabling it allows public, unauthenticated reads while the panel is open. Historical usage, prices, quotas, and budget decisions continue to use their existing local authorities whether status checks are enabled, unavailable, or disabled.

## Sources and meaning

- Codex shows the provider-wide [OpenAI public status endpoint](https://status.openai.com/api/v2/status.json).
- Claude Code shows the provider-wide [Claude public status endpoint](https://status.claude.com/api/v2/status.json).
- Gemini CLI has no supported source in this implementation and stays explicitly unavailable.

These sources were verified on October 4, 2026. A provider-wide operational response does not establish that a particular account, model, region, or session is available. The source's update time describes its public status record; the successful-check time is when Odometer received it.

## Request and data boundaries

The Rust service only accepts two fixed HTTPS destinations, with redirects and proxy discovery disabled. It sends no credentials, cookies, session identifiers, prompts, tool bodies, or usage data. The existing updater HTTP dependency provides TLS; no additional Tauri network or shell capability is added.

Each provider has one in-flight request at most. Requests have a four-second connect limit, eight-second total limit, and 32 KiB response limit. Successful reads wait at least five minutes before another request. Failures back off to one hour; a rate-limit Retry-After can defer a retry up to one day. Closing Settings stops future checks, and disabling the setting invalidates in-flight results. A request already in flight may finish within its bounded timeout.

Only allowlisted status indicators and timestamps are projected. Descriptions, incident bodies, and arbitrary JSON do not enter UI snapshots, diagnostics, or logs. Snapshots are held in memory; they are never ledger rows, quota history, budgets, or pricing evidence. Read errors return generic categories. A last-known state is labeled separately and never becomes a current state after a failed request, fifteen minutes without a successful check, or a backwards clock jump.

The local status snapshot refreshes every fifteen seconds while Settings is open. This only reads memory and asks Rust to schedule a request if due; it cannot bypass the backend cadence.
