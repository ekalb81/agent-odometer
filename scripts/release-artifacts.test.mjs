import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { collectReleaseBundleAssets, releaseBundleAssetNames } from './release-artifacts.mjs';

const VERSION = '0.6.4';

function withArtifactDirectory(callback) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'odometer-release-artifacts-'));
  try {
    return callback(directory);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

test('collects the exact signed release asset set in deterministic order', () => {
  withArtifactDirectory((directory) => {
    for (const [index, name] of releaseBundleAssetNames(VERSION).entries()) {
      const target = path.join(directory, index % 2 === 0 ? 'first' : 'second', name);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, name);
    }
    fs.writeFileSync(path.join(directory, 'first', 'updater-only.zip'), 'ignored');

    assert.deepEqual(
      collectReleaseBundleAssets(directory, VERSION).map((filePath) => path.basename(filePath)),
      releaseBundleAssetNames(VERSION),
    );
  });
});

test('rejects incomplete or ambiguous downloaded release artifacts', () => {
  withArtifactDirectory((directory) => {
    assert.throws(() => collectReleaseBundleAssets(directory, VERSION), /incomplete/);

    const name = releaseBundleAssetNames(VERSION)[0];
    fs.writeFileSync(path.join(directory, name), 'one');
    fs.mkdirSync(path.join(directory, 'duplicate'));
    fs.writeFileSync(path.join(directory, 'duplicate', name), 'two');
    assert.throws(() => collectReleaseBundleAssets(directory, VERSION), /more than once/);
  });
});
