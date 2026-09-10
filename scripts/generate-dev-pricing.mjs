// Regenerates only synthetic browser responses. The frozen conformance oracle
// is intentionally outside this script's input and output paths.
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createPricingFixtureInput } from '../src/dev-mock/fixtures.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outputPath = path.join(root, 'src/dev-mock/pricing.generated.json');
const check = process.argv.includes('--check');
if (process.argv.slice(2).some(arg => arg !== '--check')) throw new Error('Usage: node scripts/generate-dev-pricing.mjs [--check]');
const fixtureInput = createPricingFixtureInput();
const input = JSON.stringify(fixtureInput);
const cases = fixtureInput.cases;
const result = spawnSync('cargo', ['run', '--quiet', '--locked', '--manifest-path',
  'src-tauri/Cargo.toml', '--example', 'generate_dev_pricing'], {
  cwd: root, input, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024,
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`Rust fixture generation failed:\n${result.stderr}`);
const canonical = value => Array.isArray(value) ? value.map(canonical)
  : value && typeof value === 'object'
    ? Object.fromEntries(Object.entries(value).sort(([a], [b]) => a.localeCompare(b, 'en')).map(([key, entry]) => [key, canonical(entry)]))
    : value;
const output = JSON.stringify(canonical({
  schema_version: 1,
  input_sha256: createHash('sha256').update(input).digest('hex'),
  input: fixtureInput,
  cases: JSON.parse(result.stdout),
}), null, 2) + '\n';
if (check) {
  if (readFileSync(outputPath, 'utf8').replaceAll('\r\n', '\n') !== output) {
    throw new Error('Browser pricing fixtures are stale. Run npm run mock:pricing:generate and review the changes.');
  }
  console.log(`Rust browser pricing fixtures match (${Object.keys(cases).length} synthetic cases).`);
} else {
  writeFileSync(outputPath, output);
  console.log(`Generated ${Object.keys(cases).length} Rust browser pricing fixtures.`);
}
