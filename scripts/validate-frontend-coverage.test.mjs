import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { validateFrontendCoverage } from './validate-frontend-coverage.mjs';

function fixture() {
  const rootDir = fs.mkdtempSync(path.join(os.tmpdir(), 'odometer-coverage-'));
  const relativePath = 'src/example.ts';
  const absolutePath = path.join(rootDir, relativePath);
  fs.mkdirSync(path.dirname(absolutePath), { recursive: true });
  fs.writeFileSync(absolutePath, 'const covered = true;\nconst uncovered = false;\n');
  return { rootDir, relativePath, absolutePath };
}

function reportFor(filePath, secondCount = 0) {
  return {
    [filePath]: {
      statementMap: {
        0: { start: { line: 1, column: 0 }, end: { line: 1, column: 21 } },
        1: { start: { line: 2, column: 0 }, end: { line: 2, column: 24 } },
      },
      s: { 0: 1, 1: secondCount },
    },
  };
}

test('recomputes coverage from required checked-out source lines', (t) => {
  const item = fixture();
  t.after(() => fs.rmSync(item.rootDir, { recursive: true, force: true }));
  const result = validateFrontendCoverage({
    report: reportFor(item.absolutePath.replaceAll('\\', '/'), 1),
    rootDir: item.rootDir,
    files: [item.relativePath],
    minimum: 100,
  });
  assert.equal(result.percent, 100);
  assert.equal(result.executable, 2);
});

test('fails closed when a required source is absent from the report', (t) => {
  const item = fixture();
  t.after(() => fs.rmSync(item.rootDir, { recursive: true, force: true }));
  assert.throws(
    () => validateFrontendCoverage({ report: {}, rootDir: item.rootDir, files: [item.relativePath] }),
    /omitted required source/,
  );
});

test('fails closed when a required source file is missing', (t) => {
  const item = fixture();
  t.after(() => fs.rmSync(item.rootDir, { recursive: true, force: true }));
  assert.throws(
    () => validateFrontendCoverage({ report: reportFor(item.absolutePath), rootDir: item.rootDir, files: ['src/missing.ts'] }),
    /source is missing/,
  );
});

test('rejects invalid line mappings and coverage below the threshold', (t) => {
  const item = fixture();
  t.after(() => fs.rmSync(item.rootDir, { recursive: true, force: true }));
  const invalid = reportFor(item.absolutePath);
  invalid[item.absolutePath].statementMap[1].start.line = 99;
  assert.throws(
    () => validateFrontendCoverage({ report: invalid, rootDir: item.rootDir, files: [item.relativePath] }),
    /invalid statement/,
  );
  assert.throws(
    () => validateFrontendCoverage({ report: reportFor(item.absolutePath), rootDir: item.rootDir, files: [item.relativePath], minimum: 75 }),
    /below 75%/,
  );
});
