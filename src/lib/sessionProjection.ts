import {
  apiCostFromBuckets,
  computeSummaryCredits,
  creditsFromBuckets,
  harnessCurrency,
} from './credits';
import type {
  Harness,
  RangeTotals,
  RateCard,
  SessionSummary,
  TierBucket,
  ToolMetrics,
  TokenTotals,
} from './types';

export type ViewScope = Harness | 'all';

export interface SessionFilterState {
  search: string;
  dateFrom: string;
  dateTo: string;
  model: string;
  showActive: boolean;
  showArchived: boolean;
  showSubagents: boolean;
}

export interface SessionProjection<T extends SessionSummary = SessionSummary> {
  session: T;
  tokens: TokenTotals;
  planCost: number;
  apiCost: number | null;
  displayCost: number;
  currency: string;
  missingModels: string[];
}

const projectionCache = new WeakMap<SessionSummary, {
  rates: RateCard | null;
  range: RangeTotals | undefined;
  dateScoped: boolean;
  value: SessionProjection;
}>();

export interface ModelMetric {
  harness: Harness;
  model: string;
  tokens: TokenTotals;
  cost: number;
  currency: string;
  fallbackUsed: boolean;
  tools: ToolMetrics;
}

export function defaultFilters(): SessionFilterState {
  return {
    search: '',
    dateFrom: '',
    dateTo: '',
    model: '',
    showActive: true,
    showArchived: true,
    showSubagents: true,
  };
}

export function zeroTotals(): TokenTotals {
  return {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: 0,
  };
}

export function addTotals(target: TokenTotals, value: TokenTotals): void {
  target.input_tokens += value.input_tokens;
  target.cached_input_tokens += value.cached_input_tokens;
  target.output_tokens += value.output_tokens;
  target.reasoning_output_tokens += value.reasoning_output_tokens;
  target.total_tokens += value.total_tokens;
}

export function zeroToolMetrics(): ToolMetrics {
  return { calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0,
    successes: 0, failures: 0, unknown: 0, mutation_targets: 0, one_shot_mutations: 0,
    retry_count: 0, duration_ms: 0, output_bytes: 0 };
}

function addToolMetrics(target: ToolMetrics, value: ToolMetrics): void {
  for (const key of Object.keys(target) as (keyof ToolMetrics)[]) target[key] += value[key];
}

