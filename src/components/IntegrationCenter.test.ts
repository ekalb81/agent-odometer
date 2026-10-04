import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import IntegrationCenter from './IntegrationCenter.svelte';
import { integrationFixture } from '../dev-mock/integration';
import type { IntegrationCenterReport } from '../lib/types';

const ipc = vi.hoisted(() => ({ getIntegrationStatus: vi.fn(), previewIntegrationChange: vi.fn(), applyIntegrationChange: vi.fn(), testIntegrationClient: vi.fn(), openIntegrationConfiguration: vi.fn() }));
vi.mock('../lib/ipc', () => ipc);
beforeEach(() => { vi.resetAllMocks(); ipc.getIntegrationStatus.mockResolvedValue(integrationFixture()); });

describe('Integration Center evidence boundaries', () => {
  it.each([false, null, undefined])('does not present %s coverage as complete history', async (coverage) => {
    const fixture = integrationFixture(); fixture.status.coverage_complete = coverage;
    ipc.getIntegrationStatus.mockResolvedValue(fixture);
    render(IntegrationCenter);
    await screen.findByText(coverage === false ? /Partial history — recorded totals/ : /History coverage unavailable — recorded totals/);
    expect(screen.getByText(/Ledger: 2 recorded sessions/)).toBeTruthy();
  });
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
  it('cancel discards approval and a later preview requires its own apply', async () => {
    ipc.previewIntegrationChange.mockResolvedValue({ id: 'cancelled-preview', client: 'codex', scope: 'user', action: 'remove', configuration_path: '/synthetic/config', entry_preview: 'SELECTED REMOVAL', warning: 'Restart required.' });
    const fixture = integrationFixture(); fixture.cards[0].managed = true;
    ipc.getIntegrationStatus.mockResolvedValue(fixture);
    const user = userEvent.setup(); render(IntegrationCenter);
    await user.click((await screen.findAllByRole('button', { name: 'Preview removal' }))[0]);
    await screen.findByText('SELECTED REMOVAL');
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('button', { name: 'Apply reviewed change' })).toBeNull();
    expect(ipc.applyIntegrationChange).not.toHaveBeenCalled();
  });
  it('concurrent-edit refusal cannot display a saved configuration', async () => {
    ipc.previewIntegrationChange.mockResolvedValue({ id: 'changed-config', client: 'codex', scope: 'user', action: 'install', configuration_path: '/synthetic/config', entry_preview: 'SELECTED ENTRY', warning: 'Restart required.' });
    ipc.applyIntegrationChange.mockRejectedValue(new Error('Configuration changed; preview again.'));
    const user = userEvent.setup(); render(IntegrationCenter);
    await user.click((await screen.findAllByRole('button', { name: 'Preview setup' }))[0]);
    await user.click(await screen.findByRole('button', { name: 'Apply reviewed change' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Configuration changed; preview again.');
    expect(screen.queryByText(/Configuration saved/)).toBeNull();
    expect(screen.getAllByText('Not configured')).toHaveLength(2);
  });
  it('failed discovery remains an error and can recover without mutating config', async () => {
    ipc.getIntegrationStatus.mockRejectedValueOnce(new Error('Integration unavailable'));
    const user = userEvent.setup(); render(IntegrationCenter);
    expect(await screen.findByRole('alert')).toHaveTextContent('Integration unavailable');
    expect(screen.queryByText('Server canary verified')).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Refresh status and activity' }));
    await screen.findByRole('article', { name: 'Codex integration' });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(ipc.applyIntegrationChange).not.toHaveBeenCalled();
  });
  it('partial server verification and unavailable activity never become complete evidence', async () => {
    const fixture = integrationFixture(); fixture.cards[0].configured = true; fixture.activity_available = false;
    ipc.getIntegrationStatus.mockResolvedValue(fixture);
    ipc.testIntegrationClient.mockResolvedValue({ schema_version: 1, ok: false, checks: [{ id: 'mcp_launch', status: 'pass', detail: 'Launched' }, { id: 'mcp_initialize', status: 'fail', detail: 'Protocol mismatch' }] });
    const user = userEvent.setup(); render(IntegrationCenter);
    const card = await screen.findByRole('article', { name: 'Codex integration' });
    await user.click(within(card).getByRole('button', { name: 'Test now' }));
    await waitFor(() => expect(within(card).getByText('fail')).toBeTruthy());
    expect(within(card).getByText('Not verified working')).toBeTruthy();
    expect(screen.getByText('Activity unavailable; absence does not prove no use.')).toBeTruthy();
    expect(screen.queryByText('No calls observed.')).toBeNull();
    expect(screen.getAllByText(/Supported-client agent-task proof: not verified/)).toHaveLength(2);
  });
  it('manual copy/open failures give recovery guidance without changing config', async () => {
    const user = userEvent.setup();
    vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('Clipboard denied'));
    ipc.openIntegrationConfiguration.mockRejectedValue(new Error('Directory unavailable'));
    render(IntegrationCenter);
    const card = await screen.findByRole('article', { name: 'Codex integration' });
    await user.click(within(card).getByRole('button', { name: 'Copy server command' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Select and copy the displayed text');
    await user.click(within(card).getByRole('button', { name: 'Open configuration location' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Directory unavailable');
    expect(ipc.openIntegrationConfiguration).toHaveBeenCalledWith('codex', 'user', null);
    expect(ipc.applyIntegrationChange).not.toHaveBeenCalled();
  });
});
