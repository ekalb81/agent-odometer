const { runCli } = require('./client.cjs');
const labels = { codex: 'Codex', claude_code: 'Claude Code', gemini_cli: 'Gemini CLI' };
const failures = {
  setup: 'Select the installed agent-odometer executable to read local data.',
  missing: 'The selected executable is missing. Select it again or install Odometer.',
  contract: 'Unsupported CLI response. Update Odometer and this extension together.',
  timeout: 'The local query timed out. Open Odometer to inspect history, then retry.',
  query: 'Local data is unavailable. Open the matching Odometer desktop version and wait for history preparation, then retry.',
};
const time = value => { const date = new Date(value); return typeof value === 'string' && Number.isFinite(date.valueOf()) ? date.toISOString() : 'Unknown'; };
const count = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString('en-US') : 'Unavailable';
const amount = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value.toLocaleString('en-US', { maximumFractionDigits: 6 }) : 'Unavailable';
const row = (id, label, description, children = [], command) => ({ id, label, description, children, command });
function surfaceRows(prefix, pricing, currency) {
  if (!pricing?.current) {
    const plan = pricing?.plan;
    return [row(prefix + '.legacy', 'Legacy price reference', amount(plan?.total) + ' ' + currency), row(prefix + '.quality', 'Price limitations', !plan || plan.missing_models?.length || plan.unpriced_models?.length ? 'Partial or unavailable pricing' : 'Reference estimate; inspect Odometer for rate provenance')];
  }
  const current = pricing.current;
  const result = [row(prefix + '.asof', 'Price reference as of', time(current.as_of))];
  for (const [key, label, unit] of [['purchased_credits', 'Purchased-credit reference', 'credits'], ['included_allowance', 'Included usage reference', 'credits'], ['api_estimate', 'API estimate', 'USD']]) {
    const surface = current[key];
    const partial = !surface || !Array.isArray(surface.unpriced_models) || !Array.isArray(surface.missing_models) || surface.unpriced_models.length > 0 || surface.missing_models.length > 0 || surface.by_model?.some(model => ['stale', 'fallback', 'estimated', 'unavailable'].includes(model.basis));
    result.push(row(prefix + '.' + key, label, amount(surface?.total) + ' ' + unit + (partial ? ' · partial or uncertain pricing' : '')));
  }
  result.push(row(prefix + '.basis', 'Interpretation', 'Separate references, not a bill or remaining quota'));
  return result;
}
function rows(state, now = Date.now()) {
  const result = [row('source', 'Source', 'Local read-only Odometer CLI'), row('setup', 'Setup guide', '', [], 'odometer.setup')];
  if (state.error) result.unshift(row('error', state.snapshot ? 'Refresh failed · previous snapshot only' : 'Unavailable', failures[state.error] || failures.query));
  if (state.busy) result.unshift(row('loading', 'Reading local ledger…', 'Bounded read-only queries'));
  if (!state.snapshot) return result;
  const { status, usage, fetchedAt, usageError } = state.snapshot;
  const age = now - fetchedAt;
  result.push(row('fetched', state.error || age < 0 || age > 300000 ? 'Stale query snapshot' : 'Query received', new Date(fetchedAt).toISOString()));
  result.push(row('ledger', 'Ledger', status.ledger_available ? 'Available' : 'Unavailable · open Odometer to prepare history'));
  result.push(row('coverage', 'History coverage', status.coverage_complete === true ? 'Recorded history intact; live scan completeness unknown' : status.coverage_complete === false ? 'Partial · some history was removed or could not be recovered' : 'Unverified'));
  const captured = status.observation.captured_at;
  const observedMs = typeof captured === 'string' ? Date.parse(captured) : NaN;
  const observationAge = now - observedMs;
  result.push(row('observed', 'Ledger observation', time(captured) + (!Number.isFinite(observationAge) || observationAge < 0 ? ' · age unknown' : observationAge > 300000 ? ' · older than five minutes' : '')));
  result.push(row('scan', 'Desktop scan', 'Unknown to this read-only process'));
  result.push(row('sessions', 'Recorded sessions', count(status.sessions)));
  if (usageError) result.push(row('usage-error', 'Usage unavailable', failures[usageError] || failures.query));
  if (!usage) return result;
  const children = [row('window', 'Window (CLI local day)', time(usage.from) + ' → ' + time(usage.to)), row('tokens', 'Recorded tokens', count(usage.tokens.total_tokens)), row('usage-coverage', 'Usage history coverage', usage.coverage_complete === true ? 'Intact recorded history' : usage.coverage_complete === false ? 'Partial' : 'Unverified')];
  for (const [index, provider] of usage.providers.entries()) {
    const id = 'provider.' + index;
    const name = Object.hasOwn(labels, provider.harness) ? labels[provider.harness] : 'Unknown provider';
    const currency = provider.currency === 'credits' ? 'credits' : provider.currency === 'USD' ? 'USD' : 'unverified unit';
    children.push(row(id, name, count(provider.tokens?.total_tokens) + ' tokens' + (provider.pricing_complete === true ? '' : ' · incomplete pricing'), surfaceRows(id, provider.pricing, currency)));
  }
  if (!usage.providers.length) children.push(row('no-usage', 'No recorded usage in this window', 'This does not prove no agent activity'));
  result.push(row('today', 'Today · recorded usage', usage.pricing_complete ? 'Prices supplied by Rust' : 'Partial pricing', children));
  return result;
}
class Dashboard {
  constructor(notify, runner = runCli, now = Date.now) {
    this.notify = notify; this.runner = runner; this.now = now;
    this.state = { snapshot: null, error: null, busy: false }; this.generation = 0; this.active = null; this.disposed = false;
  }
  reset(error = null) {
    this.generation++; this.active?.abort(); this.active = null;
    this.state = { snapshot: null, error, busy: false }; this.notify();
  }
  async refresh(executable) {
    if (this.disposed || this.active) return;
    const generation = ++this.generation; const active = new AbortController(); this.active = active;
    this.state.busy = true; this.notify();
    try {
      const status = await this.runner(executable, 'integration-status', active.signal);
      if (generation !== this.generation || this.disposed || active.signal.aborted) return;
      let usage = null; let usageError = null;
      if (status.ledger_available) {
        try { usage = await this.runner(executable, 'statusline', active.signal); }
        catch (error) { if (active.signal.aborted) throw error; usageError = failures[error.code] ? error.code : 'query'; }
      }
      if (generation !== this.generation || this.disposed) return;
      this.state = { snapshot: { status, usage, usageError, fetchedAt: this.now() }, error: null, busy: false };
    } catch (error) {
      if (generation !== this.generation || this.disposed) return;
      this.state.error = failures[error.code] ? error.code : 'query'; this.state.busy = false;
    } finally {
      if (generation === this.generation && !this.disposed) { this.active = null; this.notify(); }
    }
  }
  dispose() { this.disposed = true; this.generation++; this.active?.abort(); this.active = null; }
}
module.exports = { Dashboard, rows, failures };
