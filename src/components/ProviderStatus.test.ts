import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { beforeEach, expect, it, vi } from 'vitest';
import ProviderStatus from './ProviderStatus.svelte';
import { config } from '../lib/stores/config';
import type { ProviderServiceStatusSnapshot } from '../lib/types';

const mocks = vi.hoisted(() => ({ getProviderServiceStatus: vi.fn(), setProviderStatusEnabled: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const initial = get(config);
const result: ProviderServiceStatusSnapshot = { enabled: true, providers: [
  { provider: 'codex', source_url: 'https://status.openai.com/api/v2/status.json', state: 'current', current_indicator: 'major', last_known_indicator: 'major', checked_at: '2026-10-04T12:00:00Z', source_updated_at: '2026-10-04T11:00:00Z', last_attempt_at: '2026-10-04T12:00:00Z', next_attempt_at: '2026-10-04T12:05:00Z', failure: null },
  { provider: 'gemini_cli', source_url: null, state: 'unsupported', current_indicator: null, last_known_indicator: null, checked_at: null, source_updated_at: null, last_attempt_at: null, next_attempt_at: null, failure: null },
] };
beforeEach(() => {
  vi.resetAllMocks(); config.set({ ...initial, provider_status_enabled: false });
  mocks.setProviderStatusEnabled.mockImplementation(async enabled => ({ ...initial, provider_status_enabled: enabled })); mocks.getProviderServiceStatus.mockResolvedValue(result);
});
it('stays offline by default and only reads public status after the explicit setting is saved', async () => {
  render(ProviderStatus);
  expect(mocks.getProviderServiceStatus).not.toHaveBeenCalled();
  expect(screen.getByRole('checkbox')).not.toBeChecked();
  await userEvent.click(screen.getByRole('checkbox'));
  expect(mocks.setProviderStatusEnabled).toHaveBeenCalledWith(true);
  await screen.findByText('Major incident');
  expect(screen.getByText(/no supported public source/)).toBeInTheDocument();
  expect(screen.getByText('Source: https://status.openai.com/api/v2/status.json')).toBeInTheDocument();
  expect(screen.getByText(/Last successful check:/)).toBeInTheDocument();
});
it('labels stale and failed last-known status without presenting it as current or a zero', async () => {
  config.set({ ...initial, provider_status_enabled: true });
  mocks.getProviderServiceStatus.mockResolvedValue({ ...result, providers: [{ ...result.providers[0], state: 'unavailable', current_indicator: null, last_known_indicator: 'operational', failure: 'rate_limited' }] });
  render(ProviderStatus);
  await screen.findByText('Unavailable · public source could not be checked');
  expect(screen.getByText(/Last known: Operational.*not a current reading/)).toBeInTheDocument();
  expect(screen.queryByText('Operational', { exact: true })).not.toBeInTheDocument();
  expect(screen.getByText('Rate limited; waiting before retry.')).toBeInTheDocument();
});
it('rejects late observations after opt-out and does not replace newer config events', async () => {
  let resolve!: (value: ProviderServiceStatusSnapshot) => void;
  mocks.getProviderServiceStatus.mockReturnValue(new Promise(done => { resolve = done; }));
  config.set({ ...initial, provider_status_enabled: true });
  render(ProviderStatus);
  await waitFor(() => expect(mocks.getProviderServiceStatus).toHaveBeenCalledOnce());
  mocks.setProviderStatusEnabled.mockImplementation(async enabled => { const updated = { ...initial, provider_status_enabled: enabled, session_roots: ['new-authoritative-root'] }; config.set(updated); return updated; });
  await userEvent.click(screen.getByRole('checkbox'));
  resolve(result);
  await screen.findByText(/Public status checks are off/);
  expect(screen.queryByText('Major incident')).not.toBeInTheDocument();
  expect(get(config).session_roots).toEqual(['new-authoritative-root']);
});
it('keeps failed opt-in off and hides payloads from save/read errors', async () => {
  mocks.setProviderStatusEnabled.mockRejectedValue(new Error('PRIVATE_CONFIG_PATH'));
  render(ProviderStatus);
  await userEvent.click(screen.getByRole('checkbox'));
  await screen.findByText('The service-status setting could not be saved.');
  expect(mocks.getProviderServiceStatus).not.toHaveBeenCalled();
  expect(screen.getByRole('checkbox')).not.toBeChecked();
  mocks.getProviderServiceStatus.mockRejectedValue(new Error('PRIVATE_RESPONSE_BODY'));
  config.set({ ...initial, provider_status_enabled: true });
  await screen.findByText('Public status is unavailable. Local usage remains available.');
  expect(screen.queryByText(/PRIVATE_/)).not.toBeInTheDocument();
});
it('submits only the flag from an uninitialized store and retains authoritative watcher roots', async () => {
  const authoritative = { ...initial, provider_status_enabled: true, session_roots: ['synthetic-preserved-root'], session_index_path: 'synthetic-preserved-index', performance_tracking_enabled: true };
  mocks.setProviderStatusEnabled.mockResolvedValue(authoritative);
  render(ProviderStatus);
  await userEvent.click(screen.getByRole('checkbox'));
  expect(mocks.setProviderStatusEnabled).toHaveBeenCalledExactlyOnceWith(true);
  await waitFor(() => expect(get(config)).toEqual(authoritative));
  expect(get(config).session_roots).toEqual(['synthetic-preserved-root']);
  expect(get(config).performance_tracking_enabled).toBe(true);
});

it('pauses observations when hidden without changing consent and rejects a late response', async () => {
  let resolve!: (value: ProviderServiceStatusSnapshot) => void;
  config.set({ ...initial, provider_status_enabled: true });
  mocks.getProviderServiceStatus.mockReturnValue(new Promise(done => { resolve = done; }));
  const view = render(ProviderStatus, { active: false });
  expect(mocks.getProviderServiceStatus).not.toHaveBeenCalled();
  await view.rerender({ active: true });
  await waitFor(() => expect(mocks.getProviderServiceStatus).toHaveBeenCalledOnce());
  await view.rerender({ active: false });
  resolve(result);
  await Promise.resolve();
  expect(screen.queryByText('Major incident')).not.toBeInTheDocument();
  expect(get(config).provider_status_enabled).toBe(true);
  expect(mocks.setProviderStatusEnabled).not.toHaveBeenCalled();
});
