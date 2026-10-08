import { expect, test } from '@playwright/test';

test('calendar scopes effective projects and opens exact bucket sessions without widening the page', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-07-29T15:30:00Z') });
  await page.setViewportSize({ width: 800, height: 800 });
  await page.goto('/');
  await page.getByRole('button', { name: 'Codex', exact: true }).click();
  await page.getByRole('button', { name: 'Analytics', exact: true }).filter({ visible: true }).click();
  await page.getByRole('button', { name: 'Usage', exact: true }).filter({ visible: true }).click();
  const analytics = page.getByTestId('analytics-panel').filter({ visible: true });
  const previousWidth = await page.evaluate(() => document.documentElement.scrollWidth);
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByTestId('calendar-total')).toBeVisible();
  await calendar.getByLabel('Calendar timezone').selectOption('utc');
  await calendar.getByLabel('Activity project').selectOption({ index: 1 });
  await expect(calendar.getByTestId('calendar-total')).not.toContainText('updating');
  await calendar.getByLabel('Activity metric', { exact: true }).selectOption('tool_calls');
  await expect(calendar.getByTestId('calendar-total')).toContainText('tool calls');
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(previousWidth);
  const day = calendar.getByRole('button', { name: /^2026-07-29:/ });
  const count = Number((await day.getAttribute('aria-label'))!.match(/Show (\d+) event sessions/)![1]);
  await day.click();
  await expect(page.getByText(`Calendar drill-down · ${count} sessions with recorded events`)).toBeVisible();
  await expect(page.getByRole('button', { name: /^Select session / })).toHaveCount(count);
  await expect(analytics).toBeHidden();
  await expect(page.getByRole('button', { name: 'Sessions', exact: true }).filter({ visible: true })).toHaveAttribute('aria-pressed', 'true');
});

test('unavailable coverage never renders a zero calendar', async ({ page }) => {
  await page.goto('/?visualScenario=history-unavailable');
  await page.getByRole('button', { name: 'Codex', exact: true }).click();
  await page.getByRole('button', { name: 'Analytics', exact: true }).filter({ visible: true }).click();
  await page.getByRole('button', { name: 'Usage', exact: true }).filter({ visible: true }).click();
  const calendar = page.getByTestId('calendar-activity').filter({ visible: true });
  await expect(calendar.getByText(/History coverage is unavailable/)).toBeVisible();
  await expect(calendar.getByTestId('calendar-total')).toHaveCount(0);
  await expect(calendar.getByRole('button', { name: /^\d{4}-\d{2}-\d{2}:/ })).toHaveCount(0);
});
