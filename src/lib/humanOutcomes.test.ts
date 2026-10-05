import { describe, expect, it } from 'vitest';
import type { OrganizationSummary, SessionSummary } from './types';
import { humanOutcomeReport, notRated } from './humanOutcomes';
const session = (id: string, child = false) => ({ storage_id: id, id, harness: 'codex',
  parent_thread_id: child ? 'parent' : null, agent_path: null, source: 'cli',
  tokens_total: { total_tokens: 999999 }, duration_ms: 1 }) as unknown as SessionSummary;
const metadata = (id: string, outcome = notRated(), revision = 1): OrganizationSummary => ({
  identity: { session_key: id, fingerprint: 'lineage', anchor: '' }, revision,
  pinned: false, tags: [], has_note: true, outcome,
});
describe('explicit human outcome measurement', () => {
  it('keeps missing metadata, not rated, rejected and unresolved distinct without inferring from usage', () => {
    const report = humanOutcomeReport(['missing','unrated','rejected','unresolved'].map(id => session(id)), {
      unrated: metadata('unrated'), rejected: metadata('rejected', { ...notRated(), label: 'rejected' }),
      unresolved: metadata('unresolved', { ...notRated(), label: 'unresolved' }),
    });
    expect(report).toMatchObject({ tasks: 4, labelled: 2, not_rated: 1, metadata_missing: 1,
      accepted: 0, rejected: 1, unresolved: 1, first_pass_reported: 0, repair_reported: 0, repair_minutes: null });
    expect(report.rows[0]).toMatchObject({ label: null, repair_minutes: null, metadata_available: false });
    expect(JSON.stringify(report)).not.toMatch(/999999|duration_ms|note|has_note/);
  });
  it('reports first-pass and effort sample denominators, preserving explicitly reported zero', () => {
    const report = humanOutcomeReport(['first','repair','unknown'].map(id => session(id)), {
      first: metadata('first', { label: 'accepted', first_pass_accepted: true, repair_minutes: 0 }),
      repair: metadata('repair', { label: 'accepted', first_pass_accepted: false, repair_minutes: 15 }),
      unknown: metadata('unknown', { label: 'accepted', first_pass_accepted: null, repair_minutes: null }),
    });
    expect(report).toMatchObject({ labelled: 3, first_pass_accepted: 1, first_pass_reported: 2,
      repair_minutes: 15, repair_reported: 2 });
    expect(report.rows[0].repair_minutes).toBe(0);
  });
  it('counts parent tasks once and discloses excluded subagent labels', () => {
    const report = humanOutcomeReport([session('parent'), session('child', true)], {
      parent: metadata('parent', { ...notRated(), label: 'accepted' }),
      child: metadata('child', { ...notRated(), label: 'rejected' }),
    });
    expect(report).toMatchObject({ tasks: 1, labelled: 1, accepted: 1, rejected: 0, subagents_excluded: 1 });
    expect(report.rows.map(row => row.session_key)).toEqual(['parent']);
  });
  it('treats unrestored old ratings as unavailable while preserving explicit rebuilt edits', () => {
    const report = humanOutcomeReport([session('old'),session('new')], {
      old: { ...metadata('old',notRated(),1), outcome: undefined }, new: metadata('new'),
    }, true);
    expect(report).toMatchObject({ tasks: 2, metadata_missing: 1, not_rated: 1 });
    expect(humanOutcomeReport([],{})).toMatchObject({ tasks: 0, repair_minutes: null });
  });
  it('bounds exports and counts without pretending omitted tasks were assessed', () => {
    const report = humanOutcomeReport(Array.from({length:10001},(_,index) => session(String(index))), {});
    expect(report).toMatchObject({ tasks:10000, tasks_omitted:1, labelled:0, metadata_missing:10000 });
    expect(report.rows).toHaveLength(10000);
  });
});
