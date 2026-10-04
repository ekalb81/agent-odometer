// Synthetic presentation evidence only; no backend verification is implied.
import type { IntegrationCenterReport, IntegrationScope } from '../lib/types';
export function integrationFixture(scope: IntegrationScope = 'user'): IntegrationCenterReport {
  return {
    schema_version: 1,
    status: {
      schema_version: 1, server_version: '0.8.21', protocol_version: '2025-06-18', generated_at: '2026-09-01T13:00:00Z',
      ledger_available: true, coverage_complete: true, scan_status: 'desktop_scan_complete', sessions: 2,
      observation: { captured_at: '2026-09-01T12:00:00Z', age_seconds: 3600, generation: 'observation:1788264000000:2', token_event_from: '2026-09-01T12:00:00Z', token_event_to: '2026-09-01T12:30:00Z' },
      providers: [], pricing_models: [], pricing_authority: 'Observed usage, plan credits and API USD estimates are separate quantities.', quota_authority: 'Quota is not queried here. Inspect quota_report provenance.',
      dimensions: ['provider', 'model', 'project', 'session'], filters: ['inclusive UTC dates'], suggested_next_calls: ['usage_report', 'session_report'], diagnostics: [], limitations: ['Browser fixture responses do not prove backend or client behavior.'],
    },
    cards: (['codex', 'claude_code'] as const).map((client) => ({
      client, scope, configuration_path: `/synthetic/${client}/${scope}/config`, executable: `/synthetic/bin/${client}`, version: '1.2.3', installed: true, configured: false, managed: false, backup_path: null, restore_available: false,
      diagnostic: { code: 'integration_not_configured' as const, evidence: 'No matching entry verified in this fixture.', next_action: 'Preview the intended configuration.' },
      manual_command: '["/synthetic/odometer", "mcp"]', instructions: 'Call odometer_status, then discover usage through usage_report and session_report. Preserve unavailable pricing and quota evidence.',
    })),
    activity: [], activity_available: true, supported_client_task_proof: 'not_verified',
  };
}
