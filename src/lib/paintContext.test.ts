import { beforeEach, describe, expect, it } from 'vitest';
import {
  clearRenderedSessionRows,
  publishRenderedSessionRows,
  renderedSessionRows,
  resetPaintContextForTests,
} from './paintContext';

describe('paint context rows (issue #184)', () => {
  beforeEach(resetPaintContextForTests);

  it('reports 0 rows when no session list is on screen', () => {
    // A genuine zero, not missing data: no list rendering is no list work.
    // Events recorded before this existed carry no `rows_rendered` key at
    // all, which is what keeps an older export distinguishable.
    expect(renderedSessionRows()).toBe(0);
  });

  it('reports what the active list published', () => {
    publishRenderedSessionRows('all', 42);
    expect(renderedSessionRows()).toBe(42);
  });

  it('drops back to 0 when the active list is deactivated', () => {
    publishRenderedSessionRows('all', 42);
    clearRenderedSessionRows('all');
    // Not 42: a paint measured with no list on screen must not inherit the
    // count from the tab the user just left — that stale value is exactly
    // the contamination this issue is about.
    expect(renderedSessionRows()).toBe(0);
  });

  it('survives a tab switch whose effects run in either order', () => {
    // Several SessionsView instances stay mounted at once and Svelte does
    // not order effects across components, so a tab switch can run the
    // newly activated list's publish *before* the old one's withdrawal.
    publishRenderedSessionRows('all', 42);

    publishRenderedSessionRows('codex', 17); // new tab publishes first
    clearRenderedSessionRows('all'); // old tab withdraws second

    expect(renderedSessionRows()).toBe(17);
  });

  it('withdraws normally when the effects run in the expected order', () => {
    publishRenderedSessionRows('all', 42);

    clearRenderedSessionRows('all');
    publishRenderedSessionRows('codex', 17);

    expect(renderedSessionRows()).toBe(17);
  });
});
