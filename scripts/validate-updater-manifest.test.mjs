import assert from 'node:assert/strict';
import test from 'node:test';

import { rewriteUpdaterManifestUrls, validateUpdaterManifest } from './validate-updater-manifest.mjs';

const VERSION = '0.6.4';
const SHA = '0123456789abcdef0123456789abcdef01234567';

function fixture() {
  const names = [
    'latest.json',
    `Odometer_${VERSION}_aarch64.app.tar.gz`,
    `Odometer_${VERSION}_aarch64.app.tar.gz.sig`,
    `Odometer_${VERSION}_aarch64.dmg`,
    `Odometer_${VERSION}_amd64.AppImage`,
    `Odometer_${VERSION}_amd64.AppImage.sig`,
    `Odometer_${VERSION}_amd64.deb`,
    `Odometer_${VERSION}_amd64.deb.sig`,
    `Odometer-${VERSION}-1.x86_64.rpm`,
    `Odometer-${VERSION}-1.x86_64.rpm.sig`,
    `Odometer_${VERSION}_x64-setup.exe`,
    `Odometer_${VERSION}_x64-setup.exe.sig`,
    `Odometer_${VERSION}_x64_en-US.msi`,
    `Odometer_${VERSION}_x64_en-US.msi.sig`,
  ];
  const assets = names.map((name, index) => ({
    id: 1000 + index,
    name,
    size: 100,
    digest: `sha256:${String(index).padStart(64, '0')}`,
    browser_download_url: `https://github.com/example/app/releases/download/v${VERSION}/${name}`,
  }));
  const platformNames = [
    'darwin-aarch64',
    'darwin-aarch64-app',
    'linux-x86_64',
    'linux-x86_64-appimage',
    'linux-x86_64-deb',
    'linux-x86_64-rpm',
    'windows-x86_64',
    'windows-x86_64-msi',
    'windows-x86_64-nsis',
  ];
  const platforms = Object.fromEntries(platformNames.map((name, index) => [name, {
    signature: 'signed',
    url: assets[(index % (assets.length - 1)) + 1].browser_download_url,
  }]));
  return {
    manifest: { version: VERSION, notes: 'Release notes', pub_date: '2026-07-25T23:00:13.471Z', platforms },
    release: { tag_name: `v${VERSION}`, draft: true, immutable: false, target_commitish: SHA, assets },
  };
}

test('accepts a complete Tauri updater manifest', () => {
  const { manifest, release } = fixture();
  assert.doesNotThrow(() => validateUpdaterManifest(manifest, release, VERSION, SHA));
});

test('rewrites API asset URLs to public release downloads for every platform', () => {
  const { manifest, release } = fixture();
  for (const [index, entry] of Object.values(manifest.platforms).entries()) {
    entry.url = `https://api.github.com/repos/example/app/releases/assets/${release.assets[(index % (release.assets.length - 1)) + 1].id}`;
  }

  rewriteUpdaterManifestUrls(manifest, release);

  assert.doesNotThrow(() => validateUpdaterManifest(manifest, release, VERSION, SHA));
  for (const entry of Object.values(manifest.platforms)) {
    assert.match(entry.url, new RegExp(`^https://github\\.com/example/app/releases/download/v${VERSION}/`));
  }
});

test('rejects API asset URLs in a published manifest', () => {
  const { manifest, release } = fixture();
  manifest.platforms['windows-x86_64'].url = `https://api.github.com/repos/example/app/releases/assets/${release.assets[1].id}`;
  assert.throws(
    () => validateUpdaterManifest(manifest, release, VERSION, SHA),
    /must use a public GitHub release download URL/,
  );
});

test('rejects notes serialized as a sequence', () => {
  const { manifest, release } = fixture();
  manifest.notes = ['Release', 'notes'];
  assert.throws(
    () => validateUpdaterManifest(manifest, release, VERSION, SHA),
    /manifest\.notes must be a non-empty string/,
  );
});

test('rejects placeholder release notes', () => {
  const { manifest, release } = fixture();
  manifest.notes = '<!-- Fill in release notes before publishing. -->';
  assert.throws(
    () => validateUpdaterManifest(manifest, release, VERSION, SHA),
    /must not contain the release-notes placeholder/,
  );
});

test('rejects updater URLs outside the validated release', () => {
  const { manifest, release } = fixture();
  manifest.platforms['windows-x86_64'].url = 'https://example.com/unverified.msi';
  assert.throws(
    () => validateUpdaterManifest(manifest, release, VERSION, SHA),
    /must use a public GitHub release download URL/,
  );
});
