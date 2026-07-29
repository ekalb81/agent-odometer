import test from 'node:test';
import assert from 'node:assert/strict';
import {
  changedPathsForEvent,
  eventHasVisualImpact,
  hasVisualImpact,
  isVisualImpactPath,
  normalizeChangedPath,
  visualImpactPaths,
} from './visual-impact.mjs';

test('normalizes git paths from Windows and POSIX output', () => {
  assert.equal(normalizeChangedPath('./src\\App.svelte'), 'src/App.svelte');
});

test('source implementation and visual harness files trigger visual checks', () => {
  assert.equal(isVisualImpactPath('src/App.svelte'), true);
  assert.equal(isVisualImpactPath('src/components/App.test.ts'), false);
  assert.equal(isVisualImpactPath('tests/visual/visual.spec.ts'), true);
  assert.equal(isVisualImpactPath('playwright.config.ts'), true);
  assert.equal(isVisualImpactPath('src-tauri/rates.json'), true);
  assert.equal(isVisualImpactPath('src-tauri/tauri.conf.json'), true);
  assert.equal(isVisualImpactPath('.env.visual'), true);
  assert.equal(isVisualImpactPath('.env.production'), true);
  assert.equal(isVisualImpactPath('src-tauri/src/commands.rs'), true);
  assert.equal(isVisualImpactPath('docs/ARCHITECTURE.md'), false);
});

test('impact paths are unique and sorted', () => {
  assert.deepEqual(visualImpactPaths(['src/App.svelte', './src\\App.svelte', 'README.md']), ['src/App.svelte']);
  assert.equal(hasVisualImpact(['src/components/foo.test.ts']), false);
});

test('pull request diff uses base and head SHAs', () => {
  let range;
  const paths = changedPathsForEvent({
    eventName: 'pull_request',
    event: { pull_request: { base: { sha: 'base' }, head: { sha: 'head' } } },
    git: (value) => {
      range = value;
      return ['src/App.svelte'];
    },
  });
  assert.equal(range, 'base...head');
  assert.deepEqual(paths, ['src/App.svelte']);
});

test('push diff uses before and after SHAs', () => {
  let range;
  changedPathsForEvent({
    eventName: 'push',
    event: { before: 'before', after: 'after' },
    git: (value) => {
      range = value;
      return [];
    },
  });
  assert.equal(range, 'before..after');
});

test('initial push uses root-aware changed paths', () => {
  let commit;
  const paths = changedPathsForEvent({
    eventName: 'push',
    event: { before: '0000000000000000000000000000000000000000', after: 'after' },
    git: () => assert.fail('regular diff must not be used for an initial push'),
    gitRoot: (value) => {
      commit = value;
      return ['src/App.svelte'];
    },
  });
  assert.equal(commit, 'after');
  assert.deepEqual(paths, ['src/App.svelte']);
});

test('git failures fail closed instead of reporting an empty diff', () => {
  assert.throws(() => changedPathsForEvent({
    eventName: 'push',
    event: { before: 'before', after: 'after' },
    git: () => { throw new Error('bad revision'); },
  }), /bad revision/);
});

test('manual workflow dispatch is always visual-impacting without a diff', () => {
  let called = false;
  assert.deepEqual(changedPathsForEvent({ eventName: 'workflow_dispatch', git: () => { called = true; return []; } }), []);
  assert.equal(called, false);
  assert.equal(hasVisualImpact([]), false);
  assert.equal(eventHasVisualImpact('workflow_dispatch', []), true);
  assert.equal(eventHasVisualImpact('push', []), false);
});
