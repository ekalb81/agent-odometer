import { defineConfig, devices } from '@playwright/test';

/**
 * Visual snapshots are deliberately rendered in one pinned browser/runtime.
 * Baselines are generated and compared on Linux Chromium in CI.
 */
export default defineConfig({
  testDir: './tests/visual',
  globalSetup: './scripts/prepare-visual-captures.mjs',
  outputDir: 'output/playwright/test-results',
  snapshotPathTemplate: '{testDir}/{testFilePath}-snapshots/{arg}{ext}',
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: [
    ['line'],
    ['html', { outputFolder: 'output/playwright/report', open: 'never' }],
  ],
  use: {
    ...devices['Desktop Chrome'],
    baseURL: 'http://127.0.0.1:4173',
    browserName: 'chromium',
    locale: 'en-US',
    timezoneId: 'UTC',
    colorScheme: 'light',
    reducedMotion: 'reduce',
    viewport: { width: 1200, height: 800 },
    deviceScaleFactor: 1,
    screenshot: 'off',
    trace: 'retain-on-failure',
    video: 'off',
  },
  expect: {
    timeout: 10_000,
    toHaveScreenshot: {
      animations: 'disabled',
      caret: 'hide',
      scale: 'css',
      maxDiffPixels: 50,
    },
  },
  webServer: {
    command: 'npm run build -- --mode visual && npm run preview -- --host 127.0.0.1 --port 4173',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
