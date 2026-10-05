const now = '2026-10-05T01:00:00Z';
const surface = total => ({ total, by_model: [{ basis: 'direct', cost: total, unpriced: false }], missing_models: [], unpriced_models: [] });
function status() { return { schema_version: 1, ledger_available: true, coverage_complete: true, sessions: 2, observation: { captured_at: now }, generated_at: now }; }
function usage() { return { schema_version: 1, cost_surface: 'legacy_reference', from: '2026-10-05T00:00:00Z', to: now, coverage_complete: true, tokens: { total_tokens: 1100 }, pricing_complete: true, providers: [{ harness: 'codex', currency: 'credits', tokens: { total_tokens: 1100 }, pricing_complete: true, pricing: { current: { as_of: now, purchased_credits: surface(0.1775), included_allowance: surface(0.1775), api_estimate: surface(0.0071) } } }] }; }
const envelope = (report, data) => JSON.stringify({ schema_version: 2, report, data });
module.exports = { now, status, usage, envelope };
