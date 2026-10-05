<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { getCuratedDataset, getOfflineExperiments, getOfflineExperiment, previewOfflineExperiment, commitOfflineExperiment, previewOfflineImport, commitOfflineImport, removeOfflineExperiment, exportOfflineExperiment } from '../lib/ipc';
  import { organizationStore } from '../lib/stores/organization.svelte';
  import type { CuratedDataset, ExperimentHeader, ExperimentReport, FreezePreview, ImportPreview, PromptVariant, ImportedRun, ComparisonMeasure } from '../lib/types';
  let { onclose }: { onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let dataset = $state<CuratedDataset | null>(null), headers = $state<ExperimentHeader[]>([]), report = $state<ExperimentReport | null>(null);
  let freeze = $state<FreezePreview | null>(null), imported = $state<ImportPreview | null>(null);
  let name = $state(''), rubric = $state(''), phrases = $state(''), rowsText = $state('');
  let variants = $state<PromptVariant[]>(['a', 'b'].map(id => ({ id, prompt_id: '', prompt_version: '', prompt: '', provider: 'codex', model: 'gpt-5.5', service_tier: 'standard' })));
  let reviewed = $state(false), removal = $state(false), busy = $state(false), loading = $state(false), message = $state('');
  let generation = 0, alive = true, seenEpoch = organizationStore.epoch;
  const format = (value: number | null) => value === null ? 'Unavailable' : new Intl.NumberFormat('en-US', { maximumSignificantDigits: 6 }).format(value);
  const describe = (value: ComparisonMeasure) => `mean ${format(value.mean)} · n=${value.count} · range ${format(value.minimum)} to ${format(value.maximum)}`;
  function invalidate() { generation++; freeze = null; imported = null; reviewed = false; message = ''; removal = false; }
  $effect(() => { const epoch = organizationStore.epoch; if (epoch !== seenEpoch) { seenEpoch = epoch; dataset = null; report = null; headers = []; rowsText = ''; invalidate(); void reload(); } });
  onDestroy(() => { alive = false; generation++; });
  onMount(() => { dialog.showModal(); void reload(); });
  async function reload() {
    invalidate(); report = null; loading = true; const request = generation;
    try { const [data, list] = await Promise.all([getCuratedDataset(), getOfflineExperiments()]); if (alive && request === generation) { dataset = data; headers = list; } }
    catch { if (alive && request === generation) message = 'Offline comparisons unavailable. Reload history before retrying.'; }
    finally { if (alive && request === generation) loading = false; }
  }
  async function open(id: number) {
    invalidate(); report = null; rowsText = ''; loading = true; const request = generation;
    try { const value = await getOfflineExperiment(id); if (alive && request === generation) report = value; }
    catch { if (alive && request === generation) message = 'Comparison unavailable. It may have been removed; reload the list.'; }
    finally { if (alive && request === generation) loading = false; }
  }
  async function prepareFreeze() {
    if (!dataset || busy) return; invalidate(); busy = true; const request = generation;
    try { const value = await previewOfflineExperiment({ dataset_revision: dataset.revision, name, rubric, variants, redact_phrases: phrases.split(/\r?\n/).filter(Boolean) }); if (alive && request === generation) freeze = value; }
    catch { if (alive && request === generation) message = 'Freeze preview unavailable. Check the fields, reload the dataset, and review again.'; }
    finally { if (alive) busy = false; }
  }
  async function prepareImport() {
    if (!report || busy) return; invalidate(); busy = true; const request = generation;
    try {
      if (rowsText.length > 1024 * 1024) throw new Error('bounded import');
      const rows = JSON.parse(rowsText) as ImportedRun[];
      const value = await previewOfflineImport({ experiment_id: report.id, revision: report.revision, rows, redact_phrases: phrases.split(/\r?\n/).filter(Boolean) });
      if (alive && request === generation) imported = value;
    } catch { if (alive && request === generation) message = 'Import preview unavailable. Use a bounded JSON array for frozen case IDs; check statuses, token subsets, and measurements.'; }
    finally { if (alive) busy = false; }
  }
  async function save() {
    if ((!freeze && !imported) || !reviewed || busy) return; busy = true; const request = generation;
    try {
      const value = freeze ? await commitOfflineExperiment(freeze.token, true) : await commitOfflineImport(imported!.token, true);
      if (!alive || request !== generation) return;
      report = value; freeze = null; imported = null; reviewed = false; rowsText = ''; message = 'Reviewed offline comparison saved locally.';
      const list = await getOfflineExperiments(); if (alive && request === generation) headers = list;
    } catch { if (alive && request === generation) { freeze = null; imported = null; reviewed = false; message = 'Comparison was not saved. The dataset or results changed; reload and rebuild the preview.'; } }
    finally { if (alive) busy = false; }
  }
  async function remove() {
    if (!report || !removal || busy) return; busy = true; const request = generation;
    try { await removeOfflineExperiment(report.id, report.revision); if (alive && request === generation) { await reload(); message = 'Comparison removed, including its copied inputs and imported outputs. Earlier exports and backups remain separate files.'; } }
    catch { if (alive && request === generation) message = 'Comparison was not removed. Reload before retrying.'; }
    finally { if (alive) busy = false; }
  }
  async function exportReport() {
    if (!report || busy) return; busy = true; const request = generation;
    try { const saved = await exportOfflineExperiment(report.id, report.revision, report.export_digest); if (alive && request === generation) message = saved ? 'Saved this frozen comparison to your selected file.' : 'Comparison export cancelled.'; }
    catch { if (alive && request === generation) message = 'Comparison export could not be saved. No destination details were recorded.'; }
    finally { if (alive) busy = false; }
  }
  function template() {
    if (!report) return;
    rowsText = JSON.stringify([{ case_id: report.manifest.members[0]?.case_id, variant: 'a', status: 'completed', output: 'Replace with the actual output', elapsed_ms: null, actual_cost_usd: null, tokens: null, quality: null, observed_input_hash: '', observed_model: '', observed_provider: '', observed_service_tier: '', observed_prompt_id: '', observed_prompt_version: '', observed_prompt_hash: '', observed_rate_hash: '' }], null, 2);
    invalidate();
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={event => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="offline-heading">
  <header><h2 id="offline-heading">Frozen offline comparisons</h2><button onclick={() => dialog.close()} disabled={busy}>Close comparisons</button></header>
  <div class="body">
    <p>Import results from runs you performed separately. Odometer does not execute prompts or models, access credentials, or send these examples anywhere. Active replay is not enabled.</p>
    <p>These are selected, uncontrolled examples. Cost, elapsed time, and human quality stay separate. Small samples and observed ranges do not establish confidence intervals, causal effects, or a best model.</p>
    <button onclick={() => void reload()} disabled={busy || loading}>Reload comparisons</button>
    {#if loading}<p role="status">Loading offline comparisons…</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    {#if dataset?.recovery_backup_unrestored || report?.recovery_backup_unrestored}<p class="warning">Earlier private comparisons remain in the preserved recovery backup and were not restored.</p>{/if}
    <nav aria-label="Saved offline comparisons">{#each headers as value (value.id)}<button onclick={() => void open(value.id)} disabled={busy || loading}>{value.name} · dataset {value.dataset_revision} · {value.expected_cases} cases</button>{/each}</nav>
    {#if !headers.length && !loading}<p>No frozen comparisons yet. At most eight comparisons can be stored.</p>{/if}
    {#if report}
      <section aria-label="Offline comparison report"><h3>{report.manifest.name} · dataset {report.manifest.dataset_revision} · results revision {report.revision}</h3>
        <p>Frozen {report.manifest.captured_at}. Original denominator: {report.manifest.members.length} cases per variant. Removed or purged cases: {report.removed_cases}. Current dataset: {report.current_dataset_revision}{report.current_dataset_revision !== report.manifest.dataset_revision ? ' · changed since freezing' : ''}.</p>
        <p>Pricing: USD API base estimates from frozen effective rates, separate from observed cost and subscription payments. Snapshot: {report.manifest.rate_snapshot_hash}. {report.current_selected_pricing_differs ? 'At report load, current selected pricing differs; frozen estimates remain unchanged.' : 'At report load, selected configured pricing matches the frozen snapshot.'}</p>
        <p>Rubric: {report.manifest.rubric || 'No additional rubric supplied.'} Quality labels are human-entered imported observations. Matching reported conditions are declarations, not verified execution.</p>
        {#each report.manifest.variants as variant}<details><summary>Variant {variant.conditions.id}: {variant.conditions.provider} · {variant.conditions.model} · {variant.conditions.service_tier} · {variant.rate.basis}</summary><pre>{JSON.stringify(variant, null, 2)}</pre></details>{/each}
        <div class="table"><table><caption>Separate observations with their own sample counts</caption><thead><tr><th>Variant</th><th>Run status</th><th>Human quality</th><th>Elapsed ms</th><th>Observed USD</th><th>Estimated API USD</th></tr></thead><tbody>
          {#each report.summaries as summary}<tr><th>{summary.variant}</th><td>{summary.completed} completed; {summary.failed} failed; {summary.missing} missing / {summary.expected_cases}. {summary.missing_output} completed without output. {summary.conditions_unverified} with unverified conditions.</td><td>{summary.accepted} accepted; {summary.rejected} rejected; {summary.unresolved} unresolved; {summary.not_rated} not rated; {summary.quality_missing} missing / {summary.expected_cases}</td><td>{describe(summary.elapsed_ms)}</td><td>{describe(summary.actual_cost_usd)}</td><td>{describe(summary.estimated_api_usd)}</td></tr>{/each}
        </tbody></table></div>
        <p>Paired B − A: elapsed ms {describe(report.paired_elapsed_delta_ms)}; observed USD {describe(report.paired_actual_cost_delta_usd)}; estimated API USD {describe(report.paired_estimated_cost_delta_usd)}. Each uses only pairs reporting that dimension; failed runs may still report consumed cost and time.</p>
        {#if report.changed_conditions.length}<p class="warning">Reported conditions: {report.changed_conditions.join('; ')}. These observations are not directly comparable under identical conditions.</p>{/if}
        {#each report.cases as value (value.member.case_id)}<details><summary>Frozen case {value.member.case_id} · {value.member.expected_outcome} · {value.input ? value.input.name : 'removed or purged'}</summary>
          <p>Dataset case version {value.member.dataset_case_version}; input hash {value.member.input_hash}.</p>
          {#if value.input}{#each value.input.blocks as block}<h4>{block.role}{block.truncated ? ' · shortened excerpt' : ''}</h4><pre>{block.text}</pre>{/each}{:else}<p>Copied input and imported outputs were removed. The original denominator is retained.</p>{/if}
          {#each [value.a, value.b] as run, index}<h4>Variant {index === 0 ? 'a' : 'b'}</h4>{#if run}<p>{run.record.status}; quality {run.record.quality ?? 'missing'}; elapsed ms {format(run.record.elapsed_ms)}; observed USD {format(run.record.actual_cost_usd)}; estimated API USD {format(run.estimated_api_usd)} ({run.estimate_basis}){run.output_truncated ? ' · shortened output' : ''}.</p><pre>{run.record.output}</pre><p>{run.condition_notes.join('; ') || 'Reported conditions match; execution is not independently verified.'}</p>{:else}<p>Missing run. No output or measurement is assumed.</p>{/if}{/each}
        </details>{/each}
        <button onclick={() => void exportReport()} disabled={busy}>Export frozen comparison JSON</button><button onclick={() => { removal = true; }} disabled={busy}>Review comparison removal</button>
        {#if removal}<p>Remove this comparison and all its copied inputs and outputs? Earlier exports and backups remain separate files.</p><button onclick={() => void remove()} disabled={busy}>Remove reviewed comparison</button><button onclick={() => { removal = false; }}>Cancel removal</button>{/if}
      </section>
      <fieldset disabled={busy || loading}><legend>Import actual offline results</legend>
        <p>One JSON array, at most 128 rows and 1 MiB. Status: completed, failed, or missing. Cost is observed USD; elapsed time is milliseconds. Use null for unreported measurements. Token subsets must reconcile. Human quality requires a completed output. Only frozen case IDs and variants a/b are accepted.</p>
        <button onclick={template}>Insert empty import template</button>
        <p>The template leaves condition evidence blank. Fill it from your run records; do not copy the frozen manifest to imply unverified conditions.</p>
        <label>Imported result JSON<textarea bind:value={rowsText} oninput={invalidate} rows="8" maxlength="1048576"></textarea></label>
        <button onclick={() => void prepareImport()} disabled={!rowsText.trim()}>Build minimized import preview</button>
      </fieldset>
    {/if}
    <fieldset disabled={busy || loading}><legend>Freeze the current curated dataset</legend>
      <p>Dataset {dataset?.revision ?? 'unavailable'} · {dataset?.cases.length ?? 0} cases. Every current case is included. Inputs and prompts are minimized again before the exact preview. Later dataset edits do not rewrite frozen inputs; case removal or source purge removes copied content.</p>
      <label>Comparison name<input bind:value={name} maxlength="128" oninput={invalidate} /></label>
      <label>Outcome review rubric<textarea bind:value={rubric} rows="2" maxlength="1024" oninput={invalidate}></textarea></label>
      {#each variants as variant, index}<fieldset><legend>Variant {variant.id}</legend>
        <label>Prompt identifier {variant.id}<input bind:value={variants[index].prompt_id} maxlength="128" oninput={invalidate} /></label>
        <label>Prompt version {variant.id}<input bind:value={variants[index].prompt_version} maxlength="128" oninput={invalidate} /></label>
        <label>Prompt text {variant.id}<textarea bind:value={variants[index].prompt} rows="3" maxlength="4096" oninput={invalidate}></textarea></label>
        <label>Provider {variant.id}<select bind:value={variants[index].provider} onchange={invalidate}><option value="codex">Codex</option><option value="claude_code">Claude Code</option><option value="gemini_cli">Gemini CLI</option></select></label>
        <label>Model identifier {variant.id}<input bind:value={variants[index].model} maxlength="128" oninput={invalidate} /></label>
        <label>Service tier {variant.id}<select bind:value={variants[index].service_tier} onchange={invalidate}><option value="standard">Standard</option><option value="fast">Fast</option><option value="ultrafast">Ultrafast</option></select></label>
      </fieldset>{/each}
      <label>Also redact these exact phrases, one per line<textarea bind:value={phrases} rows="2" maxlength="25700" oninput={invalidate}></textarea></label>
      <p>Automatic redaction cannot find every secret. Inspect all text, frozen inputs, prompts, and imported outputs before storing or exporting them.</p>
      <button onclick={() => void prepareFreeze()} disabled={!dataset?.cases.length || !name.trim() || variants.some(v => !v.prompt_id.trim() || !v.prompt_version.trim() || !v.prompt.trim() || !v.model.trim())}>Build frozen comparison preview</button>
    </fieldset>
    {#if freeze || imported}<section aria-label="Exact offline comparison preview"><h3>Exact minimized {freeze ? 'frozen comparison' : 'result import'} preview</h3><pre>{JSON.stringify(freeze ? { manifest: freeze.manifest, inputs: freeze.inputs } : imported?.rows, null, 2)}</pre>
      <label><input type="checkbox" bind:checked={reviewed} />I reviewed all included content, conditions, and redactions for local storage</label>
      <button onclick={() => void save()} disabled={!reviewed || busy}>Save reviewed {freeze ? 'frozen comparison' : 'offline results'}</button>
    </section>{/if}
  </div>
</dialog>

<style>
  dialog { width: min(1100px, calc(100vw - 24px)); max-height: calc(100vh - 24px); margin: auto; padding: 0; color: var(--text); background: var(--panel); border: 1px solid var(--border); border-radius: 8px; }
  dialog::backdrop { background: #0009; } header { position: sticky; top: 0; z-index: 1; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 16px; background: var(--panel); border-bottom: 1px solid var(--border); } h2 { margin: 0; font-size: 1rem; font-weight: 600; } .body { padding: 16px; font-size: .75rem; display: flex; flex-direction: column; gap: 12px; }
  label { display: block; margin: 10px 0; } input:not([type=checkbox]), textarea, select { display: block; width: 100%; box-sizing: border-box; color: inherit; background: var(--bg); border: 1px solid var(--border); padding: 7px; } fieldset, section { padding: 12px; border: 1px solid var(--border); border-radius: 4px; } fieldset { margin: 16px 0; } pre { white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg); padding: 10px; } button { margin: 4px 8px 4px 0; border: 1px solid var(--border); border-radius: 4px; padding: 5px 10px; } button:disabled { opacity: .5; } details { border: 1px solid var(--border); margin: 10px 0; padding: 10px; } .warning { color: var(--amber, #b66d00); } .table { overflow: auto; } table { width: 100%; border-collapse: collapse; } th, td { text-align: left; vertical-align: top; padding: 8px; border: 1px solid var(--border); min-width: 115px; } p { overflow-wrap: anywhere; } nav { display: flex; flex-wrap: wrap; }
</style>
