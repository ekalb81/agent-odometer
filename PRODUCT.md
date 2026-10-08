# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The interface is a Svelte web UI inside a Tauri desktop application for Windows,
macOS, and Linux. Design for a resizable desktop window, keyboard and pointer
input, and native desktop integration. The browser preview is a development
surface; mobile and browser-hosted production products are not established scope.

## Users

Individual developers who use AI coding agents and want to understand and improve
their own usage, costs, quota, and efficiency. This audience and job were confirmed
by the maintainer during initialization on 2026-10-04.

They work across local agent sessions, projects, models, and subagents. They need
both a quick usage check and enough detail to investigate an unexpectedly costly
or slow session.

## Product Purpose

Odometer turns existing local agent session files into searchable usage history
with token counts, estimated costs, and supporting evidence. It helps developers
answer what ran, where usage went, what remains in supported quota windows, and
which workflow changes deserve investigation.

Success means users can find the relevant project, session, or turn, understand
the scope and reliability of its numbers, and make an informed next decision.

## Positioning

Odometer combines local session evidence from multiple agent harnesses with
durable history, shared pricing, and explicit provenance. Its useful distinction
is the connection between an overview and the sessions, turns, models, and tool
activity behind it. Privacy and honest accounting are central to that value.

## Operating Context

- Supported harnesses are Codex, Claude Code, and Gemini CLI. Available dimensions
  vary by provider and source format.
- On startup, discover configured local session roots and populate history while
  keeping preparation and scan status visible. Continue updating as agents work.
- Start from All or a provider tab, then search, filter dates, group by project,
  or compare models. Open session details to inspect turns and parent/subagent
  relationships. Preserve the user's scope when moving between views.
- Switch from Sessions to Analytics without changing provider or filter scope.
  Choose Usage, Tools & context, Changes & review, or Outcomes to investigate
  supporting evidence. Export the selected projection for further analysis.
- Settings groups General & sources, Projects & history, Pricing, Integrations,
  Alerts & widget, and Diagnostics. Visited forms retain drafts across section
  and provider-tab navigation. Pricing evidence is searchable by model; changed
  rate cards require a reload before a conflicting draft can be saved.
  Instruction inventory, turn receipts, and performance recording remain explicit
  opt-ins. The tray supports quick checks outside the main window.

## Capabilities and Constraints

- Local transcript evidence and the durable ledger govern historical usage.
  Preserve history across source moves or disappearance, and distinguish archive
  state, source availability, preparation, and failure.
- Codex plan credits and API USD estimates have different meanings. Claude Code
  and Gemini CLI costs are API-rate estimates. Keep unlike units separate and
  expose direct, alias, fallback, estimated, and unpriced provenance as applicable.
  An estimate must not imply an actual invoice or subscription charge.
- Dates change the accounting scope, not just which rows are visible. Preserve
  timezone conversion and the distinction between cumulative and scoped totals.
- Unsupported dimensions, missing evidence, stale external state, and failures
  must remain explicit. A missing measurement must not look like a measured zero;
  partial history must not look complete. Quota projections require enough evidence.
- Timing and tool-impact views describe observations. Recorded turn time includes
  tools, reasoning, and waiting; response measurements use a separate source.
  These views do not establish causal improvements or accepted-task delivery speed.
- Session text and durable snapshots are sensitive local data. Keep prompts,
  replies, credentials, raw tool payloads, and unrestricted paths out of diagnostic
  exports and performance recordings. Follow the exact outbound-network and
  provider-consent boundaries in [AGENTS.md](AGENTS.md).
- Rust owns filesystem access, parsing, persistence, and pricing. The Svelte
  frontend receives typed data and formats or aggregates already-priced values.
  Browser fixtures must retain the shared Rust pricing authority.
- Follow existing provider capabilities. Current Gemini CLI support reads the
  JSONL format; unsupported older formats or unavailable dimensions must not be
  filled with guesses.

## Brand Commitments

The product name is Odometer. Existing assets include the gauge wordmark in
`src/App.svelte` and application icons in `src-tauri/icons/`. Product language
should identify the scope, units, source, and uncertainty of a number plainly.
No additional aesthetic direction was established during initialization.

## Evidence on Hand

- [README.md](README.md) supplies current product capabilities and user workflows.
  [AGENTS.md](AGENTS.md) supplies accounting, privacy, consent, and validation
  requirements. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains implementation
  contracts; reconcile older descriptions with current source when they differ.
- Current provider descriptors live in `src-tauri/src/provider.rs`; navigation is
  derived through `src/lib/appViews.ts` and `src/App.svelte`.
- `docs/screenshots/` and `tests/visual/visual.spec.ts-snapshots/` contain existing
  interface captures. They demonstrate captured states and fixture scenarios,
  not every current capability or verified behavior against a live backend.
- `tests/conformance/` holds frozen pricing expectations. Synthetic parser
  fixtures and integration tests live under `src-tauri/tests/`.
- `npm run dev` serves the frontend at `http://localhost:1420` with browser fixture
  data, selected in `src/main.ts`. `npm run tauri dev` exercises the desktop
  application with native IPC. Keep these validation scopes distinct.

## Product Principles

1. Make every number interpretable: show its scope, units, provenance, and limits.
2. Keep useful detail reachable from the overview without losing the user's context.
3. Protect local session privacy and make optional collection or integration deliberate.
4. Preserve accounting truth through partial data, changing files, and unavailable sources.
5. Help users investigate efficiency with evidence; keep observations separate from claims.

## Accessibility & Inclusion

Preserve keyboard operation and usable narrow-window behavior, including loading,
empty, error, and unavailable states, as required by the repository's UI checklist.
Light and dark themes already exist. A formal accessibility conformance target
and any specific assistive-technology needs remain open; initialization does not
establish conformance.
