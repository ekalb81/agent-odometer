const { execFile } = require('node:child_process');
const path = require('node:path');
const os = require('node:os');

const MAX_BYTES = 8 * 1024 * 1024;
const REPORTS = new Set(['integration-status', 'statusline']);
class LocalError extends Error {
  constructor(code) { super(code); this.code = code; }
}
function executablePath(value) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || value.includes('\0') || /^(?:\\\\|\/\/)/.test(value) || (process.platform === 'win32' && !/^[A-Za-z]:[\\/]/.test(value)) || path.basename(value).toLowerCase() !== (process.platform === 'win32' ? 'agent-odometer.exe' : 'agent-odometer')) throw new LocalError('setup');
  return value;
}
function validSurface(surface) {
  return surface && (surface.total === null || typeof surface.total === 'number' && Number.isFinite(surface.total) && surface.total >= 0)
    && Array.isArray(surface.missing_models) && Array.isArray(surface.unpriced_models) && Array.isArray(surface.by_model)
    && surface.by_model.length <= 50000 && surface.by_model.every(model => model && typeof model.basis === 'string');
}
function validPricing(pricing) {
  if (pricing == null) return true;
  if (typeof pricing !== 'object') return false;
  if (pricing.plan != null && !validSurface(pricing.plan)) return false;
  const current = pricing.current;
  return current == null || typeof current === 'object' && ['purchased_credits', 'included_allowance', 'api_estimate'].every(key => validSurface(current[key]));
}
function parseReport(text, report) {
  let envelope;
  try { envelope = JSON.parse(text); } catch { throw new LocalError('contract'); }
  const data = envelope?.data;
  if (envelope?.schema_version !== 2 || envelope.report !== report || !data || data.schema_version !== 1) throw new LocalError('contract');
  if (report === 'integration-status') {
    if (typeof data.ledger_available !== 'boolean' || !data.observation || typeof data.observation !== 'object' || Array.isArray(data.observation) || ![true, false, null].includes(data.coverage_complete)) throw new LocalError('contract');
  } else if (report === 'statusline') {
    if (!Number.isSafeInteger(data.tokens?.total_tokens) || data.tokens.total_tokens < 0 || !Array.isArray(data.providers) || data.providers.length > 64 || typeof data.pricing_complete !== 'boolean' || ![true, false, null].includes(data.coverage_complete)) throw new LocalError('contract');
    if (data.providers.some(provider => !provider || !Number.isSafeInteger(provider.tokens?.total_tokens) || provider.tokens.total_tokens < 0 || typeof provider.pricing_complete !== 'boolean' || (provider.harness != null && typeof provider.harness !== 'string') || !validPricing(provider.pricing))) throw new LocalError('contract');
  } else throw new LocalError('contract');
  return data;
}
function runCli(executable, report, signal, execute = execFile) {
  if (!REPORTS.has(report)) return Promise.reject(new LocalError('contract'));
  let file;
  try { file = executablePath(executable); } catch (error) { return Promise.reject(error); }
  return new Promise((resolve, reject) => {
    execute(file, [report, '--schema-version', '2', '--format', 'json'], {
      cwd: os.homedir(), shell: false, windowsHide: true, timeout: 12000,
      killSignal: 'SIGKILL', maxBuffer: MAX_BYTES, encoding: 'utf8', signal,
    }, (error, stdout) => {
      if (error) {
        const code = signal?.aborted ? 'cancelled' : error.code === 'ENOENT' ? 'missing' : error.killed || error.code === 'ETIMEDOUT' ? 'timeout' : 'query';
        reject(new LocalError(code)); return;
      }
      try { resolve(parseReport(stdout, report)); } catch (failure) { reject(failure); }
    });
  });
}
module.exports = { LocalError, executablePath, parseReport, runCli, MAX_BYTES };
