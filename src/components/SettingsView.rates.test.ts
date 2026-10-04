import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import userEvent from '@testing-library/user-event';
import SettingsView from './SettingsView.svelte';
import { rates } from '../lib/stores/rates';
import type { RateCard } from '../lib/types';

const mocks = vi.hoisted(() => {
  globalThis.matchMedia = ((query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} })) as unknown as typeof matchMedia;
  return { setRates: vi.fn().mockResolvedValue(undefined), getBundledRates: vi.fn() };
});
vi.mock('../lib/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/ipc')>();
  const mocked: Record<string, unknown> = { ...actual };
  for (const [key, value] of Object.entries(actual)) {
    if (typeof value === 'function') mocked[key] = vi.fn().mockResolvedValue(key.startsWith('on') ? () => {} : undefined);
  }
  return { ...mocked, ...mocks, getHistoryRebuildStatus: vi.fn().mockResolvedValue({ phase: 'idle', done: 0, total: 0, error: null }) };
});
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0-test') }));

const card = {
  version: 12, currency: 'credits', unit: 'per_1m_tokens', source_url: 'https://example.test', fetched_at: '2026-09-10',
  models: { 'synthetic-model': { input: 2, cached_input: .2, cache_creation_input: null, output: 10, reasoning: 10 } },
  fallback_model: 'synthetic-model', currencies: {}, fallback_models: {}, api_models: {}, unpriced_models: [],
  pricing_catalog: { rate_periods: [], conditional_modifiers: [], notes: [] }, model_aliases: {},
  floating_model_aliases: { 'synthetic-latest': { target: 'synthetic-model', expires_at: '2026-09-01', source_url: 'https://example.test' } },
  rate_provenance: { 'models/synthetic-model': { evidence: 'Synthetic evidence', source_url: 'https://example.test', verified_at: '2026-09-10T00:00:00Z', note: null } },
  upgrade_review: ['floating_model_aliases/synthetic-latest'], flat_rate_expires_at: { 'synthetic-model': '2027-01-01' },
  free_local_models: [], subscription_plans: {}, display_currency: null,
  refresh: { last_success_at: null, last_attempt_at: null, last_failure_reason: null, max_cache_age_secs: 604800 },
} satisfies RateCard;

beforeEach(() => {
  vi.clearAllMocks();
  mocks.setRates.mockImplementation(async (saved: RateCard) => ({ ...saved, delivery: { source: 'saved_override', app_version: '0.8.21', card_version: saved.version, last_failure_reason: null } }));
  rates.set(structuredClone(card));
});
afterEach(() => { cleanup(); rates.set(null); vi.unstubAllGlobals(); });

it('preserves alias expiry and review metadata on save, removes edited-row evidence, and resets only after confirmation', async () => {
  render(SettingsView);
  expect(screen.getByText(/Retained custom or unverified entries/)).toHaveTextContent('floating_model_aliases/synthetic-latest');
  expect(screen.getByText(/^card reference/)).toHaveTextContent('2026-09-10');
  const row = screen.getByRole('cell', { name: /^synthetic-model/ }).closest('tr')!;
  await fireEvent.input(row.querySelector('input')!, { target: { value: '7' } });
  await fireEvent.click(within(row.closest('section')!).getByRole('button', { name: /^Save$/ }));
  await waitFor(() => expect(mocks.setRates).toHaveBeenCalledOnce());
  const saved = mocks.setRates.mock.calls[0][0] as RateCard;
  expect(saved.models['synthetic-model'].input).toBe(7);
  expect(saved.floating_model_aliases).toEqual(card.floating_model_aliases);
  expect(saved.flat_rate_expires_at).toEqual({});
  expect(saved.rate_provenance).toEqual({});
  expect(saved.upgrade_review).toContain('models/synthetic-model');
  const confirm = vi.fn().mockReturnValue(false);
  vi.stubGlobal('confirm', confirm);
  await fireEvent.click(screen.getByRole('button', { name: 'Reset to shipped defaults' }));
  expect(mocks.getBundledRates).not.toHaveBeenCalled();
  const bundled = { ...card, upgrade_review: [], floating_model_aliases: {} };
  mocks.getBundledRates.mockResolvedValue(bundled);
  confirm.mockReturnValue(true);
  await fireEvent.click(screen.getByRole('button', { name: 'Reset to shipped defaults' }));
  await waitFor(() => expect(mocks.setRates).toHaveBeenLastCalledWith(bundled));
  expect(screen.queryByText(/Retained custom or unverified entries/)).not.toBeInTheDocument();
});

it('requires explicit monetary FX evidence and preserves the credit card when saving or disabling it', async () => {
  render(SettingsView);
  const row = screen.getByRole('cell', { name: /^synthetic-model/ }).closest('tr')!;
  const save = within(row.closest('section')!).getByRole('button', { name: /^Save$/ });
  await fireEvent.click(screen.getByLabelText('Use a user-supplied FX rate'));
  await fireEvent.click(save);
  expect(mocks.setRates).not.toHaveBeenCalled();
  expect(screen.getByText(/FX requires supported monetary currencies/)).toBeInTheDocument();
  await fireEvent.change(screen.getByLabelText('Original money currency'), { target: { value: 'USD' } });
  await fireEvent.change(screen.getByLabelText('Display currency'), { target: { value: 'EUR' } });
  const user = userEvent.setup();
  const rate = screen.getByLabelText('Display units per original unit');
  await user.clear(rate);
  await user.type(rate, '0.');
  expect(rate).toHaveValue('0.');
  await user.type(rate, '9');
  const timestamp = screen.getByLabelText('Rate timestamp (UTC)');
  await user.type(timestamp, '2026-10-');
  expect(timestamp).toHaveValue('2026-10-');
  await user.type(timestamp, '01T12:30:00');
  await user.type(screen.getByLabelText('User-supplied source'), 'Synthetic offline quote');
  await fireEvent.click(save);
  await waitFor(() => expect(mocks.setRates).toHaveBeenCalledOnce());
  const saved = mocks.setRates.mock.calls[0][0] as RateCard;
  expect(saved.display_currency).toEqual({ from_currency: 'USD', target_currency: 'EUR', rate: .9, as_of: '2026-10-01T12:30:00.000Z', source: 'Synthetic offline quote' });
  expect(saved.currency).toBe('credits');
  expect(saved.models).toEqual(card.models);
  expect(saved.floating_model_aliases).toEqual(card.floating_model_aliases);
  expect(saved.rate_provenance).toEqual(card.rate_provenance);
  expect(screen.getByText(/Loaded source: saved override/)).toHaveTextContent('card v12 · app v0.8.21');
  await fireEvent.click(screen.getByLabelText('Use a user-supplied FX rate'));
  await fireEvent.click(save);
  await waitFor(() => expect(mocks.setRates).toHaveBeenCalledTimes(2));
  expect(mocks.setRates.mock.calls[1][0].display_currency).toBeNull();
  expect(mocks.setRates.mock.calls[1][0].models).toEqual(card.models);
});

it('shows recovery evidence without claiming an independent card download', () => {
  rates.set({ ...structuredClone(card), delivery: { source: 'last_valid_fallback', app_version: '0.8.21', card_version: 12, last_failure_reason: 'Saved card is invalid; using last validated backup.' } });
  render(SettingsView);
  expect(screen.getByText(/last valid fallback/)).toBeInTheDocument();
  expect(screen.getByText(/Saved card is invalid; using last validated backup/)).toBeInTheDocument();
  expect(screen.getByText(/There is no independent card or FX download/)).toBeInTheDocument();
});
