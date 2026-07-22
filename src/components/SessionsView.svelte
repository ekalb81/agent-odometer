<script lang="ts">
  import { onDestroy } from 'svelte';
  import { sessionsStore, type TrackedSession } from '../lib/stores/sessions.svelte';
  import { scanStore } from '../lib/stores/scan.svelte';
  import { rates } from '../lib/stores/rates';
  import { apiCostFromBuckets, computeSummaryCredits, creditsFromBuckets, formatCredits, harnessCurrency } from '../lib/credits';
  import { getSessionDetails, sessionsInRange } from '../lib/ipc';
  import type { Harness, RangeTotals, Session, TierBucket, TokenTotals } from '../lib/types';
  import type { FilterState } from './Filters.svelte';
  import DetailPane from './DetailPane.svelte';

  interface Props {
    harness?: Harness;
    active?: boolean;
    filters: FilterState;
    onfilterschange: (f: FilterState) => void;
  }

  let { harness = 'codex' as Harness, active = true, filters, onfilterschange }: Props = $props();

  const fmt = new Intl.NumberFormat();
  const fmt2 = new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  const fmt4 = new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 4 });
  const fmtUsd = (amount: number) => formatCredits(amount, 'USD');

  /** Plain-number money cells: 2 decimals, but sub-cent amounts keep enough
   *  significant digits to stay honest (same rule as formatCredits). */
  function fmtAmount(n: number): string {
    return n !== 0 && Math.abs(n) < 0.005 ? fmt4.format(n) : fmt2.format(n);
  }

  // Codex additionally shows what the usage would cost at OpenAI API rates;
  // its money column and analytics use that figure. Claude Code prices at
  // Anthropic API rates directly.
  const showApiCost = $derived(harness === 'codex' && Object.keys($rates?.api_models ?? {}).length > 0);

  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

  function fmtCompact(n: number): string {
    if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
    if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
    if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
    return String(n);
  }

  function sessionName(s: {
    thread_name: string | null;
    first_user_message: string | null;
    working_directory: string | null;
    id: string;
  }): string {
    if (s.thread_name) return s.thread_name;
    if (s.first_user_message) return s.first_user_message;
    if (s.working_directory) {
      const parts = s.working_directory.replace(/\\/g, '/').split('/');
      const base = parts[parts.length - 1];
      if (base) return base;
    }
    return s.id.slice(0, 8);
  }

  function truncate(str: string, max: number): string {
    return str.length > max ? str.slice(0, max) + '…' : str;
  }

  function isSub(s: TrackedSession): boolean {
    return Boolean(s.parent_thread_id || s.agent_path || s.source === 'subagent');
  }

  function isPulsing(lastUpdatedAt: number): boolean {
    // pulseGen is bumped once ~2s after the last store change so highlights
    // expire even when no further updates trigger a re-render.
    void pulseGen;
    return Date.now() - lastUpdatedAt < 2000;
  }

  let pulseGen = $state(0);
  $effect(() => {
    void sessionsStore.map;
    const t = setTimeout(() => {
      pulseGen += 1;
    }, 2100);
    return () => clearTimeout(t);
  });

  function defaultFilters(): FilterState {
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

  // ---------------------------------------------------------------------------
  // Sort state — 3-state (asc → desc → cleared). Cleared shows the
  // day-grouped view ordered by start time.
  // ---------------------------------------------------------------------------
  type SortKey = 'name' | 'started' | 'model' | 'total' | 'cost' | null;
  type SortDir = 'asc' | 'desc';

  let sortKey = $state<SortKey>(null);
  let sortDir = $state<SortDir>('asc');

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      if (sortDir === 'asc') {
        sortDir = 'desc';
      } else {
        // Third click clears the sort.
        sortKey = null;
        sortDir = 'asc';
      }
    } else {
      sortKey = key;
      sortDir = 'asc';
    }
  }

  function ariaSortAttr(key: SortKey): 'ascending' | 'descending' | 'none' {
    if (sortKey !== key) return 'none';
    return sortDir === 'asc' ? 'ascending' : 'descending';
  }

  function caretFor(key: SortKey): string {
    if (sortKey !== key) return '';
    return sortDir === 'asc' ? ' ▲' : ' ▼';
  }

  // ---------------------------------------------------------------------------
  // Filtered list. All list computation is gated on `active`: the inactive
  // harness tab stays mounted (so its filters survive switching) but must
  // not re-filter/re-sort/re-price on every live update.
  // ---------------------------------------------------------------------------
  const allSessions = $derived(
    active ? [...sessionsStore.map.values()].filter((s) => s.harness === harness) : [],
  );

  // Convert datetime-local strings (local time) to UTC ISO once, so the rest
  // of the pipeline can do lexical comparison against the UTC ISO timestamps
  // we store on sessions and history points.
  function toUtcIso(local: string): string | null {
    if (!local) return null;
    const d = new Date(local);
    return isNaN(d.getTime()) ? null : d.toISOString();
  }
  const fromUtc = $derived(toUtcIso(filters.dateFrom));
  const toUtc = $derived(toUtcIso(filters.dateTo));

  // Everything except the date bounds. Kept separate so the analytics
  // previous-window totals can include sessions that were active then but
  // fall outside the current date range.
  const filteredNoDate = $derived((() => {
    const lc = filters.search.toLowerCase();
    return allSessions.filter((s) => {
      // Status filter.
      if (s.archived && !filters.showArchived) return false;
      if (!s.archived && !filters.showActive) return false;
      if (!filters.showSubagents && isSub(s)) return false;

      // Model filter.
      if (filters.model && s.model !== filters.model) return false;

      // Free-text search.
      if (lc) {
        const haystack = [
          s.thread_name ?? '',
          s.id,
          s.first_user_message ?? '',
          s.working_directory ?? '',
          s.agent_path ?? '',
          s.agent_nickname ?? '',
        ].join('\0').toLowerCase();
        if (!haystack.includes(lc)) return false;
      }

      return true;
    });
  })());

  // Datetime range — overlap semantics: include any session whose
  // [started_at, last_event_at] window intersects the filter range.
  // Comparison is lexical on UTC ISO strings, which sorts chronologically.
  const filtered = $derived(
    filteredNoDate.filter(
      (s) => !(fromUtc && s.last_event_at < fromUtc) && !(toUtc && s.started_at > toUtc),
    ),
  );

  // True when the user has narrowed by date — drives whether per-session
  // tokens and costs are "all-time" or scoped to the visible window.
  const dateScoped = $derived(Boolean(filters.dateFrom || filters.dateTo));

  // ---------------------------------------------------------------------------
  // Date-scoped rollups come from the backend (summaries don't carry event
  // histories). Refetched, debounced, when the range or the session set changes.
  // ---------------------------------------------------------------------------
  let rangeTotals = $state<Record<string, RangeTotals>>({});
  let rangeFetchTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const from = fromUtc;
    const to = toUtc;
    // Depend on the session map so live session updates refresh range data;
    // skip entirely while this tab is hidden.
    void sessionsStore.map;
    if (!active) return;
    if (!from && !to) {
      rangeTotals = {};
      return;
    }
    if (rangeFetchTimer !== null) clearTimeout(rangeFetchTimer);
    rangeFetchTimer = setTimeout(() => {
      sessionsInRange(from, to)
        .then((result) => {
          // Drop stale responses if the filter moved on meanwhile.
          if (fromUtc === from && toUtc === to) rangeTotals = result;
        })
        .catch((e) => console.error('sessions_in_range failed:', e));
    }, 250);
    return () => {
      if (rangeFetchTimer !== null) clearTimeout(rangeFetchTimer);
    };
  });

  function zeroTotals(): TokenTotals {
    return {
      input_tokens: 0,
      cached_input_tokens: 0,
      output_tokens: 0,
      reasoning_output_tokens: 0,
      total_tokens: 0,
    };
  }

  // Per-session display values: tokens AND costs, both scoped to the date
  // filter when one is active so the row numbers add up to the totals row.
  const sessionDisplayMap = $derived((() => {
    const r = $rates;
    const out = new Map<
      string,
      { tokens: TokenTotals; total: number; refCost: number; apiTotal: number; missingModels: string[] }
    >();
    for (const s of filtered) {
      const rt = dateScoped ? rangeTotals[s.id] : undefined;
      const tokens = dateScoped ? (rt?.tokens ?? zeroTotals()) : s.tokens_total;
      if (!r) {
        out.set(s.id, { tokens, total: 0, refCost: 0, apiTotal: 0, missingModels: [] });
        continue;
      }
      // Reference cost (à-la-carte equivalent) is always all-time for the
      // unlimited-plan tooltip — that's the figure people compare against.
      const allTime = computeSummaryCredits(s, r);
      const credits = dateScoped ? creditsFromBuckets(rt?.buckets ?? [], r, s.harness) : allTime;
      const buckets = dateScoped ? (rt?.buckets ?? []) : s.buckets;
      const apiTotal = apiCostFromBuckets(buckets, r, s.harness)?.total ?? 0;
      out.set(s.id, {
        tokens,
        total: credits.total,
        refCost: allTime.total,
        apiTotal,
        missingModels: credits.missingModels,
      });
    }
    return out;
  })());

  /** The money-column value for a session (Est.$ on Codex, Cost elsewhere). */
  function costOf(id: string): number {
    const d = sessionDisplayMap.get(id);
    if (!d) return 0;
    return showApiCost ? d.apiTotal : d.total;
  }

  function compareSession(a: TrackedSession, b: TrackedSession): number {
    if (sortKey === null) {
      // Default: start time desc — matches the day-grouped presentation.
      return b.startedMs - a.startedMs;
    }
    let cmp = 0;
    switch (sortKey) {
      case 'name':
        cmp = sessionName(a).localeCompare(sessionName(b));
        break;
      case 'started':
        cmp = a.startedMs - b.startedMs;
        break;
      case 'model':
        cmp = (a.model ?? '').localeCompare(b.model ?? '');
        break;
      case 'total': {
        const at = sessionDisplayMap.get(a.id)?.tokens ?? a.tokens_total;
        const bt = sessionDisplayMap.get(b.id)?.tokens ?? b.tokens_total;
        cmp = at.total_tokens - bt.total_tokens;
        break;
      }
      case 'cost':
        cmp = costOf(a.id) - costOf(b.id);
        break;
    }
    return sortDir === 'asc' ? cmp : -cmp;
  }

  // In the default order, subagent rows tuck in directly beneath their parent
  // (when the parent is in view); explicit column sorts keep strict order.
  // anchorMs records which day group a row belongs to — children inherit
  // their parent's group so a nested row never splits a day section.
  const displayedWithAnchors = $derived((() => {
    const sorted = [...filtered].sort(compareSession);
    const anchorMs = new Map<string, number>();
    if (sortKey !== null) {
      for (const s of sorted) anchorMs.set(s.id, s.startedMs);
      return { list: sorted, anchorMs };
    }
    const ids = new Set(sorted.map((s) => s.id));
    const children = new Map<string, TrackedSession[]>();
    const roots: TrackedSession[] = [];
    for (const s of sorted) {
      if (s.parent_thread_id && ids.has(s.parent_thread_id)) {
        const arr = children.get(s.parent_thread_id);
        if (arr) arr.push(s);
        else children.set(s.parent_thread_id, [s]);
      } else {
        roots.push(s);
      }
    }
    const list: TrackedSession[] = [];
    const append = (s: TrackedSession, anchor: number) => {
      list.push(s);
      anchorMs.set(s.id, anchor);
      if (collapsedParents.has(s.id)) return;
      for (const c of children.get(s.id) ?? []) append(c, anchor);
    };
    for (const r of roots) append(r, r.startedMs);
    return { list, anchorMs };
  })());

  const displayed = $derived(displayedWithAnchors.list);

  // Collapsed parents hide their nested subagent rows (default order only —
  // explicit column sorts show a flat list where nesting doesn't apply).
  let collapsedParents = $state<ReadonlySet<string>>(new Set());
  function toggleCollapsed(id: string) {
    const next = new Set(collapsedParents);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    collapsedParents = next;
  }

  // Combined cost (session + all in-view descendant subagents) per row that
  // has in-view children. Row cost stays per-thread; this feeds the Σ line.
  const combinedCost = $derived((() => {
    const childrenOf = new Map<string, TrackedSession[]>();
    const ids = new Set(filtered.map((s) => s.id));
    for (const s of filtered) {
      if (s.parent_thread_id && ids.has(s.parent_thread_id)) {
        const arr = childrenOf.get(s.parent_thread_id);
        if (arr) arr.push(s);
        else childrenOf.set(s.parent_thread_id, [s]);
      }
    }
    const memo = new Map<string, number>();
    const total = (id: string): number => {
      const cached = memo.get(id);
      if (cached !== undefined) return cached;
      memo.set(id, costOf(id)); // pre-set guards against parent-id cycles
      let sum = costOf(id);
      for (const c of childrenOf.get(id) ?? []) sum += total(c.id);
      memo.set(id, sum);
      return sum;
    };
    const out = new Map<string, number>();
    for (const s of filtered) {
      if ((childrenOf.get(s.id)?.length ?? 0) > 0) out.set(s.id, total(s.id));
    }
    return out;
  })());

  // ---------------------------------------------------------------------------
  // Day groups ("Today · Jul 19", "Yesterday…", "Earlier this week", months).
  // Only shown in the default (unsorted) order.
  // ---------------------------------------------------------------------------
  const dayBoundaries = $derived((() => {
    // pulseGen ticks keep "today" fresh across midnight without a timer.
    void pulseGen;
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    return {
      today: today.getTime(),
      yesterday: today.getTime() - 86_400_000,
      weekAgo: today.getTime() - 6 * 86_400_000,
    };
  })());

  function fmtMonthDay(ms: number): string {
    const d = new Date(ms);
    return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  }

  function groupLabelFor(startedMs: number): string {
    const b = dayBoundaries;
    if (startedMs >= b.today) return `Today · ${fmtMonthDay(b.today)}`;
    if (startedMs >= b.yesterday) return `Yesterday · ${fmtMonthDay(b.yesterday)}`;
    if (startedMs >= b.weekAgo) return 'Earlier this week';
    const d = new Date(startedMs);
    return `${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
  }

  const groups = $derived((() => {
    if (sortKey !== null) return [{ label: null as string | null, sessions: displayed }];
    const out: { label: string | null; sessions: TrackedSession[] }[] = [];
    for (const s of displayed) {
      const label = groupLabelFor(displayedWithAnchors.anchorMs.get(s.id) ?? s.startedMs);
      const last = out[out.length - 1];
      if (last && last.label === label) last.sessions.push(s);
      else out.push({ label, sessions: [s] });
    }
    return out;
  })());

  /** Started column: time-of-day for today/yesterday, date otherwise. */
  function fmtStarted(startedMs: number): string {
    if (startedMs >= dayBoundaries.yesterday) {
      const d = new Date(startedMs);
      const pad = (n: number) => String(n).padStart(2, '0');
      return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    }
    return fmtMonthDay(startedMs);
  }

  // Children per parent id → "N subagents" chips on parent rows.
  const childCounts = $derived((() => {
    const m = new Map<string, number>();
    for (const s of allSessions) {
      if (s.parent_thread_id) m.set(s.parent_thread_id, (m.get(s.parent_thread_id) ?? 0) + 1);
    }
    return m;
  })());

  const filteredTotal = $derived(
    filtered.reduce((sum, s) => sum + (sessionDisplayMap.get(s.id)?.tokens.total_tokens ?? 0), 0),
  );

  // Money total for the pinned totals row (matches the column semantics).
  const costTotal = $derived(filtered.reduce((sum, s) => sum + costOf(s.id), 0));

  // ---------------------------------------------------------------------------
  // Analytics band: spend-by-day series + window totals for the delta pills.
  // Day buckets come from sessions_in_range (summaries carry no history);
  // pricing happens client-side so rate-card edits recompute without refetch.
  // ---------------------------------------------------------------------------
  interface DayBucket {
    label: string;
    data: Record<string, RangeTotals>;
  }
  let analyticsBuckets = $state<DayBucket[]>([]);
  let analyticsPrev = $state<Record<string, RangeTotals> | null>(null);
  let analyticsCurrent = $state<Record<string, RangeTotals> | null>(null);
  let analyticsTimer: ReturnType<typeof setTimeout> | null = null;

  const DAY_MS = 86_400_000;
  const MAX_CHART_BUCKETS = 14;

  // The chart window: the date filter when set, else the last 7 days.
  const windowBounds = $derived((() => {
    void pulseGen; // stay fresh across midnight
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const endMs = toUtc ? new Date(toUtc).getTime() : Date.now();
    let startMs: number;
    if (fromUtc) {
      startMs = new Date(fromUtc).getTime();
    } else if (toUtc) {
      startMs = endMs - 7 * DAY_MS;
    } else {
      startMs = today.getTime() - 6 * DAY_MS;
    }
    if (startMs >= endMs) startMs = endMs - DAY_MS;
    return { startMs, endMs };
  })());

  const windowLabel = $derived(dateScoped ? 'in range' : 'last 7 days');

  $effect(() => {
    const { startMs, endMs } = windowBounds;
    void sessionsStore.map;
    if (!active) return;
    if (analyticsTimer !== null) clearTimeout(analyticsTimer);
    analyticsTimer = setTimeout(async () => {
      // Day-aligned buckets, coalesced so long ranges stay ≤14 fetches.
      const dayStart = (ms: number) => {
        const d = new Date(ms);
        d.setHours(0, 0, 0, 0);
        return d.getTime();
      };
      const totalDays = Math.max(1, Math.ceil((endMs - dayStart(startMs)) / DAY_MS));
      const daysPerBucket = Math.max(1, Math.ceil(totalDays / MAX_CHART_BUCKETS));
      const bounds: { from: number; to: number }[] = [];
      for (let t = dayStart(startMs); t < endMs; t += daysPerBucket * DAY_MS) {
        bounds.push({ from: Math.max(t, startMs), to: Math.min(t + daysPerBucket * DAY_MS - 1, endMs) });
      }
      const prevFrom = startMs - (endMs - startMs);
      try {
        const [current, prev, ...days] = await Promise.all([
          sessionsInRange(new Date(startMs).toISOString(), new Date(endMs).toISOString()),
          sessionsInRange(new Date(prevFrom).toISOString(), new Date(startMs - 1).toISOString()),
          ...bounds.map((b) => sessionsInRange(new Date(b.from).toISOString(), new Date(b.to).toISOString())),
        ]);
        // Drop stale responses if the window moved on meanwhile.
        if (windowBounds.startMs !== startMs || windowBounds.endMs !== endMs) return;
        analyticsCurrent = current;
        analyticsPrev = prev;
        analyticsBuckets = days.map((data, i) => ({ label: fmtMonthDay(bounds[i].from), data }));
      } catch (e) {
        console.error('analytics sessions_in_range failed:', e);
      }
    }, 300);
    return () => {
      if (analyticsTimer !== null) clearTimeout(analyticsTimer);
    };
  });

  /** Price one range-rollup map for the sessions currently in view. Uses the
   *  non-date-filtered set: the rollup itself scopes usage to its window, and
   *  the previous window's sessions may not intersect the current date range. */
  function priceRange(data: Record<string, RangeTotals> | null): { cost: number; tokens: number } {
    const r = $rates;
    if (!data || !r) return { cost: 0, tokens: 0 };
    const buckets: TierBucket[] = [];
    let tokens = 0;
    for (const s of filteredNoDate) {
      const rt = data[s.id];
      if (!rt) continue;
      tokens += rt.tokens.total_tokens;
      buckets.push(...rt.buckets);
    }
    const priced = showApiCost
      ? apiCostFromBuckets(buckets, r, harness)
      : creditsFromBuckets(buckets, r, harness);
    return { cost: priced?.total ?? 0, tokens };
  }

  const spendSeries = $derived(analyticsBuckets.map((b) => ({ label: b.label, ...priceRange(b.data) })));
  const windowTotals = $derived(priceRange(analyticsCurrent));
  const prevTotals = $derived(priceRange(analyticsPrev));

  // ---------------------------------------------------------------------------
  // Window-scoped stats for the rest of the analytics band. Every card reads
  // from the same rollup map as the spend card (the date filter when set, else
  // the last 7 days) and the same non-date filters, so the band tells one
  // consistent story. A session counts as active when it has usage in window.
  // ---------------------------------------------------------------------------
  const windowStats = $derived((() => {
    const r = $rates;
    const data = analyticsCurrent;
    const out = {
      sessionCount: 0,
      byModel: [] as { model: string; cost: number }[],
      credits: { billedTotal: 0, unlimitedCount: 0 },
      subagents: { count: 0, cost: 0 },
      allUnlimited: false,
    };
    if (!data || !r) return out;
    const buckets: TierBucket[] = [];
    for (const s of filteredNoDate) {
      const rt = data[s.id];
      if (!rt || rt.tokens.total_tokens === 0) continue;
      out.sessionCount++;
      buckets.push(...rt.buckets);
      if (harness === 'codex') {
        if (s.credits_unlimited === true) out.credits.unlimitedCount++;
        else out.credits.billedTotal += creditsFromBuckets(rt.buckets, r, s.harness).total;
      }
      if (isSub(s)) {
        out.subagents.count++;
        const priced = showApiCost
          ? apiCostFromBuckets(rt.buckets, r, s.harness)
          : creditsFromBuckets(rt.buckets, r, s.harness);
        out.subagents.cost += priced?.total ?? 0;
      }
    }
    const priced = showApiCost
      ? apiCostFromBuckets(buckets, r, harness)
      : creditsFromBuckets(buckets, r, harness);
    out.byModel = [...(priced?.byModel ?? [])].sort((a, b) => b.cost - a.cost);
    out.allUnlimited = out.sessionCount > 0 && out.credits.unlimitedCount === out.sessionCount;
    return out;
  })());

  function deltaPct(cur: number, prev: number): number | null {
    if (prev <= 0) return null;
    const pct = Math.round(((cur - prev) / prev) * 100);
    // A near-empty previous window produces junk percentages — not worth a pill.
    return Math.abs(pct) > 500 ? null : pct;
  }
  const costDelta = $derived(deltaPct(windowTotals.cost, prevTotals.cost));
  const tokensDelta = $derived(deltaPct(windowTotals.tokens, prevTotals.tokens));

  // Area-chart geometry (viewBox 0 0 700 72, preserveAspectRatio none).
  const chart = $derived((() => {
    const vals = spendSeries.map((p) => p.cost);
    const max = vals.reduce((m, v) => Math.max(m, v), 0);
    const n = vals.length;
    if (n === 0) return { line: '', area: '', endX: 700, endY: 66 };
    const x = (i: number) => (n === 1 ? 700 : (i * 700) / (n - 1));
    const y = (v: number) => (max > 0 ? 66 - (v / max) * 56 : 66);
    const pts = vals.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`);
    return {
      line: pts.join(' '),
      area: `${pts.join(' ')} 700,72 0,72`,
      endX: x(n - 1),
      endY: y(vals[n - 1]),
    };
  })());

  // Thin out x-axis labels for long ranges.
  const chartLabels = $derived((() => {
    const n = spendSeries.length;
    if (n === 0) return [];
    const step = Math.max(1, Math.ceil(n / 7));
    return spendSeries.filter((_, i) => i % step === 0).map((p) => p.label);
  })());

  // Cost by model — aggregated from the same window as the spend card, top four.
  const costByModel = $derived((() => {
    const sorted = windowStats.byModel;
    const max = sorted[0]?.cost ?? 0;
    const rows = sorted.slice(0, 4).map((m) => ({
      model: m.model,
      cost: m.cost,
      pct: max > 0 ? Math.max(2, Math.round((m.cost / max) * 100)) : 0,
    }));
    return { rows, more: Math.max(0, sorted.length - 4) };
  })());

  // Money formatting for analytics: USD gets the $ prefix, plan credits get
  // a plain number (the card labels carry the unit).
  const moneyIsUsd = $derived(
    showApiCost || ($rates ? /^[A-Z]{3}$/.test(harnessCurrency($rates, harness)) : false),
  );
  function fmtMoney(n: number): string {
    return moneyIsUsd ? fmtUsd(n) : fmtAmount(n);
  }

  const spendCardLabel = $derived(
    harness === 'codex'
      ? (showApiCost ? `Est. API cost · ${windowLabel}` : `Credits · ${windowLabel}`)
      : `Est. spend · ${windowLabel}`,
  );
  const spendCardNote = $derived(
    harness === 'codex'
      ? (windowStats.allUnlimited ? 'à la carte · all sessions unlimited' : 'OpenAI API rates')
      : 'Anthropic API rates',
  );

  // ---------------------------------------------------------------------------
  // Selection + detail pane. The table only holds summaries, so the full
  // session (turns, token history) is fetched on select and refreshed when
  // the session's summary is upserted by a live event. The first fetch for a
  // session is immediate; refreshes are debounced so a busy live session
  // doesn't re-serialize its full history several times a second.
  // ---------------------------------------------------------------------------
  let selectedSessionId = $state<string | null>(null);
  let selectedSession = $state<Session | null>(null);
  let detailsFetchTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const id = selectedSessionId;
    // Reactive dep: refetch details when this session's summary updates.
    void (id !== null ? sessionsStore.map.get(id)?.lastUpdatedAt : undefined);
    if (id === null) {
      selectedSession = null;
      return;
    }
    let cancelled = false;
    const fetchDetails = () => {
      getSessionDetails(id)
        .then((s) => {
          if (!cancelled && selectedSessionId === id) selectedSession = s;
        })
        .catch((e) => console.error('get_session_details failed:', e));
    };
    if (selectedSession?.id === id) {
      // Refresh of an already-selected session: debounce.
      detailsFetchTimer = setTimeout(fetchDetails, 400);
    } else {
      fetchDetails();
    }
    return () => {
      cancelled = true;
      if (detailsFetchTimer !== null) {
        clearTimeout(detailsFetchTimer);
        detailsFetchTimer = null;
      }
    };
  });

  function selectSession(id: string) {
    selectedSessionId = id;
  }

  function deselect() {
    selectedSessionId = null;
    selectedSession = null;
  }

  // Escape deselects (kept from the drawer flow).
  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && selectedSessionId !== null) deselect();
  }
  $effect(() => {
    if (active) {
      window.addEventListener('keydown', handleKeydown);
      return () => window.removeEventListener('keydown', handleKeydown);
    }
  });

  // Below ~1100px the fixed pane doesn't fit — fall back to an overlay drawer.
  const wideQuery = window.matchMedia('(min-width: 1100px)');
  let isWide = $state(wideQuery.matches);
  const onWideChange = (e: MediaQueryListEvent) => (isWide = e.matches);
  wideQuery.addEventListener('change', onWideChange);
  onDestroy(() => wideQuery.removeEventListener('change', onWideChange));

  const gridCols = 'grid-template-columns: minmax(0,2.4fr) 0.9fr 1.1fr 1fr 0.8fr;';
