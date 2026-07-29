# Visual regression testing

Odometer's visual checks use deterministic Playwright scenarios against the frontend fixture mode. The suite renders the important views and states, captures screenshots, and compares them with committed references. It is a review aid for UI changes, not a substitute for native Tauri smoke testing.

## What runs in CI

The `Visual regression` job is present on every pull request, push to `main`, and manual workflow run. `scripts/visual-impact.mjs` reads the event diff locally and marks the job as impacted when a changed file can affect rendering: frontend source (excluding source-only unit tests), HTML/config/assets, rates, package files, Playwright scenarios/configuration, visual helpers, or CI workflow files. Manual runs always execute the browser lane. Non-impacting changes still get a successful, explicit skipped result.

When impacted, CI runs `npm ci` and `npm run visual:test` inside the same digest-pinned Playwright Jammy container used to create the baselines. The operating system, browser engine, fonts, viewport, locale, and timezone therefore remain identical instead of depending on GitHub runner font rendering. The update command also requires an explicit marker supplied by the documented container invocation, so a host OS cannot overwrite the shared canonical references.

Failed or successful impacted runs upload an artifact containing:

- `output/playwright/gallery/` — an HTML gallery and the current PNGs;
- `output/playwright/report/` — the Playwright HTML report;
- `output/playwright/test-results/` — expected, actual, and diff files when an assertion fails.

Open the artifact before deciding whether a change is intentional. CI never updates references automatically.

## Scenario and state coverage

The scenario manifest is the coverage contract. Every top-level view must have a canonical primary state, and states with meaningful layout or interaction differences should have a named scenario. Current canonical primary IDs are:

```text
primary-all-{light,dark}-desktop
primary-codex-{light,dark}-desktop
primary-claude-{light,dark}-desktop
primary-instructions-{light,dark}-desktop
primary-settings-{light,dark}-desktop
```

The manifest also covers relevant empty, filtered-empty, loading/progress, selected-detail, error, archived/source-missing, pricing fallback/unpriced, narrow-window, disabled/busy, and validation-error states. Keep state data synthetic and stable; freeze time, locale, timezone, theme, and network/IPC responses. Add a scenario when a new screen or state becomes user-visible, and remove its baseline when the scenario is intentionally deleted. `scripts/validate-visual-baselines.mjs` fails for duplicate IDs, missing references, or orphaned PNGs.

## Local workflow

Install dependencies and run the complete visual suite:

```powershell
npm ci
npx playwright install chromium
npm run visual:test
```

To inspect the current captures, generate a local gallery:

```powershell
npm run visual:gallery
# open output/playwright/gallery/index.html
```

When a visual change is deliberate, review the diff and update references explicitly. Use the same pinned Playwright container as CI; its tag must match the `@playwright/test` version in `package-lock.json`:

```powershell
docker run --rm --ipc=host --volume "${PWD}:/work" --mount type=volume,target=/work/node_modules --env ODOMETER_VISUAL_BASELINE_ENV=playwright-v1.62.0-jammy -w /work mcr.microsoft.com/playwright:v1.62.0-jammy@sha256:b012874f829d298730411256666afcaeaeebaf505a0cf4c2f668d6dedb3d1e80 bash -lc "npm ci && npm run visual:update"
```

The `v1.62.0-jammy` tag and digest above match CI's Ubuntu 22.04 image and the currently pinned package version; update all three together whenever any changes. The separate container volume keeps Linux dependencies out of the host `node_modules`. `npm run visual:update` is accepted only inside this explicitly marked container; ordinary `npm run visual:test` runs remain available locally for iteration.

Commit updated snapshots together with the UI change and describe the visual consequence in the pull request. Never use a blanket snapshot update to hide unrelated differences.

Documentation images are optional derivatives of reviewed files in `output/playwright/current`. After a passing run, the maintainer may run `npm run visual:docs:update` (or pass `--source`/`--map` to `scripts/generate-doc-screenshots.mjs`) to copy them into `docs/screenshots/`. Existing documentation images make the command fail; after reviewing the replacement, rerun it as `npm run visual:docs:update -- --force`. This command is not part of CI.

## Privacy boundary

Visual fixtures must contain invented prompts, model names, paths, timestamps, and usage values. Do not point the browser at a real Codex or Claude Code corpus, and do not commit screenshots containing prompts, local paths, credentials, or other session data. The fixture IPC boundary exists specifically so visual artifacts can be uploaded safely for review.

## Native limitations

Browser screenshots verify Svelte layout, styling, content projection, responsive behavior, and most application states. They do not prove Windows WebView2 rendering, Tauri window chrome, native save dialogs, OS permission prompts, tray menus, auto-updater UI, or platform-specific filesystem behavior. Keep a smaller Windows/macOS/Linux `npm run tauri dev` smoke pass for release validation when those surfaces change; do not make native dialogs a per-PR screenshot baseline.
