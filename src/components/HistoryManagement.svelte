<script lang="ts">
  import { onMount } from 'svelte';
  import { getHistoryRecoveryStatus, getRetentionStatus, setRetentionPolicy, previewHistoryPurge, purgeRetainedHistory, recoverHistory, retryHistoryOpen, onHistoryProgress } from '../lib/ipc';
  import type { HistoryRecoveryStatus, RetentionStatus, PurgePreview } from '../lib/types';
  import { formatBytes } from '../lib/format';

  let health = $state<HistoryRecoveryStatus | null>(null);
  let retention = $state<RetentionStatus | null>(null);
  let days = $state('');
  let policyDirty = $state(false);
  let refreshRevision = 0;
  let preview = $state<PurgePreview | null>(null);
  let confirmation = $state('');
  let recoveryReview = $state(false);
  let recoveryConfirmation = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let result = $state<string | null>(null);
  const count = (n: number) => new Intl.NumberFormat().format(n);

  async function refresh() {
    const revision = ++refreshRevision;
    try {
      const nextHealth = await getHistoryRecoveryStatus();
      const nextRetention = nextHealth.status === 'ready' ? await getRetentionStatus() : null;
      if (revision !== refreshRevision) return;
      health = nextHealth;
      retention = nextRetention;
      if (!policyDirty && nextRetention) days = nextRetention.policy.retained_days?.toString() ?? '';
      error = null;
    } catch (e) { if (revision === refreshRevision) error = String(e); }
  }

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void refresh();
    void onHistoryProgress(() => { if (!disposed) void refresh(); }).then(fn => { if (disposed) fn(); else unlisten = fn; }).catch(() => {});
    return () => { disposed = true; refreshRevision += 1; unlisten?.(); };
  });

  async function action(run: () => Promise<void>) {
    busy = true;
    error = null;
    result = null;
    try { await run(); } catch (e) { error = String(e); }
    finally { busy = false; }
  }

  function savePolicy() {
    void action(async () => {
      retention = await setRetentionPolicy({ retained_days: days === '' ? null : Number(days) });
      policyDirty = false;
      preview = null;
      confirmation = '';
      result = 'Retention policy saved. Nothing was deleted.';
    });
  }

  function reviewPurge() {
    void action(async () => {
      preview = await previewHistoryPurge();
      confirmation = '';
    });
  }

  function confirmPurge() {
    if (!preview || confirmation !== `PURGE ${preview.sessions}`) return;
    const reviewed = preview;
    void action(async () => {
      const removed = await purgeRetainedHistory(reviewed.revision, confirmation);
      preview = null;
      confirmation = '';
      await refresh();
      result = `${count(removed.removed_keys.length)} retained sessions purged. Provider source files were preserved.`;
    });
  }

  function confirmRecovery() {
    if (recoveryConfirmation !== 'REBUILD READABLE HISTORY') return;
    void action(async () => {
      await recoverHistory(recoveryConfirmation);
      recoveryReview = false;
      recoveryConfirmation = '';
      await refresh();
      result = 'Original history preserved. Readable configured sources are being scanned into a replacement with incomplete historical coverage.';
    });
  }
</script>

