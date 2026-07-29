import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const DOC_SCREENSHOT_MAP = {
  'primary-codex-light-desktop.png': 'codex-tab.png',
  'primary-claude-light-desktop.png': 'claude-code-tab.png',
  'session-selected-detail.png': 'session-details.png',
};

function parseArgs(argv) {
  const options = { source: 'output/playwright/current', output: 'docs/screenshots', mapping: { ...DOC_SCREENSHOT_MAP }, force: false };
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--source') options.source = argv[++index];
    else if (argv[index] === '--output') options.output = argv[++index];
    else if (argv[index] === '--force') options.force = true;
    else if (argv[index] === '--map') {
      const [source, destination] = String(argv[++index] ?? '').split('=', 2);
      if (!source || !destination) throw new Error('--map must be SOURCE=DESTINATION');
      options.mapping[source] = destination;
    }
  }
  return options;
}

function collectFiles(root, current = root) {
  if (!fs.existsSync(current)) return [];
  return fs.readdirSync(current, { withFileTypes: true }).flatMap((entry) => {
    const absolute = path.join(current, entry.name);
    if (entry.isDirectory()) return collectFiles(root, absolute);
    return entry.isFile() ? [{ absolute, name: entry.name, relative: path.relative(root, absolute) }] : [];
  });
}

function findSource(files, requestedName) {
  return files.find((file) => file.name === requestedName)
    ?? files.find((file) => file.name.endsWith(`-${requestedName}`));
}

/** Copy reviewed canonical screenshots into documentation paths. This is never run by CI. */
export function generateDocScreenshots({ source = 'output/playwright/current', output = 'docs/screenshots', mapping = DOC_SCREENSHOT_MAP, force = false } = {}) {
  const sourceRoot = path.resolve(source);
  const outputRoot = path.resolve(output);
  const files = fs.statSync(sourceRoot, { throwIfNoEntry: false })?.isFile()
    ? [{ absolute: sourceRoot, name: path.basename(sourceRoot), relative: path.basename(sourceRoot) }]
    : collectFiles(sourceRoot);
  fs.mkdirSync(outputRoot, { recursive: true });
  const copied = [];
  const missing = [];
  const existing = [];

  for (const [requestedName, destination] of Object.entries(mapping)) {
    const sourceFile = findSource(files, requestedName);
    if (!sourceFile) {
      missing.push(requestedName);
      continue;
    }
    const destinationPath = path.resolve(outputRoot, destination);
    if (!destinationPath.startsWith(`${outputRoot}${path.sep}`)) throw new Error(`Destination escapes output directory: ${destination}`);
    if (!force && fs.existsSync(destinationPath)) {
      existing.push(destination);
      continue;
    }
    fs.mkdirSync(path.dirname(destinationPath), { recursive: true });
    fs.copyFileSync(sourceFile.absolute, destinationPath);
    copied.push({ source: sourceFile.relative, destination });
  }

  return { copied, missing, existing, source: sourceRoot, output: outputRoot };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = generateDocScreenshots(parseArgs(process.argv.slice(2)));
    for (const item of result.copied) console.log(`Copied ${item.source} -> ${item.destination}`);
    if (result.missing.length) {
      console.error(`Missing canonical screenshot(s): ${result.missing.join(', ')}`);
      process.exitCode = 1;
    }
    if (result.existing.length) {
      console.error(`Documentation screenshot(s) already exist: ${result.existing.join(', ')}. Re-run with --force after reviewing them.`);
      process.exitCode = 1;
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
