import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import AttentionSettings from './AttentionSettings.svelte';
import { attention, attentionError } from '../lib/stores/attention';
import type { AttentionSnapshot } from '../lib/types';
const { getAttentionStatus, setAttentionPreferences } = vi.hoisted(() => ({ getAttentionStatus: vi.fn(), setAttentionPreferences: vi.fn() }));
vi.mock('../lib/ipc', () => ({ getAttentionStatus, setAttentionPreferences }));
function snapshot(): AttentionSnapshot { return { available: true, preferences: { revision: 0, categories: [], providers: [], tool_kind: null, stale_after_seconds: 300 }, observations: [], alerts: [] }; }
beforeEach(() => { vi.clearAllMocks(); attention.set(snapshot()); attentionError.set(false); getAttentionStatus.mockResolvedValue(snapshot()); setAttentionPreferences.mockResolvedValue(snapshot().preferences); });
describe('attention preferences', () => {
  it('starts off and saves only explicitly selected metadata categories and filters', async () => {
    render(AttentionSettings);
    const checkbox = screen.getByRole('checkbox', { name: 'Tool failed' });
    expect(checkbox).not.toBeChecked(); expect(setAttentionPreferences).not.toHaveBeenCalled();
    await fireEvent.click(checkbox);
    await fireEvent.change(screen.getByLabelText('Provider'), { target: { value: 'codex' } });
    await fireEvent.change(screen.getByLabelText('Tool category'), { target: { value: 'command' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save attention preferences' }));
    await waitFor(() => expect(setAttentionPreferences).toHaveBeenCalledWith({ revision: 0, categories: ['tool_failed'], providers: ['codex'], tool_kind: 'command', stale_after_seconds: 300 }));
    expect(await screen.findByText(/Only new observations can notify/)).toBeVisible();
  });
  it('shows stale evidence as unknown and keeps source and observation time visible', () => {
    const value = snapshot(); value.preferences.categories = ['input_requested'];
    value.observations = [{ session_ref: '1234567890abcdef', provider: 'codex', state: 'unknown', observed_state: 'waiting', observed_at: '2026-10-04T12:00:00Z', source: 'retained tool observation', stale: true, partial: true }]; attention.set(value);
    render(AttentionSettings);
    expect(screen.getByText(/Session 12345678 · unknown/)).toBeVisible();
    expect(screen.getByText(/last observed state was waiting/)).toBeVisible();
    expect(screen.getByText(/retained tool observation · Observed/)).toBeVisible();
    expect(screen.getByText(/bounded recent slice/)).toBeVisible();
  });
  it('preserves a valid saved expiry that is outside the suggested presets', async () => {
    const value = snapshot(); value.preferences.stale_after_seconds = 30; attention.set(value);
    render(AttentionSettings);
    expect(screen.getByLabelText('Evidence expires after')).toHaveValue('30');
    await fireEvent.click(screen.getByRole('button', { name: 'Save attention preferences' }));
    await waitFor(() => expect(setAttentionPreferences).toHaveBeenCalledWith(value.preferences));
  });
  it('never exposes backend error details and reloads authoritative revisions', async () => {
    setAttentionPreferences.mockRejectedValue(new Error('PRIVATE_PATH secret payload'));
    render(AttentionSettings);
    await fireEvent.click(screen.getByRole('button', { name: 'Save attention preferences' }));
    expect(await screen.findByText(/could not be saved/)).toBeVisible();
    expect(screen.queryByText(/PRIVATE_PATH/)).not.toBeInTheDocument();
    const next = snapshot(); next.preferences.revision = 3; next.preferences.categories = ['turn_completed']; getAttentionStatus.mockResolvedValue(next);
    await fireEvent.click(screen.getByRole('button', { name: 'Refresh attention' }));
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Turn completed' })).toBeChecked());
  });
  it('treats unreadable state as unavailable and absence of signals as unknown', () => {
    const value = snapshot(); value.available = false; value.preferences.categories = ['turn_started']; attention.set(value);
    render(AttentionSettings);
    expect(screen.getByText(/Alerts are suppressed/)).toBeVisible();
    expect(screen.getByText(/State is unknown until/)).toBeVisible();
  });
});
