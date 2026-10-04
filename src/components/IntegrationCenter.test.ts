import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import IntegrationCenter from './IntegrationCenter.svelte';
import { integrationFixture } from '../dev-mock/integration';
import type { IntegrationCenterReport } from '../lib/types';

const ipc = vi.hoisted(() => ({ getIntegrationStatus: vi.fn(), previewIntegrationChange: vi.fn(), applyIntegrationChange: vi.fn(), testIntegrationClient: vi.fn(), openIntegrationConfiguration: vi.fn() }));
vi.mock('../lib/ipc', () => ipc);
beforeEach(() => { vi.resetAllMocks(); ipc.getIntegrationStatus.mockResolvedValue(integrationFixture()); });

describe('Integration Center evidence boundaries', () => {
  it('configuration cannot masquerade as successful server or client use', async () => {
    const fixture = integrationFixture(); fixture.cards[0].configured = true;
    ipc.getIntegrationStatus.mockResolvedValue(fixture);
    render(IntegrationCenter);
    await screen.findByText('Matching entry');
    expect(screen.getAllByText('Not verified working')).toHaveLength(2);
    expect(screen.getAllByText('No successful client call observed')).toHaveLength(2);
    expect(screen.getAllByText('unknown')).toHaveLength(8);
  });
  it('preview requires an explicit apply and selected scope reaches Rust', async () => {
    ipc.previewIntegrationChange.mockResolvedValue({ id: 'test-preview', client: 'codex', scope: 'user', action: 'install', configuration_path: '/synthetic/config', entry_preview: 'ONLY THE SELECTED ENTRY', warning: 'Restart required.' });
    ipc.applyIntegrationChange.mockResolvedValue({ configuration_path: '/synthetic/config', backup_path: '/synthetic/backup', restart_required: true });
    const user = userEvent.setup(); render(IntegrationCenter);
    const setup = await screen.findAllByRole('button', { name: 'Preview setup' });
    await user.click(setup[0]);
    await screen.findByText('ONLY THE SELECTED ENTRY');
    expect(ipc.previewIntegrationChange).toHaveBeenCalledWith('codex', 'user', 'install', null);
    expect(ipc.applyIntegrationChange).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Apply reviewed change' }));
    await waitFor(() => expect(ipc.applyIntegrationChange).toHaveBeenCalledWith('test-preview'));
    await screen.findByText(/Configuration saved/);
  });
  it('verifier activity does not count as actual client use', async () => {
    const fixture = integrationFixture();
    fixture.activity.push({ timestamp: '2026-09-01T13:00:00Z', initialized_at: '2026-09-01T12:59:59Z', client: 'verifier', client_version: '1', identity_authority: 'self_reported', tool: 'odometer_status', duration_ms: 2, success: true, error_code: null, result_bytes: 512, result_rows: null, schema_version: 1, fallback_or_unavailable: false });
    ipc.getIntegrationStatus.mockResolvedValue(fixture); render(IntegrationCenter);
    await screen.findByText(/verifier \(self-reported\)/);
    expect(screen.getAllByText('No successful client call observed')).toHaveLength(2);
  });
  it('a spoofable supported-client name cannot authenticate an agent task', async () => {
    const fixture = integrationFixture();
    fixture.activity.push({ timestamp: '2026-09-01T13:00:00Z', initialized_at: '2026-09-01T12:59:59Z', client: 'codex', client_version: '1.2.3', identity_authority: 'self_reported', tool: 'usage_report', duration_ms: 2, success: true, error_code: null, result_bytes: 512, result_rows: 2, schema_version: 1, fallback_or_unavailable: false });
    ipc.getIntegrationStatus.mockResolvedValue(fixture); render(IntegrationCenter);
    await screen.findByText('Unverified; reported call 2026-09-01T13:00:00Z');
    expect(screen.getAllByText(/Supported-client agent-task proof: not verified/)).toHaveLength(2);
    expect(screen.getAllByText('Not verified working')).toHaveLength(2);
  });
  it('a superseded scope response cannot replace the current scope', async () => {
    let resolve!: (value: IntegrationCenterReport) => void;
    ipc.getIntegrationStatus.mockImplementationOnce(() => new Promise<IntegrationCenterReport>((done) => resolve = done));
    const project = integrationFixture('project'); project.cards[0].version = '9.9.9';
    ipc.getIntegrationStatus.mockResolvedValue(project);
    const user = userEvent.setup(); render(IntegrationCenter);
    await user.selectOptions(screen.getByLabelText('Configuration scope'), 'project');
    await user.type(screen.getByLabelText('Project directory'), '/synthetic/project');
    await screen.findByText('9.9.9');
    resolve(integrationFixture());
    await waitFor(() => expect(screen.getByText('9.9.9')).toBeTruthy());
  });
});
