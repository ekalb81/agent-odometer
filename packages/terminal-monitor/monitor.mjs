#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { emitKeypressEvents } from 'node:readline';
import { cpus, freemem, totalmem } from 'node:os';
import { pathToFileURL } from 'node:url';

const REPORTS = ['status', 'statusline', 'sessions', 'quota'];
const TABS = ['Usage', 'Sessions', 'Quota'];
const HELP = `Odometer terminal monitor (local read-only CLI)
node packages/terminal-monitor/monitor.mjs [--bin PATH] [--interval 5..300] [--host] [--once]
Keys: q/Esc/Ctrl-C exit; 1/2/3 or Tab change view; arrows/j/k navigate;
r refresh; Space pause CLI refresh; h toggle opt-in local Host OS sampling.
Requires Node22+ and a matching agent-odometer executable; no desktop needed.
--host explicitly enables CPU and host memory samples (off by default).
--once prints one plain snapshot and exits; no timers or background workers.
`;
export function options(args) {
  const out = { binary: process.platform === 'win32' ? 'agent-odometer.exe' : 'agent-odometer', interval: 30, host: false, once: false, help: false };
  const seen = new Set();
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (seen.has(key)) throw new Error('Duplicate option');
    seen.add(key);
    if (key === '--help' || key === '-h') out.help = true;
    else if (key === '--host') out.host = true;
    else if (key === '--once') out.once = true;
    else if (key === '--bin' || key === '--interval') {
      const value = args[++index];
      if (!value || value.startsWith('--')) throw new Error('Option requires a value');
      if (key === '--bin') { if (value.includes('\0')) throw new Error('Invalid executable'); out.binary = value; }
      else { out.interval = Number(value); if (!Number.isInteger(out.interval) || out.interval < 5 || out.interval > 300) throw new Error('Interval must be 5..300 seconds'); }
    } else throw new Error('Unsupported option');
  }
  return out;
}
// Only our renderer emits terminal controls. Source labels cannot emit ANSI/OSC or bidi controls.
export const clean = value => (['string', 'number', 'boolean'].includes(typeof value) ? String(value) : 'unavailable').replace(/[^\x20-\x7e]/g, '?');
const count = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString('en-US') : 'unavailable';
const amount = value => typeof value === 'number' && Number.isFinite(value) ? value.toFixed(2) : 'unavailable';
const age = (stamp, now) => stamp === null || stamp === undefined ? 'unavailable' : stamp > now ? 'clock skew' : `${Math.floor((now - stamp) / 1000)}s`;
const object = value => value && typeof value === 'object' && !Array.isArray(value);
export function reportData(document, report) {
  if (!object(document) || document.schema_version !== 2 || document.report !== report) throw new Error('Unsupported CLI report schema');
  const data = document.data;
  if (report === 'quota') { if (!Array.isArray(data) || data.length > 16 || !data.every(row => object(row) && Array.isArray(row.windows) && row.windows.length <= 32 && row.windows.every(object))) throw new Error('Invalid quota report'); }
  else {
    if (!object(data) || data.schema_version !== 1) throw new Error('Unsupported report data schema');
    if (report === 'status' && typeof data.ledger_available !== 'boolean') throw new Error('Invalid ledger status');
    if (report === 'statusline' && (!object(data.tokens) || !Array.isArray(data.providers) || data.providers.length > 16 || !data.providers.every(object))) throw new Error('Invalid usage report');
    if (report === 'sessions' && (!Array.isArray(data.sessions) || data.sessions.length > 25 || !data.sessions.every(object))) throw new Error('Invalid session report');
  }
  return data;
}
export function readReport(binary, report, signal, { spawnFn = spawn, timeout = 12_000, maxBytes = 1024 * 1024 } = {}) {
  if (!REPORTS.includes(report)) return Promise.reject(new Error('Unsupported report'));
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(new Error('Read cancelled')); return; }
    const args = [report, '--format', 'json', '--schema-version', '2'];
    if (report === 'sessions') args.push('--limit', '25');
    const child = spawnFn(binary, args, { shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let failure = null; let stdout = []; let bytes = 0; let stderrBytes = 0; let killTimer = null;
    const fail = message => { failure ??= message; child.kill(); killTimer ??= setTimeout(() => child.kill('SIGKILL'), 250); };
    const cancel = () => fail('Read cancelled');
    const timer = setTimeout(() => fail('CLI read timed out'), timeout);
    signal.addEventListener('abort', cancel, { once: true });
    child.stdout.on('data', chunk => { bytes += chunk.length; if (bytes > maxBytes) fail('CLI output exceeded monitor limit'); else stdout.push(chunk); });
    child.stderr.on('data', chunk => { stderrBytes += chunk.length; if (stderrBytes > 8192) fail('CLI error output exceeded monitor limit'); });
    child.on('error', error => { failure = error.code === 'ENOENT' ? 'Backend executable missing' : 'Backend could not start'; });
    child.on('close', code => {
      clearTimeout(timer); clearTimeout(killTimer); signal.removeEventListener('abort', cancel);
      if (failure || code !== 0) { reject(new Error(failure ?? 'CLI refresh failed; matching local ledger may be unavailable')); return; }
      try { resolve(reportData(JSON.parse(Buffer.concat(stdout).toString('utf8')), report)); }
      catch { reject(new Error('CLI response invalid or incompatible')); }
      stdout = [];
    });
  });
}

