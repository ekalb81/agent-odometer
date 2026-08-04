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
  unpricedModels: string[];
  /** Aggregate buckets omit event timestamps and request input, so a dated
   * conditional API scenario cannot be computed honestly here. */
  timeAwareApiStatus: 'unavailable_requires_request_history' | null;
  pricingCatalogAvailable: boolean;
  rateCardVersion: number | null;
  rateCardFetchedAt: string | null;
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
  unpriced: boolean;
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

/** A privacy-preserving project label for list views. Session summaries retain
 * the working directory for local actions, but the grid never needs to expose
 * the full path: its final segment is stable for display and grouping. */
export function repositoryLabel(session: Pick<SessionSummary, 'working_directory'>): string | null {
  if (!session.working_directory) return null;
  const parts = session.working_directory.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts.at(-1) || null;
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
      unpricedModels: [],
      timeAwareApiStatus: null,
      pricingCatalogAvailable: false,
      rateCardVersion: null,
      rateCardFetchedAt: null,
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
  const directApiCost = api && api.missingModels.length === 0 && api.unpricedModels.length === 0
    ? api.total
    : null;

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
    unpricedModels: useApi ? (api?.unpricedModels ?? []) : plan.unpricedModels,
    timeAwareApiStatus: rates.pricing_catalog.rate_periods.some((period) =>
      period.surface === (session.harness === 'codex' ? 'openai_api_usd' : 'anthropic_api_usd'))
      ? 'unavailable_requires_request_history'
      : null,
    pricingCatalogAvailable: rates.pricing_catalog.rate_periods.length > 0,
    rateCardVersion: rates.version,
    rateCardFetchedAt: rates.fetched_at,
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
    result.set(session.storage_id, projectSession(session, rates, ranges[session.storage_id], dateScoped));
  }
  return result;
}

