import { expect, test, type Locator, type Page, type TestInfo } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { appViews } from '../../src/lib/appViews';
import visualManifest from './manifest.json' with { type: 'json' };

type Theme = 'light' | 'dark';
type ViewId = 'all' | 'codex' | 'claude' | 'instructions' | 'settings';

const FIXED_TIME = new Date('2026-07-29T15:30:00.000Z');
const visualStyles = readFileSync(new URL('./screenshot.css', import.meta.url), 'utf8');

interface VisitOptions {
  scenario?: string;
  theme?: Theme;
  view?: ViewId;
}

interface ScreenshotOptions {
  fullPage?: boolean;
}

type VisualTestBody = (page: Page) => Promise<void>;

function visualUrl(scenario: string): string {
  return `/?visualScenario=${encodeURIComponent(scenario)}`;
}

function isAllowedConsoleError(page: Page, text: string): boolean {
  // Playwright's installed clock tries to run inside every frame. The export's
  // script-free opaque sandbox correctly rejects that injected harness script.
  if (text === "Blocked script execution in 'about:srcdoc' because the document's frame is sandboxed and the 'allow-scripts' permission is not set."
    && page.frames().some(frame => frame.url() === 'about:srcdoc')) return true;
  const scenario = new URL(page.url()).searchParams.get('visualScenario');
  return scenario === 'updater-error'
    && /^update install failed: Error: Fixture updater install failed\.(?:\n|$)/.test(text);
}

async function visit(page: Page, options: VisitOptions = {}): Promise<void> {
  const { scenario = 'default', theme = 'light', view = 'all' } = options;
  await page.addInitScript(({ selectedTheme, css }) => {
    // Only configure the application. Export previews deliberately have opaque origins.
    if (window !== window.top) return;
    localStorage.setItem('themePreference', selectedTheme);
    const applyVisualCss = () => {
      const style = document.createElement('style');
      style.dataset.visualTest = 'true';
      style.textContent = css;
      document.head.append(style);
    };
    if (document.head) applyVisualCss();
    else document.addEventListener('DOMContentLoaded', applyVisualCss, { once: true });
  }, { selectedTheme: theme, css: visualStyles });

  await page.goto(visualUrl(scenario), { waitUntil: 'domcontentloaded' });
  await page.locator('nav[aria-label="Views"]').waitFor();
  await page.evaluate(async () => { await document.fonts.ready; });
  await expect(page.locator('nav[aria-label="Views"]')).toBeVisible();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);

  if (view !== 'all') {
    await page.getByRole('button', { name: viewLabel(view), exact: true }).click();
  }
}

function viewLabel(view: ViewId): string {
  return {
    all: 'All',
    codex: 'Codex',
    claude: 'Claude Code',
    instructions: 'Instructions',
    settings: 'Settings',
  }[view];
}

async function expectVisualReady(page: Page): Promise<void> {
  await page.evaluate(async () => { await document.fonts.ready; });
  await expect(page.locator('body')).toBeVisible();
}

async function expectSessionRollup(page: Page, visible: number, total = visible): Promise<void> {
  const sessionKpi = page
    .getByText('Sessions · All time', { exact: true })
    .filter({ visible: true })
    .locator('..');
  await expect(sessionKpi).toContainText(`${visible} of ${total}`);
}

async function scrollHeadingToTop(page: Page, name: string): Promise<void> {
  const heading = page.getByRole('heading', { name, exact: true });
  await heading.evaluate((element) => element.scrollIntoView({ block: 'start' }));
  await expect.poll(async () => (await heading.boundingBox())?.y ?? Number.POSITIVE_INFINITY).toBeLessThan(160);
}

async function capture(
  page: Page,
  testInfo: TestInfo,
  id: string,
  options: ScreenshotOptions = {},
): Promise<void> {
  await expectVisualReady(page);
  const currentDir = join(process.cwd(), 'output', 'playwright', 'current');
  await mkdir(currentDir, { recursive: true });
  const currentPath = join(currentDir, `${id}.png`);
  await page.screenshot({ path: currentPath, fullPage: options.fullPage ?? false, animations: 'disabled', caret: 'hide' });
  await testInfo.attach(`current-${id}`, { path: currentPath, contentType: 'image/png' });
  await expect(page).toHaveScreenshot(`${id}.png`, {
    fullPage: options.fullPage ?? false,
    animations: 'disabled',
    caret: 'hide',
  });
}

const declaredSnapshotIds = new Set<string>(visualManifest.snapshots);
const registeredSnapshotIds = new Set<string>();
const duplicateManifestIds = visualManifest.snapshots.filter(
  (id, index, ids) => ids.indexOf(id) !== index,
);

if (duplicateManifestIds.length > 0) {
  throw new Error(`Duplicate visual manifest IDs: ${[...new Set(duplicateManifestIds)].join(', ')}`);
}

function visualTest(
  id: string,
  title: string,
  body: VisualTestBody,
  options: ScreenshotOptions = {},
): void {
  if (!declaredSnapshotIds.has(id)) {
    throw new Error(`Visual test "${title}" registered undeclared snapshot ID: ${id}`);
  }
  if (registeredSnapshotIds.has(id)) {
    throw new Error(`Visual snapshot ID registered more than once: ${id}`);
  }
  registeredSnapshotIds.add(id);

  test(title, async ({ page }, testInfo) => {
    await body(page);
    await capture(page, testInfo, id, options);
  });
}