<section id="history-retention-settings" class="max-w-3xl" aria-label="History retention and recovery">
  <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Retention and recovery</h2>
  <p class="text-xs text-ink-faint mb-3">Missing or replaced transcripts keep their local history until you review and confirm a purge. Provider archival status is separate.</p>
  <div class="bg-card border border-edge rounded-lg px-4 py-3 space-y-3">
    {#if !health}
      <p class="text-xs text-ink-faint">Checking durable history…</p>
    {:else if health.status === 'pending'}
      <p class="text-xs text-ink-faint">Durable history is preparing. Live source transcripts remain accessible.</p>
    {:else if health.status === 'unavailable'}
      <p class="text-xs text-amber-500" role="status">{health.failure?.message ?? 'Durable history is unavailable. Live source transcripts remain accessible.'}</p>
      <div class="flex gap-3 flex-wrap">
        {#if health.can_retry}<button class="px-3 py-1.5 text-xs border border-edge rounded-sm" disabled={busy} onclick={() => void action(async () => { await retryHistoryOpen(); await refresh(); })}>Retry opening history</button>{/if}
        {#if health.can_recover}
          <button class="px-3 py-1.5 text-xs border border-edge rounded-sm" disabled={busy} onclick={() => { recoveryReview = true; recoveryConfirmation = ''; }}>Preserve and rebuild readable history…</button>
        {/if}
      </div>
    {:else if retention}
      <p class="text-xs text-ink-muted">{count(retention.present_sessions)} present · {count(retention.retained_sessions)} retained · {count(retention.superseded_sessions)} superseded · {count(retention.purged_sessions)} exclusion records</p>
      {#if !retention.coverage_complete}
        <p class="text-xs text-amber-500" role="status">Historical coverage is incomplete. History was intentionally purged or historical sources were not recovered. Token and money budgets are unavailable.</p>
      {/if}
      <div class="flex items-end gap-3 flex-wrap">
        <label class="text-xs text-ink-muted">Retained-history policy
          <select aria-label="Retained-history policy" class="block mt-1 bg-card border border-edge rounded-sm px-2 py-1.5" bind:value={days} onchange={() => { policyDirty = true; }} disabled={busy}>
            <option value="">Keep all history</option>
            <option value="30">Review activity older than 30 days</option>
            <option value="90">Review activity older than 90 days</option>
            <option value="365">Review activity older than 365 days</option>
            {#if days !== '' && !['30', '90', '365'].includes(days)}<option value={days}>Review activity older than {days} days</option>{/if}
          </select>
        </label>
        <button class="px-3 py-1.5 text-xs border border-edge rounded-sm" disabled={busy} onclick={savePolicy}>Save policy</button>
        <button class="px-3 py-1.5 text-xs border border-edge rounded-sm" disabled={busy || policyDirty || retention.policy.retained_days === null} onclick={reviewPurge}>Review eligible history…</button>
      </div>
      <p class="text-[11px] text-ink-faint">The policy selects candidates and never deletes automatically. Present sessions and groups with a present or newer sibling are excluded.</p>
    {/if}

    {#if preview}
      <div class="border border-amber-500/40 rounded-sm p-3 space-y-2" aria-label="Purge preview">
        <p class="text-xs text-ink-muted">{count(preview.sessions)} retained {preview.sessions === 1 ? 'session' : 'sessions'} in {count(preview.identity_groups)} identity {preview.identity_groups === 1 ? 'group' : 'groups'} with activity before {preview.cutoff_utc_day} UTC. Selected snapshots contain {formatBytes(preview.snapshot_bytes)}.</p>
        <p class="text-xs text-amber-500">This removes their local snapshots, usage facts, rollups, session-specific project assignments, pins, tags, and private notes. Human outcome labels, record bookmarks, and curated examples from these sessions are also removed. Saved searches and tag definitions remain. Historical coverage becomes incomplete, so token and money budgets become unavailable. Provider files and earlier recovery backups remain. Minimal fingerprint exclusions prevent copied, moved, or resumed erased history from reimporting. New fingerprints can be imported.</p>
        {#if preview.sessions > 0}
          <label class="block text-xs text-ink-muted">Type PURGE {preview.sessions} to confirm
            <input aria-label="Purge confirmation" class="block w-full mt-1 px-2 py-1.5 bg-card border border-edge rounded-sm" bind:value={confirmation} disabled={busy} autocomplete="off" />
          </label>
          <button class="px-3 py-1.5 text-xs border border-red-500/50 rounded-sm text-red-500 disabled:opacity-50" disabled={busy || confirmation !== `PURGE ${preview.sessions}`} onclick={confirmPurge}>Purge reviewed history</button>
        {:else}<p class="text-xs text-ink-faint">No retained history is eligible under this policy.</p>{/if}
        <button class="ml-3 text-xs text-ink-muted hover:underline" disabled={busy} onclick={() => { preview = null; confirmation = ''; }}>Cancel review</button>
      </div>
    {/if}

    {#if recoveryReview}
      <div class="border border-amber-500/40 rounded-sm p-3 space-y-2" aria-label="Recovery review">
        <p class="text-xs text-amber-500">The original database and SQLite sidecars will be preserved in a new local backup. A replacement can rebuild readable configured sources only. Missing-source history remains in the backup and will not count as recovered. Verified purge exclusions are preserved; unverifiable exclusions stop recovery.</p>
        <label class="block text-xs text-ink-muted">Type REBUILD READABLE HISTORY to confirm
          <input aria-label="Recovery confirmation" class="block w-full mt-1 px-2 py-1.5 bg-card border border-edge rounded-sm" bind:value={recoveryConfirmation} disabled={busy} autocomplete="off" />
        </label>
        <button class="px-3 py-1.5 text-xs border border-amber-500/50 rounded-sm disabled:opacity-50" disabled={busy || recoveryConfirmation !== 'REBUILD READABLE HISTORY'} onclick={confirmRecovery}>Preserve and rebuild</button>
        <button class="ml-3 text-xs text-ink-muted hover:underline" disabled={busy} onclick={() => { recoveryReview = false; }}>Cancel recovery</button>
      </div>
    {/if}

    {#if health?.backup_directory}<p class="text-[11px] text-ink-faint break-all">Preserved history: {health.backup_directory}</p>{/if}
    {#if error}<p class="text-xs text-red-500" role="alert">{error}</p>{/if}
    {#if result}<p class="text-xs text-ink-muted" role="status">{result}</p>{/if}
    <button class="text-xs text-ink-muted hover:underline" disabled={busy} onclick={() => void refresh()}>Refresh history status</button>
  </div>
</section>
