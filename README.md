# Odometer

[![CI](https://github.com/ekalb81/agent-odometer/actions/workflows/ci.yml/badge.svg)](https://github.com/ekalb81/agent-odometer/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ekalb81/agent-odometer)](https://github.com/ekalb81/agent-odometer/releases/latest)
[![Coverage](https://codecov.io/github/ekalb81/agent-odometer/branch/main/graph/badge.svg?token=FtbdQEOLFu)](https://codecov.io/github/ekalb81/agent-odometer)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

**How far have your AI agents driven?** Odometer is a local desktop dashboard for your AI coding-agent usage. It reads the session files that [Codex](https://openai.com/codex) (OpenAI's coding agent) and [Claude Code](https://claude.com/claude-code) already write to your machine and turns them into a searchable, sortable view of every session: what you asked, which models ran, how many tokens they consumed, and what that usage costs.

![Codex tab with the session list, cost overview, and a session open in the detail pane](docs/screenshots/codex-tab.png)

Everything happens on your machine. Odometer never uploads, phones home, or sends your prompts anywhere — it only reads the local files your agents already produced (and checks GitHub for its own updates).

## What you can see

- **Every session, across harnesses** — Codex, Claude Code, and an All tab that keeps credits and USD estimates explicitly separated.
- **Tokens where they went** — input, cached, output, and reasoning tokens per session, per model, and per turn.
- **What it costs** — Codex sessions show plan credits *and* an informational "what would this cost at OpenAI API rates" estimate; Claude Code sessions show Anthropic API-rate estimates. Rates live in an editable rate card.
- **Turn-by-turn detail** — click any session for its full story: prompts, replies, per-turn tokens and cost, context-window fill, and a tokens-over-time sparkline.
- **Subagents included** — background agents spawned by your sessions appear as their own badged, filterable entries linked to their parent.
- **Live** — sessions update in the list while your agents are still running.
- **Time-scoped answers** — filter by date range and the token/cost columns re-total to exactly that window ("what did I burn last week?").
- **Export and compare** — save the exact filtered projection as CSV/JSON and compare every model's token mix, cost, calls, retries, failures, and one-shot mutation rate.
- **Local efficiency signals** — normalized tool metrics, deterministic task categories, prioritized optimization opportunities with turn-level evidence and next actions, configuration-change correlations, and opt-in local Git outcome scans never retain raw tool arguments or output.
- **Tool impact comparison** — choose any observed tool provider or individual tool and compare turns where it was used with turns where it was not observed; when enough data exists, Odometer matches the baseline by harness, model, task category, and nearby time before comparing tokens and elapsed time.
- **Optional instruction inventory** — enable a hideable Instructions tab to find `AGENTS.md` and `CLAUDE.md` files across global, observed-project, and explicitly configured roots; review nested effective chains, deterministic warning signals, sanitized Markdown previews, and linked before/after usage evidence. Discovery is read-only, cancellable, progress-visible, bounded, and off by default.
- **Opt-in performance evidence** — default-off local timings cover startup, scans/cache/parsers, analytics, exports, and UI work; logs are size-bounded and exportable as JSONL or CSV.
- **Opt-in turn receipts** — add a reversible Codex or Claude Code `Stop` hook that shows the completed turn's tokens and estimated cost inside the harness. Codex receipts also preserve provider-reported quota precision and label per-turn changes as account-wide observations.
- **Quick glance** — the tray menu mirrors today's tokens, Codex credits/API estimate, and Claude USD with native show, hide, settings, and quit controls.
- **Light and dark** — follows your OS theme by default; switchable in Settings.

![Claude Code tab with subagent sessions and per-model spend](docs/screenshots/claude-code-tab.png)
![Session detail pane in dark mode showing per-turn costs and the turn history](docs/screenshots/session-details.png)

## Install

Download the installer for your platform from the [latest release](https://github.com/ekalb81/agent-odometer/releases/latest):

| Platform | File | Note |
| --- | --- | --- |
| Windows | `.msi` (recommended) or `-setup.exe` | Installers aren't code-signed yet; SmartScreen may warn — choose "More info → Run anyway". |
| macOS (Apple Silicon) | `.dmg` | Not notarized yet; right-click the app → Open on first launch. |
| Linux | `.AppImage` (no install) or `.deb` / `.rpm` | Mark the AppImage executable, then run it. |

Odometer checks for new releases on launch (and periodically while running) and offers a one-click in-place update.

The UI follows Tailwind 4's browser floor: Chrome, Edge, and WebView2 111+, Safari and WKWebView 16.4+, or Firefox 128+. Linux packages likewise require a current WebKitGTK system webview with equivalent CSS support. Older embedded webviews are unsupported, so keep the operating system webview current.

### First run

If Codex or Claude Code is installed with default paths, there is nothing to configure — Odometer finds your sessions automatically:

- Codex: `$CODEX_HOME` if set, otherwise `~/.codex` (`sessions/`, `archived_sessions/`, `session_index.jsonl`)
- Claude Code: `$CLAUDE_CONFIG_DIR/projects` if set, otherwise `~/.claude/projects`

Custom locations can be added under **Settings → Watched roots**.

The separate **Settings → Instruction inventory** section accepts project or project-container roots and lets each root scan only that folder or include subfolders. Recursive discovery skips common dependency, generated-output, and VCS directories instead of crawling every file on the machine.

### Turn receipts

Turn receipts are disabled by default. To enable them, open **Settings → Turn receipts**, select
Codex and/or Claude Code, and choose **Save setup**. Odometer adds one identifiable `Stop` hook to
the selected user-level harness configuration while preserving unrelated settings and hooks.

- In Codex, open `/hooks` once after setup to inspect and trust the command. New or changed
  non-managed hooks do not run until Codex records that trust.
- In Claude Code, use `/hooks` to inspect the installed command.
- Start a new harness task if an existing session does not reload configuration automatically.
- **Refresh status** shows the exact configuration file, whether the hook is present, its last run,
  and the last receipt. **Repair setup** reconciles missing or stale Odometer-owned entries.

The helper receives the harness-provided transcript path, verifies that it is a JSONL file inside
Odometer's configured roots, and parses that exact file. It exits successfully on every error and
never asks the agent to continue. Turning the feature off removes only Odometer-owned hook entries;
when disabled, the ordinary watcher/UI path is unchanged and no receipt helper runs.

## Privacy

Session files contain your prompts, the agents' replies, tool output, and local file paths. Odometer processes them entirely locally and stores nothing outside your machine: settings and rate overrides live under the OS config directory, while the scan cache and redacted configuration-event hashes live under the OS cache/data directories. Tool telemetry stores hashed target identities and byte counts, never raw arguments or output. Optional application performance tracking is off by default and stores only operation timings, success flags, and aggregate counts — never prompts, session IDs, paths, commands, tool arguments, or output. Its rotating local JSONL can be exported from Settings. Treat the session files themselves as sensitive — don't share or commit them.

When turn receipts are enabled, Odometer stores one bounded local health record per harness: the
last run time, success state, and rendered receipt or a sanitized error category. It does not store
the hook's session ID, transcript path, prompt, or response.

## How costs are estimated

Costs are computed from token counts against a bundled, editable rate card (per one million tokens):

- **Codex** usage is priced in plan credits per the OpenAI Codex rate card, with documented Fast-mode multipliers applied per event. A second column estimates the same usage at OpenAI **API** USD rates — informational if you're on a subscription, but useful for comparison.
- **Claude Code** usage is priced at Anthropic API USD rates. Cache reads are billed at the cached-input rate; cache *writes* (1.25×) aren't modeled, so estimates run slightly low. Thinking tokens are billed as ordinary output, matching Anthropic's billing.
- Unknown models fall back to a configurable per-harness fallback rate and are flagged in the UI. Models explicitly listed as unpriced are excluded and named instead of being assigned an unrelated fallback price.

Edit any rate under **Settings → Rate card**; your overrides persist and automatically inherit newly bundled models on upgrades.

---

## Development

Built with Tauri 2 + Rust (filesystem, parsing, IPC) and Svelte 5 + TypeScript + Tailwind (UI). See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for data flow, wire contracts, invariants, and known limitations.

Visual regression coverage and baseline-update guidance lives in [docs/VISUAL_TESTING.md](docs/VISUAL_TESTING.md).

Prerequisites: Node.js 22, Rust 1.95 or later, and the [Tauri 2 platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```powershell
npm ci
npm run tauri dev
```

| Command | Purpose |
| --- | --- |
| `npm run tauri dev` | Run the desktop app with hot reload |
| `npm run dev` | Frontend dev server only (port 1420; no native IPC — a fixture mock supplies demo data in plain browsers) |
| `npm run check` | Type-check TypeScript and Svelte |
| `npm test` | Run frontend unit and component tests with Vitest |
| `npm run test:coverage` | Run frontend tests and enforce the source-backed initial coverage slice |
| `npm run build` | Build the frontend into `dist/` |
| `npm run tauri build` | Build and bundle the desktop app |
| `npm run visual:test` | Run deterministic Playwright screenshot comparisons |
| `npm run visual:update` | Review and intentionally update Playwright baselines (Linux only; use the pinned container elsewhere) |
| `npm run visual:gallery` | Build an HTML gallery from current Playwright screenshots |
| `npm run visual:docs:update` | Copy selected current images into `docs/screenshots/` (add `-- --force` to replace reviewed files) |

The docs screenshot command is an explicit local action: it copies reviewed images from `output/playwright/current`, refuses to overwrite without `--force`, and is never run automatically in CI.

Match CI before handing off:

```powershell
npm run check
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Frontend tests live beside the modules and components they cover as `*.test.ts`. Parser integration tests and synthetic fixtures live in `src-tauri/tests/`; never commit real session data. Set `RUST_LOG` (e.g. `$env:RUST_LOG = 'odometer_lib=info'`) for native tracing.

### Repository layout

```text
src/                     Svelte frontend
  components/            Views and reusable UI
  lib/ipc.ts             Typed Tauri command/event boundary
  lib/types.ts           TypeScript mirrors of Rust wire models
  lib/credits.ts         Credit / API-cost calculations
  lib/sessionProjection.ts Shared filter, pricing, model-comparison, and export projection
src-tauri/
  src/                   Rust parsers, telemetry, correlation, config events, git outcomes, tray, and commands
  tests/                 Parser integration tests and fixtures
  capabilities/          Tauri permissions
  rates.json             Bundled rate card
  tauri.conf.json        Desktop build/window/updater configuration
```

Generated schemas under `src-tauri/gen/schemas/` are not hand-edited. Both lockfiles stay committed.

### Releases (maintainers)

Cutting a release, in order:

```sh
git switch main
git pull --ff-only origin main
git tag -s vX.Y.Z -m "Odometer vX.Y.Z" main  # annotated tag signed with your Git signing key
git push origin vX.Y.Z                        # triggers the cross-platform build
```

All three version fields (`package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`) must already equal `X.Y.Z`; the release workflow rejects a mismatched `vX.Y.Z` tag before any platform builds start. The tagged commit must also have a successful CI run. `git tag -s` creates an annotated, cryptographically signed Git tag; this Git signature is separate from the updater artifact signature.

The workflow creates or validates one mutable draft release during preflight, then builds and signs macOS, Linux, and Windows bundles in parallel without allowing matrix jobs to write the GitHub release or `latest.json`. After all platform builds succeed, one publisher downloads the one-day workflow artifacts, validates and uploads the exact signed asset set, and assembles and uploads a single complete `latest.json`. A final job downloads the draft manifest and validates its Tauri field types, release notes, complete signed platform map, asset names, sizes, SHA-256 digests, release ownership, tag, and exact commit. Do not create or publish the GitHub release manually before the workflow finishes: a published release is immutable and cannot accept corrected assets. Publish only after every release job succeeds. Updater packages are minisign-signed — the workflow needs the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets. The in-app updater follows the latest published release. OS code signing/notarization is not configured yet. A manual Actions run is build-only: it validates the three internal versions and bundles every platform, but never creates a tag or GitHub release.

## Contributing

Issues and pull requests welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for setup, the pre-PR checklist, and the one hard rule: never include real session data. Security issues go through [private reporting](SECURITY.md).

## License

[MIT](LICENSE)