function visualPanelTest(id: string, title: string, body: (page: Page) => Promise<Locator>): void {
  if (!declaredSnapshotIds.has(id) || registeredSnapshotIds.has(id)) {
    throw new Error(`Visual panel snapshot ID missing or duplicate: ${id}`);
  }
  registeredSnapshotIds.add(id);
  test(title, async ({ page }, testInfo) => {
    const sourcePanel = await body(page);
    await expectVisualReady(page);
    await sourcePanel.evaluate((element, snapshotId) => {
      const clone = element.cloneNode(true) as HTMLElement;
      clone.dataset.visualPanelCapture = snapshotId;
      clone.style.position = 'absolute';
      clone.style.top = '0';
      clone.style.left = '0';
      clone.style.width = `${element.getBoundingClientRect().width}px`;
      clone.style.zIndex = '2147483647';
      document.body.appendChild(clone);
    }, id);
    const panel = page.locator(`[data-visual-panel-capture="${id}"]`);
    const currentDir = join(process.cwd(), 'output', 'playwright', 'current');
    await mkdir(currentDir, { recursive: true });
    const currentPath = join(currentDir, `${id}.png`);
    await panel.screenshot({ path: currentPath, animations: 'disabled', caret: 'hide' });
    await testInfo.attach(`current-${id}`, { path: currentPath, contentType: 'image/png' });
    await expect(panel).toHaveScreenshot(`${id}.png`, { animations: 'disabled', caret: 'hide' });
  });
}

function assertManifestCasesAreRegistered(): void {
  const unregistered = visualManifest.snapshots.filter((id) => !registeredSnapshotIds.has(id));
  if (unregistered.length > 0) {
    throw new Error(`Manifest snapshots without a visual test: ${unregistered.join(', ')}`);
  }
}

visualTest('calendar-activity', 'calendar heatmap and daily trend', async (page) => {
  await visit(page, { view: 'codex' });
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  await analytics.locator('summary').first().click();
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByTestId('calendar-total')).toBeVisible();
  await calendar.scrollIntoViewIfNeeded();
});

visualTest('calendar-partial-narrow', 'partial recorded history calendar at narrow width', async (page) => {
  await page.setViewportSize({ width: 520, height: 900 });
  await visit(page, { scenario: 'history-partial', view: 'codex' });
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  await analytics.locator('summary').first().click();
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByTestId('calendar-total')).toContainText('partial history');
  await calendar.scrollIntoViewIfNeeded();
});

visualTest('activity-summary-preview', 'local activity SVG and Markdown preview', async (page) => {
  await visit(page, { view: 'codex' });
  await page.clock.setFixedTime(FIXED_TIME);
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  await analytics.locator('summary').first().click();
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByTestId('calendar-total')).toBeVisible();
  await calendar.getByRole('button', { name: 'Preview summary card' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByAltText('Exact local SVG activity summary preview')).toBeVisible();
});

visualTest('activity-summary-partial-narrow', 'partial activity summary keeps coverage at narrow width', async (page) => {
  await page.setViewportSize({ width: 520, height: 900 });
  await visit(page, { scenario: 'history-partial', view: 'codex' });
  await page.clock.setFixedTime(FIXED_TIME);
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  await analytics.locator('summary').first().click();
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByTestId('calendar-total')).toContainText('partial history');
  await calendar.getByLabel('Activity metric').selectOption('tool_calls');
  await calendar.getByRole('button', { name: 'Preview summary card' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByLabel('Companion Markdown')).toHaveValue(/Partial recorded history/);
});

test.beforeEach(async ({ page }) => {
  await page.clock.install({ time: FIXED_TIME });
  page.on('pageerror', (error) => {
    throw new Error(`Unexpected page error: ${error.message}`);
  });
  page.on('console', (message) => {
    if (message.type() === 'error' && !isAllowedConsoleError(page, message.text())) {
      throw new Error(`Unexpected browser console error: ${message.text()}`);
    }
  });
});

const primaryViews: ReadonlyArray<{ id: ViewId; label: string }> = [
  { id: 'all', label: 'All' },
  { id: 'codex', label: 'Codex' },
  { id: 'claude', label: 'Claude Code' },
  { id: 'instructions', label: 'Instructions' },
  { id: 'settings', label: 'Settings' },
];

for (const { id: view, label } of primaryViews) {
  for (const theme of ['light', 'dark'] as const) {
    visualTest(`primary-${view}-${theme}-desktop`, `primary ${label} ${theme} desktop`, async (page) => {
      await visit(page, { theme, view });
      if (view === 'instructions') {
        await expect(page.getByRole('heading', { name: 'Instructions', exact: true })).toBeVisible();
        await expect(page.getByText('Project instructions')).toBeVisible();
      } else if (view === 'settings') {
        await expect(page.getByRole('heading', { name: 'About & updates', exact: true })).toBeVisible();
      } else {
        const grid = page.locator('[data-testid="session-grid-region"]:visible');
        await expect(grid).toBeVisible();
        await expect(grid).toContainText(view === 'claude' ? 'Dark mode palette sweep' : 'Add dark mode toggle');
        await expectSessionRollup(page, view === 'all' ? 15 : view === 'codex' ? 8 : 7);
      }
    });
  }
}

