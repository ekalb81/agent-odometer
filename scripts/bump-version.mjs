import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

/**
 * Every manifest that must agree on the release version. The release workflow's
 * preflight compares package.json, Cargo.toml, and tauri.conf.json and refuses
 * to build on a mismatch; the two lockfiles are not checked there but drift
 * silently into the built artifacts if they are left behind.
 */
export const VERSIONED_MANIFESTS = [
  'package.json',
  'package-lock.json',
  'src-tauri/Cargo.toml',
  'src-tauri/Cargo.lock',
  'src-tauri/tauri.conf.json',
];

const CRATE_NAME = 'agent-odometer';
const SEMVER = /^(\d+)\.(\d+)\.(\d+)$/;

export function parseVersion(value) {
  const match = SEMVER.exec(value);
  if (!match) {
    throw new Error(`Not a plain MAJOR.MINOR.PATCH version: ${value}`);
  }
  return match.slice(1, 4).map(Number);
}

function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) {
      return a[index] < b[index] ? -1 : 1;
    }
  }
  return 0;
}

/**
 * Resolve `spec` — either a release kind (`major`/`minor`/`patch`) or an
 * explicit version, with or without a leading `v` — against the current
 * version. Refuses anything that does not move the version forward, because a
 * repeated or lowered version silently reuses a tag that the repository's tag
 * ruleset will not let anyone delete.
 */
export function resolveNextVersion(current, spec) {
  const [major, minor, patch] = parseVersion(current);
  let next;
  if (spec === 'major') {
    next = `${major + 1}.0.0`;
  } else if (spec === 'minor') {
    next = `${major}.${minor + 1}.0`;
  } else if (spec === 'patch') {
    next = `${major}.${minor}.${patch + 1}`;
  } else {
    next = String(spec ?? '').replace(/^v/, '');
    parseVersion(next);
  }

  if (compareVersions(next, current) <= 0) {
    throw new Error(`Refusing to bump ${current} to ${next}: the version must increase`);
  }
  return next;
}

/**
 * Rewrite a JSON manifest through parse/serialize so the result stays valid
 * regardless of where the version keys sit. `mutate` receives the parsed value.
 * Bails out when the file is not npm-standard 2-space JSON, since re-emitting
 * it would produce an unrelated whitespace diff.
 */
function rewriteJson(text, filePath, mutate) {
  const parsed = JSON.parse(text);
  if (`${JSON.stringify(parsed, null, 2)}\n` !== text) {
    throw new Error(`${filePath} is not 2-space-indented JSON; bump it by hand`);
  }
  mutate(parsed);
  return `${JSON.stringify(parsed, null, 2)}\n`;
}

export function bumpPackageJson(text, version, filePath = 'package.json') {
  return rewriteJson(text, filePath, (manifest) => {
    manifest.version = version;
  });
}

export function bumpTauriConf(text, version, filePath = 'src-tauri/tauri.conf.json') {
  return rewriteJson(text, filePath, (config) => {
    config.version = version;
  });
}

/**
 * npm records the project version twice: once at the document root and once on
 * the root package entry. Both must move or `npm ci` reports the lockfile as
 * out of sync with package.json.
 */
export function bumpPackageLock(text, version, filePath = 'package-lock.json') {
  return rewriteJson(text, filePath, (lock) => {
    lock.version = version;
    if (lock.packages?.['']) {
      lock.packages[''].version = version;
    }
  });
}

/** Rewrite the `version` key of Cargo.toml's `[package]` table only. */
export function bumpCargoToml(text, version) {
  let inPackage = false;
  let replaced = false;
  const lines = text.split('\n').map((line) => {
    const section = /^\s*\[([^\]]+)\]/.exec(line);
    if (section) {
      inPackage = section[1].trim() === 'package';
      return line;
    }
    if (inPackage && !replaced && /^\s*version\s*=/.test(line)) {
      replaced = true;
      return `version = "${version}"`;
    }
    return line;
  });

  if (!replaced) {
    throw new Error('Could not find a version key in the [package] table of Cargo.toml');
  }
  return lines.join('\n');
}

