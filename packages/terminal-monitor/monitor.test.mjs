import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { EventEmitter } from 'node:events';
import { clean, options, readReport, reportData, hostSample, render, run, onceOutput } from './monitor.mjs';

test('rejects duplicate/invalid options and enables host collection only explicitly', () => {
  assert.equal(options([]).host, false);
  assert.equal(options(['--host']).host, true);
  assert.equal(options(['--bin', 'path with spaces']).binary, 'path with spaces');
  for (const args of [['--interval', '4'], ['--interval', '301'], ['--interval', 'NaN'], ['--bin'], ['--host', '--host'], ['--include-paths']]) assert.throws(() => options(args));
});
test('validates the selected CLI contract and rejects malformed rows before rendering', () => {
  assert.deepEqual(reportData({ schema_version: 2, report: 'quota', data: [] }, 'quota'), []);
  for (const value of [{ schema_version: 1 }, { schema_version: 2, report: 'quota', data: [null] }, { schema_version: 2, report: 'quota', data: [{ windows: [null] }] }]) assert.throws(() => reportData(value, 'quota'));
  assert.throws(() => reportData({ schema_version: 2, report: 'sessions', data: { schema_version: 1, sessions: Array(26).fill({}) } }, 'sessions'));
});
test('host CPU requires two comparable samples; changed/regressed counters never fabricate zero', () => {
  const cpu = (user, idle) => ({ times: { user, idle, sys: 0, nice: 0, irq: 0 } });
  const first = hostSample(null, 1000, { cpus: [cpu(100, 100)], total: 1024, free: 256 });
  assert.equal(first.cpuPercent, null); assert.equal(first.usedBytes, 768);
  const second = hostSample(first.baseline, 3000, { cpus: [cpu(150, 150)], total: 1024, free: 256 });
  assert.equal(second.cpuPercent, 50); assert.equal(second.intervalMs, 2000);
  assert.equal(hostSample(first.baseline, 3000, { cpus: [cpu(50, 50)], total: 1, free: 2 }).cpuPercent, null);
  assert.equal(hostSample(first.baseline, 3000, { cpus: [cpu(150, 150), cpu(150, 150)], total: 1, free: 0 }).cpuPercent, null);
  const absent = hostSample(null, 1000, { cpus: [], total: 0, free: 0 });
  assert.equal(absent.cpuPercent, null); assert.equal(absent.usedBytes, null);
});
test('terminal source text cannot inject controls and narrow rows stay bounded', () => {
  assert.equal(clean('\x1b]52;c;secret\x07\u202e'), '?]52;c;secret??');
  const state = { tab: 1, selected: 24, reports: { sessions: { fetchedAt: 1000, data: { sessions: Array.from({ length: 25 }, (_, index) => ({ harness: '\x1b[2J', session_key: `key${index}`, tokens: { total_tokens: index }, cost: null, currency: 'USD', lifecycle: 'retained', source_availability: 'missing' })), truncated_to: 25 } } }, host: { enabled: false } };
  const text = render(state, 80, 24, 2000);
  assert.match(text, /> 25\./); assert.match(text, /Execution: UNKNOWN/); assert.match(text, /Activity observation age: unavailable/);
  assert.ok(!text.includes('\x1b'));
  for (const line of render(state, 26, 12).split('\n')) assert.ok(line.length <= 26);
  assert.ok(render(state, 26, 12).split('\n').length <= 12);
});
test('quota preserves stale recorded values and keeps unavailable, unlimited, and zero distinct', () => {
  const window = { kind: 'burst', unit: 'percent', remaining: 0, observed_at: '2026-01-01T00:00:00Z', resets_at: '2026-01-01T00:01:00Z', stale: false };
  const state = { tab: 2, quotaOffset: 0, reports: { quota: { data: [{ provider: 'codex', provenance: 'transcript_derived', windows: [window, { ...window, unlimited: true }, { ...window, unavailable: 'no_observation', remaining: null }] }], fetchedAt: 0, error: 'refresh failed' } }, host: { enabled: false } };
  const text = render(state, 120, 30, Date.parse('2026-01-01T00:02:00Z'));
  assert.match(text, /recorded 0\.00 percent remaining STALE/); assert.match(text, /Unlimited/); assert.match(text, /unavailable:no_observation/); assert.match(text, /STALE previous read/);
});
test('malformed display primitives cannot throw and three-row host warmup retains quit hint', () => {
  const malformed = { toString: 'x' };
  const data = reportData({ schema_version: 2, report: 'statusline', data: { schema_version: 1, tokens: {}, providers: [{ harness: malformed, currency: malformed }], from: malformed, to: malformed, cost_surface: malformed } }, 'statusline');
  const state = { tab: 0, reports: { statusline: { data } }, host: { enabled: true, sample: { cpuPercent: null, cpuReason: 'warmup', usedBytes: null } } };
  assert.match(render(state, 100, 24), /bounds: unavailable to unavailable/);
  assert.match(render(state, 100, 3), /q exit/);
  state.tab = 2; state.reports.quota = { data: [{ provider: malformed, provenance: malformed, windows: [{ kind: malformed, unit: malformed, observed_at: malformed, resets_at: malformed, unavailable: malformed }] }] };
  assert.doesNotThrow(() => render(state));
  state.tab = 1; state.reports.sessions = { data: { truncated_to: malformed, sessions: [{ harness: malformed, session_key: malformed, currency: malformed, lifecycle: malformed, source_availability: malformed }] } }; state.selected = 0;
  assert.doesNotThrow(() => render(state));
});
test('one-shot prints bounded failures for hidden reports without exposing child errors', () => {
  const state = { tab: 0, reports: { statusline: { data: { tokens: { total_tokens: 123 }, providers: [], from: 'a', to: 'b' } }, sessions: { error: 'synthetic-private-error' }, quota: { error: 'private-quota' } }, host: { enabled: false } };
  const text = onceOutput(state);
  assert.match(text, /123 tokens/); assert.match(text, /Report sessions: unavailable/); assert.match(text, /Report quota: unavailable/);
  assert.ok(!text.includes('synthetic-private-error') && !text.includes('private-quota'));
});
test('unexpected renderer output failure restores terminal state and completes shutdown', async () => {
  const input = new EventEmitter(); input.isTTY = true; const modes = []; let paused = false;
  input.setRawMode = enabled => { modes.push(enabled); }; input.resume = () => {}; input.pause = () => { paused = true; };
  const output = new EventEmitter(); output.isTTY = true; output.columns = 80; output.rows = 24;
  const writes = []; output.write = text => { writes.push(text); if (writes.length === 2) throw new Error('synthetic renderer failure'); };
  assert.equal(await run(options(['--host']), input, output), 2);
  assert.deepEqual(modes, [true, false]); assert.ok(paused); assert.ok(writes.at(-1).includes('\x1b[?25h\x1b[?1049l'));
  assert.equal(input.listenerCount('keypress'), 0); assert.equal(output.listenerCount('resize'), 0);
});
test('CLI subprocess checks errors, output bounds and timeouts, and reaps aborted readers', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'odometer-monitor-test-'));
  const script = join(directory, 'fake.mjs');
  let child;
  const launch = (binary, args, config) => { assert.equal(config.shell, false); child = spawn(process.execPath, [script, ...args], config); return child; };
  const controller = new AbortController();
  try {
    await writeFile(script, `console.log(JSON.stringify({schema_version:2,report:process.argv[2],data:[]}));`);
    assert.deepEqual(await readReport('unused', 'quota', controller.signal, { spawnFn: launch }), []);
    await writeFile(script, `process.stderr.write('synthetic-private-error');process.exitCode=2;`);
    await assert.rejects(readReport('unused', 'quota', controller.signal, { spawnFn: launch }), /CLI refresh failed/);
    await writeFile(script, `process.stdout.write('x'.repeat(2048));`);
    await assert.rejects(readReport('unused', 'quota', controller.signal, { spawnFn: launch, maxBytes: 100 }), /exceeded monitor limit/);
    await writeFile(script, `process.on('SIGTERM',()=>{});setInterval(()=>{},1000);`);
    await assert.rejects(readReport('unused', 'quota', controller.signal, { spawnFn: launch, timeout: 200 }), /timed out/);
    assert.ok(child.exitCode !== null || child.signalCode !== null);
    const pending = readReport('unused', 'quota', controller.signal, { spawnFn: launch }); controller.abort();
    await assert.rejects(pending, /cancelled/); assert.ok(child.exitCode !== null || child.signalCode !== null);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
