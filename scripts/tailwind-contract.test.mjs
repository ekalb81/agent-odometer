import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = (relativePath) => fs.readFileSync(path.join(root, relativePath), 'utf8');

function sourceFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const absolutePath = path.join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(absolutePath);
    return /\.(?:css|svelte|ts)$/.test(entry.name) ? [absolutePath] : [];
  });
}

test('Tailwind 4 runs through Vite without the obsolete PostCSS pipeline', () => {
  const packageJson = JSON.parse(read('package.json'));
  const viteConfig = read('vite.config.ts');

  assert.match(packageJson.devDependencies.tailwindcss, /^\^4\./);
  assert.equal(
    packageJson.devDependencies['@tailwindcss/vite'],
    packageJson.devDependencies.tailwindcss,
  );
  assert.equal(packageJson.devDependencies.postcss, undefined);
  assert.equal(packageJson.devDependencies.autoprefixer, undefined);
  assert.equal(packageJson.devDependencies['@tailwindcss/postcss'], undefined);
  assert.match(viteConfig, /import tailwindcss from '@tailwindcss\/vite';/);
  assert.match(viteConfig, /plugins:\s*\[tailwindcss\(\), svelte\(\)\]/);
  assert.equal(fs.existsSync(path.join(root, 'postcss.config.js')), false);
  assert.equal(fs.existsSync(path.join(root, 'tailwind.config.js')), false);
});

test('the CSS-first theme preserves runtime semantic tokens and pointer behavior', () => {
  const css = read('src/app.css');
  const declarations = [
    '--color-app: var(--bg);',
    '--color-chrome: var(--chrome);',
    '--color-panel: var(--panel);',
    '--color-card: var(--card);',
    '--color-edge: var(--border);',
    '--color-edgerow: var(--row-border);',
    '--color-track: var(--track);',
    '--color-tablebg: var(--table);',
    '--color-ink: var(--text);',
    '--color-ink-2: var(--text-2);',
    '--color-ink-muted: var(--muted);',
    '--color-ink-faint: var(--faint);',
    '--color-accent: var(--accent);',
    '--color-accent-dim: var(--accent-dim);',
    '--color-accent-tab: var(--accent-tab);',
    '--color-accent-cost: var(--accent-cost);',
    '--color-accent-chipbg: var(--accent-chip-bg);',
    '--color-accent-chipfg: var(--accent-chip-fg);',
    '--color-accent-rowbg: var(--accent-row-bg);',
    '--color-pos: var(--positive);',
    '--text-xs--line-height: 1rem;',
    '--text-sm--line-height: 1.25rem;',
    '--text-base--line-height: 1.5rem;',
    '--text-xl--line-height: 1.75rem;',
  ];

  assert.match(css, /^@import 'tailwindcss';/);
  assert.match(css, /@theme inline\s*\{/);
  for (const declaration of declarations) {
    assert.ok(css.includes(declaration), `missing Tailwind theme declaration: ${declaration}`);
  }
  assert.match(css, /border-color: #e5e7eb;/);
  assert.match(css, /button:not\(:disabled\),\s*\n\s*\[role='button'\]:not\(:disabled\)\s*\{\s*\n\s*cursor: pointer;/);
});

test('source templates do not regress to Tailwind 3 compatibility-sensitive utilities', () => {
  const legacyUtilities = [];
  for (const file of sourceFiles(path.join(root, 'src'))) {
    const relativePath = path.relative(root, file);
    for (const [index, line] of fs.readFileSync(file, 'utf8').split(/\r?\n/).entries()) {
      if (/\bflex-shrink-/.test(line) || /focus:outline-none/.test(line) || /(?<![\w-])rounded(?![\w-])/.test(line)) {
        legacyUtilities.push(`${relativePath}:${index + 1}`);
      }
    }
  }

  assert.deepEqual(legacyUtilities, []);
});
