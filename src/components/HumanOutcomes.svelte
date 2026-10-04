<script lang="ts">
  import type { SessionSummary } from '../lib/types';
  import { humanOutcomeReport } from '../lib/humanOutcomes';
  import { organizationStore } from '../lib/stores/organization.svelte';
  import { writeExport } from '../lib/ipc';
  let { sessions }: { sessions: SessionSummary[] } = $props();
  const report = $derived(humanOutcomeReport(sessions, organizationStore.summaries, organizationStore.recoveryUnrestored));
  let busy = $state(false);
  let error = $state<string | null>(null);
  async function exportOutcomes() {
    busy = true; error = null;
    try {
      await writeExport('odometer-human-outcomes.json', 'json', JSON.stringify({
        measurement: 'Explicit human labels. One root session plus linked subagent work forms a task; subagent labels are excluded from this denominator. Repair minutes are user-reported; observed session duration and accepted end-to-end delivery time are not measured here.',
        recovery_backup_unrestored: organizationStore.recoveryUnrestored, ...report,
      }, null, 2) + '\n');
    } catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
</script>

<details class="bg-card border border-edge rounded-lg px-3 py-2" aria-label="Human task outcomes">
  <summary class="cursor-pointer text-xs font-semibold text-ink">Human outcomes · sessions in current filter</summary>
  <div class="space-y-2 mt-3 text-xs">
    <p class="text-ink-muted">Labels are entered in Edit organization. One root session and its linked subagent work form one task. Subagent labels remain local and are excluded from this summary.</p>
    {#if organizationStore.busy}<p role="status">Loading human outcome metadata…</p>
    {:else if organizationStore.error}<p role="alert">Human outcomes unavailable: {organizationStore.error}</p>
    {:else}
      {#if organizationStore.recoveryUnrestored}<p class="text-amber-500">Earlier labels and repair notes remain in the recovery backup. Unrestored ratings are unavailable, not unrated.</p>{/if}
      {#if report.tasks_omitted}<p class="text-amber-500">Limited to the first {report.tasks} filtered tasks; {report.tasks_omitted} tasks are outside these counts. Narrow filters for a complete report.</p>{/if}
      <p>{report.labelled} labelled / {report.tasks} tasks · {report.not_rated} not rated · {report.metadata_missing} ratings unavailable</p>
      <p>{report.accepted} accepted · {report.rejected} rejected · {report.unresolved} unresolved · {report.subagents_excluded} subagent sessions excluded</p>
      <p>First-pass acceptance: {report.first_pass_reported ? `${report.first_pass_accepted} / ${report.first_pass_reported} explicitly reported` : 'Unavailable — no explicit reports'}. {report.labelled - report.first_pass_reported} labelled tasks lack first-pass evidence.</p>
      <p>User-reported repair time: {report.repair_minutes == null ? 'Unavailable' : `${report.repair_minutes} minutes`} · reported for {report.repair_reported} / {report.tasks} tasks.</p>
      <p class="text-ink-faint">Ratings apply to the entire task, even when the date filter selects part of its activity. Repair time is separate from observed session duration. Accepted end-to-end delivery time is not measured. These labels produce no productivity score or causal claim.</p>
      <button class="text-accent underline" disabled={busy || !report.tasks} onclick={() => void exportOutcomes()}>Export structured human outcomes JSON</button>
      <p class="text-ink-faint">This explicit export contains labels, optional effort, and coverage counts. Private notes stay out of all exports, diagnostics, logs, and MCP; ordinary session exports omit human outcomes.</p>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </div>
</details>