function priceBucket(bucket: TierBucket, harness: Harness, rates: RateCard): {
  cost: number;
  currency: string;
  fallbackUsed: boolean;
  unpriced: boolean;
} {
  const useApi = harness === 'codex' && Object.keys(rates.api_models ?? {}).length > 0;
  const priced = useApi
    ? apiCostFromBuckets([bucket], rates, harness)
    : creditsFromBuckets([bucket], rates, harness);
  return {
    cost: priced?.total ?? 0,
    currency: useApi ? 'USD' : harnessCurrency(rates, harness),
    fallbackUsed: (priced?.missingModels.length ?? 0) > 0,
    unpriced: (priced?.unpricedModels.length ?? 0) > 0,
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
    const range = ranges[session.storage_id];
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
          unpriced: false,
          tools: zeroToolMetrics(),
        };
        grouped.set(key, metric);
      }
      addTotals(metric.tokens, bucket.tokens);
      metric.cost += priced.cost;
      metric.fallbackUsed ||= priced.fallbackUsed;
      metric.unpriced ||= priced.unpriced;
    }
    for (const [model, tools] of Object.entries(range.tool_metrics_by_model ?? {})) {
      const key = `${session.harness}\0${model}`;
      let metric = grouped.get(key);
      if (!metric) {
        metric = { harness: session.harness, model, tokens: zeroTotals(), cost: 0,
          currency: displayCurrency(session, rates), fallbackUsed: false, unpriced: false,
          tools: zeroToolMetrics() };
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
  return [...projections].map(({
    session,
    tokens,
    planCost,
    apiCost,
    currency,
    missingModels,
    unpricedModels,
    timeAwareApiStatus,
    pricingCatalogAvailable,
    rateCardVersion,
    rateCardFetchedAt,
  }) => {
    const row: Record<string, string | number | boolean | null> = {
      id: session.id,
      storage_id: session.storage_id,
      harness: session.harness,
      name: session.thread_name ?? session.id.slice(0, 8),
      started_at: session.started_at,
      last_event_at: session.last_event_at,
      archived: session.archived,
      source_availability: session.source_availability,
      subagent: isSubagent(session),
      parent_thread_id: session.parent_thread_id,
      model: session.model,
      turns: session.total_turns,
      input_tokens: tokens.input_tokens,
      cached_input_tokens: tokens.cached_input_tokens,
      output_tokens: tokens.output_tokens,
      reasoning_output_tokens: tokens.reasoning_output_tokens,
      total_tokens: tokens.total_tokens,
      codex_credits: session.harness === 'codex' ? planCost : null,
      codex_estimated_api_usd: session.harness === 'codex' ? apiCost : null,
      codex_time_aware_api_usd: null,
      codex_time_aware_api_status: session.harness === 'codex' ? timeAwareApiStatus : null,
      time_aware_api_status: timeAwareApiStatus,
      pricing_catalog_available: pricingCatalogAvailable,
      rate_card_version: rateCardVersion,
      rate_card_fetched_at: rateCardFetchedAt,
      cache_write_pricing: session.harness === 'codex' && pricingCatalogAvailable
        ? 'unmodeled_not_observed'
        : null,
      claude_estimated_usd: session.harness === 'claude_code' ? planCost : null,
      display_currency: currency,
      fallback_models: missingModels.join(';'),
      unpriced_models: unpricedModels.join(';'),
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

/** Minimum a row needs to be ordered: identity plus its day-group anchor. */
export interface OrderableSession {
  storage_id: string;
  startedMs: number;
}

export interface OrderOptions<T extends OrderableSession> {
  /** Storage id of the row's parent, or null when it is a top-level thread. */
  parentOf: (session: T) => string | null;
  /** Parents whose subagent rows are hidden. Ignored when flat. */
  collapsed?: ReadonlySet<string>;
  /**
   * Flat drops the parent/child nesting so the caller's sort ranks every
   * thread against every other. Tree keeps children directly beneath their
   * parent, which means a child can never outrank its parent.
   */
  flat?: boolean;
}

/**
 * Orders already-sorted rows for the session grid and reports which day group
 * each row belongs to.
 *
 * In tree mode a child inherits its parent's anchor, so a nested row never
 * splits a day section — but a subagent that ran today under a weeks-old
 * parent is then filed under the parent's start day. Flat mode anchors every
 * row to its own start time, which is what makes long-lived parents with many
 * short subagent runs readable.
 */
export function orderSessionsForDisplay<T extends OrderableSession>(
  sorted: readonly T[],
  options: OrderOptions<T>,
): { list: T[]; anchorMs: Map<string, number> } {
  const anchorMs = new Map<string, number>();
  if (options.flat) {
    for (const session of sorted) anchorMs.set(session.storage_id, session.startedMs);
    return { list: [...sorted], anchorMs };
  }

  const collapsed = options.collapsed ?? new Set<string>();
  const ids = new Set(sorted.map((session) => session.storage_id));
  const children = new Map<string, T[]>();
  const roots: T[] = [];
  for (const session of sorted) {
    const parentId = options.parentOf(session);
    // A child whose parent was filtered out becomes a root rather than
    // disappearing with it.
    if (parentId && ids.has(parentId)) {
      const arr = children.get(parentId);
      if (arr) arr.push(session);
      else children.set(parentId, [session]);
    } else {
      roots.push(session);
    }
  }

  const list: T[] = [];
  const seen = new Set<string>();
  // Descendants of a collapsed parent: withheld on purpose, so the
  // unreachable sweep below must not resurrect them as top-level rows.
  const withheld = new Set<string>();
  const withhold = (id: string) => {
    for (const child of children.get(id) ?? []) {
      if (withheld.has(child.storage_id)) continue;
      withheld.add(child.storage_id);
      withhold(child.storage_id);
    }
  };
  const append = (session: T, anchor: number) => {
    // Guards against a cyclic parent chain from corrupt lineage metadata.
    if (seen.has(session.storage_id)) return;
    seen.add(session.storage_id);
    list.push(session);
    anchorMs.set(session.storage_id, anchor);
    if (collapsed.has(session.storage_id)) {
      withhold(session.storage_id);
      return;
    }
    for (const child of children.get(session.storage_id) ?? []) append(child, anchor);
  };
  for (const root of roots) append(root, root.startedMs);
  // A parent cycle leaves its members unreachable from every root. Emit them
  // at top level rather than dropping rows the caller filtered in.
  for (const session of sorted) {
    if (!seen.has(session.storage_id) && !withheld.has(session.storage_id)) {
      append(session, session.startedMs);
    }
  }
  return { list, anchorMs };
}