export function hostSample(previous, observedAt = Date.now(), readings = { cpus: cpus(), total: totalmem(), free: freemem() }) {
  let total = 0; let idle = 0; let valid = Array.isArray(readings.cpus) && readings.cpus.length > 0 && readings.cpus.length <= 4096;
  if (valid) for (const cpu of readings.cpus) for (const key of ['user', 'nice', 'sys', 'idle', 'irq']) {
    const value = cpu?.times?.[key];
    if (!Number.isSafeInteger(value) || value < 0) valid = false;
    else { total += value; if (key === 'idle') idle += value; }
  }
  valid &&= Number.isSafeInteger(total) && Number.isSafeInteger(idle);
  const cpuCount = readings.cpus?.length ?? 0;
  const baseline = valid ? { total, idle, cpuCount, observedAt } : null;
  const delta = previous && valid && previous.cpuCount === cpuCount && observedAt > previous.observedAt ? total - previous.total : 0;
  const idleDelta = previous ? idle - previous.idle : 0;
  const cpuPercent = delta > 0 && idleDelta >= 0 && idleDelta <= delta ? 100 * (delta - idleDelta) / delta : null;
  const memoryValid = Number.isSafeInteger(readings.total) && readings.total > 0 && Number.isSafeInteger(readings.free) && readings.free >= 0 && readings.free <= readings.total;
  return { baseline, observedAt, intervalMs: delta > 0 ? observedAt - previous.observedAt : null, cpuPercent, cpuReason: valid ? 'requires two valid OS samples; changed counters reset the baseline' : 'CPU counters unavailable',
    usedBytes: memoryValid ? readings.total - readings.free : null, totalBytes: memoryValid ? readings.total : null };
}