visualTest('session-selected-detail', 'session selected detail', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.clock.runFor(500);
  await expect(page.locator('[aria-label="Session details"]:visible')).toContainText('Add dark mode toggle');
  await expectSessionRollup(page, 8);
});

visualTest('session-context-menu', 'session context menu', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click({ button: 'right' });
  await expect(page.getByRole('menu', { name: /Export Add dark mode toggle/ })).toBeVisible();
  await expectSessionRollup(page, 8);
});

visualTest('sessions-filtered-empty', 'session filtered empty', async (page) => {
  await visit(page, { view: 'all' });
  await page.getByRole('searchbox', { name: 'Search sessions' }).fill('no matching visual fixture');
  await expect(page.getByText('No sessions match the current filters.')).toBeVisible();
});

visualTest('sessions-true-empty', 'session true empty', async (page) => {
  await visit(page, { scenario: 'sessions-empty', view: 'all' });
  await expect(page.getByText('No sessions found').filter({ visible: true })).toBeVisible();
});

visualTest('sessions-scanning', 'session scanning', async (page) => {
  await visit(page, { scenario: 'sessions-scanning', view: 'all' });
  await expect(page.getByText(/Scanning your sessions|Scanning sessions/).first()).toBeVisible();
});

visualTest('sessions-subagents-collapsed', 'session subagents collapsed', async (page) => {
  await visit(page, { view: 'codex' });
  const collapse = page.getByRole('button', { name: /Collapse subagent rows for Add dark mode toggle/ });
  await expect(collapse).toBeVisible();
  await collapse.click();
  await expect(page.getByRole('button', { name: /Expand subagent rows for Add dark mode toggle/ })).toBeVisible();
  await expectSessionRollup(page, 8);
});

visualTest('sessions-subagent-drilldown', 'session subagent drill-down with per-run detail', async (page) => {
  await visit(page, { view: 'codex' });
  // The subagent-count chip scopes the grid to one parent and its runs, which
  // also flattens the ordering so a column sort ranks the runs against
  // each other rather than nesting them under the parent.
  await page.getByRole('button', { name: /Show only .* and its \d+ subagent runs?/ }).first().click();
  await expect(page.getByRole('button', { name: 'Show all sessions' })).toBeVisible();
  await expect(page.getByText('and its subagent runs')).toBeVisible();
});

visualTest('sessions-availability-fallback', 'session availability, fallback, and unpriced indicators', async (page) => {
  await visit(page, { scenario: 'sessions-availability-fallback', view: 'codex' });
  await expect(page.getByText(/^estimate · 1 unpriced model excluded$/i)).toBeVisible();
  await expect(page.getByText(/^1 unpriced model excluded · 1 fallback rate used$/i)).toBeVisible();
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.clock.runFor(500);
  await expect(page.locator('[aria-label="Session details"]:visible')).toContainText('source missing');
  await expect(page.locator('[title^="Fallback rate used"]:visible').first()).toBeVisible();
  await expectSessionRollup(page, 8);
});

visualTest('tool-dimensions', 'issue #44 tool, MCP, shell, and context attribution', async (page) => {
  // Issue #44's aggregate panel (ToolImpact.svelte) had zero visual coverage
  // before this test — the fixtures never rendered it. The 'tool-dimensions'
  // scenario adds a genuine Gemini CLI session (dev-mock.ts) rather than
  // overriding any provider's capability flags — Gemini CLI truly lacks
  // mcp_dimension/shell_dimension in provider.rs, so viewing only its tab
  // makes the panel render a real "Unavailable" for those two dimensions
  // while still showing real language and context-source data (both
  // provider-agnostic signals it does support) — the exact
  // unavailable-vs-zero distinction this issue exists to capture, with no
  // fixture-only fiction involved.
  await visit(page, { scenario: 'tool-dimensions' });
  await page.getByRole('button', { name: 'Gemini CLI', exact: true }).click();
  await page.getByText('Analytics & exports', { exact: false }).filter({ visible: true }).click();
  const dimensionSummary = page
    .getByText('Tool, MCP, shell & context attribution', { exact: false })
    .filter({ visible: true });
  await expect(dimensionSummary).toBeVisible();
  await dimensionSummary.click();
  await expect(page.getByText('Context source', { exact: true }).filter({ visible: true })).toBeVisible();
  await expect(page.getByText(/Unavailable — no provider/).filter({ visible: true }).first()).toBeVisible();
  await expect(page.getByText('python', { exact: true }).filter({ visible: true })).toBeVisible();
  const conversationCacheRow = page.getByText('Conversation / cache reuse').filter({ visible: true });
  await expect(conversationCacheRow).toBeVisible();
  // Scroll so the screenshot itself shows both the "Unavailable" rendering
  // and the real language/context-source data in one frame, not just DOM
  // presence.
  await conversationCacheRow.evaluate((element) => element.scrollIntoView({ block: 'end' }));
});