</script>

<div class="flex flex-col h-full overflow-hidden">
  <!-- Analytics band -->
  <div class="grid gap-3.5 p-4 flex-shrink-0" style="grid-template-columns: 1.8fr 1fr 0.9fr;">
    <!-- Spend card -->
    <div class="bg-card border border-edge rounded-xl px-5 pt-4 pb-3 min-w-0">
      <div class="flex items-baseline gap-3">
        <div>
          <div class="text-[11px] text-ink-muted font-medium">{spendCardLabel}</div>
          <div class="text-[30px] font-bold tracking-[-0.03em] font-mono mt-0.5 {showApiCost ? 'text-accent-cost' : 'text-ink'}">
            {fmtMoney(windowTotals.cost)}
          </div>
        </div>
        {#if costDelta !== null}
          <span class="text-[11px] font-semibold bg-accent-chipbg text-accent-chipfg rounded-full px-[9px] py-[2px] whitespace-nowrap">
            {costDelta >= 0 ? '▲' : '▼'} {Math.abs(costDelta)}% vs previous {windowLabel === 'in range' ? 'period' : 'week'}
          </span>
        {/if}
        <span class="ml-auto text-[11px] text-ink-faint whitespace-nowrap">{spendCardNote}</span>
      </div>
      <svg width="100%" height="72" viewBox="0 0 700 72" preserveAspectRatio="none" class="mt-2 block" aria-hidden="true">
        {#if chart.line}
          <polygon points={chart.area} fill="var(--accent-fill)" />
          <polyline points={chart.line} fill="none" stroke="var(--accent)" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" />
          <circle cx={chart.endX} cy={chart.endY} r="3.5" fill="var(--accent)" />
        {:else}
          <line x1="0" y1="66" x2="700" y2="66" stroke="var(--accent-dim)" stroke-width="1.5" stroke-dasharray="4 4" />
        {/if}
      </svg>
      <div class="flex justify-between text-[10px] text-ink-faint mt-[5px] font-mono">
        {#each chartLabels as label}
          <span>{label}</span>
        {/each}
      </div>
    </div>

    <!-- Cost by model -->
    <div class="bg-card border border-edge rounded-xl px-5 py-4 min-w-0">
      <div class="text-[11px] text-ink-muted font-medium mb-3">
        {harness === 'codex' ? 'Cost by model' : 'Spend by model'} · {windowLabel}
      </div>
      {#if costByModel.rows.length === 0}
        <div class="text-[11px] text-ink-faint">No priced usage in this window</div>
      {:else}
        <div class="flex flex-col gap-2.5 text-xs">
          {#each costByModel.rows as row, i (row.model)}
            <div>
              <div class="flex justify-between mb-1 gap-2">
                <span class="font-mono text-[11px] text-ink-2 truncate" title={row.model}>{row.model}</span>
                <span class="font-semibold font-mono text-ink whitespace-nowrap">{fmtMoney(row.cost)}</span>
              </div>
              <div class="h-[6px] bg-track rounded-[3px]">
                <div class="h-[6px] rounded-[3px]" style="width: {row.pct}%; background: var(--bar-{i + 1});"></div>
              </div>
            </div>
          {/each}
          {#if costByModel.more > 0}
            <div class="text-[10px] text-ink-faint">+{costByModel.more} more</div>
          {/if}
        </div>
      {/if}
    </div>

    <!-- KPI stack -->
    <div class="bg-card border border-edge rounded-xl px-5 py-4 flex flex-col justify-between gap-2 min-w-0">
      <div>
        <div class="text-[11px] text-ink-muted font-medium">Sessions · {windowLabel}</div>
        <div class="text-xl font-bold font-mono mt-0.5 text-ink">
          {windowStats.sessionCount}
          <span class="text-[11px] text-ink-faint font-normal">of {allSessions.length}</span>
        </div>
      </div>
      <div>
        <div class="text-[11px] text-ink-muted font-medium">Tokens · {windowLabel}</div>
        <div class="text-xl font-bold font-mono mt-0.5 text-ink">
          {fmtCompact(windowTotals.tokens)}
          {#if tokensDelta !== null}
            <span class="text-[11px] font-medium {tokensDelta >= 0 ? 'text-pos' : 'text-ink-faint'}">
              {tokensDelta >= 0 ? '▲' : '▼'} {Math.abs(tokensDelta)}%
            </span>
          {/if}
        </div>
      </div>
      {#if harness === 'codex'}
        <div>
          <div class="text-[11px] text-ink-muted font-medium">Credits · {windowLabel}</div>
          {#if windowStats.credits.billedTotal > 0}
            <div class="text-xl font-bold font-mono mt-0.5 text-ink">
              {fmtAmount(windowStats.credits.billedTotal)}
              {#if windowStats.credits.unlimitedCount > 0}
                <span class="text-[11px] text-ink-faint font-normal">{windowStats.credits.unlimitedCount} unlimited excluded</span>
              {/if}
            </div>
          {:else if windowStats.credits.unlimitedCount > 0}
            <!-- Never print 0.00 next to an unlimited count. -->
            <div class="text-xl font-bold font-mono mt-0.5 text-ink">
              — <span class="text-[11px] text-ink-faint font-normal">all sessions unlimited</span>
            </div>
          {:else}
            <div class="text-xl font-bold font-mono mt-0.5 text-ink">—</div>
          {/if}
        </div>
      {:else}
        <div>
          <div class="text-[11px] text-ink-muted font-medium">Subagents · {windowLabel}</div>
          <div class="text-xl font-bold font-mono mt-0.5 text-ink">
            {windowStats.subagents.count}
            {#if windowStats.subagents.count > 0}
              <span class="text-[11px] text-ink-faint font-normal">{fmtMoney(windowStats.subagents.cost)} total</span>
            {/if}
          </div>
        </div>
      {/if}
    </div>
  </div>

  <!-- Main split: table + detail pane -->
  <div class="flex-1 flex min-h-0 border-t border-edge">
    <div class="flex-1 min-w-0 flex flex-col bg-tablebg {isWide ? 'border-r border-edge' : ''}">
      {#if allSessions.length === 0}
        <div class="flex flex-col items-center justify-center h-full gap-3 text-ink-faint px-6 text-center">
          {#if !scanStore.status.complete}
            <p class="text-base text-ink-muted">Scanning your sessions…</p>
            <p class="text-xs">Results appear as files are parsed. The first launch reads everything; later launches use a cache and are much faster.</p>
          {:else}
            <p class="text-base text-ink-muted">No sessions found</p>
            {#if harness === 'claude_code'}
              <p class="text-xs">Start a Claude Code session or check your Claude session roots in Settings.</p>
            {:else}
              <p class="text-xs">Start a Codex task in ChatGPT or check your config roots.</p>
            {/if}
          {/if}
        </div>
      {:else if displayed.length === 0}
        <div class="flex flex-col items-center justify-center h-full gap-3 text-ink-faint">
          <p class="text-base text-ink-muted">No sessions match the current filters.</p>
          <button
            onclick={() => onfilterschange(defaultFilters())}
            class="text-xs text-accent-chipfg hover:underline underline-offset-2"
          >
            Clear filters
          </button>
        </div>
      {:else}
        <div class="flex-1 overflow-y-auto min-h-0 relative">
          <!-- Column header -->
          <div
            class="grid px-5 py-2 border-b border-edge bg-panel sticky top-0 z-10 section-label"
            style={gridCols}
            role="row"
          >
            <span role="columnheader" aria-sort={ariaSortAttr('name')} class="text-left"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('name')}>Name{caretFor('name')}</button></span>
            <span role="columnheader" aria-sort={ariaSortAttr('started')} class="text-left"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('started')}>Started{caretFor('started')}</button></span>
            <span role="columnheader" aria-sort={ariaSortAttr('model')} class="text-left"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('model')}>Model{caretFor('model')}</button></span>
            <span role="columnheader" aria-sort={ariaSortAttr('total')} class="text-right"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('total')}>Total tok{caretFor('total')}</button></span>
            <span role="columnheader" aria-sort={ariaSortAttr('cost')} class="text-right"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('cost')}>{showApiCost ? 'Est. $' : 'Cost'}{caretFor('cost')}</button></span>
          </div>

          {#each groups as group (group.label ?? '·')}
            {#if group.label}
              <div class="px-5 pt-2 pb-[3px] section-label">{group.label}</div>
            {/if}
            {#each group.sessions as session (session.id)}
              {@const name = sessionName(session)}
              {@const display = sessionDisplayMap.get(session.id)}
              {@const rowTokens = display?.tokens ?? session.tokens_total}
              {@const sub = isSub(session)}
              {@const kids = childCounts.get(session.id) ?? 0}
              {@const combined = combinedCost.get(session.id)}
              {@const collapsed = collapsedParents.has(session.id)}
              {@const selected = selectedSessionId === session.id}
              <div
                role="button"
                tabindex="0"
                class="grid px-5 py-2 border-b border-edgerow items-center cursor-pointer transition-colors
                       {session.archived ? 'opacity-55' : ''}
                       {selected
                         ? 'bg-accent-rowbg shadow-[inset_2px_0_0_var(--accent)]'
                         : isPulsing(session.lastUpdatedAt)
                           ? 'bg-accent-rowbg animate-pulse'
                           : 'hover:bg-[var(--row-hover)]'}"
                style={gridCols}
                onclick={() => selectSession(session.id)}
                onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSession(session.id); } }}
                aria-label="Select session {name}"
              >
                <span class="truncate min-w-0 {sub ? 'pl-7' : ''} {selected ? 'font-semibold text-ink' : 'text-[var(--row-name)]'}" title={name}>
                  {#if combined !== undefined && sortKey === null}
                    <button
                      class="text-ink-faint hover:text-ink w-4 -ml-1 mr-0.5 text-center"
                      onclick={(e) => { e.stopPropagation(); toggleCollapsed(session.id); }}
                      aria-expanded={!collapsed}
                      aria-label="{collapsed ? 'Expand' : 'Collapse'} subagent rows for {name}"
                    >{collapsed ? '▸' : '▾'}</button>
                  {/if}
                  {#if sub}<span class="text-[var(--subagent-chip-fg)] font-semibold mr-1.5" aria-hidden="true">↳</span>{/if}{truncate(name, 90)}
                  {#if sub}
                    <span class="text-[10px] font-semibold px-[7px] py-px rounded-full bg-[var(--subagent-chip-bg)] text-[var(--subagent-chip-fg)] ml-1 whitespace-nowrap">subagent</span>
                  {:else if kids > 0}
                    <span class="text-[10px] font-semibold px-[7px] py-px rounded-full bg-[var(--subagent-chip-bg)] text-[var(--subagent-chip-fg)] ml-1 whitespace-nowrap">{kids} {kids === 1 ? 'subagent' : 'subagents'}</span>
                  {/if}
                  {#if session.archived}
                    <span class="text-[10px] font-semibold px-[7px] py-px rounded-full bg-[var(--archived-chip-bg)] text-[var(--archived-chip-fg)] ml-1 whitespace-nowrap">archived</span>
                  {/if}
                </span>
                <span class="text-ink-muted font-mono text-xs">{fmtStarted(session.startedMs)}</span>
                <span class="text-ink-muted font-mono text-xs truncate" title={session.model ?? ''}>{session.model ?? '—'}</span>
                <span class="text-right font-mono text-xs text-ink">{fmt.format(rowTokens.total_tokens)}</span>
                <span class="text-right font-mono text-xs text-accent-cost {selected ? 'font-semibold' : ''}">
                  {fmtAmount(costOf(session.id))}{#if display && display.missingModels.length > 0}<span
                      class="text-amber-500 cursor-help"
                      title="Fallback rate used for: {display.missingModels.join(', ')}">&nbsp;⚠</span>{/if}
                  {#if combined !== undefined}
                    <div
                      class="text-[10px] text-ink-faint font-normal cursor-help"
                      title="This session plus its subagent threads (in view)"
                    >Σ {fmtAmount(combined)}</div>
                  {/if}
                </span>
              </div>
            {/each}
          {/each}

          <!-- Pinned totals -->
          <div
            class="grid px-5 py-2 items-center border-t border-edge bg-panel font-semibold sticky bottom-0"
            style={gridCols}
          >
            <span class="section-label">Totals · in view</span>
            <span></span><span></span>
            <span class="text-right font-mono text-xs text-ink">{fmt.format(filteredTotal)}</span>
            <span class="text-right font-mono text-xs text-accent-cost">{fmtAmount(costTotal)}</span>
          </div>
        </div>
      {/if}
    </div>

    <!-- Persistent detail pane (wide layouts) -->
    {#if isWide}
      <div class="w-[410px] flex-shrink-0 min-h-0">
        <DetailPane
          session={selectedSession}
          childCount={selectedSessionId ? (childCounts.get(selectedSessionId) ?? 0) : 0}
          onclose={deselect}
        />
      </div>
    {/if}
  </div>
</div>

<!-- Narrow layouts: the pane collapses back to an overlay drawer -->
{#if !isWide && selectedSessionId !== null}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="fixed inset-0 bg-black/50 z-40" onclick={deselect} aria-hidden="true"></div>
  <div
    class="fixed top-0 right-0 h-full w-[410px] max-w-full border-l border-edge shadow-2xl z-50"
    role="dialog"
    aria-modal="true"
    aria-label="Session details"
  >
    <DetailPane
      session={selectedSession}
      childCount={childCounts.get(selectedSessionId) ?? 0}
      onclose={deselect}
    />
  </div>
{/if}
