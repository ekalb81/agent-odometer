import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.{ts,svelte.ts}'],
    coverage: {
      provider: 'v8',
      reporter: ['json'],
      reportsDirectory: './coverage/frontend',
      include: [
        'src/components/DiagnosticsPanel.svelte',
        'src/components/SessionContextMenu.svelte',
        'src/components/SessionGridControls.svelte',
        'src/lib/configTimeline.ts',
        'src/lib/defenderStatus.ts',
        'src/lib/diagnosticsExport.ts',
        'src/lib/flushCadence.ts',
        'src/lib/rangeData.ts',
        'src/lib/sessionGrid.ts',
        'src/lib/sessionExport.ts',
        'src/lib/stores/defender.svelte.ts',
        'src/lib/stores/sessionGrid.svelte.ts',
        'src/lib/trayTotals.ts',
      ],
    },
  },
});