test.describe('narrow session drawer', () => {
  test.use({ viewport: { width: 800, height: 600 } });

  for (const view of ['all', 'codex', 'claude'] as const) {
    visualTest(`primary-${view}-narrow`, `primary ${view} narrow`, async (page) => {
      await visit(page, { view });
      await expect(page.locator('[data-testid="session-grid-region"]:visible')).toBeVisible();
      await expectSessionRollup(page, view === 'all' ? 15 : view === 'codex' ? 8 : 7);
    });
  }

  visualTest('session-narrow-detail-overlay', 'selected session is rendered in an overlay', async (page) => {
    await visit(page, { view: 'codex' });
    await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
    await page.clock.runFor(500);
    await expect(page.getByRole('dialog', { name: 'Session details' })).toBeVisible();
    await expectSessionRollup(page, 8);
  });
});

visualTest('instructions-preview', 'instructions preview', async (page) => {
  await visit(page, { view: 'instructions' });
  await expect(page.getByText('Project instructions')).toBeVisible();
});

visualTest('instructions-raw', 'instructions raw content', async (page) => {
  await visit(page, { view: 'instructions' });
  await page.getByRole('button', { name: 'Raw', exact: true }).click();
  await expect(page.locator('pre')).toContainText('Project instructions');
});

visualTest('instructions-empty', 'instructions empty inventory', async (page) => {
  await visit(page, { scenario: 'instructions-empty', view: 'instructions' });
  await expect(page.getByText(/No matching instruction files/)).toBeVisible();
});

visualTest('instructions-loading', 'instructions loading inventory', async (page) => {
  await visit(page, { scenario: 'instructions-loading', view: 'instructions' });
  await expect(page.getByRole('button', { name: /Cancel|Cancelling/ })).toBeVisible();
});

visualTest('instructions-inventory-error', 'instructions inventory error', async (page) => {
  await visit(page, { scenario: 'instructions-error', view: 'instructions' });
  await expect(page.getByRole('alert')).toBeVisible();
});

visualTest('instructions-content-error', 'instructions content error', async (page) => {
  await visit(page, { scenario: 'instructions-content-error', view: 'instructions' });
  await expect(page.getByText(/could not|failed|error/i).last()).toBeVisible();
});

