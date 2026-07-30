<script lang="ts">
  import { config } from '../lib/stores/config';
  import { rates } from '../lib/stores/rates';
  import { updaterStore } from '../lib/stores/updater.svelte';
  import { themeStore, type ThemePreference } from '../lib/stores/theme.svelte';
  import { setConfig, setRates, getBundledRates, addDefenderExclusions, exportPerformanceData, getPerformanceStatus, getTurnReceiptStatus, repairTurnReceiptIntegrations } from '../lib/ipc';
  import { getVersion } from '@tauri-apps/api/app';
  import { isTauri } from '@tauri-apps/api/core';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { onMount } from 'svelte';
  import type { RateCard, ModelRate, PerformanceStatus, InstructionRoot, TurnReceiptIntegrationStatus, PricingCatalog } from '../lib/types';
  import { configurePerformanceTracking } from '../lib/performance';

  const RELEASE_NOTES_URL = 'https://github.com/ekalb81/agent-odometer/releases';

  interface Props { onopeninstructions?: () => void; }
  let { onopeninstructions = () => {} }: Props = $props();

  let appVersion = $state('');
  let releaseNotesError = $state<string | null>(null);
  onMount(() => {
    void getVersion().then((v) => (appVersion = v)).catch(() => {});
    void refreshPerformanceStatus();
    void refreshTurnReceiptStatus();
  });

  async function handleReleaseNotes() {
    releaseNotesError = null;
    try {
      if (isTauri()) {
        await openUrl(RELEASE_NOTES_URL);
      } else {
        window.open(RELEASE_NOTES_URL, '_blank', 'noopener,noreferrer');
      }
    } catch (error) {
      releaseNotesError = String(error);
    }
  }

  const themeOptions: { value: ThemePreference; label: string }[] = [
    { value: 'system', label: 'System' },
    { value: 'dark', label: 'Dark' },
    { value: 'light', label: 'Light' },
  ];

  // ---------------------------------------------------------------------------
  // Editable watched roots — local copies diverge from the store until saved.
  // ---------------------------------------------------------------------------

  let editedSessionRoots = $state<string[]>([]);
  let editedArchiveRoots = $state<string[]>([]);
  let editedClaudeRoots = $state<string[]>([]);
  let editedIndexPath = $state('');
  let newSessionRoot = $state('');
  let newArchiveRoot = $state('');
  let newClaudeRoot = $state('');

  let rootsDirty = $state(false);
  let rootsSaving = $state(false);
  let rootsSavedAt = $state<string | null>(null);
  let rootsSaveError = $state<string | null>(null);

  // Sync editable roots from the config store whenever config changes, but
  // only when there are no pending edits (don't clobber user's in-progress work).
  $effect(() => {
    const c = $config;
    if (!rootsDirty) {
      editedSessionRoots = [...c.session_roots];
      editedArchiveRoots = [...c.archive_roots];
      editedClaudeRoots = [...(c.claude_session_roots ?? [])];
      editedIndexPath = c.session_index_path ?? '';
    }
  });

  const hasDuplicateRoots = $derived(
    new Set(editedSessionRoots).size !== editedSessionRoots.length ||
    new Set(editedArchiveRoots).size !== editedArchiveRoots.length ||
    new Set(editedClaudeRoots).size !== editedClaudeRoots.length,
  );

  function markRootsDirty() {
    rootsDirty = true;
    rootsSavedAt = null;
    rootsSaveError = null;
  }

  function addSessionRoot() {
    const v = newSessionRoot.trim();
    if (!v) return;
    editedSessionRoots = [...editedSessionRoots, v];
    newSessionRoot = '';
    markRootsDirty();
  }

  function removeSessionRoot(i: number) {
    editedSessionRoots = editedSessionRoots.filter((_, idx) => idx !== i);
    markRootsDirty();
  }

  function addArchiveRoot() {
    const v = newArchiveRoot.trim();
    if (!v) return;
    editedArchiveRoots = [...editedArchiveRoots, v];
    newArchiveRoot = '';
    markRootsDirty();
  }

  function removeArchiveRoot(i: number) {
    editedArchiveRoots = editedArchiveRoots.filter((_, idx) => idx !== i);
    markRootsDirty();
  }

  function addClaudeRoot() {
    const v = newClaudeRoot.trim();
    if (!v) return;
    editedClaudeRoots = [...editedClaudeRoots, v];
    newClaudeRoot = '';
    markRootsDirty();
  }

  function removeClaudeRoot(i: number) {
    editedClaudeRoots = editedClaudeRoots.filter((_, idx) => idx !== i);
    markRootsDirty();
  }

  function resetRoots() {
    rootsDirty = false;
    rootsSavedAt = null;
    rootsSaveError = null;
    // Let the $effect above re-sync from the store.
  }

  // ---------------------------------------------------------------------------
  // Performance: Windows Defender exclusions (Windows only).
  // ---------------------------------------------------------------------------
  const isWindows = navigator.userAgent.includes('Windows');
  let defenderState = $state<'idle' | 'pending' | 'done'>('idle');
  let defenderError = $state<string | null>(null);

  async function handleDefenderExclusions() {
    defenderError = null;
    defenderState = 'pending';
    try {
      // Resolves only after the elevated process finishes, so success here
      // means the exclusions really were added.
      await addDefenderExclusions();
      defenderState = 'done';
    } catch (e) {
      defenderState = 'idle';
      defenderError = String(e);
    }
  }

  async function saveRoots() {
    rootsSaving = true;
    rootsSaveError = null;
    try {
      await setConfig({
        ...$config,
        session_roots: editedSessionRoots,
        archive_roots: editedArchiveRoots,
        session_index_path: editedIndexPath.trim(),
        claude_session_roots: editedClaudeRoots,
      });
      rootsDirty = false;
      rootsSavedAt = new Date().toLocaleTimeString();
    } catch (e) {
      rootsSaveError = String(e);
    } finally {
      rootsSaving = false;
    }
  }

  // ---------------------------------------------------------------------------
  // Opt-in read-only instruction inventory.
  // ---------------------------------------------------------------------------
  let instructionsEnabled = $state(false);
  let instructionsTabVisible = $state(true);
  let editedInstructionRoots = $state<InstructionRoot[]>([]);
  let newInstructionRoot = $state('');
  let newInstructionRootRecursive = $state(true);
  let instructionsDirty = $state(false);
  let instructionsSaving = $state(false);
  let instructionsSavedAt = $state<string | null>(null);
  let instructionsError = $state<string | null>(null);

  $effect(() => {
    const current = $config;
    if (!instructionsDirty) {
      instructionsEnabled = current.instructions_enabled ?? false;
      instructionsTabVisible = current.instructions_tab_visible ?? true;
      editedInstructionRoots = (current.instruction_roots ?? []).map((root) => ({ ...root }));
    }
  });

  const hasDuplicateInstructionRoots = $derived((() => {
    const normalized = editedInstructionRoots.map((root) =>
      root.path.replaceAll('\\', '/').replace(/\/$/, '').toLocaleLowerCase(),
    );
    return new Set(normalized).size !== normalized.length;
  })());

  function markInstructionsDirty() {
    instructionsDirty = true;
    instructionsSavedAt = null;
    instructionsError = null;
  }

  function addInstructionRoot() {
    const path = newInstructionRoot.trim();
    if (!path) return;
    editedInstructionRoots = [
      ...editedInstructionRoots,
      { path, recursive: newInstructionRootRecursive },
    ];
    newInstructionRoot = '';
    markInstructionsDirty();
  }

  function removeInstructionRoot(index: number) {
    editedInstructionRoots = editedInstructionRoots.filter((_, candidate) => candidate !== index);
    markInstructionsDirty();
  }

  function setInstructionRootRecursive(index: number, recursive: boolean) {
    editedInstructionRoots = editedInstructionRoots.map((root, candidate) =>
      candidate === index ? { ...root, recursive } : root,
    );
    markInstructionsDirty();
  }

  async function saveInstructionSettings() {
    if (hasDuplicateInstructionRoots) {
      instructionsError = 'Remove duplicate instruction roots before saving.';
      return;
    }
    instructionsSaving = true;
    instructionsError = null;
    try {
      const updated = {
        ...$config,
        instructions_enabled: instructionsEnabled,
        instructions_tab_visible: instructionsTabVisible,
        instruction_roots: editedInstructionRoots,
      };
      await setConfig(updated);
      config.set(updated);
      instructionsDirty = false;
      instructionsSavedAt = new Date().toLocaleTimeString();
    } catch (reason) {
      instructionsError = String(reason);
    } finally {
      instructionsSaving = false;
    }
  }

  // Opt-in harness turn receipts.
  // ---------------------------------------------------------------------------
  let turnReceiptsEnabled = $state(false);
  let turnReceiptsCodex = $state(true);
  let turnReceiptsClaude = $state(true);
  let turnReceiptsDirty = $state(false);
  let turnReceiptsSaving = $state(false);
  let turnReceiptsSavedAt = $state<string | null>(null);
  let turnReceiptsError = $state<string | null>(null);
  let turnReceiptStatus = $state<TurnReceiptIntegrationStatus | null>(null);

  $effect(() => {
    const current = $config;
    if (!turnReceiptsDirty) {
      turnReceiptsEnabled = current.turn_receipts_enabled ?? false;
      turnReceiptsCodex = current.turn_receipts_codex ?? true;
      turnReceiptsClaude = current.turn_receipts_claude ?? true;
    }
  });

  function markTurnReceiptsDirty() {
    turnReceiptsDirty = true;
    turnReceiptsSavedAt = null;
    turnReceiptsError = null;
  }

  async function refreshTurnReceiptStatus() {
    try {
      turnReceiptStatus = await getTurnReceiptStatus();
    } catch {
      turnReceiptStatus = null;
    }
  }

  async function saveTurnReceipts() {
    if (turnReceiptsEnabled && !turnReceiptsCodex && !turnReceiptsClaude) {
      turnReceiptsError = 'Select at least one harness, or turn the feature off.';
      return;
    }
    turnReceiptsSaving = true;
    turnReceiptsError = null;
    try {
      const updatedConfig = {
        ...$config,
        turn_receipts_enabled: turnReceiptsEnabled,
        turn_receipts_codex: turnReceiptsCodex,
        turn_receipts_claude: turnReceiptsClaude,
      };
      await setConfig(updatedConfig);
      config.set(updatedConfig);
      turnReceiptsDirty = false;
      turnReceiptsSavedAt = new Date().toLocaleTimeString();
      await refreshTurnReceiptStatus();
    } catch (error) {
      turnReceiptsError = String(error);
      await refreshTurnReceiptStatus();
    } finally {
      turnReceiptsSaving = false;
    }
  }

  async function repairTurnReceipts() {
    turnReceiptsSaving = true;
    turnReceiptsError = null;
    try {
      turnReceiptStatus = await repairTurnReceiptIntegrations();
    } catch (error) {
      turnReceiptsError = String(error);
      await refreshTurnReceiptStatus();
    } finally {
      turnReceiptsSaving = false;
    }
  }

  function formatHookRun(value: string | null): string {
    if (!value) return 'No receipt observed yet';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : `Last observed ${date.toLocaleString()}`;
  }

  // ---------------------------------------------------------------------------
  // Opt-in local performance measurements.
  // ---------------------------------------------------------------------------
  let performanceEnabled = $state(false);
  let performanceMaxMb = $state(64);
  let performanceDirty = $state(false);
  let performanceSaving = $state(false);
  let performanceSavedAt = $state<string | null>(null);
  let performanceError = $state<string | null>(null);
  let performanceStatus = $state<PerformanceStatus | null>(null);
  let performanceExporting = $state(false);

  $effect(() => {
    const current = $config;
    if (!performanceDirty) {
      performanceEnabled = current.performance_tracking_enabled ?? false;
      performanceMaxMb = current.performance_log_max_mb ?? 64;
    }
  });

  function markPerformanceDirty() {
    performanceDirty = true;
    performanceSavedAt = null;
    performanceError = null;
  }

  async function refreshPerformanceStatus() {
    try {
      performanceStatus = await getPerformanceStatus();
    } catch {
      performanceStatus = null;
    }
  }

  async function savePerformanceSettings() {
    const maxMb = Math.round(Number(performanceMaxMb));
    if (!Number.isFinite(maxMb) || maxMb < 1 || maxMb > 1024) {
      performanceError = 'Log size must be between 1 and 1024 MiB.';
      return;
    }
    performanceSaving = true;
    performanceError = null;
    try {
      const updatedConfig = {
        ...$config,
        performance_tracking_enabled: performanceEnabled,
        performance_log_max_mb: maxMb,
      };
      await setConfig(updatedConfig);
      config.set(updatedConfig);
      configurePerformanceTracking(performanceEnabled);
      performanceDirty = false;
      performanceSavedAt = new Date().toLocaleTimeString();
      await refreshPerformanceStatus();
    } catch (error) {
      performanceError = String(error);
    } finally {
      performanceSaving = false;
    }
  }

  async function exportPerformance(format: 'jsonl' | 'csv') {
    performanceExporting = true;
    performanceError = null;
    try {
      await exportPerformanceData(format);
      await refreshPerformanceStatus();
    } catch (error) {
      performanceError = String(error);
    } finally {
      performanceExporting = false;
    }
  }

  function formatBytes(value: number): string {
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
    return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  }

  // ---------------------------------------------------------------------------
  // Local editable copy of the rate card, kept in sync with the store on mount.
  // ---------------------------------------------------------------------------

  // Each row in the editor: model name + four rate fields as strings (for input binding).
  interface RateRow {
    name: string;
    input: string;
    cached_input: string;
    output: string;
    reasoning: string;
  }

  let rows = $state<RateRow[]>([]);
  let fallbackModel = $state('');
  let sourceUrl = $state('');
  let fetchedAt = $state<string | null>(null);
  let ratesVersion = $state(1);
  let ratesCurrency = $state('credits');
  let ratesUnit = $state('per_1m_tokens');
  // Per-harness maps are not editable here yet; carry them through saves.
  let ratesCurrencies = $state<Record<string, string>>({});
  let ratesFallbackModels = $state<Record<string, string>>({});
  // API USD rates for Codex models; not editable here yet, carried through.
  let ratesApiModels = $state<Record<string, ModelRate>>({});
  let ratesUnpricedModels = $state<string[]>([]);
  // Catalog rules have their own provenance and validation contract. The
  // editor does not modify them yet, so preserve the complete object verbatim.
  let pricingCatalog = $state<PricingCatalog>({ rate_periods: [], conditional_modifiers: [], notes: [] });

  // New-model form.
  let newName = $state('');
  let newInput = $state('');
  let newCachedInput = $state('');
  let newOutput = $state('');
  let newReasoning = $state('');

  // UI state.
  let dirty = $state(false);
  let saving = $state(false);
  let savedAt = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let validationError = $state<string | null>(null);

  // Populate local state from the store whenever the rates store changes.
  $effect(() => {
    const r = $rates;
    if (!r) return;
    rows = Object.entries(r.models).map(([name, rate]) => ({
      name,
      input: String(rate.input),
      cached_input: String(rate.cached_input),
      output: String(rate.output),
      reasoning: String(rate.reasoning),
    }));
    fallbackModel = r.fallback_model;
    sourceUrl = r.source_url;
    fetchedAt = r.fetched_at;
    ratesVersion = r.version;
    ratesCurrency = r.currency;
    ratesUnit = r.unit;
    ratesCurrencies = { ...(r.currencies ?? {}) };
    ratesFallbackModels = { ...(r.fallback_models ?? {}) };
    ratesApiModels = { ...(r.api_models ?? {}) };
    ratesUnpricedModels = [...(r.unpriced_models ?? [])];
    pricingCatalog = r.pricing_catalog;
    dirty = false;
  });

  function markDirty() {
    dirty = true;
    savedAt = null;
    saveError = null;
    validationError = null;
  }

  function parseRate(s: string): number | null {
    const n = parseFloat(s);
    if (isNaN(n) || n < 0) return null;
    return n;
  }

  /** Catalog intervals are intentionally shown as UTC half-open windows so a
   * temporary price does not look like a current default. */
  function fmtPricingWindow(from: string, to: string | null): string {
    const date = (iso: string) => `${iso.slice(0, 10)} UTC`;
    return `[${date(from)}, ${to ? date(to) : 'open-ended'})`;
  }

  function fmtPricingSurface(surface: PricingCatalog['rate_periods'][number]['surface']): string {
    if (surface === 'codex_plan_credits') return 'Codex plan credits';
    if (surface === 'openai_api_usd') return 'OpenAI API USD';
    return 'Anthropic API USD';
  }

  function buildRateCard(): RateCard | null {
    // Validate fallback model.
    if (!fallbackModel) {
      validationError = 'A fallback model must be selected.';
      return null;
    }
    // Validate rows.
    const models: Record<string, ModelRate> = {};
    for (const row of rows) {
      if (!row.name.trim()) {
        validationError = 'All model names must be non-empty.';
        return null;
      }
      const input = parseRate(row.input);
      const cached_input = parseRate(row.cached_input);
      const output = parseRate(row.output);
      const reasoning = parseRate(row.reasoning);
      if (input === null || cached_input === null || output === null || reasoning === null) {
        validationError = `Rates for "${row.name}" must be non-negative numbers.`;
        return null;
      }
      models[row.name.trim()] = { input, cached_input, output, reasoning };
    }
    if (!models[fallbackModel]) {
      validationError = `Fallback model "${fallbackModel}" is not in the model list.`;
      return null;
    }
    validationError = null;
    return {
      version: ratesVersion,
      currency: ratesCurrency,
      unit: ratesUnit,
      source_url: sourceUrl,
      fetched_at: fetchedAt,
      models,
      fallback_model: fallbackModel,
      currencies: ratesCurrencies,
      fallback_models: ratesFallbackModels,
      api_models: ratesApiModels,
      unpriced_models: ratesUnpricedModels,
      pricing_catalog: pricingCatalog,
    };
  }

  async function handleSave() {
    const card = buildRateCard();
    if (!card) return;
    saving = true;
    saveError = null;
    try {
      await setRates(card);
      // rates store will update via the rates-updated event, but also set locally.
      rates.set(card);
      dirty = false;
      const now = new Date();
      savedAt = now.toLocaleTimeString();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  async function handleReset() {
    if (!window.confirm('Reset to shipped defaults? This will overwrite your current rates.')) return;
    saving = true;
    saveError = null;
    try {
      const bundled = await getBundledRates();
      await setRates(bundled);
      rates.set(bundled);
      dirty = false;
      const now = new Date();
      savedAt = now.toLocaleTimeString();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  function removeRow(index: number) {
    // Capture the name before filtering — afterwards rows[index] is a different row.
    const removedName = rows[index]?.name;
    rows = rows.filter((_, i) => i !== index);
    if (fallbackModel === removedName) fallbackModel = '';
    markDirty();
  }

  function addModel() {
    const name = newName.trim();
    if (!name) {
      validationError = 'Model name is required.';
      return;
    }
    const input = parseRate(newInput);
    const cached_input = parseRate(newCachedInput);
    const output = parseRate(newOutput);
    const reasoning = parseRate(newReasoning);
    if (input === null || cached_input === null || output === null || reasoning === null) {
      validationError = 'All rate fields must be non-negative numbers.';
      return;
    }
    rows = [...rows, {
      name,
      input: String(input),
      cached_input: String(cached_input),
      output: String(output),
      reasoning: String(reasoning),
    }];
    newName = '';
    newInput = '';
    newCachedInput = '';
    newOutput = '';
    newReasoning = '';
    validationError = null;
    markDirty();
  }
</script>

<div class="flex flex-col gap-6 p-6 overflow-auto h-full">

  <!-- About & updates -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">About &amp; updates</h2>
    <div class="flex items-center gap-3 flex-wrap text-sm text-ink-2">
      <span>Odometer <span class="font-mono">{appVersion ? `v${appVersion}` : '…'}</span></span>
      <button
        type="button"
        onclick={() => void handleReleaseNotes()}
        class="text-xs text-accent-chipfg hover:opacity-80 underline underline-offset-2 transition-colors"
        aria-label="Open release notes in the default browser"
      >release notes</button>
      {#if updaterStore.available}
        <button
          onclick={() => void updaterStore.install()}
          disabled={updaterStore.phase === 'installing'}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 text-white transition-colors"
        >
          {updaterStore.phase === 'installing'
            ? `Downloading v${updaterStore.available.version}…`
            : `Update to v${updaterStore.available.version} & restart`}
        </button>
      {:else}
        <button
          onclick={() => void updaterStore.checkNow(true)}
          disabled={updaterStore.phase === 'checking'}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-app hover:bg-(--row-hover) disabled:opacity-40 text-ink transition-colors"
        >
          {updaterStore.phase === 'checking'
            ? 'Checking…'
            : updaterStore.lastManualResult === 'uptodate'
              ? 'Up to date'
              : 'Check for updates'}
        </button>
        {#if updaterStore.lastManualResult === 'uptodate'}
          <span class="text-xs text-pos" role="status" aria-live="polite">
            Odometer {appVersion ? `v${appVersion}` : ''} is the latest version.
          </span>
        {:else if updaterStore.lastManualResult === 'failed'}
          <span class="text-xs text-red-500" role="alert">Check failed: {updaterStore.error}</span>
        {/if}
      {/if}
      {#if releaseNotesError}
        <span class="text-xs text-red-500" role="alert">Could not open release notes: {releaseNotesError}</span>
      {/if}
    </div>
  </section>

  <!-- Appearance -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Appearance</h2>
    <div class="flex items-center gap-3">
      <span class="text-xs text-ink-faint">Theme</span>
      <div class="flex bg-app rounded-lg p-[2px] gap-[2px] border border-edge">
        {#each themeOptions as opt (opt.value)}
          <button
            onclick={() => themeStore.set(opt.value)}
            class="px-3.5 py-1 rounded-md text-xs transition-colors {themeStore.preference === opt.value
              ? 'bg-ink text-app font-semibold'
              : 'text-ink-muted hover:text-ink'}"
            aria-pressed={themeStore.preference === opt.value}
          >
            {opt.label}
          </button>
        {/each}
      </div>
      {#if themeStore.preference === 'system'}
        <span class="text-xs text-ink-faint">following OS ({themeStore.resolved})</span>
      {/if}
    </div>
  </section>

  <!-- Editable watched roots -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-3">Watched roots</h2>

    <!-- Action bar -->
    <div class="flex items-center gap-3 mb-3 flex-wrap">
      <button
        onclick={saveRoots}
        disabled={!rootsDirty || rootsSaving}
        class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed text-white transition-colors"
      >
        {rootsSaving ? 'Saving…' : 'Save changes'}
      </button>
      <button
        onclick={resetRoots}
        disabled={!rootsDirty || rootsSaving}
        class="px-3 py-1.5 text-xs font-medium rounded-sm bg-app hover:bg-(--row-hover) disabled:opacity-40 disabled:cursor-not-allowed text-ink transition-colors"
      >
        Reset
      </button>
      {#if rootsSavedAt && !rootsDirty}
        <span class="text-xs text-pos">Saved at {rootsSavedAt} · scanning…</span>
      {/if}
      {#if rootsSaveError}
        <span class="text-xs text-red-500">{rootsSaveError}</span>
      {/if}
    </div>

    <!-- Session roots -->
    <h3 class="text-xs text-ink-faint uppercase tracking-wider mb-1">Session roots</h3>
    <ul class="bg-card border border-edge rounded-lg divide-y divide-edge overflow-hidden mb-2">
      {#if editedSessionRoots.length === 0}
        <li class="px-4 py-2 text-xs text-ink-faint italic">None configured</li>
      {:else}
        {#each editedSessionRoots as root, i}
          <li class="flex items-center justify-between gap-2 px-4 py-2">
            <span class="font-mono text-xs text-ink-2 break-all">{root}</span>
            <button
              onclick={() => removeSessionRoot(i)}
              class="shrink-0 text-ink-faint hover:text-red-500 transition-colors text-xs px-1.5 py-0.5 rounded-sm hover:bg-(--row-hover)"
              aria-label="Remove {root}"
              title="Remove"
            >Remove</button>
          </li>
        {/each}
      {/if}
      <li class="flex items-center gap-2 px-4 py-2">
        <input
          type="text"
          placeholder="/absolute/path/to/sessions"
          bind:value={newSessionRoot}
          onkeydown={(e) => { if (e.key === 'Enter') addSessionRoot(); }}
          class="flex-1 bg-app border border-edge rounded-sm px-2 py-0.5 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono"
        />
        <button
          onclick={addSessionRoot}
          class="shrink-0 text-xs px-2 py-0.5 rounded-sm bg-accent-tab hover:opacity-90 text-white transition-colors"
        >Add</button>
      </li>
    </ul>

    <!-- Archive roots -->
    <h3 class="text-xs text-ink-faint uppercase tracking-wider mb-1">Archive roots</h3>
    <ul class="bg-card border border-edge rounded-lg divide-y divide-edge overflow-hidden mb-2">
      {#if editedArchiveRoots.length === 0}
        <li class="px-4 py-2 text-xs text-ink-faint italic">None configured</li>
      {:else}
        {#each editedArchiveRoots as root, i}
          <li class="flex items-center justify-between gap-2 px-4 py-2">
            <span class="font-mono text-xs text-ink-2 break-all">{root}</span>
            <button
              onclick={() => removeArchiveRoot(i)}
              class="shrink-0 text-ink-faint hover:text-red-500 transition-colors text-xs px-1.5 py-0.5 rounded-sm hover:bg-(--row-hover)"
              aria-label="Remove {root}"
              title="Remove"
            >Remove</button>
          </li>
        {/each}
      {/if}
      <li class="flex items-center gap-2 px-4 py-2">
        <input
          type="text"
          placeholder="/absolute/path/to/archives"
          bind:value={newArchiveRoot}
          onkeydown={(e) => { if (e.key === 'Enter') addArchiveRoot(); }}
          class="flex-1 bg-app border border-edge rounded-sm px-2 py-0.5 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono"
        />
        <button
          onclick={addArchiveRoot}
          class="shrink-0 text-xs px-2 py-0.5 rounded-sm bg-accent-tab hover:opacity-90 text-white transition-colors"
        >Add</button>
      </li>
    </ul>

    <!-- Claude Code session roots -->
    <h3 class="text-xs text-ink-faint uppercase tracking-wider mb-1">Claude Code session roots</h3>
    <p class="text-xs text-ink-faint mb-1">
      Directories containing Claude Code session files. Claude Code writes them under
      <span class="font-mono">~/.claude/projects</span> by default.
    </p>
    <ul class="bg-card border border-edge rounded-lg divide-y divide-edge overflow-hidden mb-2">
      {#if editedClaudeRoots.length === 0}
        <li class="px-4 py-2 text-xs text-ink-faint italic">None configured</li>
      {:else}
        {#each editedClaudeRoots as root, i}
          <li class="flex items-center justify-between gap-2 px-4 py-2">
            <span class="font-mono text-xs text-ink-2 break-all">{root}</span>
            <button
              onclick={() => removeClaudeRoot(i)}
              class="shrink-0 text-ink-faint hover:text-red-500 transition-colors text-xs px-1.5 py-0.5 rounded-sm hover:bg-(--row-hover)"
              aria-label="Remove {root}"
              title="Remove"
            >Remove</button>
          </li>
        {/each}
      {/if}
      <li class="flex items-center gap-2 px-4 py-2">
        <input
          type="text"
          placeholder="/absolute/path/to/.claude/projects"
          bind:value={newClaudeRoot}
          onkeydown={(e) => { if (e.key === 'Enter') addClaudeRoot(); }}
          class="flex-1 bg-app border border-edge rounded-sm px-2 py-0.5 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono"
        />
        <button
          onclick={addClaudeRoot}
          class="shrink-0 text-xs px-2 py-0.5 rounded-sm bg-accent-tab hover:opacity-90 text-white transition-colors"
        >Add</button>
      </li>
    </ul>

    <!-- Session index file -->
    <h3 class="text-xs text-ink-faint uppercase tracking-wider mb-1">Session index file</h3>
    <p class="text-xs text-ink-faint mb-1">
      JSONL file mapping session ids to thread names. Codex writes this at
      <span class="font-mono">~/.codex/session_index.jsonl</span> by default.
    </p>
    <input
      type="text"
      placeholder="/absolute/path/to/session_index.jsonl"
      bind:value={editedIndexPath}
      oninput={markRootsDirty}
      class="w-full bg-card border border-edge rounded-sm px-3 py-2 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono mb-2"
    />

    {#if hasDuplicateRoots}
      <p class="text-xs text-amber-500">Warning: duplicate paths detected in the lists above.</p>
    {/if}
  </section>

  <!-- Opt-in instruction inventory -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Instruction inventory</h2>
    <p class="text-xs text-ink-faint mb-3 max-w-3xl">
      Finds supported agent instruction files locally and provides a read-only hierarchy, effective
      chain, warnings, Markdown preview, and change-impact history. Global harness folders and
      projects already observed in session history are included automatically. Add container roots
      below for projects that have not appeared in Odometer yet.
    </p>
    <div class="bg-card border border-edge rounded-lg px-4 py-3 max-w-4xl">
      <div class="flex items-center gap-5 flex-wrap">
        <label class="flex items-center gap-2 text-xs text-ink-2">
          <input
            type="checkbox"
            bind:checked={instructionsEnabled}
            onchange={markInstructionsDirty}
          />
          Enable instruction inventory
        </label>
        <label class="flex items-center gap-2 text-xs text-ink-muted">
          <input
            type="checkbox"
            bind:checked={instructionsTabVisible}
            onchange={markInstructionsDirty}
          />
          Show Instructions tab
        </label>
        <button
          onclick={saveInstructionSettings}
          disabled={!instructionsDirty || instructionsSaving}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 text-white transition-colors"
        >{instructionsSaving ? 'Saving…' : 'Save'}</button>
        <button
          onclick={onopeninstructions}
          disabled={!$config.instructions_enabled || !$config.instructions_tab_visible || instructionsDirty}
          class="px-3 py-1.5 text-xs rounded-sm border border-edge bg-app hover:bg-(--row-hover) disabled:opacity-40"
        >Open Instructions</button>
        {#if instructionsSavedAt && !instructionsDirty}
          <span class="text-xs text-pos">Saved at {instructionsSavedAt}</span>
        {/if}
      </div>
      <p class="text-[11px] text-ink-faint mt-2">
        Turning this off hides the tab, stops configured-root instruction watching, and denies file
        preview/open requests. Hiding only the tab leaves discovery and change tracking enabled.
      </p>

      <div class="mt-4 pt-3 border-t border-edgerow">
        <div class="flex items-baseline justify-between gap-3 mb-2">
          <div>
            <h3 class="text-xs font-semibold text-ink">Additional discovery roots</h3>
            <p class="text-[11px] text-ink-faint">Choose folder-only or recursive discovery per path. Generated and dependency folders are skipped.</p>
          </div>
          <span class="text-[10px] text-ink-faint">AGENTS.md · CLAUDE.md</span>
        </div>
        <ul class="border border-edge rounded-lg divide-y divide-edge overflow-hidden">
          {#if editedInstructionRoots.length === 0}
            <li class="px-3 py-2 text-xs text-ink-faint italic">No additional roots configured</li>
          {:else}
            {#each editedInstructionRoots as root, index}
              <li class="flex items-center gap-3 px-3 py-2">
                <span class="flex-1 min-w-0 font-mono text-xs text-ink-2 break-all">{root.path}</span>
                <label class="flex items-center gap-1.5 text-[11px] text-ink-muted shrink-0">
                  <input
                    type="checkbox"
                    checked={root.recursive}
                    onchange={(event) => setInstructionRootRecursive(index, event.currentTarget.checked)}
                  />
                  Include subfolders
                </label>
                <button
                  onclick={() => removeInstructionRoot(index)}
                  class="px-1.5 py-0.5 text-xs text-ink-faint hover:text-red-500"
                  aria-label="Remove {root.path}"
                >Remove</button>
              </li>
            {/each}
          {/if}
          <li class="flex items-center gap-2 px-3 py-2 flex-wrap">
            <input
              type="text"
              bind:value={newInstructionRoot}
              onkeydown={(event) => { if (event.key === 'Enter') addInstructionRoot(); }}
              placeholder="D:\projects"
              class="flex-1 min-w-[260px] bg-app border border-edge rounded-sm px-2 py-1 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono"
            />
            <label class="flex items-center gap-1.5 text-[11px] text-ink-muted">
              <input type="checkbox" bind:checked={newInstructionRootRecursive} /> Include subfolders
            </label>
            <button
              onclick={addInstructionRoot}
              class="px-2.5 py-1 text-xs rounded-sm bg-accent-tab text-white hover:opacity-90"
            >Add</button>
          </li>
        </ul>
        {#if hasDuplicateInstructionRoots}
          <p class="text-xs text-amber-500 mt-2">Duplicate paths must be removed before saving.</p>
        {/if}
        {#if instructionsError}
          <p class="text-xs text-red-500 mt-2" role="alert">{instructionsError}</p>
        {/if}
      </div>
    </div>
  </section>

  <!-- Opt-in harness turn receipts -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Turn receipts</h2>
    <p class="text-xs text-ink-faint mb-3 max-w-3xl">
      Show a compact token, estimated cost, and available subscription-usage receipt after each
      completed agent turn. Odometer adds an identifiable <span class="font-mono">Stop</span> hook
      only for the harnesses you select. This is off by default; while off, Odometer does not run a
      receipt helper, read extra transcript data, poll accounts, or change harness behavior.
    </p>
    <div class="bg-card border border-edge rounded-lg px-4 py-3 max-w-3xl space-y-3">
      <label class="flex items-start gap-2 text-xs text-ink-2">
        <input
          type="checkbox"
          bind:checked={turnReceiptsEnabled}
          onchange={markTurnReceiptsDirty}
          class="mt-0.5"
        />
        <span>
          <span class="font-medium text-ink">Enable turn receipts</span>
          <span class="block text-[11px] text-ink-faint mt-0.5">
            Hook failures are fail-open: they never keep an agent turn running or block completion.
          </span>
        </span>
      </label>

      <div class="flex items-center gap-5 pl-5 flex-wrap">
        <label class="flex items-center gap-2 text-xs text-ink-2">
          <input
            type="checkbox"
            bind:checked={turnReceiptsCodex}
            onchange={markTurnReceiptsDirty}
            disabled={!turnReceiptsEnabled}
          />
          Codex
        </label>
        <label class="flex items-center gap-2 text-xs text-ink-2">
          <input
            type="checkbox"
            bind:checked={turnReceiptsClaude}
            onchange={markTurnReceiptsDirty}
            disabled={!turnReceiptsEnabled}
          />
          Claude Code
        </label>
      </div>

      <div class="flex items-center gap-3 flex-wrap">
        <button
          onclick={saveTurnReceipts}
          disabled={!turnReceiptsDirty || turnReceiptsSaving}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 text-white transition-colors"
        >{turnReceiptsSaving ? 'Saving…' : 'Save setup'}</button>
        <button
          onclick={repairTurnReceipts}
          disabled={turnReceiptsSaving || turnReceiptsDirty}
          class="px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover) disabled:opacity-40"
        >Repair setup</button>
        <button
          onclick={refreshTurnReceiptStatus}
          disabled={turnReceiptsSaving}
          class="px-2 py-1 text-xs text-accent-chipfg hover:underline disabled:opacity-40"
        >Refresh status</button>
        {#if turnReceiptsSavedAt && !turnReceiptsDirty}
          <span class="text-xs text-pos">Saved at {turnReceiptsSavedAt}</span>
        {/if}
      </div>

      {#if turnReceiptsError}
        <p class="text-xs text-red-500">{turnReceiptsError}</p>
      {/if}

      {#if turnReceiptStatus}
        <div class="grid grid-cols-1 md:grid-cols-2 gap-2 pt-1">
          {#each [
            { label: 'Codex', value: turnReceiptStatus.codex },
            { label: 'Claude Code', value: turnReceiptStatus.claude_code },
          ] as item}
            <div class="bg-app border border-edgerow rounded-md px-3 py-2 min-w-0">
              <div class="flex items-center justify-between gap-2">
                <span class="text-xs font-medium text-ink">{item.label}</span>
                <span class:text-pos={item.value.installed && item.value.requested}
                  class:text-ink-faint={!item.value.installed || !item.value.requested}
                  class="text-[11px]">
                  {item.value.installed ? 'Hook installed' : 'No hook installed'}
                </span>
              </div>
              <p class="text-[11px] text-ink-faint mt-1">{item.value.detail}</p>
              <p class="text-[11px] text-ink-faint mt-1 font-mono break-all">{item.value.config_path}</p>
              <p class="text-[11px] mt-2"
                class:text-pos={item.value.last_run_success === true}
                class:text-red-500={item.value.last_run_success === false}
                class:text-ink-faint={item.value.last_run_success === null}>
                {formatHookRun(item.value.last_run_at)}
              </p>
              {#if item.value.last_run_detail}
                <p class="text-[11px] text-red-500 mt-1">{item.value.last_run_detail}</p>
              {/if}
              {#if item.value.last_receipt}
                <pre class="text-[11px] text-ink-2 whitespace-pre-wrap mt-1 font-mono">{item.value.last_receipt}</pre>
              {/if}
            </div>
          {/each}
        </div>
        {#if turnReceiptStatus.enabled && turnReceiptStatus.codex.requested}
          <p class="text-[11px] text-ink-faint">
            Codex requires explicit trust for new or changed command hooks. Open
            <span class="font-mono">/hooks</span> in Codex after setup. Start a new task if the
            current harness session does not reload configuration automatically.
          </p>
        {/if}
      {:else}
         <p class="text-[11px] text-ink-faint">Status is available in the installed desktop app.</p>
       {/if}
    </div>
  </section>

  <!-- Opt-in application performance tracking -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Performance tracking</h2>
    <p class="text-xs text-ink-faint mb-3 max-w-3xl">
      Records local timings and aggregate counts for startup, scans, cache behavior, parsing,
      watchers, analytics IPC, exports, and UI loading/rendering. It never stores prompts, tool
      arguments, session IDs, repository paths, or command output. Recording is off by default.
    </p>
    <div class="bg-card border border-edge rounded-lg px-4 py-3 max-w-3xl">
      <div class="flex items-center gap-4 flex-wrap">
        <label class="flex items-center gap-2 text-xs text-ink-2">
          <input
            type="checkbox"
            bind:checked={performanceEnabled}
            onchange={markPerformanceDirty}
          />
          Enable local performance tracking
        </label>
        <label class="flex items-center gap-2 text-xs text-ink-muted">
          Segment size
          <input
            type="number"
            min="1"
            max="1024"
            step="1"
            bind:value={performanceMaxMb}
            oninput={markPerformanceDirty}
            class="w-20 text-right bg-app border border-edge rounded-sm px-2 py-1 text-ink font-mono"
          />
          MiB
        </label>
        <button
          onclick={savePerformanceSettings}
          disabled={!performanceDirty || performanceSaving}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 text-white transition-colors"
        >{performanceSaving ? 'Saving…' : 'Save'}</button>
        {#if performanceSavedAt && !performanceDirty}
          <span class="text-xs text-pos">Saved at {performanceSavedAt}</span>
        {/if}
      </div>
      <p class="text-[11px] text-ink-faint mt-2">
        The recorder keeps a current and previous segment, so storage is bounded to approximately
        twice the configured segment size.
      </p>
      <div class="flex items-center gap-2 flex-wrap mt-3 pt-3 border-t border-edgerow">
        <button
          onclick={() => exportPerformance('jsonl')}
          disabled={performanceExporting || (performanceStatus?.stored_bytes ?? 0) === 0}
          class="px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover) disabled:opacity-40"
        >Export JSONL…</button>
        <button
          onclick={() => exportPerformance('csv')}
          disabled={performanceExporting || (performanceStatus?.stored_bytes ?? 0) === 0}
          class="px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover) disabled:opacity-40"
        >Export CSV…</button>
        <button
          onclick={refreshPerformanceStatus}
          disabled={performanceExporting}
          class="px-2 py-1 text-xs text-accent-chipfg hover:underline"
        >Refresh status</button>
        {#if performanceStatus}
          <span class="text-[11px] text-ink-faint ml-auto">
            {formatBytes(performanceStatus.stored_bytes)} stored ·
            {performanceStatus.recorded_this_run.toLocaleString()} recorded this run
            {#if performanceStatus.dropped_this_run > 0}
              · {performanceStatus.dropped_this_run.toLocaleString()} dropped
            {/if}
          </span>
        {/if}
      </div>
      {#if performanceError}
        <p class="text-xs text-red-500 mt-2">{performanceError}</p>
      {/if}
    </div>
  </section>

  {#if isWindows}
    <!-- Windows scan performance -->
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Windows scan performance</h2>
      <p class="text-xs text-ink-faint mb-2 max-w-2xl">
        Windows Defender scans every session file as it's read, which usually dominates the first
        load of a large history. You can exclude the watched session folders above from Defender's
        real-time scanning. Trade-off: files in those folders are no longer scanned for threats —
        they normally contain only text session logs written by Codex and Claude Code. Windows will
        ask for administrator approval.
      </p>
      <div class="flex items-center gap-3 flex-wrap">
        <button
          onclick={handleDefenderExclusions}
          disabled={defenderState === 'pending'}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-card border border-edge hover:bg-(--row-hover) text-ink transition-colors disabled:opacity-50"
        >
          Exclude session folders from Defender…
        </button>
        {#if defenderState === 'pending'}
          <span class="text-xs text-ink-muted">Waiting for approval in the Windows security prompt…</span>
        {:else if defenderState === 'done'}
          <span class="text-xs text-pos">Done — the session folders are excluded from real-time scanning.</span>
        {/if}
        {#if defenderError}
          <span class="text-xs text-red-500">{defenderError}</span>
        {/if}
      </div>
    </section>
  {/if}

  <!-- Rate card editor -->
  {#if $rates}
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Rate card</h2>

      <!-- Metadata row -->
      <div class="flex flex-wrap items-center gap-x-4 gap-y-1 mb-3 text-xs text-ink-faint">
        <span>v{ratesVersion} · {ratesCurrency} · {ratesUnit}</span>
        {#if fetchedAt}
          <span>fetched {fetchedAt}</span>
        {/if}
        {#if sourceUrl}
          <a
            href={sourceUrl}
            target="_blank"
            rel="noopener noreferrer"
            class="text-accent-chipfg hover:opacity-80 underline underline-offset-2 transition-colors"
          >{sourceUrl}</a>
        {/if}
      </div>

      <!-- Action bar -->
      <div class="flex items-center gap-3 mb-4 flex-wrap">
        <button
          onclick={handleSave}
          disabled={!dirty || saving}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed text-white transition-colors"
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
        <button
          onclick={handleReset}
          disabled={saving}
          class="px-3 py-1.5 text-xs font-medium rounded-sm bg-app hover:bg-(--row-hover) disabled:opacity-40 disabled:cursor-not-allowed text-ink transition-colors"
        >
          Reset to shipped defaults
        </button>
        {#if savedAt && !dirty}
          <span class="text-xs text-pos">Saved at {savedAt}</span>
        {/if}
        {#if saveError}
          <span class="text-xs text-red-500">{saveError}</span>
        {/if}
        {#if validationError}
          <span class="text-xs text-amber-500">{validationError}</span>
        {/if}
      </div>

      <!-- Fallback model selector -->
      <div class="flex items-center gap-3 mb-4">
        <label for="fallback-model" class="text-xs text-ink-muted whitespace-nowrap">Fallback model</label>
        <select
          id="fallback-model"
          bind:value={fallbackModel}
          onchange={markDirty}
          class="bg-card border border-edge rounded-sm px-2 py-1 text-xs text-ink focus:outline-hidden focus:ring-1 focus:ring-(--accent)"
        >
          <option value="">— select —</option>
          {#each rows as row}
            <option value={row.name}>{row.name}</option>
          {/each}
        </select>
      </div>

      <!-- Rate table -->
      <div class="overflow-x-auto">
        <table class="w-full text-xs border-collapse bg-card rounded-lg overflow-hidden">
          <thead>
            <tr class="border-b border-edge">
              <th class="text-left px-3 py-2 text-ink-muted font-medium">Model</th>
              <th class="text-right px-3 py-2 text-ink-muted font-medium">Input $/1M</th>
              <th class="text-right px-3 py-2 text-ink-muted font-medium">Cached $/1M</th>
              <th class="text-right px-3 py-2 text-ink-muted font-medium">Output $/1M</th>
              <th class="text-right px-3 py-2 text-ink-muted font-medium">Reasoning $/1M</th>
              <th class="px-3 py-2"></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row, i (row.name + i)}
              <tr class="border-b border-edgerow">
                <td class="px-3 py-1.5 font-mono text-ink-2">{row.name}</td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.input}
                    oninput={markDirty}
                    class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.cached_input}
                    oninput={markDirty}
                    class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.output}
                    oninput={markDirty}
                    class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.reasoning}
                    oninput={markDirty}
                    class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5 text-center">
                  <button
                    onclick={() => removeRow(i)}
                    class="text-ink-faint hover:text-red-500 transition-colors"
                    title="Remove model"
                    aria-label="Remove {row.name}"
                  >
                    <!-- Trash icon -->
                    <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2"
                        d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"/>
                    </svg>
                  </button>
                </td>
              </tr>
            {/each}

            <!-- Add model inline row -->
            <tr class="border-t border-edge bg-card">
              <td class="px-3 py-1.5">
                <input
                  type="text"
                  placeholder="model-name"
                  bind:value={newName}
                  class="w-full bg-app border border-edge rounded-sm px-2 py-0.5 text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) font-mono text-xs"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newInput}
                  class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newCachedInput}
                  class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newOutput}
                  class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newReasoning}
                  class="w-24 text-right bg-app border border-edge rounded-sm px-2 py-0.5 text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent) tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5 text-center">
                <button
                  onclick={addModel}
                  class="text-xs px-2 py-0.5 rounded-sm bg-accent-tab hover:opacity-90 text-white transition-colors"
                >
                  Add
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Managed, read-only catalog. Rule provenance and stable IDs are
           deliberately not editable in the base rate editor. -->
      <div class="mt-5 rounded-lg border border-edge bg-card px-4 py-3">
        <h3 class="text-xs font-semibold uppercase tracking-wider text-ink-muted">Effective-dated pricing catalog</h3>
        <p class="mt-1 text-[11px] text-ink-faint max-w-3xl">
          Read-only managed rules refresh by stable ID when a newer Odometer bundled card ships. “Reset to shipped defaults” reloads the currently shipped card; saving the base-rate editor preserves this catalog unchanged.
        </p>
        {#if pricingCatalog.rate_periods.length === 0 && pricingCatalog.conditional_modifiers.length === 0}
          <p class="mt-2 text-xs text-ink-faint">No effective-dated rules are available in this rate card.</p>
        {:else}
          <div class="mt-3 space-y-2">
            {#each pricingCatalog.rate_periods as period (period.id)}
              <div class="border-t border-edgerow pt-2 text-[11px] text-ink-2">
                <div class="font-medium text-ink">Base rate · {period.model} · {fmtPricingSurface(period.surface)}</div>
                <div class="mt-0.5 font-mono text-ink-faint break-all">{period.id}</div>
                <div class="mt-0.5 text-ink-faint">Applies {fmtPricingWindow(period.from, period.to)} · verified {period.provenance.verified_at.slice(0, 10)} UTC</div>
                <div class="mt-0.5">
                  <a class="text-accent hover:underline" href={period.provenance.source_url} target="_blank" rel="noreferrer">{period.label} · source</a>
                  {#if period.cache_write_input_multiplier !== null && period.cache_write_input_multiplier !== undefined}
                    <span class="text-ink-faint"> · {period.cache_write_input_multiplier}× cache-write input (unobserved)</span>
                  {/if}
                </div>
              </div>
            {/each}
            {#each pricingCatalog.conditional_modifiers as modifier (modifier.id)}
              <div class="border-t border-edgerow pt-2 text-[11px] text-ink-2">
                <div class="font-medium text-ink">Conditional rule · {modifier.model} · {fmtPricingSurface(modifier.surface)}</div>
                <div class="mt-0.5 font-mono text-ink-faint break-all">{modifier.id}</div>
                <div class="mt-0.5 text-ink-faint">Applies {fmtPricingWindow(modifier.from, modifier.to)} · verified {modifier.provenance.verified_at.slice(0, 10)} UTC</div>
                <div class="mt-0.5 text-ink-faint">When request input exceeds {modifier.condition.greater_than.toLocaleString()} tokens · input {modifier.multipliers.input}× · output {modifier.multipliers.output}×</div>
                <div class="mt-0.5"><a class="text-accent hover:underline" href={modifier.provenance.source_url} target="_blank" rel="noreferrer">{modifier.label} · source</a></div>
              </div>
            {/each}
          </div>
          {#if pricingCatalog.notes.length > 0}
            <div class="mt-3 space-y-1 text-[11px] text-ink-faint">
              {#each pricingCatalog.notes as note}
                <p>{note}</p>
              {/each}
            </div>
          {/if}
        {/if}
      </div>
    </section>
  {:else}
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Rate card</h2>
      <p class="text-xs text-ink-faint italic">Loading…</p>
    </section>
  {/if}

</div>
