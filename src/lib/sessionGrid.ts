import type { SessionSummary } from './types';

/** Format a session start in the viewer's local wall-clock time. Optional
 * locale/time-zone arguments keep DST behavior deterministic in tests.
 *
 * Minute precision, not second: the medium time style overflowed the grid's
 * started column, wrapping every row onto two lines and halving how many
 * sessions fit on screen. The exact instant stays available in the cell's
 * title attribute. */
export function formatStartedLocal(
  startedMs: number,
  locales?: Intl.LocalesArgument,
  timeZone?: string,
): string {
  return new Intl.DateTimeFormat(locales, {
    dateStyle: 'medium',
    timeStyle: 'short',
    timeZone,
  }).format(new Date(startedMs));
}

/** Zero is the aggregate wire representation for a category that may be
 * measured as zero, omitted, or not applicable. Keep that ambiguity visible. */
export function formatTokenCategory(value: number, locales?: Intl.LocalesArgument): string {
  return value === 0 ? '—' : new Intl.NumberFormat(locales).format(value);
}

/** Grouped digits for a per-row value, compact notation once the magnitude
 * would overflow a numeric column. Corpus-wide totals reach twelve digits,
 * which at the grid's fixed column width ran into the neighbouring cell; the
 * exact value stays available in a title attribute. */
export function formatTokenTotal(value: number, locales?: Intl.LocalesArgument): string {
  if (value === 0) return '—';
  if (Math.abs(value) < 1_000_000_000) return new Intl.NumberFormat(locales).format(value);
  return new Intl.NumberFormat(locales, {
    notation: 'compact',
    maximumFractionDigits: 2,
  }).format(value);
}

export interface ModelProviderVisual {
  key: 'openai' | 'anthropic' | 'google' | 'xai' | 'other';
  label: string;
  tint: string;
}

/** Map provider metadata to a bounded visual palette. Harness fallback keeps
 * historical summaries useful while unknown providers stay visibly neutral. */
export function modelProviderVisual(
  session: Pick<SessionSummary, 'harness' | 'model_provider'>,
): ModelProviderVisual {
  const raw = session.model_provider?.trim();
  const normalized = (raw || (session.harness === 'codex' ? 'openai' : 'anthropic')).toLowerCase();
  if (normalized === 'openai' || normalized === 'codex') {
    return { key: 'openai', label: 'OpenAI', tint: 'var(--provider-openai-row)' };
  }
  if (normalized === 'anthropic' || normalized === 'claude') {
    return { key: 'anthropic', label: 'Anthropic', tint: 'var(--provider-anthropic-row)' };
  }
  if (normalized === 'google' || normalized === 'gemini' || normalized === 'vertex') {
    return { key: 'google', label: 'Google', tint: 'var(--provider-google-row)' };
  }
  if (normalized === 'xai' || normalized === 'grok') {
    return { key: 'xai', label: 'xAI', tint: 'var(--provider-xai-row)' };
  }
  return { key: 'other', label: raw?.slice(0, 40) || 'Other provider', tint: 'var(--provider-other-row)' };
}