visualTest('history-purge-review', 'history purge review requires confirmation', async (page) => {
  await visit(page, { scenario: 'history-purge', view: 'settings' });
  await page.getByRole('button', { name: 'Review eligible history…' }).click();
  await expect(page.getByLabel('Purge confirmation')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Purge reviewed history' })).toBeDisabled();
  await scrollHeadingToTop(page, 'Retention and recovery');
});

visualTest('history-recovery-review', 'history recovery review stays usable at narrow width', async (page) => {
  await page.setViewportSize({ width: 640, height: 900 });
  await visit(page, { scenario: 'history-recovery', view: 'settings' });
  await page.getByRole('button', { name: 'Preserve and rebuild readable history…' }).click();
  await expect(page.getByLabel('Recovery confirmation')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Preserve and rebuild', exact: true })).toBeDisabled();
  await scrollHeadingToTop(page, 'Retention and recovery');
});

visualTest('settings-roots', 'settings roots', async (page) => {
  await visit(page, { view: 'settings' });
  await scrollHeadingToTop(page, 'Watched roots');
});

visualTest('settings-instructions', 'settings instruction inventory', async (page) => {
  await visit(page, { view: 'settings' });
  await scrollHeadingToTop(page, 'Instruction inventory');
});

visualTest('integration-dark', 'integration evidence states in dark theme', async (page) => {
  await visit(page, { view: 'settings', theme: 'dark' });
  await expect(page.getByRole('article', { name: 'Codex integration' })).toContainText('Not configured');
  await scrollHeadingToTop(page, 'Integration Center');
});

visualTest('integration-narrow-preview', 'integration review remains usable in a narrow window', async (page) => {
  await page.setViewportSize({ width: 500, height: 800 });
  await visit(page, { view: 'settings' });
  const card = page.getByRole('article', { name: 'Codex integration' });
  await card.getByRole('button', { name: 'Preview setup', exact: true }).click();
  await scrollHeadingToTop(page, 'Review install for Codex');
  // Existing top-level navigation is wider than this viewport; keep the
  // component evidence at the left edge instead of inheriting scrollIntoView's
  // horizontal navigation adjustment.
  await page.evaluate(() => { document.querySelectorAll('*').forEach((element) => { element.scrollLeft = 0; }); });
  await expect(page.getByRole('button', { name: 'Apply reviewed change' })).toBeVisible();
  const integration = page.locator('section[aria-labelledby="integration-heading"]');
  expect(await integration.evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(500);
  expect((await integration.boundingBox())!.x + (await integration.boundingBox())!.width).toBeLessThanOrEqual(500);
  expect((await integration.boundingBox())!.x).toBeGreaterThanOrEqual(0);
});

visualTest('settings-rates', 'settings rates frame', async (page) => {
  await visit(page, { view: 'settings' });
  await page.getByRole('heading', { name: 'Rate card', exact: true }).scrollIntoViewIfNeeded();
});

test('offline FX draft survives keyboard input and saves backend delivery evidence', async ({ page }, testInfo) => {
  await visit(page, { view: 'settings' });
  await page.getByLabel('Use a user-supplied FX rate').check();
  await page.getByLabel('Original money currency').selectOption('USD');
  await page.getByLabel('Display currency').selectOption('EUR');
  const rate = page.getByLabel('Display units per original unit');
  await rate.fill('');
  await rate.pressSequentially('0.');
  await expect(rate).toHaveValue('0.');
  await rate.pressSequentially('9');
  const timestamp = page.getByLabel('Rate timestamp (UTC)');
  await timestamp.pressSequentially('2026-10-');
  await expect(timestamp).toHaveValue('2026-10-');
  await timestamp.pressSequentially('01T12:30:00');
  await page.getByLabel('User-supplied source').pressSequentially('Synthetic offline quote');
  const section = page.getByRole('heading', { name: 'Rate card', exact: true }).locator('..');
  await section.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(section.getByText(/Loaded source: saved override/)).toBeVisible();
  await expect(rate).toHaveValue('0.9');
  await expect(timestamp).toHaveValue('2026-10-01T12:30:00');
  await timestamp.scrollIntoViewIfNeeded();
  await testInfo.attach('offline-fx-editor', { body: await page.screenshot(), contentType: 'image/png' });
});

visualTest('settings-rate-validation-error', 'settings rate validation error', async (page) => {
  await visit(page, { view: 'settings' });
  await page.getByRole('heading', { name: 'Rate card', exact: true }).scrollIntoViewIfNeeded();
  const fallback = page.locator('#fallback-model');
  await fallback.selectOption('');
  await page.getByRole('button', { name: 'Save', exact: true }).last().click();
  await expect(page.getByText(/fallback model/i).last()).toBeVisible();
});

visualTest('settings-save-error', 'settings save error', async (page) => {
  await visit(page, { scenario: 'settings-save-error', view: 'settings' });
  await page.getByRole('heading', { name: 'Watched roots', exact: true }).scrollIntoViewIfNeeded();
  await page.getByPlaceholder('/absolute/path/to/sessions').fill('/visual/failing-root');
  await page.getByRole('button', { name: 'Add', exact: true }).first().click();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await expect(page.getByText(/could not|failed|error/i).last()).toBeVisible();
});

visualTest('defender-slow-banner', 'defender slow scan banner', async (page) => {
  await visit(page, { scenario: 'defender-slow', view: 'all' });
  await expect(page.getByText(/Windows Defender/)).toBeVisible();
  await expectSessionRollup(page, 15);
});

visualTest('defender-error', 'defender error', async (page) => {
  await visit(page, { scenario: 'defender-error', view: 'all' });
  await page.getByRole('button', { name: 'Add exclusions…', exact: true }).click();
  await expect(page.getByText(/could not|failed|error/i).first()).toBeVisible();
  await expectSessionRollup(page, 15);
});

test('Defender verification survives Settings remount and suppresses the slow-scan prompt', async ({ page }) => {
  await visit(page, { scenario: 'defender-slow', view: 'settings' });
  await page.getByRole('heading', { name: 'Windows scan performance', exact: true }).scrollIntoViewIfNeeded();
  await page.getByRole('button', { name: 'Exclude session folders from Defender…', exact: true }).click();
  const verifiedStatus = page.getByRole('status').filter({ hasText: 'Last verified' });
  await expect(verifiedStatus).toContainText('for 3 session folders.');

  await page.getByRole('button', { name: 'All', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Add exclusions…', exact: true })).toHaveCount(0);

  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('heading', { name: 'Windows scan performance', exact: true }).scrollIntoViewIfNeeded();
  await expect(page.getByRole('status').filter({ hasText: 'Last verified' })).toContainText('for 3 session folders.');
});

visualTest('updater-available', 'updater available banner', async (page) => {
  await visit(page, { scenario: 'updater-available', view: 'all' });
  await expect(page.getByText('Version 9.9.9 is available.')).toBeVisible();
  await expectSessionRollup(page, 15);
});

visualTest('updater-installing', 'updater installing banner', async (page) => {
  await visit(page, { scenario: 'updater-installing', view: 'all' });
  await page.getByRole('button', { name: 'Update & restart', exact: true }).click();
  await expect(page.getByText('Downloading v9.9.9… 40%')).toBeVisible();
  await expectSessionRollup(page, 15);
});

visualTest('updater-error', 'updater installation error', async (page) => {
  await visit(page, { scenario: 'updater-error', view: 'all' });
  await page.getByRole('button', { name: 'Update & restart', exact: true }).click();
  await expect(page.getByText('Install failed — see console; you can retry.')).toBeVisible();
  await expectSessionRollup(page, 15);
});

test('project reassignment and restore stay usable in the narrow detail drawer', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 });
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: 'Select session Add dark mode toggle', exact: true }).click();
  const editor = page.getByRole('region', { name: 'Session project', exact: true });
  await editor.getByRole('button', { name: 'Change project' }).click();
  await editor.getByRole('button', { name: 'Make standalone project' }).click();
  await expect(editor).toContainText('Standalone project');
  await editor.getByRole('button', { name: 'Change project' }).click();
  const destination = editor.getByLabel('Destination project');
  await destination.focus();
  await expect(destination).toBeFocused();
  await destination.selectOption({ label: 'demo' });
  await editor.getByRole('button', { name: 'Move session' }).click();
  await expect(editor).not.toContainText('Standalone project');
  await editor.getByRole('button', { name: 'Change project' }).click();
  await editor.getByRole('button', { name: 'Restore detected project' }).click();
  await editor.getByRole('button', { name: 'Change project' }).click();
  await expect(editor.getByRole('button', { name: 'Restore detected project' })).toHaveCount(0);
  await editor.getByRole('button', { name: 'Cancel', exact: true }).click();
});

