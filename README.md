# Codex Activity Viewer

A local activity viewer for agent CLI harnesses — the ChatGPT desktop app's Codex experience and Claude Code — presented as per-harness tabs. It explores local task history, token usage, turn lifecycle, subagent activity, and estimated cost directly from each harness's session files.

## Features

- Per-harness tabs: Codex (ChatGPT app) and Claude Code, each with its own totals and cost currency.
- Search and filter active or archived sessions by text, model, and date/time.
- Inspect session metadata, prompts, context use, token history, and per-model totals.
- Distinguish subagent tasks and completed, aborted, or rolled-back turns — including Claude Code subagent transcripts, which appear as their own linked sessions.
- Open a Codex task (or a subagent's parent task) in ChatGPT through the supported `codex://` deep link.
- Estimate usage cost from an editable per-million-token rate card: plan credits and an OpenAI-API-rate USD estimate for Codex, Anthropic API USD rates for Claude Code.
- Watch multiple session/archive roots and overlay current names from Codex's session index.
- Reveal a session transcript in the platform file manager.
- Check for new releases at launch and self-update in place (signed updater packages).

All session processing is local. Session files can contain sensitive prompts, responses, tool output, and filesystem paths; do not share or commit them.

## Stack

- Tauri 2 and Rust for native filesystem and application logic.
- Svelte 5, TypeScript, Vite 6, and Tailwind CSS 3 for the UI.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full data flow, contracts, invariants, and known limitations.

## Prerequisites

- Node.js 22 and npm.
- Stable Rust; the crate declares Rust 1.77 as its minimum version.
- The [Tauri 2 platform prerequisites](https://v2.tauri.app/start/prerequisites/) for your operating system.

## Development

Install the locked frontend dependencies and start the desktop app:

```powershell
npm ci
npm run tauri dev
```

`npm run dev` starts only Vite on the fixed port `1420`; use it for frontend-only work where native IPC is not needed.

Useful commands:

| Command | Purpose |
| --- | --- |
| `npm run tauri dev` | Run the complete desktop app with hot reload |
| `npm run dev` | Run the frontend development server only |
| `npm run check` | Type-check TypeScript and Svelte |
| `npm run build` | Build the frontend into `dist/` |
| `npm run tauri build` | Build and bundle the desktop app for the host platform |
| `npm run preview` | Preview the already-built frontend |

Set `RUST_LOG` when native tracing is needed, for example `$env:RUST_LOG = 'debug'` in PowerShell before starting Tauri.

## Verification

Run the same checks used by CI:

```powershell
npm ci
npm run check
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

Rust parser integration tests and synthetic fixtures are under `src-tauri/tests/`. Rust unit tests live beside their modules. A frontend unit-test runner is not currently configured.

## Configuration and data

Codex inputs default to `$CODEX_HOME` when that environment variable is set, otherwise `~/.codex`:

- `$CODEX_HOME/sessions`
- `$CODEX_HOME/archived_sessions`
- `$CODEX_HOME/session_index.jsonl`

Claude Code sessions default to `$CLAUDE_CONFIG_DIR/projects`, otherwise `~/.claude/projects`.

Settings are editable in the app. They persist under the operating system's configuration directory as `codex-data-viewer/config.json`. A customized rate card is stored beside it as `rates.json`; otherwise the app uses the bundled `src-tauri/rates.json`.

The bundled rate card includes the current GPT-5.6 Sol, Terra, and Luna credit rates, their OpenAI API USD rates (for the Codex tab's est.-cost column), and Anthropic API USD rates for current Claude models. Rate values are per one million tokens. Cached input and reasoning output are subsets of input and output, not additional tokens. Unknown models use the configured per-harness fallback model's rate. When rollout settings identify Fast mode, the documented GPT-5.5 or GPT-5.4 multiplier is applied. Older user rate overrides automatically inherit newly bundled models without overwriting user-edited entries.

## Repository layout

```text
src/                     Svelte frontend
  components/            Views and reusable UI
  lib/ipc.ts             Typed Tauri command/event boundary
  lib/types.ts           TypeScript mirrors of Rust wire models
  lib/credits.ts         Token-range and credit calculations
src-tauri/
  src/                   Rust application modules
  tests/                 Parser integration tests and fixtures
  capabilities/          Tauri permissions
  rates.json             Bundled fallback rate card
  tauri.conf.json         Desktop build/window configuration
```

Tauri-generated schemas under `src-tauri/gen/schemas/` should not be edited manually. Keep `package-lock.json` and `src-tauri/Cargo.lock` committed.

## Releases

The GitHub release workflow builds Windows, Apple Silicon macOS, and Linux bundles for `v*` tags and creates a draft release for review. Updater packages are minisign-signed; the workflow requires the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets, and `tauri-action` uploads the `latest.json` manifest the in-app updater reads from the latest published release. The in-app updater only finds releases once they are published (not drafts) and publicly reachable. Platform code signing/notarization must be configured separately before distributing trusted production installers.
