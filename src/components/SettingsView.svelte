<script lang="ts">
  import { config } from '../lib/stores/config';
  import { rates } from '../lib/stores/rates';
  import { setRates, getBundledRates } from '../lib/ipc';
  import type { RateCard, ModelRate } from '../lib/types';

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
      currency: 'USD',
      unit: 'per_1m_tokens',
      source_url: sourceUrl,
      fetched_at: fetchedAt,
      models,
      fallback_model: fallbackModel,
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
    rows = rows.filter((_, i) => i !== index);
    // If the deleted model was the fallback, clear fallback selection.
    if (fallbackModel === rows[index]?.name) fallbackModel = '';
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

  <!-- Watched session roots -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Watched session roots</h2>
    <ul class="bg-slate-800 rounded-lg divide-y divide-slate-700 overflow-hidden">
      {#if $config.session_roots.length === 0}
        <li class="px-4 py-2 text-xs text-slate-500 italic">None configured</li>
      {:else}
        {#each $config.session_roots as root}
          <li class="px-4 py-2 font-mono text-xs text-slate-300 break-all">{root}</li>
        {/each}
      {/if}
    </ul>
  </section>

  <!-- Watched archive roots -->
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Watched archive roots</h2>
    <ul class="bg-slate-800 rounded-lg divide-y divide-slate-700 overflow-hidden">
      {#if $config.archive_roots.length === 0}
        <li class="px-4 py-2 text-xs text-slate-500 italic">None configured</li>
      {:else}
        {#each $config.archive_roots as root}
          <li class="px-4 py-2 font-mono text-xs text-slate-300 break-all">{root}</li>
        {/each}
      {/if}
    </ul>
  </section>

  <!-- Rate card editor -->
  {#if $rates}
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Rate card</h2>

      <!-- Metadata row -->
      <div class="flex flex-wrap items-center gap-x-4 gap-y-1 mb-3 text-xs text-slate-500">
        <span>v{ratesVersion} · USD · per_1m_tokens</span>
        {#if fetchedAt}
          <span>fetched {fetchedAt}</span>
        {/if}
        {#if sourceUrl}
          <a
            href={sourceUrl}
            target="_blank"
            rel="noopener noreferrer"
            class="text-blue-400 hover:text-blue-300 underline underline-offset-2 transition-colors"
          >{sourceUrl}</a>
        {/if}
      </div>

      <!-- Action bar -->
      <div class="flex items-center gap-3 mb-4 flex-wrap">
        <button
          onclick={handleSave}
          disabled={!dirty || saving}
          class="px-3 py-1.5 text-xs font-medium rounded bg-blue-600 hover:bg-blue-500 disabled:opacity-40 disabled:cursor-not-allowed text-white transition-colors"
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
        <button
          onclick={handleReset}
          disabled={saving}
          class="px-3 py-1.5 text-xs font-medium rounded bg-slate-700 hover:bg-slate-600 disabled:opacity-40 disabled:cursor-not-allowed text-slate-200 transition-colors"
        >
          Reset to shipped defaults
        </button>
        {#if savedAt && !dirty}
          <span class="text-xs text-emerald-400">Saved at {savedAt}</span>
        {/if}
        {#if saveError}
          <span class="text-xs text-red-400">{saveError}</span>
        {/if}
        {#if validationError}
          <span class="text-xs text-amber-400">{validationError}</span>
        {/if}
      </div>

      <!-- Fallback model selector -->
      <div class="flex items-center gap-3 mb-4">
        <label for="fallback-model" class="text-xs text-slate-400 whitespace-nowrap">Fallback model</label>
        <select
          id="fallback-model"
          bind:value={fallbackModel}
          onchange={markDirty}
          class="bg-slate-800 border border-slate-600 rounded px-2 py-1 text-xs text-slate-200 focus:outline-none focus:ring-1 focus:ring-blue-500"
        >
          <option value="">— select —</option>
          {#each rows as row}
            <option value={row.name}>{row.name}</option>
          {/each}
        </select>
      </div>

      <!-- Rate table -->
      <div class="overflow-x-auto">
        <table class="w-full text-xs border-collapse bg-slate-800 rounded-lg overflow-hidden">
          <thead>
            <tr class="border-b border-slate-700">
              <th class="text-left px-3 py-2 text-slate-400 font-medium">Model</th>
              <th class="text-right px-3 py-2 text-slate-400 font-medium">Input $/1M</th>
              <th class="text-right px-3 py-2 text-slate-400 font-medium">Cached $/1M</th>
              <th class="text-right px-3 py-2 text-slate-400 font-medium">Output $/1M</th>
              <th class="text-right px-3 py-2 text-slate-400 font-medium">Reasoning $/1M</th>
              <th class="px-3 py-2"></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row, i (row.name + i)}
              <tr class="border-b border-slate-700/50">
                <td class="px-3 py-1.5 font-mono text-slate-300">{row.name}</td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.input}
                    oninput={markDirty}
                    class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.cached_input}
                    oninput={markDirty}
                    class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.output}
                    oninput={markDirty}
                    class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5">
                  <input
                    type="number"
                    min="0"
                    step="0.001"
                    bind:value={row.reasoning}
                    oninput={markDirty}
                    class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                  />
                </td>
                <td class="px-3 py-1.5 text-center">
                  <button
                    onclick={() => removeRow(i)}
                    class="text-slate-500 hover:text-red-400 transition-colors"
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
            <tr class="border-t border-slate-600 bg-slate-800/80">
              <td class="px-3 py-1.5">
                <input
                  type="text"
                  placeholder="model-name"
                  bind:value={newName}
                  class="w-full bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1 focus:ring-blue-500 font-mono text-xs"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newInput}
                  class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newCachedInput}
                  class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newOutput}
                  class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.001"
                  placeholder="0"
                  bind:value={newReasoning}
                  class="w-24 text-right bg-slate-700 border border-slate-600 rounded px-2 py-0.5 text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1 focus:ring-blue-500 tabular-nums"
                />
              </td>
              <td class="px-3 py-1.5 text-center">
                <button
                  onclick={addModel}
                  class="text-xs px-2 py-0.5 rounded bg-blue-600 hover:bg-blue-500 text-white transition-colors"
                >
                  Add
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  {:else}
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Rate card</h2>
      <p class="text-xs text-slate-500 italic">Loading…</p>
    </section>
  {/if}

</div>