async function openQuotaPanel(page: Page, id: string): Promise<ReturnType<Page['locator']>> {
  const analytics = page.locator('[data-testid="analytics-panel"]:visible');
  await analytics.locator(':scope > summary').click();
  const panel = analytics.getByTestId(id);
  await panel.locator(':scope > summary').click();
  await panel.scrollIntoViewIfNeeded();
  return panel;
}

visualTest('quota-budget-editor-narrow', 'quota project USD editor at narrow width', async (page) => {
  await page.setViewportSize({ width: 800, height: 800 });
  await visit(page, { view: 'codex' });
  const budgets = await openQuotaPanel(page, 'quota-budgets-panel');
  await budgets.getByRole('button', { name: 'Add budget' }).click();
  await budgets.getByLabel('Budget type').selectOption('usd');
  await budgets.getByLabel('Project scope').selectOption({ label: 'demo' });
  await budgets.getByLabel('Threshold (USD)').fill('10');
  await expect(budgets.getByRole('button', { name: 'Save budget' })).toBeEnabled();
  await budgets.getByRole('button', { name: 'Save budget' }).focus();
  await expect(budgets.getByRole('button', { name: 'Save budget' })).toBeFocused();
  await budgets.scrollIntoViewIfNeeded();
  // Scrolling can place the save button under the earlier Add budget pointer.
  // Keep the keyboard focus assertion while capturing a stable non-hover state.
  await page.mouse.move(0, 0);
});

visualTest('quota-live-off-and-lookup-error', 'quota lookup failure keeps live polling off', async (page) => {
  await visit(page, { view: 'codex', theme: 'dark' });
  const live = await openQuotaPanel(page, 'live-quota-accounts');
  await expect(live).toContainText('No accounts approved. Live polling is off.');
  await live.getByRole('button', { name: 'Allow one account lookup' }).click();
  await expect(live.getByRole('alert')).toBeVisible();
  await expect(live).toContainText('No accounts approved. Live polling is off.');
  await expect(live.getByRole('button', { name: 'Enable polling for this account' })).toHaveCount(0);
  await live.scrollIntoViewIfNeeded();
});

visualTest('transcript-recorded-edit', 'inspector shows recorded tool result and edit in source order', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Inspect transcript', exact: true }).click();
  const inspector = page.getByRole('dialog', { name: 'Transcript inspector' });
  await expect(inspector).toBeVisible();
  await inspector.getByRole('button', { name: 'Next page' }).click();
  await inspector.getByRole('button', { name: 'Expand tool', exact: true }).click();
  await inspector.getByRole('button', { name: 'Expand assistant', exact: true }).click();
  await expect(inspector.getByText('Recorded replacement · demo.ts')).toBeVisible();
  await expect(inspector.getByRole('button', { name: 'Jump to tool call', exact: true })).toBeVisible();
});

visualTest('transcript-narrow-anchor', 'inspector supports narrow anchor navigation and escape', async (page) => {
  await page.setViewportSize({ width: 500, height: 800 });
  await visit(page, { view: 'codex', theme: 'dark' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Inspect transcript', exact: true }).click();
  const inspector = page.getByRole('dialog', { name: 'Transcript inspector' });
  await inspector.getByLabel('Record anchor').fill('synthetic:2');
  await inspector.getByRole('button', { name: 'Jump to record', exact: true }).click();
  await expect(inspector.getByRole('button', { name: 'Collapse tool', exact: true })).toBeVisible();
  await expect(inspector.getByText(/earlier records not inspected/)).toBeVisible();
  const bounds = await inspector.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(500);
  await page.keyboard.press('Escape');
  await expect(inspector).toHaveCount(0);
  await page.getByRole('button', { name: 'Inspect transcript', exact: true }).click();
  await inspector.getByRole('button', { name: 'Expand user', exact: true }).click();
});

visualTest('transcript-bookmarks-narrow', 'private record bookmarks preserve exact keyboard navigation at narrow width', async (page) => {
  await page.setViewportSize({ width: 500, height: 800 });
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Inspect transcript', exact: true }).click();
  const inspector = page.getByRole('dialog', { name: 'Transcript inspector' });
  await inspector.getByRole('button', { name: 'Bookmark record', exact: true }).first().click();
  await expect(inspector.getByText('Record bookmarks (1)', { exact: true })).toBeVisible();
  await inspector.getByText('Record bookmarks (1)', { exact: true }).click();
  await inspector.getByRole('button', { name: 'Next page', exact: true }).click();
  const bookmark = inspector.getByRole('button', { name: 'Open bookmarked record 1', exact: true });
  await bookmark.focus();
  await page.keyboard.press('Enter');
  await expect(inspector.locator('[id="transcript-synthetic:0"]')).toBeFocused();
  await expect(inspector.getByText('Update the greeting in the synthetic demo.', { exact: true })).toBeVisible();
  const bounds = await inspector.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(500);
});

visualTest('transcript-export-preview', 'export previews conversation text before local save', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Export transcript', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Export transcript', exact: true });
  await dialog.getByRole('button', { name: 'Build preview' }).click();
  await expect(dialog.getByRole('button', { name: 'Save reviewed HTML…' })).toBeDisabled();
  const preview = page.frameLocator('iframe[title="Exact transcript export preview"]');
  await expect(preview.locator('script,img,object,link,iframe')).toHaveCount(0);
  await expect(preview.getByText('Update the greeting in the synthetic demo.', { exact: true })).toBeVisible();
  await expect(preview.getByText('export const greeting = "Hello";', { exact: true })).toHaveCount(0);
});

