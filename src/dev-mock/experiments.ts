/** Synthetic imported observations. Never a model runner or pricing calculator. */
import type { ExperimentReport, FreezePreview, FreezeRequest, ImportPreview, ImportRequest, CuratedDataset, FrozenRun } from '../lib/types';
let report: ExperimentReport | null = null;
let freeze: FreezePreview | null = null;
let imported: ImportPreview | null = null;
const emptyMeasure = () => ({ count: 0, mean: null, minimum: null, maximum: null });
export function mockExperiments(command: string, payload: Record<string, unknown>, dataset: CuratedDataset): unknown {
  if (command === 'get_offline_experiments') return report ? [{ id: report.id, revision: report.revision, name: report.manifest.name, dataset_revision: report.manifest.dataset_revision, expected_cases: report.manifest.members.length, captured_at: report.manifest.captured_at }] : [];
  if (command === 'get_offline_experiment') { if (!report) throw new Error('Synthetic comparison removed'); return report; }
  if (command === 'preview_offline_experiment') {
    const request = payload.request as FreezeRequest;
    if (request.dataset_revision !== dataset.revision || !dataset.cases.length) throw new Error('Synthetic dataset changed');
    const variants = request.variants.map(conditions => ({ conditions, prompt_hash: 'synthetic-prompt-hash', rate: { configured_version: 1, currency: 'USD', table: 'frozen_api_base_estimate', quoted_at: '2026-07-29T15:30:00Z', resolved_model: conditions.model, basis: 'unavailable' as const, cache_creation_basis: 'unavailable' as const, effective_per_million: null, applied_tier_multiplier: null, modifier: null } }));
    freeze = { token: 'synthetic-freeze-preview', manifest: { format_version: 1, mode: 'imported_offline', active_replay: 'no_go', name: request.name, rubric: request.rubric, dataset_revision: dataset.revision, captured_at: '2026-07-29T15:30:00Z', rate_snapshot_hash: 'synthetic-unavailable-rate-snapshot', variants, members: dataset.cases.map(c => ({ case_id: c.id, dataset_case_version: c.version, dataset_content_hash: c.content_hash, input_hash: 'synthetic-input-hash', expected_outcome: c.content.expected_outcome })) }, inputs: dataset.cases.map(c => [c.id, c.content]) };
    return freeze;
  }
  if (command === 'commit_offline_experiment') {
    if (!freeze || payload.token !== freeze.token || !payload.reviewed || dataset.revision !== freeze.manifest.dataset_revision) throw new Error('Synthetic freeze changed');
    report = { export_digest: 'synthetic-reviewed-report-digest', id: 1, revision: 1, manifest: freeze.manifest, cases: freeze.inputs.map(([id, input]) => ({ member: freeze!.manifest.members.find(m => m.case_id === id)!, input, a: null, b: null })), summaries: ['a', 'b'].map(variant => ({ variant, expected_cases: freeze!.inputs.length, completed: 0, failed: 0, missing: freeze!.inputs.length, missing_output: 0, accepted: 0, rejected: 0, unresolved: 0, not_rated: 0, quality_missing: freeze!.inputs.length, conditions_unverified: 0, elapsed_ms: emptyMeasure(), actual_cost_usd: emptyMeasure(), estimated_api_usd: emptyMeasure() })), removed_cases: 0, current_dataset_revision: dataset.revision, current_selected_pricing_differs: false, changed_conditions: [], paired_elapsed_delta_ms: emptyMeasure(), paired_actual_cost_delta_usd: emptyMeasure(), paired_estimated_cost_delta_usd: emptyMeasure(), recovery_backup_unrestored: false };
    freeze = null; return report;
  }
  if (command === 'preview_offline_import') {
    const request = payload.request as ImportRequest;
    if (!report || report.revision !== request.revision) throw new Error('Synthetic results changed');
    // Browser fixtures deliberately have no priceable rate. Native tests prove estimates.
    imported = { token: 'synthetic-import-preview', experiment_id: report.id, revision: report.revision, rows: request.rows.map(record => ({ record, output_truncated: false, estimated_api_usd: null, estimate_basis: 'unavailable', condition_notes: ['Synthetic fixture: reported execution conditions are unverified'] })) };
    return imported;
  }
  if (command === 'commit_offline_import') {
    if (!report || !imported || payload.token !== imported.token || !payload.reviewed || report.revision !== imported.revision) throw new Error('Synthetic import changed');
    for (const row of imported.rows) { const target = report.cases.find(c => c.member.case_id === row.record.case_id); if (!target) throw new Error('Synthetic case missing'); if (row.record.variant === 'a') target.a = row; else target.b = row; }
    report.revision++;
    report.summaries = ['a', 'b'].map(variant => {
      const runs = report!.cases.map(c => variant === 'a' ? c.a : c.b).filter((r): r is FrozenRun => !!r);
      const measure = (values: number[]) => values.length ? ({ count: values.length, mean: values.reduce((a, b) => a + b, 0) / values.length, minimum: Math.min(...values), maximum: Math.max(...values) }) : emptyMeasure();
      return { variant, expected_cases: report!.cases.length, completed: runs.filter(r => r.record.status === 'completed').length, failed: runs.filter(r => r.record.status === 'failed').length, missing: report!.cases.length - runs.filter(r => r.record.status !== 'missing').length, missing_output: runs.filter(r => r.record.status === 'completed' && !r.record.output).length, accepted: runs.filter(r => r.record.quality === 'accepted').length, rejected: runs.filter(r => r.record.quality === 'rejected').length, unresolved: runs.filter(r => r.record.quality === 'unresolved').length, not_rated: runs.filter(r => r.record.quality === 'not_rated').length, quality_missing: report!.cases.length - runs.filter(r => r.record.quality).length, conditions_unverified: runs.length, elapsed_ms: measure(runs.flatMap(r => r.record.elapsed_ms === null ? [] : [r.record.elapsed_ms])), actual_cost_usd: measure(runs.flatMap(r => r.record.actual_cost_usd === null ? [] : [r.record.actual_cost_usd])), estimated_api_usd: emptyMeasure() };
    });
    report.changed_conditions = ['Synthetic fixture: reported execution conditions are unverified']; imported = null; return report;
  }
  if (command === 'remove_offline_experiment') { report = null; return null; }
  throw new Error('Unknown synthetic offline command');
}
