import { expect, test } from '@playwright/test';

test('Codex speed report defaults to turn throughput and can switch to response throughput', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-07-29T15:30:00Z') });
  await page.setViewportSize({ width: 800, height: 700 });
  await page.goto('/');
  await page.getByRole('button', { name: 'Codex', exact: true }).click();
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  await analytics.locator('summary').first().click();
  const existingScrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
  const panel = page.getByTestId('speed-panel').filter({ visible: true });
  await panel.locator('summary').click();
  await expect(panel.getByText('Unknown configured mode', { exact: true })).toBeVisible();
  await expect(panel.getByText('30.0 tok/s', { exact: true })).toBeVisible();
  await expect(panel.getByText('15.0 tok/s', { exact: true })).toBeVisible();
  await expect(panel.getByLabel('Speed report measurement')).toHaveValue('turn');
  await expect(panel.getByText('Comparable turn groups', { exact: true })).toBeVisible();
  await panel.getByLabel('Speed report window').selectOption('7days');
  await expect(panel.getByText('30.0 tok/s', { exact: true })).toBeVisible();
  await panel.getByLabel('Speed report measurement').selectOption('response');
  await expect(panel.getByText('90.0 tok/s', { exact: true })).toBeVisible();
  await expect(panel.getByText('40.0 tok/s', { exact: true })).toBeVisible();
  await expect(panel.getByText('30.0 tok/s', { exact: true })).toHaveCount(0);
  await panel.getByLabel('Speed report model').selectOption('gpt-5.5');
  await panel.getByLabel('Speed report effort').selectOption('high');
  const latest = panel.getByText('Latest responses (up to 20)');
  await latest.scrollIntoViewIfNeeded();
  await expect(latest).toBeVisible();
  await expect(panel.getByRole('button', { name: 'Export CSV', exact: true })).toBeEnabled();
  await panel.getByRole('button', { name: 'Export CSV', exact: true }).click();
  await expect(panel.getByRole('button', { name: 'Export CSV', exact: true })).toBeEnabled();
  await panel.screenshot({ path: 'output/playwright/speed-report-narrow.png' });
  // The existing header overflows at this width. Opening this panel must not
  // widen it further, and the new content itself must stay inside the viewport.
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(existingScrollWidth);
  const panelBounds = await panel.boundingBox();
  expect(panelBounds).not.toBeNull();
  expect(panelBounds!.x + panelBounds!.width).toBeLessThanOrEqual(800);
  await page.getByRole('button', { name: 'Claude Code', exact: true }).click();
  await expect(page.getByTestId('speed-panel').filter({ visible: true })).toHaveCount(0);
});