visualTest('transcript-export-narrow', 'export inclusion changes require a new reviewed preview', async (page) => {
  await page.setViewportSize({ width: 500, height: 800 });
  await visit(page, { view: 'codex', theme: 'dark' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Export transcript', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Export transcript', exact: true });
  await dialog.getByRole('checkbox', { name: 'Tool calls and arguments' }).check();
  await dialog.getByRole('checkbox', { name: 'Tool results and errors' }).check();
  await dialog.getByRole('button', { name: 'Build preview' }).click();
  await expect(page.frameLocator('iframe').getByText('export const greeting = "Hello";', { exact: true })).toBeVisible();
  await dialog.getByRole('checkbox', { name: /I reviewed every/ }).check();
  await expect(dialog.getByRole('button', { name: 'Save reviewed HTML…' })).toBeEnabled();
  const bounds = await dialog.boundingBox();
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(500);
});

visualTest('organization-saved-search', 'organization saved search and tag management', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  const editor = page.getByRole('region', { name: 'Private session organization' });
  await editor.getByRole('button', { name: 'Edit organization' }).click();
  await editor.getByLabel('Pin this session').check();
  await editor.getByLabel('Tags (comma separated)').fill('review');
  await editor.getByLabel('Private note').fill('Synthetic local note.');
  await editor.getByRole('button', { name: 'Save organization' }).click();
  await expect(editor.getByText(/Pinned.*review.*Private note/)).toBeVisible();
  await page.getByText('Organize', { exact: true }).click();
  const toolbar = page.locator('[aria-label="Local session organization"]');
  await toolbar.getByLabel('Pinned sessions only').check();
  await toolbar.getByLabel('New search name').fill('Review work');
  await toolbar.getByRole('button', { name: 'Save current search' }).click();
  await toolbar.getByRole('combobox', { name: 'Saved search', exact: true }).selectOption({ label: 'Review work · summary · codex' });
  await expect(toolbar.getByRole('button', { name: 'Run saved search' })).toBeVisible();
  await expect(toolbar.getByText('Local organization saved.', { exact: true })).toBeVisible();
});

visualTest('organization-editor-narrow', 'private organization editor and keyboard at narrow width', async (page) => {
  await page.setViewportSize({ width: 800, height: 900 });
  await visit(page, { view: 'codex' });
  await expect(page.getByRole('button', { name: 'Filters', exact: true })).toBeVisible();
  await page.getByText('Organize', { exact: true }).click();
  await page.keyboard.press('Escape');
  await expect(page.locator('details[open] [aria-label="Local session organization"]')).toHaveCount(0);
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  const editor = page.getByRole('region', { name: 'Private session organization' });
  await editor.getByRole('button', { name: 'Edit organization' }).click();
  await editor.getByLabel('Pin this session').check();
  await editor.getByLabel('Tags (comma separated)').fill('review, follow-up');
  await editor.getByLabel('Private note').fill('Synthetic private note for this session only.');
  await editor.scrollIntoViewIfNeeded();
  await expect(editor.getByRole('button', { name: 'Discard changes' })).toBeVisible();
});

visualTest('organization-recovery-unavailable', 'recovered organization filters fail unavailable', async (page) => {
  await visit(page, { scenario: 'organization-recovered', view: 'codex' });
  await page.getByText('Organize', { exact: true }).click();
  const toolbar = page.locator('[aria-label="Local session organization"]');
  await expect(toolbar).toContainText('They were not reconstructed from source transcripts.');
  await toolbar.getByLabel('Pinned sessions only').check();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('alert').filter({ visible: true })).toContainText('Organization-filtered results are unavailable.');
});