/** Rewrite the version of the workspace's own crate entry in Cargo.lock. */
export function bumpCargoLock(text, version) {
  let inCrate = false;
  let replaced = false;
  const lines = text.split('\n').map((line) => {
    if (line.trim() === '[[package]]') {
      inCrate = false;
      return line;
    }
    if (/^name\s*=/.test(line)) {
      inCrate = line === `name = "${CRATE_NAME}"`;
      return line;
    }
    if (inCrate && !replaced && /^version\s*=/.test(line)) {
      replaced = true;
      return `version = "${version}"`;
    }
    return line;
  });

  if (!replaced) {
    throw new Error(`Could not find the ${CRATE_NAME} package entry in Cargo.lock`);
  }
  return lines.join('\n');
}

export function readCurrentVersion(rootDir) {
  const manifest = JSON.parse(fs.readFileSync(path.join(rootDir, 'package.json'), 'utf8'));
  return manifest.version;
}

/** Read back every manifest so a partial bump can never be reported as done. */
export function readManifestVersions(rootDir) {
  const read = (relativePath) => fs.readFileSync(path.join(rootDir, relativePath), 'utf8');
  const cargoToml = /^\s*\[package\][\s\S]*?^\s*version\s*=\s*"([^"]+)"/m.exec(
    read('src-tauri/Cargo.toml'),
  );
  const cargoLock = new RegExp(
    `^name = "${CRATE_NAME}"\\nversion = "([^"]+)"`,
    'm',
  ).exec(read('src-tauri/Cargo.lock'));
  const lock = JSON.parse(read('package-lock.json'));

  return {
    'package.json': JSON.parse(read('package.json')).version,
    'package-lock.json': lock.version,
    'package-lock.json (root package)': lock.packages?.['']?.version,
    'src-tauri/Cargo.toml': cargoToml?.[1],
    'src-tauri/Cargo.lock': cargoLock?.[1],
    'src-tauri/tauri.conf.json': JSON.parse(read('src-tauri/tauri.conf.json')).version,
  };
}

export function bumpVersion(rootDir, spec) {
  const current = readCurrentVersion(rootDir);
  const version = resolveNextVersion(current, spec);

  const rewrites = [
    ['package.json', bumpPackageJson],
    ['package-lock.json', bumpPackageLock],
    ['src-tauri/Cargo.toml', bumpCargoToml],
    ['src-tauri/Cargo.lock', bumpCargoLock],
    ['src-tauri/tauri.conf.json', bumpTauriConf],
  ];

  // Rewrite everything in memory first so a failure part-way through cannot
  // leave the working tree in the mixed state that broke v0.8.3.
  const updates = rewrites.map(([relativePath, rewrite]) => {
    const filePath = path.join(rootDir, relativePath);
    return [filePath, rewrite(fs.readFileSync(filePath, 'utf8'), version, relativePath)];
  });
  for (const [filePath, contents] of updates) {
    fs.writeFileSync(filePath, contents);
  }

  const observed = readManifestVersions(rootDir);
  const mismatched = Object.entries(observed).filter(([, value]) => value !== version);
  if (mismatched.length > 0) {
    const detail = mismatched.map(([name, value]) => `${name}=${value ?? '<unreadable>'}`).join(' ');
    throw new Error(`Bump left manifests out of sync at ${version}: ${detail}`);
  }

  return { current, version };
}

function main(argv) {
  const spec = argv[0];
  if (!spec) {
    process.stderr.write(
      'Usage: npm run version:bump -- <major|minor|patch|X.Y.Z>\n' +
        '\nBumps every release manifest and verifies they agree:\n' +
        VERSIONED_MANIFESTS.map((name) => `  - ${name}\n`).join(''),
    );
    process.exitCode = 1;
    return;
  }

  const rootDir = process.cwd();
  const { current, version } = bumpVersion(rootDir, spec);
  process.stdout.write(
    `Bumped ${current} -> ${version} in:\n` +
      VERSIONED_MANIFESTS.map((name) => `  ${name}\n`).join('') +
      `\nNext: commit as "Release v${version}", merge to main, wait for CI to pass on the\n` +
      `merge commit, then tag v${version} on it.\n`,
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
