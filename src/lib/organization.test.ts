import { describe, expect, it } from 'vitest';
import { savedSummarySearch, restoredSummaryFilters, matchesOrganization } from './organization';
import { defaultFilters, filterBounds } from './sessionProjection';
import type { SavedSearchDefinition } from './types';

describe('saved private organization queries', () => {
  it('preserves inclusive UTC seconds/milliseconds and every selected filter', () => {
    const definition: SavedSearchDefinition = {
      ...savedSummarySearch('Review', 'claude_code', defaultFilters(), true, ['Review']),
      query: 'summary phrase', model: 'claude-test', show_active: false,
      show_archived: true, show_subagents: false,
      from: '2026-10-01T04:00:12.345Z', to: '2026-10-02T03:59:59.999Z',
    };
    const restored = restoredSummaryFilters(definition)!;
    expect(filterBounds(restored)).toEqual({ from: definition.from, to: definition.to });
    expect(restored.dateFrom).toMatch(/:12\.345$/);
    expect(restored.dateTo).toMatch(/:59\.999$/);
    expect(savedSummarySearch('Review', definition.scope, restored, true, ['Review'])).toEqual(definition);
  });

  it('keeps absolute UTC authoritative even when a repeated local hour is ambiguous', () => {
    const saved = savedSummarySearch('Repeated hour', 'all', defaultFilters(), false, []);
    saved.from = '2026-11-01T06:30:00.123Z';
    saved.to = '2026-11-01T06:59:59.999Z';
    const restored = restoredSummaryFilters(saved)!;
    // A timezone edit or DST-fold formatter must not reinterpret these instants.
    restored.dateFrom = '2026-11-01T01:30:00.123';
    restored.dateTo = '2026-11-01T01:59:59.999';
    expect(filterBounds(restored)).toEqual({ from: saved.from, to: saved.to });
  });

  it('rejects invalid date filters and never downgrades content scope to summaries', () => {
    expect(() => savedSummarySearch('Invalid', 'all', { ...defaultFilters(), dateFrom: 'bad-date' }, false, [])).toThrow('Invalid date filter');
    const saved = savedSummarySearch('Content', 'all', defaultFilters(), false, []);
    expect(restoredSummaryFilters({ ...saved, content_scope: 'session_content' })).toBeNull();
    expect(matchesOrganization(undefined, false, [])).toBe(true);
    expect(matchesOrganization(undefined, true, [])).toBe(false);
  });
});