visualTest('content-search-scopes', 'content search exposes explicit tool scope and exact source landing', async (page) => {
  await visit(page, { view: 'codex', theme: 'dark' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Search content', exact: true }).click();
  const search = page.getByRole('dialog', { name: 'Search session content' });
  await search.getByLabel('Find text').fill('greeting');
  await search.getByRole('button', { name: 'Search from start' }).click();
  await expect(search.getByText(/Matching records on this page: 1/)).toBeVisible();
  await search.getByLabel('Tool results and errors').check();
  await search.getByRole('button', { name: 'Search from start' }).click();
  await expect(search.getByText(/Matching records on this page: 2/)).toBeVisible();
});

visualTest('content-search-retained-narrow', 'missing source search shows separate retained coverage and exact field', async (page) => {
  await page.setViewportSize({ width: 500, height: 800 });
  await visit(page, { scenario: 'content-search-retained', view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Search content', exact: true }).click();
  const search = page.getByRole('dialog', { name: 'Search session content' });
  await search.getByLabel('Find text').fill('greeting');
  await search.getByRole('button', { name: 'Search from start' }).click();
  await search.getByRole('button', { name: 'Search retained messages' }).click();
  await search.getByRole('button', { name: 'Open retained message · user message' }).click();
  await expect(search.getByRole('region', { name: 'Selected retained message' })).toBeFocused();
  await expect(search.getByText(/may overlap them/)).toBeVisible();
  const bounds = await search.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(500);
});

test('content search lands on a source record and Escape returns to the search without deselecting the session', async ({ page }) => {
  await visit(page, { view: 'codex' });
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.getByRole('button', { name: 'Search content', exact: true }).click();
  const search = page.getByRole('dialog', { name: 'Search session content' });
  await search.getByLabel('Find text').fill('greeting');
  await search.getByRole('button', { name: 'Search from start' }).click();
  await search.getByRole('button', { name: 'Open source record · text' }).click();
  const inspector = page.getByRole('dialog', { name: 'Transcript inspector' });
  await expect(inspector.getByText('Update the greeting in the synthetic demo.', { exact: true })).toBeVisible();
  await expect(inspector.locator('[id="transcript-synthetic:0-block-0"]')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(inspector).toHaveCount(0);
  await expect(search).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(search).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Search content', exact: true })).toBeVisible();
});

visualPanelTest('workflow-measurement-desktop', 'workflow measurement shows before and after evidence with unavailable human outcomes', async (page) => {
  await visit(page, { view: 'codex' });
  await page.getByTestId('analytics-panel').filter({ visible: true }).locator('summary').first().click();
  const panel = page.getByTestId('workflow-panel').filter({ visible: true });
  await panel.locator('summary').first().click();
  await expect(panel.getByText('Tool failure rate')).toBeVisible();
  await expect(panel.getByText('User correction rate')).toBeVisible();
  await expect(panel.getByText('Unavailable').first()).toBeVisible();
  await expect(panel.getByText(/This is a scenario, not measured or causal savings/)).toBeVisible();
  return panel;
});

visualPanelTest('workflow-lifecycle-narrow', 'workflow finding lifecycle and comparison limits remain usable at narrow width', async (page) => {
  await page.setViewportSize({ width: 520, height: 900 });
  await visit(page, { view: 'codex' });
  await page.getByTestId('analytics-panel').filter({ visible: true }).locator('summary').first().click();
  const panel = page.getByTestId('workflow-panel').filter({ visible: true });
  await panel.locator('summary').first().click();
  await expect(panel.getByText('Tool failure rate')).toBeVisible();
  await panel.getByRole('button', { name: 'Record measurement' }).click();
  await expect(panel.getByRole('button', { name: 'Suppress finding' })).toBeVisible();
  await panel.getByRole('button', { name: 'Suppress finding' }).click();
  await expect(panel.getByRole('button', { name: 'Unsuppress finding' })).toBeVisible();
  await expect(panel.getByText(/observational comparison/)).toBeVisible();
  return panel;
});

visualPanelTest('workflow-action-preview-narrow', 'workflow remediation stays a reviewed dry run at narrow width', async (page) => {
  await page.setViewportSize({ width: 520, height: 900 });
  await visit(page, { view: 'codex' });
  await page.getByTestId('analytics-panel').filter({ visible: true }).locator('summary').first().click();
  const panel = page.getByTestId('workflow-panel').filter({ visible: true });
  await panel.locator('summary').first().click();
  await expect(panel.getByText('Tool failure rate')).toBeVisible();
  await panel.getByRole('button', { name: 'Record measurement' }).click();
  await panel.getByRole('button', { name: 'Preview future action' }).click();
  await expect(panel.getByText('Dry run only', { exact: false })).toBeVisible();
  await expect(panel.getByText(/Apply and undo are unavailable/)).toBeVisible();
  return panel;
});

visualPanelTest('quota-guard-preview-narrow', 'quota guard stays a reviewed dry run at narrow width', async (page) => {
  await page.setViewportSize({ width: 520, height: 900 });
  await visit(page, { view: 'codex' });
  const panel = await openQuotaPanel(page, 'quota-budgets-panel');
  await panel.getByRole('button', { name: 'Add budget' }).click();
  await panel.getByRole('button', { name: 'Save budget' }).click();
  await panel.getByRole('button', { name: 'Preview future guard' }).click();
  await expect(panel.getByText('Dry run only', { exact: false })).toBeVisible();
  await expect(panel.getByText(/Apply and undo are unavailable/)).toBeVisible();
  await panel.evaluate((element) => element.classList.add('bg-card'));
  return panel;
});

assertManifestCasesAreRegistered();

test('visual manifest covers every registered top-level view in light and dark', () => {
  // Mirrors the dev-mock `list_providers` fixture (see src/dev-mock.ts) since
  // tabs are generated from provider descriptors rather than a static list.
  const registeredIds = appViews([
    { id: 'codex', display_name: 'Codex', archived_sources: true, session_index: true },
    { id: 'claude_code', display_name: 'Claude Code', archived_sources: false, session_index: false },
  ]).map((view) => view.id).sort();
  const expectedIds = primaryViews.map((view) => view.id).sort();
  expect(registeredIds).toEqual(expectedIds);
});
