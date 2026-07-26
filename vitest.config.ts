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
        'src/components/SessionGridControls.svelte',
<<<<<<< HEAD
=======
        'src/lib/configTimeline.ts',
>>>>>>> origin/main
        'src/lib/sessionGrid.ts',
        'src/lib/stores/sessionGrid.svelte.ts',
      ],
    },
  },
});
