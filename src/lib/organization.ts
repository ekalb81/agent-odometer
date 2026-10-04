import type { SessionFilterState, ViewScope } from './sessionProjection';
import type { AnnotationIdentity, OrganizationSummary, SavedSearchDefinition } from './types';
import { toLocalInputValuePrecise } from './dateRange';
import { filterBounds } from './sessionProjection';

export function savedSummarySearch(
  name: string, scope: ViewScope, filters: SessionFilterState,
  pinnedOnly: boolean, tags: string[],
): SavedSearchDefinition {
  const { from, to } = filterBounds(filters);
  if ((filters.dateFrom && !from) || (filters.dateTo && !to)) throw new Error('Invalid date filter; review the bounds before saving.');
  return {
    name, query: filters.search, scope, content_scope: 'summary',
    content_classes: { conversation: true, tool_calls: false, tool_results: false },
    session_key: null, fingerprint: null,
    from, to,
    model: filters.model, show_active: filters.showActive,
    show_archived: filters.showArchived, show_subagents: filters.showSubagents,
    pinned_only: pinnedOnly, tags: [...tags],
  };
}

/** A saved content search must never execute against summary fields silently. */
export function restoredSummaryFilters(search: SavedSearchDefinition): SessionFilterState | null {
  if (search.content_scope !== 'summary') return null;
  return {
    search: search.query,
    dateFrom: search.from ? toLocalInputValuePrecise(new Date(search.from)) : '',
    dateTo: search.to ? toLocalInputValuePrecise(new Date(search.to)) : '',
    model: search.model, showActive: search.show_active,
    showArchived: search.show_archived, showSubagents: search.show_subagents,
    utcBounds: { from: search.from, to: search.to },
  };
}

export function matchesOrganization(
  summary: OrganizationSummary | undefined, pinnedOnly: boolean, tags: string[],
): boolean {
  if (!pinnedOnly && tags.length === 0) return true;
  return !!summary && (!pinnedOnly || summary.pinned) && tags.every(tag => summary.tags.includes(tag));
}

export function sameAnnotationTarget(a: AnnotationIdentity, b: AnnotationIdentity): boolean {
  return a.session_key === b.session_key && a.fingerprint === b.fingerprint && a.anchor === b.anchor;
}
