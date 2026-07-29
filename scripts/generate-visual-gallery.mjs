import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

function parseArgs(argv) {
  const options = { input: 'output/playwright/current', output: 'output/playwright/gallery' };
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--input') options.input = argv[++index];
    if (argv[index] === '--output') options.output = argv[++index];
  }
  return options;
}

function collectPngs(root, current = root) {
  if (!fs.existsSync(current)) return [];
  const entries = fs.readdirSync(current, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const absolute = path.join(current, entry.name);
    if (entry.isDirectory()) return collectPngs(root, absolute);
    return entry.isFile() && entry.name.toLowerCase().endsWith('.png')
      ? [{ absolute, relative: path.relative(root, absolute) }]
      : [];
  });
}

function escapeHtml(value) {
  return value.replace(/[&<>"']/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[character]);
}

export function generateVisualGallery({ input = 'output/playwright/current', output = 'output/playwright/gallery' } = {}) {
  const inputRoot = path.resolve(input);
  const outputRoot = path.resolve(output);
  fs.mkdirSync(outputRoot, { recursive: true });
  for (const entry of fs.readdirSync(outputRoot, { withFileTypes: true })) {
    if (entry.isFile() && (entry.name === 'index.html' || entry.name.toLowerCase().endsWith('.png'))) {
      fs.rmSync(path.join(outputRoot, entry.name));
    }
  }
  const images = collectPngs(inputRoot).sort((a, b) => a.relative.localeCompare(b.relative));
  const cards = [];

  for (const [index, image] of images.entries()) {
    const extension = path.extname(image.relative).toLowerCase();
    const name = `${String(index + 1).padStart(3, '0')}-${path.basename(image.relative, extension).replace(/[^a-z0-9._-]+/gi, '-')}${extension}`;
    fs.copyFileSync(image.absolute, path.join(outputRoot, name));
    cards.push(`<figure><a href="${encodeURIComponent(name)}"><img loading="lazy" src="${encodeURIComponent(name)}" alt="${escapeHtml(image.relative)}"></a><figcaption>${escapeHtml(image.relative)}</figcaption></figure>`);
  }

  const title = `Odometer visual screenshots (${images.length})`;
  const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${title}</title>
<style>body{font:14px system-ui,sans-serif;background:#f5f7fa;color:#1f2937;margin:2rem}main{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:1rem}figure{background:#fff;border:1px solid #d1d5db;border-radius:8px;margin:0;padding:.75rem;box-shadow:0 1px 2px #0001}img{display:block;width:100%;height:auto;border-radius:4px}figcaption{font-family:ui-monospace,monospace;font-size:12px;margin-top:.5rem;overflow-wrap:anywhere}</style></head>
<body><h1>${title}</h1>${images.length ? `<main>${cards.join('')}</main>` : '<p>No PNG screenshots were found. Run <code>npm run visual:test</code> first.</p>'}</body></html>`;
  fs.writeFileSync(path.join(outputRoot, 'index.html'), html, 'utf8');
  return { count: images.length, output: outputRoot };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = generateVisualGallery(parseArgs(process.argv.slice(2)));
  console.log(`Wrote ${result.count} screenshot(s) to ${result.output}`);
}
