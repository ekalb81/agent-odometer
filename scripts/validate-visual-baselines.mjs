import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export function validateVisualBaselines({
  manifest = 'tests/visual/manifest.json',
  baselines = 'tests/visual/visual.spec.ts-snapshots',
} = {}) {
  const parsed = JSON.parse(fs.readFileSync(path.resolve(manifest), 'utf8'));
  const declared = Array.isArray(parsed.snapshots) ? parsed.snapshots.map(String) : [];
  const duplicateIds = [...new Set(declared.filter((id, index) => declared.indexOf(id) !== index))].sort();
  const baselineRoot = path.resolve(baselines);
  const present = fs.existsSync(baselineRoot)
    ? fs.readdirSync(baselineRoot, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith('.png'))
      .map((entry) => path.basename(entry.name, path.extname(entry.name)))
      .sort()
    : [];
  const declaredSet = new Set(declared);
  const presentSet = new Set(present);
  return {
    duplicateIds,
    missing: declared.filter((id) => !presentSet.has(id)).sort(),
    orphaned: present.filter((id) => !declaredSet.has(id)).sort(),
    declaredCount: declared.length,
    baselineCount: present.length,
  };
}

function main() {
  const result = validateVisualBaselines();
  const errors = [];
  if (result.duplicateIds.length) errors.push(`Duplicate manifest IDs: ${result.duplicateIds.join(', ')}`);
  if (result.missing.length) errors.push(`Missing baselines: ${result.missing.join(', ')}`);
  if (result.orphaned.length) errors.push(`Orphaned baselines: ${result.orphaned.join(', ')}`);
  if (errors.length) throw new Error(errors.join('\n'));
  console.log(`Visual baseline manifest valid: ${result.declaredCount} declared, ${result.baselineCount} present.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
