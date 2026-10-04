import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
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

beforeEach(() => { vi.clearAllMocks(); rates.set(structuredClone(card)); });
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
  expect(saved.flat_rate_expires_at).toEqual(card.flat_rate_expires_at);
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
