import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { assembleUpdaterManifest } from './assemble-updater-manifest.mjs';
import { releaseAssetNames, releaseBundleAssetNames } from './release-artifacts.mjs';
import { validateUpdaterManifest } from './validate-updater-manifest.mjs';

const VERSION = '0.6.4';
const TAG = `v${VERSION}`;
const REPOSITORY = 'example/odometer';
const SHA = '0123456789abcdef0123456789abcdef01234567';

function withArtifactDirectory(callback) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'odometer-updater-manifest-'));
  try {
    for (const name of releaseBundleAssetNames(VERSION)) {
      fs.writeFileSync(path.join(directory, name), name.endsWith('.sig') ? `signature:${name}` : 'bundle');
    }
    return callback(directory);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

function release() {
  return {
    tag_name: TAG,
    draft: true,
    immutable: false,
    target_commitish: SHA,
    body: 'Release notes',
    assets: releaseAssetNames(VERSION).map((name, index) => ({
      id: index + 1,
      name,
      size: 100,
      digest: `sha256:${String(index).padStart(64, '0')}`,
      browser_download_url: `https://github.com/${REPOSITORY}/releases/download/${TAG}/${name}`,
    })),
  };
}

test('assembles a complete public updater manifest after all platform bundles exist', () => {
  withArtifactDirectory((artifactRoot) => {
    const manifest = assembleUpdaterManifest({
      artifactRoot,
      release: release(),
      version: VERSION,
      repository: REPOSITORY,
      tag: TAG,
      now: new Date('2026-07-29T12:00:00.000Z'),
    });

    assert.doesNotThrow(() => validateUpdaterManifest(manifest, release(), VERSION, SHA));
    assert.equal(manifest.platforms['darwin-aarch64'].url, `https://github.com/${REPOSITORY}/releases/download/${TAG}/Odometer.app.tar.gz`);
    assert.equal(manifest.platforms['windows-x86_64-nsis'].signature, `signature:Odometer_${VERSION}_x64-setup.exe.sig`);
    assert.equal(manifest.platforms['linux-x86_64'].url, `https://github.com/${REPOSITORY}/releases/download/${TAG}/Odometer_${VERSION}_amd64.AppImage`);
  });
});

test('rejects a release whose tag does not match the intended updater version', () => {
  withArtifactDirectory((artifactRoot) => {
    const invalidRelease = { ...release(), tag_name: 'v0.0.0' };
    assert.throws(
      () => assembleUpdaterManifest({ artifactRoot, release: invalidRelease, version: VERSION, repository: REPOSITORY, tag: TAG }),
      /tag must equal/,
    );
  });
});