export function toUtcIso(local: string): string | null {
  if (!local) return null;
  const date = new Date(local);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

export function isSubagent(session: SessionSummary): boolean {
  return Boolean(
    session.parent_thread_id || session.agent_path || session.source === 'subagent',
  );
}

export function sessionName(session: Pick<
  SessionSummary,
  'thread_name' | 'first_user_message' | 'working_directory' | 'id'
>): string {
  if (session.thread_name) return session.thread_name;
  if (session.first_user_message) return session.first_user_message;
  if (session.working_directory) {
    const parts = session.working_directory.replace(/\\/g, '/').split('/');
    const base = parts[parts.length - 1];
    if (base) return base;
  }
  return session.id.slice(0, 8);
}

export function filterSessions<T extends SessionSummary>(
  sessions: Iterable<T>,
  scope: ViewScope,
  filters: SessionFilterState,
  includeDate = true,
): T[] {
  const search = filters.search.toLowerCase();
  const from = includeDate ? toUtcIso(filters.dateFrom) : null;
  const to = includeDate ? toUtcIso(filters.dateTo) : null;
  const result: T[] = [];

  for (const session of sessions) {
    if (scope !== 'all' && session.harness !== scope) continue;
    if (session.archived && !filters.showArchived) continue;
    if (!session.archived && !filters.showActive) continue;
    if (!filters.showSubagents && isSubagent(session)) continue;
    if (filters.model && session.model !== filters.model) continue;
    if (from && session.last_event_at < from) continue;
    if (to && session.started_at > to) continue;
    if (search) {
      const haystack = [
        session.thread_name ?? '',
        session.id,
        session.first_user_message ?? '',
        session.working_directory ?? '',
        session.agent_path ?? '',
        session.agent_nickname ?? '',
        session.harness,
      ]
        .join('\0')
        .toLowerCase();
      if (!haystack.includes(search)) continue;
    }
    result.push(session);
  }

  return result;
}

export function usesApiPricing(session: SessionSummary, rates: RateCard): boolean {
  return session.harness === 'codex' && Object.keys(rates.api_models ?? {}).length > 0;
}

export function displayCurrency(session: SessionSummary, rates: RateCard): string {
  return usesApiPricing(session, rates) ? 'USD' : harnessCurrency(rates, session.harness);
}

export function projectSession<T extends SessionSummary>(
  session: T,
  rates: RateCard | null,
  range: RangeTotals | undefined,
  dateScoped: boolean,
): SessionProjection<T> {
  const cached = projectionCache.get(session);
  if (cached && cached.rates === rates && cached.range === range && cached.dateScoped === dateScoped) {
    return cached.value as SessionProjection<T>;
  }
  const tokens = dateScoped ? (range?.tokens ?? zeroTotals()) : session.tokens_total;
  if (!rates) {
    const value: SessionProjection<T> = {
      session,
      tokens,
      planCost: 0,
      apiCost: null,
      displayCost: 0,
      currency: session.harness === 'claude_code' ? 'USD' : 'credits',
      missingModels: [],
    };
    projectionCache.set(session, { rates, range, dateScoped, value });
    return value;
  }

  const buckets = dateScoped ? (range?.buckets ?? []) : session.buckets;
  const plan = dateScoped
    ? creditsFromBuckets(buckets, rates, session.harness)
    : computeSummaryCredits(session, rates);
  const api = apiCostFromBuckets(buckets, rates, session.harness);
  const useApi = usesApiPricing(session, rates);
  const directApiCost = api && api.missingModels.length === 0 ? api.total : null;

  const value: SessionProjection<T> = {
    session,
    tokens,
    planCost: plan.total,
    // Exports use this field as a direct-rate estimate. Fallback-priced or
    // unavailable models stay explicit instead of becoming a misleading $0.
    apiCost: directApiCost,
    displayCost: useApi ? (api?.total ?? 0) : plan.total,
    currency: useApi ? 'USD' : harnessCurrency(rates, session.harness),
    missingModels: useApi ? (api?.missingModels ?? []) : plan.missingModels,
  };
  projectionCache.set(session, { rates, range, dateScoped, value });
  return value;
}

export function projectSessions<T extends SessionSummary>(
  sessions: Iterable<T>,
  rates: RateCard | null,
  ranges: Record<string, RangeTotals>,
  dateScoped: boolean,
): Map<string, SessionProjection<T>> {
  const result = new Map<string, SessionProjection<T>>();
  for (const session of sessions) {
    result.set(session.id, projectSession(session, rates, ranges[session.id], dateScoped));
  }
  return result;
}

function priceBucket(bucket: TierBucket, harness: Harness, rates: RateCard): {
  cost: number;
  currency: string;
  fallbackUsed: boolean;
} {
  const useApi = harness === 'codex' && Object.keys(rates.api_models ?? {}).length > 0;
  const priced = useApi
    ? apiCostFromBuckets([bucket], rates, harness)
    : creditsFromBuckets([bucket], rates, harness);
  return {
    cost: priced?.total ?? 0,
    currency: useApi ? 'USD' : harnessCurrency(rates, harness),
    fallbackUsed: (priced?.missingModels.length ?? 0) > 0,
  };
}

export function aggregateModelMetrics<T extends SessionSummary>(
  sessions: Iterable<T>,
  ranges: Record<string, RangeTotals> | null,
  rates: RateCard | null,
): ModelMetric[] {
  if (!ranges || !rates) return [];
  const grouped = new Map<string, ModelMetric>();

  for (const session of sessions) {
    const range = ranges[session.id];
    if (!range) continue;
    for (const bucket of range.buckets) {
      const key = `${session.harness}\0${bucket.model}`;
      const priced = priceBucket(bucket, session.harness, rates);
      let metric = grouped.get(key);
      if (!metric) {
        metric = {
          harness: session.harness,
          model: bucket.model,
          tokens: zeroTotals(),
          cost: 0,
          currency: priced.currency,
          fallbackUsed: false,
          tools: zeroToolMetrics(),
        };
        grouped.set(key, metric);
      }
      addTotals(metric.tokens, bucket.tokens);
      metric.cost += priced.cost;
      metric.fallbackUsed ||= priced.fallbackUsed;
    }
    for (const [model, tools] of Object.entries(range.tool_metrics_by_model ?? {})) {
      const key = `${session.harness}\0${model}`;
      let metric = grouped.get(key);
      if (!metric) {
        metric = { harness: session.harness, model, tokens: zeroTotals(), cost: 0,
          currency: displayCurrency(session, rates), fallbackUsed: false, tools: zeroToolMetrics() };
        grouped.set(key, metric);
      }
      addToolMetrics(metric.tools, tools);
    }
  }

  return [...grouped.values()].sort((a, b) => b.cost - a.cost);
}

function csvCell(value: string | number | boolean | null): string {
  const text = value === null ? '' : String(value);
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

export function exportRows<T extends SessionSummary>(
  projections: Iterable<SessionProjection<T>>,
  includeWorkingDirectory = false,
): Record<string, string | number | boolean | null>[] {
  return [...projections].map(({ session, tokens, planCost, apiCost, currency, missingModels }) => {
    const row: Record<string, string | number | boolean | null> = {
      id: session.id,
      harness: session.harness,
      name: session.thread_name ?? session.id.slice(0, 8),
      started_at: session.started_at,
      last_event_at: session.last_event_at,
      archived: session.archived,
      subagent: isSubagent(session),
      model: session.model,
      turns: session.total_turns,
      input_tokens: tokens.input_tokens,
      cached_input_tokens: tokens.cached_input_tokens,
      output_tokens: tokens.output_tokens,
      reasoning_output_tokens: tokens.reasoning_output_tokens,
      total_tokens: tokens.total_tokens,
      codex_credits: session.harness === 'codex' ? planCost : null,
      codex_estimated_api_usd: session.harness === 'codex' ? apiCost : null,
      claude_estimated_usd: session.harness === 'claude_code' ? planCost : null,
      display_currency: currency,
      fallback_models: missingModels.join(';'),
    };
    if (includeWorkingDirectory) row.working_directory = session.working_directory;
    return row;
  });
}

export function rowsToCsv(rows: Record<string, string | number | boolean | null>[]): string {
  if (rows.length === 0) return '';
  const headers = Object.keys(rows[0]);
  const lines = [headers.map(csvCell).join(',')];
  for (const row of rows) lines.push(headers.map((header) => csvCell(row[header])).join(','));
  return `${lines.join('\r\n')}\r\n`;
}
