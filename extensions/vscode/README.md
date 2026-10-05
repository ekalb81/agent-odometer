# Odometer Local for VS Code

Open **Odometer: Open Local Dashboard** from the Command Palette, or use the Odometer activity-bar icon. This optional extension shows local recorded usage, history coverage, ledger observation age, and prices returned by Odometer. It has no runtime package dependencies and does not add a dependency to the desktop app.

## Install and connect

1. Install the matching Odometer desktop application (0.8.22 or newer) and let it prepare its history once. Headless queries require the current ledger schema; run the matching desktop version after upgrading.
2. Build a local VSIX from this directory: `npm ci`, `npm test`, then `npm run package`. In VS Code use **Extensions: Install from VSIX...** and select `odometer-local.vsix`. Restart the extension host if upgrading.
3. Run **Odometer: Select Local Executable** and select the installed `agent-odometer` executable (`agent-odometer.exe` on Windows). A local absolute path with that exact executable name is required; UNC network paths are rejected. A command string, shell script, or npm wrapper is not supported.
4. Open the dashboard and use its refresh button. Automatic refresh is off. Set the user preference `odometer.refreshSeconds` to 30–3600 to refresh while the view is visible. Values 1–29 use 30 seconds.

The extension supports desktop VS Code 1.105.0 and later. Browser/virtual workspaces are unsupported. It runs on the **local UI host**, including when the editor connects to SSH/WSL/containers: select the locally installed Odometer whose ledger you want to inspect. It does not query the remote workspace's provider files. Restricted Mode disables the extension; executable selection is stored only in user settings, never workspace settings.

## Reading the view

- Hover a row, or focus it and press **Enter**, to read the full value when a narrow sidebar truncates it. Screen readers receive the complete label and value.
- **Query received** is when this extension fetched a response. It does not prove the desktop has completed its scan. Responses older than five minutes or from a future clock become stale.
- **Ledger observation** is the backend's recorded observation time, shown separately. An old/missing timestamp is not proof of inactivity or an unavailable provider. Live scan state is explicitly unknown.
- **Today** uses the CLI host's current local-day window; the exact UTC bounds are displayed. Empty recorded usage is distinct from an unavailable query.
- History coverage stays **partial** or **unverified** when the backend says so. A fresh query cannot repair missing historical records.
- Prices are already computed by Rust. Codex purchased credits, included-usage references, and API USD estimates remain separate. Other providers expose their legacy price reference and explicit limitations. None is a provider bill or remaining allowance. No client-side rate calculation or currency conversion is performed.
- Failed refreshes retain the previous snapshot only with a stale/failure label. Changing the executable immediately clears it and cancels old work.

Missing executable: select the installed binary again. Missing/incompatible history: open the matching desktop app and wait for history preparation. Unsupported response: update the app and extension together. Timed-out query: inspect history in the desktop app, then retry. The extension never repairs or migrates a ledger.

## Privacy and limits

Only `integration-status --schema-version 2 --format json` and `statusline --schema-version 2 --format json` are invoked, without a shell, from the user's home directory. Each child has a 12-second timeout and 8 MiB output limit. Only one refresh runs at a time; configuration changes and deactivation cancel it. No long-lived MCP server, hooks, provider polling, account credentials, or network service is used. Headless SQLite reads may use WAL coordination sidecars, as documented in the repository's `docs/HEADLESS.md`.

The extension reads no provider files. It keeps response snapshots in memory and displays only aggregate values and selected provenance. It does not log raw command output/errors or persist session content. The configured executable is trusted local software chosen by the user; the extension cannot make a substituted executable safe.

## Distribution and verification

This is a **local VSIX preview**, not a Marketplace publication. Publishing credentials and release automation are separate maintainer decisions. Keep the lockfile when building. `npm run test:host` tests registration and failure behavior in a clean VS Code profile; set `VSCODE_TEST_VERSION=1.105.0` for the minimum supported version, or `stable` for current stable. Host tests download VS Code during development only. The installed extension works offline.

Design references: [Tree View API](https://code.visualstudio.com/api/extension-guides/tree-view), [Workspace Trust](https://code.visualstudio.com/api/extension-guides/workspace-trust), and [extension testing](https://code.visualstudio.com/api/working-with-extensions/testing-extension), checked 2026-10-04. Tests for tree/projection behavior do not replace testing the real CLI against a synthetic local ledger.
