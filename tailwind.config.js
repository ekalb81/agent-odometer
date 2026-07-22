/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{svelte,ts}'],
  theme: {
    extend: {
      // Semantic colors backed by the CSS variables in app.css. Opacity
      // modifiers (e.g. bg-app/50) don't apply to these — the variables
      // already carry their alpha where the design calls for it.
      colors: {
        app: 'var(--bg)',
        chrome: 'var(--chrome)',
        panel: 'var(--panel)',
        card: 'var(--card)',
        edge: 'var(--border)',
        edgerow: 'var(--row-border)',
        track: 'var(--track)',
        tablebg: 'var(--table)',
        ink: {
          DEFAULT: 'var(--text)',
          2: 'var(--text-2)',
          muted: 'var(--muted)',
          faint: 'var(--faint)',
        },
        accent: {
          DEFAULT: 'var(--accent)',
          dim: 'var(--accent-dim)',
          tab: 'var(--accent-tab)',
          cost: 'var(--accent-cost)',
          chipbg: 'var(--accent-chip-bg)',
          chipfg: 'var(--accent-chip-fg)',
          rowbg: 'var(--accent-row-bg)',
        },
        pos: 'var(--positive)',
      },
      fontFamily: {
        sans: ['Spline Sans', 'system-ui', '-apple-system', 'Segoe UI', 'sans-serif'],
        mono: ['Spline Sans Mono', 'ui-monospace', 'SFMono-Regular', 'Menlo', 'monospace'],
      },
    },
  },
  plugins: [],
};
