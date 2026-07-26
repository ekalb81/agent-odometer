import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const COVERAGE_SLICE = [
  'src/components/SessionGridControls.svelte',
  'src/lib/sessionGrid.ts',
  'src/lib/stores/sessionGrid.svelte.ts',
];
export const MINIMUM_LINE_COVERAGE = 90;

function normalizedAbsolute(filePath, rootDir) {
  const slashed = filePath.replaceAll('\\', '/');
  if (/^[A-Za-z]:\//.test(slashed)) return slashed.toLowerCase();
  return path.resolve(rootDir, slashed).replaceAll('\\', '/');
}

function executableLinesFor(entry, sourceLineCount, label) {
  if (!entry || typeof entry !== 'object' || !entry.statementMap || !entry.s) {
    throw new Error(`Coverage entry for ${label} is missing statement data`);
  }
  const lines = new Map();
  for (const [statementId, location] of Object.entries(entry.statementMap)) {
    const line = location?.start?.line;
    const count = entry.s[statementId];
    if (!Number.isInteger(line) || line < 1 || line > sourceLineCount || !Number.isFinite(count)) {
      throw new Error(`Coverage entry for ${label} contains invalid statement ${statementId}`);
    }
    lines.set(line, (lines.get(line) ?? false) || count > 0);
  }
  if (lines.size === 0) throw new Error(`Coverage entry for ${label} contains no executable lines`);
  return lines;
}

export function validateFrontendCoverage({
  report,
  rootDir,
  files = COVERAGE_SLICE,
  minimum = MINIMUM_LINE_COVERAGE,
}) {
  const reportByPath = new Map(
    Object.entries(report).map(([filePath, entry]) => [normalizedAbsolute(filePath, rootDir), entry]),
  );
  let covered = 0;
  let executable = 0;
  const details = [];

  for (const relativePath of files) {
    const absolutePath = path.resolve(rootDir, relativePath);
    if (!fs.existsSync(absolutePath) || !fs.statSync(absolutePath).isFile()) {
      throw new Error(`Coverage source is missing: ${relativePath}`);
    }
    const entry = reportByPath.get(normalizedAbsolute(absolutePath, rootDir));
    if (!entry) throw new Error(`Coverage report omitted required source: ${relativePath}`);
    const sourceLineCount = fs.readFileSync(absolutePath, 'utf8').split(/\r?\n/).length;
    const lines = executableLinesFor(entry, sourceLineCount, relativePath);
    const fileCovered = [...lines.values()].filter(Boolean).length;
    covered += fileCovered;
    executable += lines.size;
    details.push({ path: relativePath, covered: fileCovered, executable: lines.size });
  }

  if (executable === 0) throw new Error('Coverage slice contains no executable lines');
  const percent = (covered / executable) * 100;
  if (percent + Number.EPSILON < minimum) {
    throw new Error(`Frontend coverage ${percent.toFixed(2)}% is below ${minimum}% (${covered}/${executable} lines)`);
  }
  return { covered, executable, percent, details };
}

function main() {
  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const rootDir = path.resolve(scriptDir, '..');
  const reportPath = path.join(rootDir, 'coverage', 'frontend', 'coverage-final.json');
  if (!fs.existsSync(reportPath)) throw new Error(`Coverage report is missing: ${reportPath}`);
  const report = JSON.parse(fs.readFileSync(reportPath, 'utf8'));
  const result = validateFrontendCoverage({ report, rootDir });
  console.log(`Frontend coverage slice: ${result.percent.toFixed(2)}% (${result.covered}/${result.executable} executable lines)`);
  for (const detail of result.details) {
    console.log(`  ${detail.path}: ${detail.covered}/${detail.executable}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
