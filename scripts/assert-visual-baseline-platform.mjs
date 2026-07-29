const expectedEnvironment = 'playwright-v1.62.0-jammy';

if (process.platform !== 'linux' || process.env.ODOMETER_VISUAL_BASELINE_ENV !== expectedEnvironment) {
  console.error('Canonical visual baselines may only be updated in the pinned Playwright container documented in docs/VISUAL_TESTING.md.');
  process.exitCode = 1;
} else {
  console.log(`Canonical visual baseline environment: ${expectedEnvironment}.`);
}