export function render(state, columns = 80, rows = 24, now = Date.now()) {
  const width = Math.max(1, Math.min(240, columns || 80)); const height = Math.max(3, Math.min(100, rows || 24));
  const status = state.reports.status?.data;
  const ledger = state.reports.status?.error ? `unavailable (previous ${status ? status.ledger_available ? 'available' : 'unavailable' : 'unknown'})` : status ? status.ledger_available ? 'available' : 'unavailable' : 'unknown';
  const lines = ['Odometer local terminal monitor', `${TABS.map((name, index) => `${index + 1}:${name}${state.tab === index ? '*' : ''}`).join('  ')}  ${state.paused ? 'PAUSED' : state.pending ? 'Refreshing' : 'Local reads'}`,
    `Ledger: ${ledger} | coverage: ${state.reports.status?.error ? 'stale/unverified' : status?.coverage_complete === true ? 'complete' : status?.coverage_complete === false ? 'partial' : 'unverified'}`];
  const name = ['statusline', 'sessions', 'quota'][state.tab]; const entry = state.reports[name]; const data = entry?.data;
  lines.push(`Source: local read-only CLI ${name}; fetched age ${age(entry?.fetchedAt, now)}`);
  if (entry?.error) lines.push(`${entry.data ? 'STALE previous read' : 'UNAVAILABLE'}: ${entry.error}`);
  if (state.reports.status?.error) lines.push(`Ledger refresh unavailable: ${state.reports.status.error}`);
  if (!data) lines.push(state.pending ? 'Waiting for bounded local report...' : 'No usable report; r retries. No zero is inferred.');
  else if (state.tab === 0) {
    lines.push(`CLI day window: ${count(data.tokens?.total_tokens)} tokens`, `Exact CLI UTC bounds: ${clean(data.from)} to ${clean(data.to)}`, `Pricing: ${clean(data.cost_surface)}; ${data.pricing_complete === true ? 'complete' : 'partial/unavailable'}`);
    for (const provider of data.providers) lines.push(`${clean(provider.harness)}: ${count(provider.tokens?.total_tokens)} tokens | ${amount(provider.pricing?.plan?.total)} ${clean(provider.currency)}${provider.pricing_complete === true ? '' : ' (partial/unpriced)'}`);
    lines.push('Accounting comes from Rust; source scan/activity freshness is unknown.');
  } else if (state.tab === 1) {
    lines.push(`Cumulative rows ordered by tokens; ${data.truncated_to === null ? 'complete listing' : `limited to ${clean(data.truncated_to)}`}`, 'Execution: UNKNOWN; CLI rows have no running/completion signal.', 'Activity observation age: unavailable; fetch time is not activity.');
    if (!data.sessions.length) lines.push('No sessions in this local ledger.');
    const capacity = Math.max(1, Math.floor((height - lines.length - 7) / 2)); const start = Math.max(0, Math.min(state.selected - capacity + 1, data.sessions.length - capacity));
    for (let index = start; index < Math.min(data.sessions.length, start + capacity); index++) {
      const row = data.sessions[index]; lines.push(`${index === state.selected ? '>' : ' '} ${index + 1}. ${clean(row.harness)} ${clean(row.session_key)}`, `   ${count(row.tokens?.total_tokens)} tokens | ${amount(row.cost)} ${clean(row.currency)}${row.unpriced_models?.length ? ' partial/unpriced' : ''} | ${clean(row.lifecycle)}/${clean(row.source_availability)}`);
    }
  } else {
    const quotaLines = [];
    if (!data.length) quotaLines.push('No recorded quota snapshots.');
    if (data.length > 8) quotaLines.push(`${data.length - 8} provider snapshots omitted; CLI quota report has the full output.`);
    for (const snapshot of data.slice(0, 8)) {
      quotaLines.push(`${clean(snapshot.provider)}: ${clean(snapshot.provenance)}${snapshot.unavailable ? ` unavailable:${clean(snapshot.unavailable)}` : ''}`);
      if (!Array.isArray(snapshot.windows)) { quotaLines.push('Quota windows unavailable'); continue; }
      if (snapshot.windows.length > 8) quotaLines.push(`${snapshot.windows.length - 8} quota windows omitted; CLI quota report has the full output.`);
      for (const window of snapshot.windows.slice(0, 8)) {
        const observed = Date.parse(clean(window.observed_at)); const reset = Date.parse(clean(window.resets_at));
        const stale = window.stale || !Number.isFinite(observed) || observed > now || (Number.isFinite(reset) && reset <= now);
        quotaLines.push(` ${clean(window.kind)}: ${window.unavailable ? `unavailable:${clean(window.unavailable)}` : window.unlimited === true ? 'Unlimited' : `recorded ${amount(window.remaining)} ${clean(window.unit)} remaining`}${stale ? ' STALE' : ''}`,
          ` observed ${clean(window.observed_at)}; age ${age(Number.isFinite(observed) ? observed : null, now)}; reset ${clean(window.resets_at ?? 'unrecorded')}`);
      }
    }
    quotaLines.push('Stored observations only; no credential reuse or provider polling.');
    const capacity = Math.max(1, height - lines.length - 6); const start = Math.min(state.quotaOffset ?? 0, Math.max(0, quotaLines.length - capacity));
    lines.push(`Quota lines ${start + 1}-${Math.min(start + capacity, quotaLines.length)}/${quotaLines.length}; arrows scroll.`, ...quotaLines.slice(start, start + capacity));
  }
  const host = state.host;
  const footer = [host.enabled ? `Host OS opt-in: CPU ${host.sample?.cpuPercent === null || !host.sample ? 'unavailable' : `${amount(host.sample.cpuPercent)}%`}; age ${age(host.sample?.observedAt, now)}; 2s target; window ${host.sample?.intervalMs ?? '?'}ms` : 'Host OS: disabled; h enables local CPU and host memory samples.',
    host.enabled ? `Host memory total-free: ${host.sample?.usedBytes === null || !host.sample ? 'unavailable' : `${amount(host.sample.usedBytes / 1048576)}/${amount(host.sample.totalBytes / 1048576)} MiB`}; separate from app/model memory.` : 'No host metric is inferred from tokens, model memory, or app RSS.',
    'q exit | r refresh | Space pause | h host | 1/2/3 view | arrows/j/k'];
  if (host.enabled && host.sample?.cpuPercent === null) footer.splice(1, 0, `CPU unavailable: ${host.sample.cpuReason}`);
  return [...lines.slice(0, Math.max(0, height - footer.length)), ...footer.slice(-height)].map(line => clean(line).slice(0, width)).join('\n');
}

