import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { generateDocScreenshots } from './generate-doc-screenshots.mjs';
import { generateVisualGallery } from './generate-visual-gallery.mjs';
import { prepareVisualCaptures } from './prepare-visual-captures.mjs';
import { validateVisualBaselines } from './validate-visual-baselines.mjs';

function withTempDirectory(run) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'odometer-visual-'));
  try {
    return run(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test('gallery removes stale generated files but preserves unrelated files', () => withTempDirectory((root) => {
  const input = path.join(root, 'current');
  const output = path.join(root, 'gallery');
  fs.mkdirSync(input, { recursive: true });
  fs.mkdirSync(output, { recursive: true });
  fs.writeFileSync(path.join(input, 'current.png'), 'current');
  fs.writeFileSync(path.join(output, 'stale.png'), 'stale');
  fs.writeFileSync(path.join(output, 'keep.txt'), 'keep');
  const result = generateVisualGallery({ input, output });
  assert.equal(result.count, 1);
  assert.equal(fs.existsSync(path.join(output, 'stale.png')), false);
  assert.equal(fs.readFileSync(path.join(output, 'keep.txt'), 'utf8'), 'keep');
}));

test('documentation screenshots require force before overwriting', () => withTempDirectory((root) => {
  const source = path.join(root, 'current');
  const output = path.join(root, 'docs');
  fs.mkdirSync(source, { recursive: true });
  fs.mkdirSync(output, { recursive: true });
  fs.writeFileSync(path.join(source, 'source.png'), 'new');
  fs.writeFileSync(path.join(output, 'target.png'), 'old');
  const mapping = { 'source.png': 'target.png' };
  const blocked = generateDocScreenshots({ source, output, mapping });
  assert.deepEqual(blocked.existing, ['target.png']);
  assert.equal(fs.readFileSync(path.join(output, 'target.png'), 'utf8'), 'old');
  const forced = generateDocScreenshots({ source, output, mapping, force: true });
  assert.deepEqual(forced.existing, []);
  assert.equal(fs.readFileSync(path.join(output, 'target.png'), 'utf8'), 'new');
}));

test('baseline validator reports missing, orphaned, and duplicate IDs', () => withTempDirectory((root) => {
  const manifest = path.join(root, 'manifest.json');
  const baselines = path.join(root, 'baselines');
  fs.mkdirSync(baselines);
  fs.writeFileSync(manifest, JSON.stringify({ snapshots: ['one', 'two', 'two'] }));
  fs.writeFileSync(path.join(baselines, 'one.png'), 'one');
  fs.writeFileSync(path.join(baselines, 'orphan.png'), 'orphan');
  const result = validateVisualBaselines({ manifest, baselines });
  assert.deepEqual(result.duplicateIds, ['two']);
  assert.deepEqual(result.missing, ['two', 'two']);
  assert.deepEqual(result.orphaned, ['orphan']);
}));

test('visual run setup removes the entire previous capture set', () => withTempDirectory((root) => {
  const current = path.join(root, 'current');
  fs.mkdirSync(path.join(current, 'nested'), { recursive: true });
  fs.writeFileSync(path.join(current, 'stale.png'), 'stale');
  fs.writeFileSync(path.join(current, 'nested', 'stale.txt'), 'stale');

  prepareVisualCaptures({ current });

  assert.equal(fs.existsSync(current), false);
}));
