import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { collectReleaseBundleAssets } from './release-artifacts.mjs';

export function publicReleaseDownloadUrl(repository, tag, assetName) {
  return `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(assetName)}`;
}

function releaseAssetMap(root, version) {
  return new Map(collectReleaseBundleAssets(root, version).map((filePath) => [path.basename(filePath), filePath]));
}

function platformEntry(assets, repository, tag, artifactName) {
  const artifactPath = assets.get(artifactName);
  const signaturePath = assets.get(`${artifactName}.sig`);
  if (!artifactPath || !signaturePath) {
    throw new Error(`Missing signed updater artifact for ${artifactName}`);
  }
  return {
    signature: fs.readFileSync(signaturePath, 'utf8'),
    url: publicReleaseDownloadUrl(repository, tag, artifactName),
  };
}

/**
 * Assemble the complete updater document after all signed platform bundles are
 * present. This single writer replaces Tauri Action's per-matrix latest.json
 * updates, which race when the platform builds run concurrently.
 */
export function assembleUpdaterManifest({ artifactRoot, release, version, repository, tag, now = new Date() }) {
  if (release?.tag_name !== tag) {
    throw new Error(`Release tag must equal ${tag}`);
  }
  if (typeof release.body !== 'string' || release.body.trim() === '') {
    throw new Error('Release body must be a non-empty string');
  }

  const assets = releaseAssetMap(artifactRoot, version);
  const macApp = 'Odometer.app.tar.gz';
  const appImage = `Odometer_${version}_amd64.AppImage`;
  const deb = `Odometer_${version}_amd64.deb`;
  const rpm = `Odometer-${version}-1.x86_64.rpm`;
  const msi = `Odometer_${version}_x64_en-US.msi`;
  const nsis = `Odometer_${version}_x64-setup.exe`;

  return {
    version,
    notes: release.body,
    pub_date: now.toISOString(),
    platforms: {
      'darwin-aarch64': platformEntry(assets, repository, tag, macApp),
      'darwin-aarch64-app': platformEntry(assets, repository, tag, macApp),
      'linux-x86_64': platformEntry(assets, repository, tag, appImage),
      'linux-x86_64-appimage': platformEntry(assets, repository, tag, appImage),
      'linux-x86_64-deb': platformEntry(assets, repository, tag, deb),
      'linux-x86_64-rpm': platformEntry(assets, repository, tag, rpm),
      'windows-x86_64': platformEntry(assets, repository, tag, msi),
      'windows-x86_64-msi': platformEntry(assets, repository, tag, msi),
      'windows-x86_64-nsis': platformEntry(assets, repository, tag, nsis),
    },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [artifactRoot, releasePath, version, repository, tag, outputPath] = process.argv.slice(2);
  if (!artifactRoot || !releasePath || !version || !repository || !tag || !outputPath) {
    console.error('Usage: node scripts/assemble-updater-manifest.mjs <artifact-dir> <release.json> <version> <repository> <tag> <output.json>');
    process.exit(2);
  }

  try {
    const release = JSON.parse(fs.readFileSync(releasePath, 'utf8'));
    const manifest = assembleUpdaterManifest({ artifactRoot, release, version, repository, tag });
    fs.writeFileSync(outputPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  } catch (error) {
    console.error(`Updater manifest assembly failed: ${error.message}`);
    process.exit(1);
  }
}