export function onceOutput(state, columns = 100, rows = 30) {
  const failures = REPORTS.filter(name => state.reports[name]?.error);
  return render(state, columns, rows) + '\n' + failures.map(name => `Report ${name}: unavailable (local read failed)\n`).join('');
}
export async function run(config, input = process.stdin, output = process.stdout) {
  if (!config.once && (!input.isTTY || !output.isTTY)) throw new Error('Interactive mode requires a terminal; use --once');
  const state = { tab: 0, selected: 0, quotaOffset: 0, paused: false, pending: false, reports: {}, host: { enabled: false, sample: null } };
  let closed = false; let failed = false; let terminalStarted = false; let active = null; let refreshTimer = null; let hostTimer = null; let clockTimer = null; let lastStart = 0; let generation = 0; let lastOutput = '';
  const unexpected = () => { failed = true; void finish(); };
  const draw = () => {
    if (closed || config.once) return;
    try { const text = render(state, output.columns, output.rows); if (text !== lastOutput) { lastOutput = text; output.write(`\x1b[H\x1b[2J${text.replaceAll('\n', '\r\n')}`); } }
    catch { unexpected(); }
  };
  const startRefresh = () => { void refresh().catch(unexpected); };
  const toggleHost = () => {
    state.host.enabled = !state.host.enabled; clearInterval(hostTimer); hostTimer = null; state.host.sample = null;
    if (state.host.enabled) {
      const sample = () => { try { state.host.sample = hostSample(state.host.sample?.baseline); } catch { state.host.sample = { observedAt: Date.now(), cpuPercent: null, cpuReason: 'OS telemetry unavailable', usedBytes: null, totalBytes: null }; } draw(); };
      sample(); if (!config.once && !closed) hostTimer = setInterval(sample, 2000);
    }
    draw();
  };
  const refresh = async () => {
    if (closed || state.paused || active) return;
    const request = ++generation; const controller = new AbortController(); lastStart = Date.now(); state.pending = true; draw();
    if (closed) return;
    active = (async () => {
      // ponytail: two local CLI readers maximum; retain the existing report contracts.
      for (const batch of [REPORTS.slice(0, 2), REPORTS.slice(2)]) {
        if (controller.signal.aborted) break;
        await Promise.all(batch.map(async name => {
          try { const data = await readReport(config.binary, name, controller.signal); if (!closed && request === generation) { state.reports[name] = { data, fetchedAt: Date.now(), error: null }; if (name === 'sessions') state.selected = Math.min(state.selected, Math.max(0, data.sessions.length - 1)); } }
          catch (error) { if (!closed && request === generation) state.reports[name] = { ...state.reports[name], error: error.message }; }
          draw();
        }));
      }
    })();
    active.abort = () => controller.abort();
    try { await active; } finally { active = null; if (!closed && request === generation) { state.pending = false; draw(); } }
    if (!closed && !state.paused && !config.once) { clearTimeout(refreshTimer); refreshTimer = setTimeout(startRefresh, request === generation ? config.interval * 1000 : Math.max(0, 5000 - (Date.now() - lastStart))); }
  };
  let resolveExit; const exited = new Promise(resolve => { resolveExit = resolve; });
  const finish = async () => {
    if (closed) return; closed = true; generation++; clearTimeout(refreshTimer); clearInterval(hostTimer); clearInterval(clockTimer); active?.abort();
    input.off('keypress', key); input.off('end', finish); output.off('resize', draw); process.off('SIGINT', finish); process.off('SIGTERM', finish);
    if (terminalStarted) {
      try { input.setRawMode(false); } catch { failed = true; }
      input.pause();
      try { output.write('\x1b[?25h\x1b[?1049l'); } catch { failed = true; }
    }
    try { await active; } catch { failed = true; } finally { resolveExit(); }
  };
  const key = (_text, pressed = {}) => {
    if (pressed.name === 'q' || pressed.name === 'escape' || (pressed.ctrl && ['c', 'd'].includes(pressed.name))) { void finish(); return; }
    if (pressed.ctrl || pressed.meta) return;
    if (pressed.name === 'h') toggleHost();
    else if (pressed.name === 'space') { state.paused = !state.paused; clearTimeout(refreshTimer); if (state.paused) { generation++; active?.abort(); state.pending = false; } else startRefresh(); }
    else if (pressed.name === 'r' && !state.paused && !active) { clearTimeout(refreshTimer); refreshTimer = setTimeout(startRefresh, Math.max(0, 5000 - (Date.now() - lastStart))); }
    else if (['1', '2', '3'].includes(pressed.name)) state.tab = Number(pressed.name) - 1;
    else if (pressed.name === 'tab' || pressed.name === 'right') state.tab = (state.tab + 1) % 3;
    else if (pressed.name === 'left') state.tab = (state.tab + 2) % 3;
    else if (['down', 'j', 'up', 'k', 'pagedown', 'pageup'].includes(pressed.name)) { const move = ['down', 'j'].includes(pressed.name) ? 1 : ['up', 'k'].includes(pressed.name) ? -1 : pressed.name === 'pagedown' ? 5 : -5; if (state.tab === 2) state.quotaOffset = Math.max(0, Math.min(512, state.quotaOffset + move)); else state.selected = Math.max(0, Math.min((state.reports.sessions?.data?.sessions.length ?? 1) - 1, state.selected + move)); }
    draw();
  };
  try {
    if (config.once) {
      if (config.host) toggleHost(); await refresh();
      const failures = REPORTS.filter(name => state.reports[name]?.error);
      output.write(onceOutput(state, output.columns ?? 100, output.rows ?? 30));
      return failures.length ? 2 : 0;
    }
    emitKeypressEvents(input); terminalStarted = true; input.setRawMode(true); input.resume(); input.on('keypress', key); input.on('end', finish); output.on('resize', draw); process.on('SIGINT', finish); process.on('SIGTERM', finish);
    output.write('\x1b[?1049h\x1b[?25l'); if (config.host) toggleHost(); if (!closed) clockTimer = setInterval(draw, 1000); draw(); startRefresh(); await exited;
    return failed ? 2 : 0;
  } finally { await finish(); }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { const config = options(process.argv.slice(2)); if (config.help) process.stdout.write(HELP); else process.exitCode = (await run(config)) ?? 0; }
  catch (error) { process.stderr.write(`Odometer monitor: ${clean(error.message)}\n`); process.exitCode = 2; }
}
