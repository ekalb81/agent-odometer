import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import HistoryManagement from './HistoryManagement.svelte';

const mocks = vi.hoisted(() => ({
  health: vi.fn(), status: vi.fn(), policy: vi.fn(), preview: vi.fn(), purge: vi.fn(), recover: vi.fn(), retry: vi.fn(),
}));
vi.mock('../lib/ipc', () => ({
  getHistoryRecoveryStatus: mocks.health, getRetentionStatus: mocks.status, setRetentionPolicy: mocks.policy,
  previewHistoryPurge: mocks.preview, purgeRetainedHistory: mocks.purge, recoverHistory: mocks.recover,
  retryHistoryOpen: mocks.retry, onHistoryProgress: vi.fn().mockResolvedValue(() => {}),
}));
const healthy = { status: 'ready', failure: null, coverage_complete: true, backup_directory: null, can_recover: false, can_retry: false };
const retained = { policy: { retained_days: 30 }, present_sessions: 4, retained_sessions: 2, superseded_sessions: 1, purged_sessions: 0, coverage_complete: true, recovered_at: null };
beforeEach(() => {
  vi.clearAllMocks();
  mocks.health.mockResolvedValue(healthy);
  mocks.status.mockResolvedValue(retained);
  mocks.preview.mockResolvedValue({ cutoff_utc_day: '2026-09-04', sessions: 2, identity_groups: 1, snapshot_bytes: 4096, tokens: {}, revision: 'reviewed-v1' });
  mocks.purge.mockResolvedValue({ removed_keys: ['a', 'b'], purged_at: '2026-10-04T12:00:00Z' });
});

describe('history retention confirmation', () => {
  it('requires saving an edited policy before reviewing its candidates', async () => {
    mocks.policy.mockResolvedValue({ ...retained, policy: { retained_days: 90 } });
    render(HistoryManagement);
    const review = await screen.findByRole('button', { name: 'Review eligible history…' });
    await userEvent.selectOptions(screen.getByLabelText('Retained-history policy'), '90');
    expect(review).toBeDisabled();
    expect(mocks.preview).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }));
    await waitFor(() => expect(mocks.policy).toHaveBeenCalledWith({ retained_days: 90 }));
    expect(review).toBeEnabled();
    expect(mocks.purge).not.toHaveBeenCalled();
  });

  it('does not purge while reviewing, with a wrong phrase, or after cancelling', async () => {
    render(HistoryManagement);
    await userEvent.click(await screen.findByRole('button', { name: 'Review eligible history…' }));
    const purge = screen.getByRole('button', { name: 'Purge reviewed history' });
    expect(purge).toBeDisabled();
    await userEvent.type(screen.getByLabelText('Purge confirmation'), 'PURGE 1');
    expect(purge).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel review' }));
    expect(mocks.purge).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('Purge preview')).not.toBeInTheDocument();
  });

  it('sends the exact reviewed revision only after typed confirmation and preserves a stale-preview error', async () => {
    mocks.purge.mockRejectedValue(new Error('history changed after the purge preview; review a fresh preview'));
    render(HistoryManagement);
    await userEvent.click(await screen.findByRole('button', { name: 'Review eligible history…' }));
    await userEvent.type(screen.getByLabelText('Purge confirmation'), 'PURGE 2');
    await userEvent.click(screen.getByRole('button', { name: 'Purge reviewed history' }));
    expect(mocks.purge).toHaveBeenCalledWith('reviewed-v1', 'PURGE 2');
    expect(await screen.findByRole('alert')).toHaveTextContent('history changed');
    expect(screen.getByLabelText('Purge preview')).toBeInTheDocument();
  });

  it('requires reviewed recovery confirmation and shows incomplete coverage after replacement', async () => {
    mocks.health.mockResolvedValue({ ...healthy, status: 'unavailable', can_recover: true, can_retry: true, failure: { kind: 'corrupt', message: 'History could not be read.' } });
    mocks.recover.mockImplementation(async () => {
      mocks.health.mockResolvedValue({ ...healthy, coverage_complete: false, backup_directory: '/synthetic/preserved' });
      mocks.status.mockResolvedValue({ ...retained, coverage_complete: false });
    });
    render(HistoryManagement);
    await userEvent.click(await screen.findByRole('button', { name: 'Preserve and rebuild readable history…' }));
    expect(mocks.recover).not.toHaveBeenCalled();
    const confirm = screen.getByRole('button', { name: 'Preserve and rebuild' });
    expect(confirm).toBeDisabled();
    await userEvent.type(screen.getByLabelText('Recovery confirmation'), 'REBUILD READABLE HISTORY');
    await userEvent.click(confirm);
    await waitFor(() => expect(mocks.recover).toHaveBeenCalledWith('REBUILD READABLE HISTORY'));
    expect(await screen.findByText(/Token and money budgets are unavailable/)).toBeInTheDocument();
    expect(screen.getByText(/Preserved history: \/synthetic\/preserved/)).toBeInTheDocument();
  });

  it('shows purged partial coverage without inventing a recovery backup', async () => {
    mocks.health.mockResolvedValue({ ...healthy, coverage_complete: false });
    mocks.status.mockResolvedValue({ ...retained, purged_sessions: 2, coverage_complete: false });
    render(HistoryManagement);
    expect(await screen.findByText(/History was intentionally purged or historical sources were not recovered/)).toBeInTheDocument();
    expect(screen.queryByText(/Preserved history:/)).not.toBeInTheDocument();
    expect(screen.queryByText(/incomplete after recovery/)).not.toBeInTheDocument();
  });

  it('offers no file replacement or retry while a failed open connection still needs restart', async () => {
    mocks.health.mockResolvedValue({ ...healthy, status: 'unavailable', failure: { kind: 'corrupt', message: 'Restart to close this archive.' } });
    render(HistoryManagement);
    expect(await screen.findByText('Restart to close this archive.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Preserve and rebuild readable/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry opening history' })).not.toBeInTheDocument();
  });
});
