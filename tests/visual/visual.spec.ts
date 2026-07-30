import { expect, test, type Page, type TestInfo } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { APP_VIEWS } from '../../src/lib/appViews';
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
  const scenario = new URL(page.url()).searchParams.get('visualScenario');
  return scenario === 'updater-error'
    && /^update install failed: Error: Fixture updater install failed\.(?:\n|$)/.test(text);
}

async function visit(page: Page, options: VisitOptions = {}): Promise<void> {
  const { scenario = 'default', theme = 'light', view = 'all' } = options;
  await page.addInitScript(({ selectedTheme, css }) => {
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

function assertManifestCasesAreRegistered(): void {
  const unregistered = visualManifest.snapshots.filter((id) => !registeredSnapshotIds.has(id));
  if (unregistered.length > 0) {
    throw new Error(`Manifest snapshots without a visual test: ${unregistered.join(', ')}`);
  }
}

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

visualTest('sessions-availability-fallback', 'session availability, fallback, and unpriced indicators', async (page) => {
  await visit(page, { scenario: 'sessions-availability-fallback', view: 'codex' });
  await expect(page.getByText(/unpriced model excluded/i)).toBeVisible();
  await page.getByRole('button', { name: /Select session Add dark mode toggle/ }).click();
  await page.clock.runFor(500);
  await expect(page.locator('[aria-label="Session details"]:visible')).toContainText('source missing');
  await expect(page.locator('[title^="Fallback rate used"]:visible').first()).toBeVisible();
  await expectSessionRollup(page, 8);
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

visualTest('settings-roots', 'settings roots', async (page) => {
  await visit(page, { view: 'settings' });
  await scrollHeadingToTop(page, 'Watched roots');
});

visualTest('settings-instructions', 'settings instruction inventory', async (page) => {
  await visit(page, { view: 'settings' });
  await scrollHeadingToTop(page, 'Instruction inventory');
});

visualTest('settings-rates', 'settings rates frame', async (page) => {
  await visit(page, { view: 'settings' });
  await page.getByRole('heading', { name: 'Rate card', exact: true }).scrollIntoViewIfNeeded();
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

assertManifestCasesAreRegistered();

test('visual manifest covers every registered top-level view in light and dark', () => {
  const registeredIds = APP_VIEWS.map((view) => view.id).sort();
  const expectedIds = primaryViews.map((view) => view.id).sort();
  expect(registeredIds).toEqual(expectedIds);
});
