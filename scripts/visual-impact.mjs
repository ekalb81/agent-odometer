import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const SOURCE_UNIT_TEST = /(?:^|\/)(?:tests?|__tests__|test-utils)(?:\/|$)|\.(?:test|spec)\.[^/]+$/i;
const FRONTEND_CONFIG = /^(?:vite|svelte|tailwind|postcss|playwright|vitest|eslint|prettier)\.config\.[^/]+$/i;

/** Return a repository-relative path in the format used by GitHub and Playwright. */
export function normalizeChangedPath(value) {
  return String(value ?? '')
    .trim()
    .replaceAll('\\', '/')
    .replace(/^\.\//, '')
    .replace(/^\/+/, '');
}

/** Whether a changed path can alter the rendered frontend or visual harness. */
export function isVisualImpactPath(value) {
  const changedPath = normalizeChangedPath(value);
  if (!changedPath) return false;

  if (changedPath === 'index.html' || changedPath === 'src-tauri/rates.json' || changedPath === 'src-tauri/tauri.conf.json') return true;
  if (/^\.env(?:\..+)?$/i.test(changedPath)) return true;
  if (/^package(?:-lock)?\.json$/i.test(changedPath)) return true;
  if (/^\.github\/workflows(?:\/|$)/i.test(changedPath)) return true;
  if (/^scripts\/(?:visual-impact|generate-visual-gallery|generate-doc-screenshots|validate-visual-baselines|assert-visual-baseline-platform)(?:\.|\/)/i.test(changedPath)) {
    return true;
  }
  if (/^tests\/visual(?:\/|$)/i.test(changedPath)) return true;
  if (/^playwright\.config\.[^/]+$/i.test(changedPath)) return true;
  if (/^(?:public|static)(?:\/|$)/i.test(changedPath)) return true;
  if (FRONTEND_CONFIG.test(changedPath)) return true;
  if (/^tsconfig(?:\.[^/]+)?\.json$/i.test(changedPath)) return true;
  if (/^src(?:\/|$)/i.test(changedPath)) {
    return !SOURCE_UNIT_TEST.test(changedPath);
  }

  return false;
}

export function visualImpactPaths(paths) {
  return [...new Set((paths ?? []).map(normalizeChangedPath).filter(isVisualImpactPath))].sort();
}

export function hasVisualImpact(paths) {
  return visualImpactPaths(paths).length > 0;
}

export function eventHasVisualImpact(eventName, paths) {
  return eventName === 'workflow_dispatch' || hasVisualImpact(paths);
}

function readEvent(eventPath) {
  if (!eventPath) return {};
  return JSON.parse(fs.readFileSync(eventPath, 'utf8'));
}

function gitChangedPaths(range) {
  return gitPaths(['diff', '--name-only', '--no-renames', range]);
}

function gitRootChangedPaths(commit) {
  return gitPaths(['diff-tree', '--root', '--no-commit-id', '--name-only', '-r', '--no-renames', commit]);
}

function gitPaths(args) {
  return execFileSync('git', args, {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  })
    .split(/\r?\n/)
    .map(normalizeChangedPath)
    .filter(Boolean);
}

/** Resolve changed files for a GitHub event. This function is intentionally injectable for tests. */
export function changedPathsForEvent({
  eventName = process.env.GITHUB_EVENT_NAME ?? '',
  event = readEvent(process.env.GITHUB_EVENT_PATH),
  git = gitChangedPaths,
  gitRoot = gitRootChangedPaths,
} = {}) {
  if (eventName === 'workflow_dispatch') return [];

  if (eventName === 'pull_request') {
    const base = event.pull_request?.base?.sha;
    const head = event.pull_request?.head?.sha ?? 'HEAD';
    if (base) return git(`${base}...${head}`);
  }

  if (eventName === 'push') {
    const before = event.before;
    const after = event.after ?? 'HEAD';
    if (before && !/^0+$/.test(before)) return git(`${before}..${after}`);
    return gitRoot(after);
  }

  return git('HEAD^..HEAD');
}

function writeOutputs(outputPath, outputs) {
  if (!outputPath) return;
  fs.appendFileSync(
    outputPath,
    Object.entries(outputs)
      .map(([key, value]) => `${key}=${String(value)}`)
      .join('\n') + '\n',
  );
}

function main() {
  const outputArg = process.argv.indexOf('--github-output');
  const outputPath = outputArg >= 0 ? process.argv[outputArg + 1] : process.env.GITHUB_OUTPUT;
  const eventName = process.env.GITHUB_EVENT_NAME ?? '';
  const changed = eventName === 'workflow_dispatch'
    ? []
    : changedPathsForEvent({ eventName });
  const impacted = eventHasVisualImpact(eventName, changed);
  const impactedPaths = visualImpactPaths(changed);
  writeOutputs(outputPath, {
    impacted,
    changed_count: changed.length,
    impacted_count: impactedPaths.length,
  });
  console.log(`Visual impact: ${impacted ? 'yes' : 'no'} (${eventName || 'local'}, ${changed.length} changed path(s))`);
  if (impactedPaths.length) console.log(impactedPaths.join('\n'));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
