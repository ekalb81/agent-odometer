import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { collectReleaseBundleAssets, releaseBundleAssetNames } from './release-artifacts.mjs';

const VERSION = '0.6.4';

test('embedded pricing uses the existing version-bound signed app delivery channel', () => {
  const config = JSON.parse(fs.readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  assert.equal(config.bundle.createUpdaterArtifacts, true);
  assert.equal(config.plugins.updater.requireSignedVersion, true);
  assert.deepEqual(config.plugins.updater.endpoints, ['https://github.com/ekalb81/agent-odometer/releases/latest/download/latest.json']);
  assert.ok(config.plugins.updater.pubkey);
  const bundle = JSON.parse(fs.readFileSync(new URL('../src-tauri/rates.json', import.meta.url), 'utf8'));
  assert.ok(Number.isInteger(bundle.version) && bundle.version > 0);
  assert.ok(Object.keys(bundle.models).length > 0);
  assert.ok(Object.keys(bundle.api_models).length > 0);
});

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
    assert.deepEqual(releaseBundleAssetNames(VERSION).slice(0, 2), [
      'Odometer.app.tar.gz',
      'Odometer.app.tar.gz.sig',
    ]);

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
