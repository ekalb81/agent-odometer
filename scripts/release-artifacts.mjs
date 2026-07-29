import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export function releaseAssetNames(version) {
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

export function releaseBundleAssetNames(version) {
  return releaseAssetNames(version).filter((name) => name !== 'latest.json');
}

function walkFiles(root) {
  const entries = fs.readdirSync(root, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const entryPath = path.join(root, entry.name);
    return entry.isDirectory() ? walkFiles(entryPath) : [entryPath];
  });
}

/**
 * Return exactly one path for each release asset. Downloaded workflow
 * artifacts can contain Tauri's non-release updater archives, so callers must
 * never upload the whole bundle directory wholesale.
 */
export function collectReleaseBundleAssets(root, version) {
  const wanted = new Set(releaseBundleAssetNames(version));
  const found = new Map();

  for (const filePath of walkFiles(root)) {
    const name = path.basename(filePath);
    if (!wanted.has(name)) continue;
    if (found.has(name)) {
      throw new Error(`Release artifact ${name} was found more than once`);
    }
    found.set(name, filePath);
  }

  const missing = [...wanted].filter((name) => !found.has(name));
  if (missing.length) {
    throw new Error(`Release artifact set is incomplete; missing=[${missing.join(', ')}]`);
  }

  return releaseBundleAssetNames(version).map((name) => found.get(name));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [root, version] = process.argv.slice(2);
  if (!root || !version) {
    console.error('Usage: node scripts/release-artifacts.mjs <artifact-dir> <version>');
    process.exit(2);
  }

  try {
    for (const assetPath of collectReleaseBundleAssets(root, version)) {
      console.log(assetPath);
    }
  } catch (error) {
    console.error(`Release artifact validation failed: ${error.message}`);
    process.exit(1);
  }
}
