import type { HumanOutcome, OrganizationSummary, SessionSummary } from './types';
import { isSubagent } from './sessionProjection';

export const notRated = (): HumanOutcome => ({ label: 'not_rated', repair_minutes: null, first_pass_accepted: null });

/** One root session and its linked subagent work form a task. Child labels
 * remain inspectable, but never become extra deliveries in this denominator. */
export function humanOutcomeReport(sessions: SessionSummary[], metadata: Record<string, OrganizationSummary>, recoveryUnrestored = false) {
  const roots = sessions.filter(session => !isSubagent(session));
  // ponytail: bounded 10k-task prefix; use narrower filters before adding paging.
  const rows = roots.slice(0, 10_000).map(session => {
    const summary = metadata[session.storage_id];
    const outcome = summary && !(recoveryUnrestored && !summary.outcome) ? summary.outcome ?? notRated() : null;
    return { session_key: session.storage_id, label: outcome?.label ?? null,
      repair_minutes: outcome?.repair_minutes ?? null, first_pass_accepted: outcome?.first_pass_accepted ?? null,
      metadata_available: !!outcome };
  });
  const known = rows.filter(row => row.metadata_available);
  const labelled = known.filter(row => row.label !== 'not_rated');
  const firstPass = labelled.filter(row => row.first_pass_accepted != null);
  const repair = known.filter(row => row.repair_minutes != null);
  return {
    rows, tasks: rows.length, tasks_omitted: roots.length - rows.length,
    subagents_excluded: sessions.length - roots.length,
    metadata_missing: rows.length - known.length,
    labelled: labelled.length, not_rated: known.length - labelled.length,
    accepted: labelled.filter(row => row.label === 'accepted').length,
    rejected: labelled.filter(row => row.label === 'rejected').length,
    unresolved: labelled.filter(row => row.label === 'unresolved').length,
    first_pass_accepted: firstPass.filter(row => row.first_pass_accepted).length,
    first_pass_reported: firstPass.length,
    repair_reported: repair.length,
    repair_minutes: repair.length ? repair.reduce((sum, row) => sum + row.repair_minutes!, 0) : null,
  };
}
