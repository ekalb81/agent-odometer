<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { sessionsStore, type TrackedSession } from '../lib/stores/sessions.svelte';
  import { scanStore } from '../lib/stores/scan.svelte';
  import { rates } from '../lib/stores/rates';
  import { apiCostFromBuckets, creditsFromBuckets, formatCredits, harnessCurrency } from '../lib/credits';
  import { getSessionDetails, listExternalEvents, onConfigEvent, sessionsInRanges, writeExport } from '../lib/ipc';
  import type { ExternalEvent, Harness, RangeTotals, RateCard, Session } from '../lib/types';
  import type { FilterState } from './Filters.svelte';
  import { rangeLabelFor } from '../lib/dateRange';
  import {
    aggregateModelMetrics,
    addTotals,
    defaultFilters,
    exportRows,
    filterSessions,
    isSubagent,
    projectSessions,
    rowsToCsv,
    sessionName,
    toUtcIso,
    type ViewScope,
    zeroTotals,
  } from '../lib/sessionProjection';
  import DetailPane from './DetailPane.svelte';
  import ConfigTimeline from './ConfigTimeline.svelte';
  import GitOutcomes from './GitOutcomes.svelte';
  import { measureAsync, measureNextPaint, measureSync } from '../lib/performance';

  interface Props {
    harness?: ViewScope;
    active?: boolean;
    filters: FilterState;
    onfilterschange: (f: FilterState) => void;
  }

  let { harness = 'all', active = true, filters, onfilterschange }: Props = $props();

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
  const showApiCost = $derived(harness !== 'claude_code' && Object.keys($rates?.api_models ?? {}).length > 0);
  const allUsdAvailable = $derived(
    harness !== 'all' || Boolean(
      $rates &&
      Object.keys($rates.api_models ?? {}).length > 0 &&
      harnessCurrency($rates, 'claude_code') === 'USD'
    ),
  );

  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

  function fmtCompact(n: number): string {
    if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
    if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
    if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
    return String(n);
  }

  function truncate(str: string, max: number): string {
    return str.length > max ? str.slice(0, max) + '…' : str;
  }

  function isPulsing(lastUpdatedAt: number): boolean {
    // pulseGen is bumped once ~2s after the last store change so highlights
    // expire even when no further updates trigger a re-render.
    void pulseGen;
    return Date.now() - lastUpdatedAt < 2000;
  }

  let pulseGen = $state(0);
  $effect(() => {
    if (!active) return;
    void sessionsStore.map;
    const t = setTimeout(() => {
      pulseGen += 1;
    }, 2100);
    return () => clearTimeout(t);
  });
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
    active ? filterSessions(sessionsStore.map.values(), harness, defaultFilters(), false) : [],
  );

  // Convert datetime-local strings (local time) to UTC ISO once, so the rest
  // of the pipeline can do lexical comparison against the UTC ISO timestamps
  // we store on sessions and history points.
  const fromUtc = $derived(toUtcIso(filters.dateFrom));
  const toUtc = $derived(toUtcIso(filters.dateTo));

  // Everything except the date bounds. Kept separate so the analytics
  // previous-window totals can include sessions that were active then but
  // fall outside the current date range.
  const filteredNoDate = $derived(filterSessions(allSessions, harness, filters, false));

  // Datetime range — overlap semantics: include any session whose
  // [started_at, last_event_at] window intersects the filter range.
  // Comparison is lexical on UTC ISO strings, which sorts chronologically.
  const filtered = $derived(filterSessions(filteredNoDate, harness, filters, true));
  const filteredIds = $derived(filtered.map((session) => session.id));
  const analyticsSessionIds = $derived(filteredNoDate.map((session) => session.id));

  // True when the user has narrowed by date — drives whether per-session
  // tokens and costs are "all-time" or scoped to the visible window.
  const dateScoped = $derived(Boolean(filters.dateFrom || filters.dateTo));

  // ---------------------------------------------------------------------------
  // Date-scoped rollups come from the backend (summaries don't carry event
  // histories). Refetched, debounced, when the range or the session set changes.
  // ---------------------------------------------------------------------------
  let rangeTotals = $state<Record<string, RangeTotals>>({});
  let rangeFetchTimer: ReturnType<typeof setTimeout> | null = null;
  let rangeRequestGeneration = 0;
  let lastTableRequestKey: string | null = null;
  // Debounce is only for coalescing live store flushes (~150ms apart). A
  // changed range is a discrete user action (preset click, committed input)
  // and fetches immediately.
  let lastTableRange: string | null = null;

  $effect(() => {
    const generation = ++rangeRequestGeneration;
    const from = fromUtc;
    const to = toUtc;
    const sessionIds = filteredIds;
    // Depend on the session map so live session updates refresh range data;
    // skip entirely while this tab is hidden.
    void sessionsStore.map;
    if (!active) {
      rangeTotals = {};
      lastTableRange = null;
      lastTableRequestKey = null;
      return;
    }
    if (!from && !to) {
      rangeTotals = {};
      lastTableRange = null;
      lastTableRequestKey = null;
      return;
    }
    const key = `${from}|${to}`;
    const requestKey = `${key}|${filters.search}|${filters.model}|${filters.showActive}|${filters.showArchived}|${filters.showSubagents}|${sessionIds.length}`;
    const delay = key === lastTableRange ? 250 : 0;
    if (requestKey !== lastTableRequestKey) rangeTotals = {};
    lastTableRange = key;
    lastTableRequestKey = requestKey;
    if (rangeFetchTimer !== null) clearTimeout(rangeFetchTimer);
    rangeFetchTimer = setTimeout(() => {
      rangeFetchTimer = null;
      measureAsync(
        'frontend.table_range_fetch',
        () => sessionsInRanges([{ from, to }], sessionIds),
        { sessions: sessionIds.length, ranges: 1 },
      )
        .then(([result]) => {
          if (active && generation === rangeRequestGeneration) rangeTotals = result;
        })
        .catch((e) => console.error('sessions_in_ranges failed:', e));
    }, delay);
    return () => {
      if (generation === rangeRequestGeneration) rangeRequestGeneration += 1;
      if (rangeFetchTimer !== null) {
        clearTimeout(rangeFetchTimer);
        rangeFetchTimer = null;
      }
    };
  });

  // Per-session display values: tokens AND costs, both scoped to the date
  // filter when one is active so the row numbers add up to the totals row.
  // Export and the model comparison consume this exact projection too.
  const sessionDisplayMap = $derived(
    projectSessions(filtered, $rates, rangeTotals, dateScoped),
  );

  /** The money-column value for a session (Est.$ on Codex, Cost elsewhere). */
  function costOf(id: string): number {
    if (!allUsdAvailable) return 0;
    const d = sessionDisplayMap.get(id);
    if (!d) return 0;
    return d.displayCost;
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

  // Subagent rows tuck in directly beneath their parent (when the parent is
  // in view) under every sort order: column sorts rank the parents, and each
  // parent's children among themselves, so a child never floats above its
  // parent. anchorMs records which day group a row belongs to — children
  // inherit their parent's group so a nested row never splits a day section.
  const displayedWithAnchors = $derived((() => {
    const sorted = [...filtered].sort(compareSession);
    const anchorMs = new Map<string, number>();
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

  // Collapsed parents hide their nested subagent rows.
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

  // Flatten and virtualize the list locally. Rows have explicit heights so
  // thousands of sessions do not become thousands of live DOM nodes, while
  // group headings and keyboard-selectable session rows preserve their
  // existing behavior without another frontend dependency.
  type ListRow =
    | { kind: 'group'; key: string; label: string; height: number }
    | { kind: 'session'; key: string; session: TrackedSession; height: number };
  const GROUP_ROW_HEIGHT = 28;
  const SESSION_ROW_HEIGHT = 48;
  const LIST_HEADER_HEIGHT = 33;
  const LIST_OVERSCAN = 8;
  const listRows = $derived((() => {
    const rows: ListRow[] = [];
    for (const [groupIndex, group] of groups.entries()) {
      if (group.label) {
        rows.push({
          kind: 'group',
          key: `group:${groupIndex}:${group.label}`,
          label: group.label,
          height: GROUP_ROW_HEIGHT,
        });
      }
      for (const session of group.sessions) {
        rows.push({ kind: 'session', key: `session:${session.id}`, session, height: SESSION_ROW_HEIGHT });
      }
    }
    return rows;
  })());
  const listOffsets = $derived((() => {
    const offsets = [0];
    for (const row of listRows) offsets.push(offsets[offsets.length - 1] + row.height);
    return offsets;
  })());
  let listViewport = $state<HTMLDivElement>();
  let listScrollTop = $state(0);
  let listViewportHeight = $state(600);

  function rowIndexAt(offsets: number[], position: number): number {
    let low = 0;
    let high = Math.max(0, offsets.length - 1);
    while (low < high) {
      const middle = Math.floor((low + high + 1) / 2);
      if (offsets[middle] <= position) low = middle;
      else high = middle - 1;
    }
    return Math.min(low, Math.max(0, offsets.length - 2));
  }

  const virtualList = $derived((() => {
    const offsets = listOffsets;
    const contentTop = Math.max(0, listScrollTop - LIST_HEADER_HEIGHT);
    const start = Math.max(0, rowIndexAt(offsets, contentTop) - LIST_OVERSCAN);
    const end = Math.min(
      listRows.length,
      rowIndexAt(offsets, contentTop + listViewportHeight) + LIST_OVERSCAN + 1,
    );
    return {
      rows: listRows.slice(start, end),
      top: offsets[start] ?? 0,
      bottom: Math.max(0, (offsets[offsets.length - 1] ?? 0) - (offsets[end] ?? 0)),
    };
  })());

  $effect(() => {
    if (!active) return;
    const started = performance.now();
    const rows = listRows.length;
    measureNextPaint('frontend.session_list_paint', started, { rows });
  });

  onMount(() => {
    const resize = new ResizeObserver(([entry]) => {
      listViewportHeight = entry.contentRect.height;
    });
    if (listViewport) resize.observe(listViewport);
    return () => resize.disconnect();
  });

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
  // Day buckets come from sessions_in_ranges (summaries carry no history);
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
  let analyticsRequestGeneration = 0;
  let lastAnalyticsRequestKey: string | null = null;

  const DAY_MS = 86_400_000;
  const MAX_CHART_BUCKETS = 14;

  // Earliest session start — the window floor for a "To"-only date filter.
  // Separate derived so windowBounds only tracks the session list in that case.
  const earliestStartMs = $derived((() => {
    let min = Infinity;
    for (const s of allSessions) {
      const t = new Date(s.started_at).getTime();
      if (t < min) min = t;
    }
    return min;
  })());

  // The chart window: the date filter when set, else a rolling last-7-days —
  // the same definition as the "Last 7 days" preset, so picking that preset
  // doesn't shift the numbers. A "To"-only filter reaches back to the earliest
  // session so the band covers the same sessions as the list.
  const windowBounds = $derived((() => {
    void pulseGen; // stay fresh as time advances
    const endMs = toUtc ? new Date(toUtc).getTime() : Date.now();
    let startMs: number;
    if (fromUtc) {
      startMs = new Date(fromUtc).getTime();
    } else if (toUtc) {
      startMs = Number.isFinite(earliestStartMs) ? earliestStartMs : endMs - 7 * DAY_MS;
    } else {
      startMs = endMs - 7 * DAY_MS;
    }
    if (startMs >= endMs) startMs = endMs - DAY_MS;
    return { startMs, endMs };
  })());

  // Card labels echo the filter pill's wording for the same bounds.
  const windowLabel = $derived(
    dateScoped ? rangeLabelFor(filters.dateFrom, filters.dateTo) : 'Last 7 days',
  );

  let configEvents = $state<ExternalEvent[]>([]);
  let configEventsGeneration = 0;
  $effect(() => {
    const generation = ++configEventsGeneration;
    if (!active) return;
    listExternalEvents()
      .then((events) => {
        if (active && generation === configEventsGeneration) {
          configEvents = events.filter((event) => event.source === 'config');
        }
      })
      .catch((error) => console.error('list_external_events failed:', error));
    return () => { if (generation === configEventsGeneration) configEventsGeneration += 1; };
  });
  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    onConfigEvent((event) => {
      if (!disposed) configEvents = [...configEvents.filter((item) => item.id !== event.id), event];
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    }).catch((error) => console.error('config-event listener failed:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  });

  // Mirrors lastTableRange: date-filter changes refresh immediately, only
  // store flushes are debounced. Keyed on the filter bounds, not windowBounds
  // — the default window's endMs is "now", which moves on every recompute.
  let lastAnalyticsRange: string | null = null;

  $effect(() => {
    const generation = ++analyticsRequestGeneration;
    const { startMs, endMs } = windowBounds;
    const sessionIds = analyticsSessionIds;
    void sessionsStore.map;
    if (!active) {
      analyticsBuckets = [];
      analyticsPrev = null;
      analyticsCurrent = null;
      lastAnalyticsRange = null;
      lastAnalyticsRequestKey = null;
      return;
    }
    const key = `${fromUtc}|${toUtc}`;
    const requestKey = `${key}|${filters.search}|${filters.model}|${filters.showActive}|${filters.showArchived}|${filters.showSubagents}|${sessionIds.length}`;
    const delay = key === lastAnalyticsRange ? 250 : 0;
    if (requestKey !== lastAnalyticsRequestKey) {
      analyticsBuckets = [];
      analyticsPrev = null;
      analyticsCurrent = null;
    }
    lastAnalyticsRange = key;
    lastAnalyticsRequestKey = requestKey;
    if (analyticsTimer !== null) clearTimeout(analyticsTimer);
    // Debounced so a burst of store flushes (150ms apart) coalesces into one
    // refresh. All ~16 windows go in a single batched call the backend
    // computes in one pass, so the refresh itself is cheap.
    analyticsTimer = setTimeout(async () => {
      analyticsTimer = null;
      // Day-aligned buckets, coalesced so long ranges stay ≤14 chart points.
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
      const iso = (ms: number) => new Date(ms).toISOString();
      try {
        const requestedRanges = [
          { from: iso(startMs), to: iso(endMs) },
          { from: iso(prevFrom), to: iso(startMs - 1) },
          ...bounds.map((b) => ({ from: iso(b.from), to: iso(b.to) })),
        ];
        const [current, prev, ...days] = await measureAsync(
          'frontend.analytics_range_fetch',
          () => sessionsInRanges(requestedRanges, sessionIds),
          { sessions: sessionIds.length, ranges: requestedRanges.length },
        );
        if (!active || generation !== analyticsRequestGeneration) return;
        analyticsCurrent = current;
        analyticsPrev = prev;
        analyticsBuckets = days.map((data, i) => ({ label: fmtMonthDay(bounds[i].from), data }));
      } catch (e) {
        console.error('analytics sessions_in_ranges failed:', e);
      }
    }, delay);
    return () => {
      if (generation === analyticsRequestGeneration) analyticsRequestGeneration += 1;
      if (analyticsTimer !== null) {
        clearTimeout(analyticsTimer);
        analyticsTimer = null;
      }
    };
  });

  /** Price one range-rollup map for the sessions currently in view. Uses the
   *  non-date-filtered set: the rollup itself scopes usage to its window, and
   *  the previous window's sessions may not intersect the current date range. */
  // Pricing is linear per bucket, so a range's cost equals the sum of its
  // sessions' costs. Price each fetched rollup map once (keyed by identity),
  // then every recompute — keystrokes, store flushes — just sums numbers.
  type RangePrice = {
    cost: number;
    tokens: number;
    codexCredits: number;
    codexApiUsd: number;
    claudeUsd: number;
    completeUsd: boolean;
  };
  type RangeSessionPrice = {
    tokens: number;
    planCost: number;
    planComplete: boolean;
    apiCost: number | null;
    apiComplete: boolean;
  };
  const rangePriceCache = new WeakMap<object, {
    rates: unknown;
    perSession: Map<string, RangeSessionPrice>;
  }>();

  function priceRangeSession(
    data: Record<string, RangeTotals>,
    session: TrackedSession,
    rateCard: RateCard,
  ): RangeSessionPrice | null {
    const totals = data[session.id];
    if (!totals || totals.tokens.total_tokens === 0) return null;
    let cached = rangePriceCache.get(data);
    if (!cached || cached.rates !== rateCard) {
      cached = { rates: rateCard, perSession: new Map() };
      rangePriceCache.set(data, cached);
    }
    const existing = cached.perSession.get(session.id);
    if (existing) return existing;
    const plan = creditsFromBuckets(totals.buckets, rateCard, session.harness);
    const api = session.harness === 'codex'
      ? apiCostFromBuckets(totals.buckets, rateCard, session.harness)
      : null;
    const value: RangeSessionPrice = {
      tokens: totals.tokens.total_tokens,
      planCost: plan.total,
      planComplete: plan.missingModels.length === 0,
      apiCost: api?.total ?? null,
      apiComplete: Boolean(api && api.missingModels.length === 0),
    };
    cached.perSession.set(session.id, value);
    return value;
  }

  function priceRange(data: Record<string, RangeTotals> | null): RangePrice {
    const r = $rates;
    const out: RangePrice = {
      cost: 0,
      tokens: 0,
      codexCredits: 0,
      codexApiUsd: 0,
      claudeUsd: 0,
      completeUsd: allUsdAvailable,
    };
    if (!data || !r) return out;
    for (const s of filteredNoDate) {
      const priced = priceRangeSession(data, s, r);
      if (!priced) continue;
      out.tokens += priced.tokens;
      if (s.harness === 'codex') {
        out.codexCredits += priced.planCost;
        out.codexApiUsd += priced.apiCost ?? 0;
        if (!priced.apiComplete) out.completeUsd = false;
      } else {
        out.claudeUsd += priced.planCost;
        if (!priced.planComplete || harnessCurrency(r, s.harness) !== 'USD') out.completeUsd = false;
      }
    }
    out.cost = harness === 'codex'
      ? (showApiCost ? out.codexApiUsd : out.codexCredits)
      : harness === 'claude_code'
        ? out.claudeUsd
        : allUsdAvailable && out.completeUsd
          ? out.codexApiUsd + out.claudeUsd
          : 0;
    return out;
  }

  const spendSeries = $derived(analyticsBuckets.map((b) => ({ label: b.label, ...priceRange(b.data) })));
  const windowTotals = $derived(priceRange(analyticsCurrent));
  const prevTotals = $derived(priceRange(analyticsPrev));

  // ---------------------------------------------------------------------------
  // Window-scoped stats for the rest of the analytics band. Every card reads
  // from the same rollup map as the spend card (the date filter when set, else
  // the rolling last 7 days) and the same non-date filters, so the band tells one
  // consistent story. A session counts as active when it has usage in window.
  // ---------------------------------------------------------------------------
  // Rollup objects are recreated per fetch but reused across the store
  // flushes in between; caching their priced totals by identity keeps this
  // derived cheap when only the session list changed.
  const windowStats = $derived((() => {
    const r = $rates;
    const data = analyticsCurrent;
    const out = {
      sessionCount: 0,
      byModel: [] as ReturnType<typeof aggregateModelMetrics>,
      credits: { billedTotal: 0, unlimitedCount: 0 },
      subagents: { count: 0, cost: 0 },
      findingCount: 0,
      allUnlimited: false,
    };
    if (!data || !r) return out;
    for (const s of filteredNoDate) {
      const rt = data[s.id];
      if (!rt || rt.tokens.total_tokens === 0) continue;
      const priced = priceRangeSession(data, s, r);
      if (!priced) continue;
      out.sessionCount++;
      out.findingCount += rt.optimization_findings_count ?? 0;
      if (s.harness === 'codex') {
        if (s.credits_unlimited === true) out.credits.unlimitedCount++;
        else out.credits.billedTotal += priced.planCost;
      }
      if (isSubagent(s)) {
        out.subagents.count++;
        if (allUsdAvailable) {
          out.subagents.cost += s.harness === 'codex' && priced.apiCost !== null
            ? priced.apiCost
            : priced.planCost;
        }
      }
    }
    out.byModel = allUsdAvailable ? aggregateModelMetrics(filteredNoDate, data, r) : [];
    const codexCount = filteredNoDate.filter((session) => data[session.id]?.tokens.total_tokens && session.harness === 'codex').length;
    out.allUnlimited = codexCount > 0 && out.credits.unlimitedCount === codexCount;
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

  const chartConfigMarkers = $derived((() => {
    const { startMs, endMs } = windowBounds;
    const span = Math.max(1, endMs - startMs);
    return configEvents
      .filter((event) => {
        const timestamp = new Date(event.timestamp).getTime();
        const eventHarness = event.metadata.harness;
        return timestamp >= startMs && timestamp <= endMs && (harness === 'all' || eventHarness === harness);
      })
      .slice(-50)
      .map((event) => ({
        event,
        x: ((new Date(event.timestamp).getTime() - startMs) / span) * 700,
      }));
  })());

  // Cost by model — aggregated from the same window as the spend card, top four.
  const costByModel = $derived((() => {
    const sorted = windowStats.byModel;
    const max = sorted[0]?.cost ?? 0;
    const rows = sorted.slice(0, 4).map((m) => ({
      model: `${harness === 'all' ? `${m.harness} · ` : ''}${m.model}`,
      cost: m.cost,
      pct: max > 0 ? Math.max(2, Math.round((m.cost / max) * 100)) : 0,
    }));
    return { rows, more: Math.max(0, sorted.length - 4) };
  })());

  // Money formatting for analytics: USD gets the $ prefix, plan credits get
  // a plain number (the card labels carry the unit).
  const moneyIsUsd = $derived(
    harness === 'all' || showApiCost || ($rates ? /^[A-Z]{3}$/.test(harnessCurrency($rates, harness)) : false),
  );
  function fmtMoney(n: number): string {
    return moneyIsUsd ? fmtUsd(n) : fmtAmount(n);
  }

  const spendCardLabel = $derived(
    harness === 'all'
      ? `Combined API estimate · ${windowLabel}`
      : harness === 'codex'
      ? (showApiCost ? `Est. API cost · ${windowLabel}` : `Credits · ${windowLabel}`)
      : `Est. spend · ${windowLabel}`,
  );
  const spendCardNote = $derived(
    harness === 'all'
      ? (windowTotals.completeUsd ? 'Codex + Claude USD' : 'unavailable · missing direct rates')
      : harness === 'codex'
      ? (windowStats.allUnlimited ? 'à la carte · all sessions unlimited' : 'OpenAI API rates')
      : 'Anthropic API rates',
  );

  const modelComparison = $derived(windowStats.byModel);
  const modelComparisonCostTotal = $derived(
    modelComparison.reduce((total, metric) => total + metric.cost, 0),
  );
  const categoryRows = $derived((() => {
    const rateCard = $rates;
    const grouped = new Map<string, {
      harness: Harness;
      category: string;
      turns: number;
      tokens: ReturnType<typeof zeroTotals>;
      calls: number;
      cost: number;
      currency: string;
    }>();
    if (!rateCard) return [];
    for (const session of filtered) {
      for (const [category, metric] of Object.entries(session.category_totals ?? {})) {
        if (!metric) continue;
        const key = `${session.harness}:${category}`;
        let row = grouped.get(key);
        if (!row) {
          row = {
            harness: session.harness,
            category,
            turns: 0,
            tokens: zeroTotals(),
            calls: 0,
            cost: 0,
            currency: session.harness === 'codex' && Object.keys(rateCard.api_models ?? {}).length > 0
              ? 'USD'
              : harnessCurrency(rateCard, session.harness),
          };
          grouped.set(key, row);
        }
        row.turns += metric.turns;
        row.calls += metric.tool_calls;
        addTotals(row.tokens, metric.tokens);
        const priced = session.harness === 'codex' && Object.keys(rateCard.api_models ?? {}).length > 0
          ? apiCostFromBuckets(metric.buckets, rateCard, session.harness)
          : creditsFromBuckets(metric.buckets, rateCard, session.harness);
        row.cost += priced?.total ?? 0;
      }
    }
    return [...grouped.values()].sort((a, b) => b.tokens.total_tokens - a.tokens.total_tokens);
  })());

  let exportBusy = $state(false);
  let exportError = $state<string | null>(null);
  let includeWorkingDirectory = $state(false);

  async function exportView(format: 'csv' | 'json') {
    exportBusy = true;
    exportError = null;
    try {
      const exportSessions = filtered;
      const exportRanges = dateScoped
        ? (await measureAsync(
            'frontend.session_export_range_fetch',
            () => sessionsInRanges([{ from: fromUtc, to: toUtc }], exportSessions.map((session) => session.id)),
            { sessions: exportSessions.length, ranges: 1 },
          ))[0]
        : rangeTotals;
      const exportProjection = projectSessions(exportSessions, $rates, exportRanges, dateScoped);
      const rows = measureSync(
        'frontend.session_export_build',
        () => exportRows(exportProjection.values(), includeWorkingDirectory),
        { sessions: exportProjection.size, format },
      );
      const content = format === 'json' ? `${JSON.stringify(rows, null, 2)}\n` : rowsToCsv(rows);
      await writeExport(
        `odometer-${harness}-${new Date().toISOString().slice(0, 10)}.${format}`,
        format,
        content,
      );
    } catch (error) {
      exportError = String(error);
    } finally {
      exportBusy = false;
    }
  }

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
  let detailsRequestGeneration = 0;

  $effect(() => {
    const generation = ++detailsRequestGeneration;
    const id = selectedSessionId;
    if (!active) {
      selectedSession = null;
      return;
    }
    // Reactive dep: refetch details when this session's summary updates.
    void (id !== null ? sessionsStore.map.get(id)?.lastUpdatedAt : undefined);
    if (id === null) {
      selectedSession = null;
      return;
    }
    let cancelled = false;
    const fetchDetails = () => {
      measureAsync('frontend.session_detail_fetch', () => getSessionDetails(id))
        .then((s) => {
          if (!cancelled && active && generation === detailsRequestGeneration) selectedSession = s;
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
      if (generation === detailsRequestGeneration) detailsRequestGeneration += 1;
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
            {harness === 'all' && !windowTotals.completeUsd ? 'Unavailable' : fmtMoney(windowTotals.cost)}
          </div>
        </div>
        {#if costDelta !== null}
          <span class="text-[11px] font-semibold bg-accent-chipbg text-accent-chipfg rounded-full px-[9px] py-[2px] whitespace-nowrap">
            {costDelta >= 0 ? '▲' : '▼'} {Math.abs(costDelta)}% vs previous {windowLabel === 'Last 7 days' ? 'week' : 'period'}
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
        {#each chartConfigMarkers as marker (marker.event.id)}
          <line x1={marker.x} y1="5" x2={marker.x} y2="68" stroke="var(--amber, #d97706)" stroke-width="1" stroke-dasharray="2 3">
            <title>{marker.event.kind} {marker.event.metadata.harness} configuration · {new Date(marker.event.timestamp).toLocaleString()}</title>
          </line>
        {/each}
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
        {harness === 'codex' ? 'Cost by model' : harness === 'all' ? 'USD spend by model' : 'Spend by model'} · {windowLabel}
      </div>
      {#if harness === 'all' && !allUsdAvailable}
        <div class="text-[11px] text-ink-faint">Combined USD unavailable · configure USD rates for both harnesses</div>
      {:else if costByModel.rows.length === 0}
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
      {#if harness === 'codex' || harness === 'all'}
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
              <span class="text-[11px] text-ink-faint font-normal">{allUsdAvailable ? `${fmtMoney(windowStats.subagents.cost)} total` : 'cost unavailable'}</span>
            {/if}
          </div>
        </div>
      {/if}
    </div>
  </div>

  <div class="px-4 pb-3 flex-shrink-0 flex flex-col gap-2">
    {#if harness === 'all'}
      <div class="grid grid-cols-3 gap-2 text-xs">
        <div class="bg-card border border-edge rounded-lg px-3 py-2"><span class="text-ink-muted">Codex credits</span><div class="font-mono font-semibold">{fmtAmount(windowTotals.codexCredits)}</div></div>
        <div class="bg-card border border-edge rounded-lg px-3 py-2"><span class="text-ink-muted">Codex est. API USD</span><div class="font-mono font-semibold">{allUsdAvailable ? fmtUsd(windowTotals.codexApiUsd) : 'Unavailable'}</div></div>
        <div class="bg-card border border-edge rounded-lg px-3 py-2"><span class="text-ink-muted">Claude est. USD</span><div class="font-mono font-semibold">{allUsdAvailable ? fmtUsd(windowTotals.claudeUsd) : 'Unavailable'}</div></div>
      </div>
    {/if}

    <details class="bg-card border border-edge rounded-lg px-3 py-2">
      <summary class="cursor-pointer text-xs font-semibold text-ink">Model comparison · {windowLabel} · {modelComparison.length} models</summary>
      {#if harness === 'all' && !allUsdAvailable}
        <p class="text-xs text-ink-faint py-3">Combined model shares are unavailable until both harnesses have USD rates.</p>
      {:else if modelComparison.length === 0}
        <p class="text-xs text-ink-faint py-3">No model usage in this window.</p>
      {:else}
        <div class="overflow-x-auto mt-2">
          <table class="w-full text-[11px] font-mono">
            <thead class="text-ink-muted"><tr><th class="text-left py-1">Harness / model</th><th class="text-right">Input</th><th class="text-right">Cached</th><th class="text-right">Output</th><th class="text-right">Reasoning</th><th class="text-right">Total</th><th class="text-right">Calls</th><th class="text-right">One-shot</th><th class="text-right">Retries</th><th class="text-right">Failure</th><th class="text-right">Cost/call</th><th class="text-right">Cost</th><th class="text-right">Share</th></tr></thead>
            <tbody>
              {#each modelComparison as metric (`${metric.harness}:${metric.model}`)}
                <tr class="border-t border-edgerow">
                  <td class="py-1.5 text-ink"><span class="text-ink-faint">{metric.harness === 'codex' ? 'Codex' : 'Claude'}</span> · {metric.model}{#if metric.fallbackUsed}<span class="text-amber-500" title="Configured fallback rate used"> ⚠</span>{/if}</td>
                  <td class="text-right">{fmt.format(metric.tokens.input_tokens)}</td>
                  <td class="text-right">{fmt.format(metric.tokens.cached_input_tokens)}</td>
                  <td class="text-right">{fmt.format(metric.tokens.output_tokens)}</td>
                  <td class="text-right">{fmt.format(metric.tokens.reasoning_output_tokens)}</td>
                  <td class="text-right font-semibold">{fmt.format(metric.tokens.total_tokens)}</td>
                  <td class="text-right">{fmt.format(metric.tools.calls)}</td>
                  <td class="text-right">{metric.tools.mutation_targets > 0 ? `${((metric.tools.one_shot_mutations / metric.tools.mutation_targets) * 100).toFixed(0)}%` : '—'}</td>
                  <td class="text-right">{fmt.format(metric.tools.retry_count)}</td>
                  <td class="text-right">{metric.tools.calls > 0 ? `${((metric.tools.failures / metric.tools.calls) * 100).toFixed(0)}%` : '—'}</td>
                  <td class="text-right">{metric.tools.calls > 0 ? formatCredits(metric.cost / metric.tools.calls, metric.currency) : '—'}</td>
                  <td class="text-right">{formatCredits(metric.cost, metric.currency)}</td>
                  <td class="text-right">{modelComparisonCostTotal > 0 ? `${((metric.cost / modelComparisonCostTotal) * 100).toFixed(1)}%` : '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </details>

    <details class="bg-card border border-edge rounded-lg px-3 py-2">
      <summary class="cursor-pointer text-xs font-semibold text-ink">Task categories · all-time for sessions in view · {windowStats.findingCount} optimization findings in {windowLabel}</summary>
      {#if categoryRows.length === 0}<p class="text-xs text-ink-faint py-2">No classified turns.</p>{:else}
        <div class="grid grid-cols-6 gap-2 mt-2 text-[11px]">
          <div class="section-label col-span-2">Harness / category</div><div class="section-label text-right">Turns</div><div class="section-label text-right">Tokens</div><div class="section-label text-right">Tools</div><div class="section-label text-right">Cost</div>
          {#each categoryRows as row (`${row.harness}:${row.category}`)}
            <div class="col-span-2 border-t border-edgerow pt-1"><span class="text-ink-faint">{row.harness === 'codex' ? 'Codex' : 'Claude'}</span> · {row.category}</div><div class="text-right border-t border-edgerow pt-1 font-mono">{row.turns}</div><div class="text-right border-t border-edgerow pt-1 font-mono">{fmt.format(row.tokens.total_tokens)}</div><div class="text-right border-t border-edgerow pt-1 font-mono">{row.calls}</div><div class="text-right border-t border-edgerow pt-1 font-mono">{formatCredits(row.cost, row.currency)}</div>
          {/each}
        </div>
      {/if}
    </details>

    <ConfigTimeline {active} events={configEvents} />
    <GitOutcomes />

    <div class="flex items-center gap-2 text-xs">
      <button class="px-3 py-1.5 rounded-md border border-edge bg-card hover:bg-panel disabled:opacity-50" disabled={exportBusy} onclick={() => exportView('csv')}>Export CSV</button>
      <button class="px-3 py-1.5 rounded-md border border-edge bg-card hover:bg-panel disabled:opacity-50" disabled={exportBusy} onclick={() => exportView('json')}>Export JSON</button>
      <label class="flex items-center gap-1.5 text-ink-muted"><input type="checkbox" bind:checked={includeWorkingDirectory} /> Include working directories</label>
      {#if exportError}<span class="text-neg ml-auto" role="alert">{exportError}</span>{/if}
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
            {:else if harness === 'all'}
              <p class="text-xs">Start a Codex or Claude Code task, or check both session roots in Settings.</p>
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
        <div
          class="flex-1 overflow-y-auto min-h-0 relative"
          bind:this={listViewport}
          onscroll={(event) => { listScrollTop = event.currentTarget.scrollTop; }}
        >
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
            <span role="columnheader" aria-sort={ariaSortAttr('cost')} class="text-right"><button class="uppercase tracking-[0.07em] hover:text-ink transition-colors" onclick={() => toggleSort('cost')}>{harness === 'all' ? 'Est. USD' : showApiCost ? 'Est. $' : 'Cost'}{caretFor('cost')}</button></span>
          </div>

          <div aria-hidden="true" style:height={`${virtualList.top}px`}></div>
          {#each virtualList.rows as row (row.key)}
            {#if row.kind === 'group'}
              <div class="h-7 px-5 pb-[3px] flex items-end section-label">{row.label}</div>
            {:else}
              {@const session = row.session}
              {@const name = sessionName(session)}
              {@const display = sessionDisplayMap.get(session.id)}
              {@const rowTokens = display?.tokens ?? session.tokens_total}
              {@const sub = isSubagent(session)}
              {@const kids = childCounts.get(session.id) ?? 0}
              {@const combined = combinedCost.get(session.id)}
              {@const collapsed = collapsedParents.has(session.id)}
              {@const selected = selectedSessionId === session.id}
              <div
                role="button"
                tabindex="0"
                class="grid h-12 px-5 py-1 border-b border-edgerow items-center cursor-pointer transition-colors
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
                  {#if combined !== undefined}
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
                  {#if harness === 'all'}
                    <span class="text-[10px] font-semibold px-[7px] py-px rounded-full bg-panel text-ink-muted ml-1 whitespace-nowrap">{session.harness === 'codex' ? 'Codex' : 'Claude'}</span>
                  {/if}
                </span>
                <span class="text-ink-muted font-mono text-xs">{fmtStarted(session.startedMs)}</span>
                <span class="text-ink-muted font-mono text-xs truncate" title={session.model ?? ''}>{session.model ?? '—'}</span>
                <span class="text-right font-mono text-xs text-ink">{fmt.format(rowTokens.total_tokens)}</span>
                <span class="text-right font-mono text-xs text-accent-cost {selected ? 'font-semibold' : ''}">
                  {allUsdAvailable ? fmtAmount(costOf(session.id)) : 'unavailable'}{#if allUsdAvailable && display && display.missingModels.length > 0}<span
                      class="text-amber-500 cursor-help"
                      title="Fallback rate used for: {display.missingModels.join(', ')}">&nbsp;⚠</span>{/if}
                  {#if allUsdAvailable && combined !== undefined}
                    <div
                      class="text-[10px] text-ink-faint font-normal cursor-help"
                      title="This session plus its subagent threads (in view)"
                    >Σ {fmtAmount(combined)}</div>
                  {/if}
                </span>
              </div>
            {/if}
          {/each}
          <div aria-hidden="true" style:height={`${virtualList.bottom}px`}></div>

          <!-- Pinned totals -->
          <div
            class="grid px-5 py-2 items-center border-t border-edge bg-panel font-semibold sticky bottom-0"
            style={gridCols}
          >
            <span class="section-label">Totals · in view</span>
            <span></span><span></span>
            <span class="text-right font-mono text-xs text-ink">{fmt.format(filteredTotal)}</span>
            <span class="text-right font-mono text-xs text-accent-cost">{allUsdAvailable ? fmtAmount(costTotal) : 'unavailable'}</span>
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
