import fs from 'node:fs';
import { pathToFileURL } from 'node:url';

const REQUIRED_PLATFORMS = [
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

function requireObject(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be a JSON object`);
  }
}

function expectedAssetNames(version) {
  return [
    'latest.json',
    `Odometer_${version}_aarch64.app.tar.gz`,
    `Odometer_${version}_aarch64.app.tar.gz.sig`,
    `Odometer_${version}_aarch64.dmg`,
    `Odometer_${version}_amd64.AppImage`,
    `Odometer_${version}_amd64.AppImage.sig`,
    `Odometer_${version}_amd64.deb`,
    `Odometer_${version}_amd64.deb.sig`,
    `Odometer-${version}-1.x86_64.rpm`,
    `Odometer-${version}-1.x86_64.rpm.sig`,
    `Odometer_${version}_x64-setup.exe`,
    `Odometer_${version}_x64-setup.exe.sig`,
    `Odometer_${version}_x64_en-US.msi`,
    `Odometer_${version}_x64_en-US.msi.sig`,
  ];
}

function assetIdFromUpdaterUrl(url) {
  const match = url.match(/\/releases\/assets\/(\d+)(?:$|[?#])/);
  return match ? Number(match[1]) : null;
}

export function validateUpdaterManifest(manifest, release, expectedVersion, expectedSha) {
  requireObject(manifest, 'Updater manifest');
  requireObject(release, 'Release metadata');

  if (manifest.version !== expectedVersion) {
    throw new Error(`manifest.version must equal ${expectedVersion}`);
  }
  if (typeof manifest.notes !== 'string' || manifest.notes.trim() === '') {
    throw new Error('manifest.notes must be a non-empty string');
  }
  if (manifest.notes.includes('<!-- Fill in release notes before publishing. -->')) {
    throw new Error('manifest.notes must not contain the release-notes placeholder');
  }
  if (manifest.pub_date !== undefined && manifest.pub_date !== null) {
    if (typeof manifest.pub_date !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(manifest.pub_date)) {
      throw new Error('manifest.pub_date must be an RFC 3339 string or null');
    }
  }

  if (release.tag_name !== `v${expectedVersion}`) {
    throw new Error(`release.tag_name must equal v${expectedVersion}`);
  }
  if (release.draft !== true || release.immutable === true) {
    throw new Error('release must remain a mutable draft during validation');
  }
  if (release.target_commitish !== expectedSha) {
    throw new Error(`release.target_commitish must equal ${expectedSha}`);
  }
  if (!Array.isArray(release.assets)) {
    throw new Error('release.assets must be an array');
  }

  const expectedNames = expectedAssetNames(expectedVersion);
  const actualNames = release.assets.map((asset) => asset.name);
  const missing = expectedNames.filter((name) => !actualNames.includes(name));
  const unexpected = actualNames.filter((name) => !expectedNames.includes(name));
  if (missing.length || unexpected.length || new Set(actualNames).size !== actualNames.length) {
    throw new Error(`release asset set mismatch; missing=[${missing.join(', ')}] unexpected=[${unexpected.join(', ')}]`);
  }

  for (const asset of release.assets) {
    requireObject(asset, `Asset ${asset?.name ?? '<unknown>'}`);
    if (!Number.isInteger(asset.id) || typeof asset.browser_download_url !== 'string') {
      throw new Error(`asset ${asset.name} is missing its id or download URL`);
    }
    if (typeof asset.digest !== 'string' || !/^sha256:[0-9a-f]{64}$/.test(asset.digest)) {
      throw new Error(`asset ${asset.name} is missing a SHA-256 digest`);
    }
    if (!Number.isInteger(asset.size) || asset.size <= 0) {
      throw new Error(`asset ${asset.name} must be non-empty`);
    }
  }

  requireObject(manifest.platforms, 'manifest.platforms');
  const platformNames = Object.keys(manifest.platforms);
  const missingPlatforms = REQUIRED_PLATFORMS.filter((name) => !platformNames.includes(name));
  const unexpectedPlatforms = platformNames.filter((name) => !REQUIRED_PLATFORMS.includes(name));
  if (missingPlatforms.length || unexpectedPlatforms.length) {
    throw new Error(`updater platform set mismatch; missing=[${missingPlatforms.join(', ')}] unexpected=[${unexpectedPlatforms.join(', ')}]`);
  }

  const assetIds = new Set(release.assets.map((asset) => asset.id));
  const browserUrls = new Set(release.assets.map((asset) => asset.browser_download_url));
  for (const [platform, entry] of Object.entries(manifest.platforms)) {
    requireObject(entry, `manifest.platforms.${platform}`);
    if (typeof entry.signature !== 'string' || entry.signature.trim() === '') {
      throw new Error(`${platform}.signature must be a non-empty string`);
    }
    if (typeof entry.url !== 'string') {
      throw new Error(`${platform}.url must be a string`);
    }
    let parsed;
    try {
      parsed = new URL(entry.url);
    } catch {
      throw new Error(`${platform}.url must be an absolute URL`);
    }
    if (parsed.protocol !== 'https:') {
      throw new Error(`${platform}.url must use HTTPS`);
    }
    const assetId = assetIdFromUpdaterUrl(entry.url);
    if ((assetId === null || !assetIds.has(assetId)) && !browserUrls.has(entry.url)) {
      throw new Error(`${platform}.url does not reference an asset in this release`);
    }
  }
}

function readJson(path) {
  return JSON.parse(fs.readFileSync(path, 'utf8'));
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [manifestPath, releasePath, expectedVersion, expectedSha] = process.argv.slice(2);
  if (!manifestPath || !releasePath || !expectedVersion || !expectedSha) {
    console.error('Usage: node scripts/validate-updater-manifest.mjs <latest.json> <release.json> <version> <sha>');
    process.exit(2);
  }
  try {
    validateUpdaterManifest(readJson(manifestPath), readJson(releasePath), expectedVersion, expectedSha);
    console.log(`Validated v${expectedVersion} updater manifest and release assets.`);
  } catch (error) {
    console.error(`Updater manifest validation failed: ${error.message}`);
    process.exit(1);
  }
}
