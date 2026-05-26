<script lang="ts">
  import { config } from '../lib/stores/config';
  import { rates } from '../lib/stores/rates';
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

  <!-- Rate card -->
  {#if $rates}
    <section>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Rate card</h2>
      <p class="text-xs text-slate-500 mb-3">
        v{$rates.version} · {$rates.currency} · {$rates.unit}
        {#if $rates.fetched_at} · fetched {$rates.fetched_at}{/if}
      </p>
      <div class="overflow-x-auto">
        <table class="w-full text-xs border-collapse bg-slate-800 rounded-lg overflow-hidden">
          <thead>
            <tr class="border-b border-slate-700">
              <th class="text-left px-4 py-2.5 text-slate-400 font-medium">Model</th>
              <th class="text-right px-4 py-2.5 text-slate-400 font-medium">Input</th>
              <th class="text-right px-4 py-2.5 text-slate-400 font-medium">Cached input</th>
              <th class="text-right px-4 py-2.5 text-slate-400 font-medium">Output</th>
              <th class="text-right px-4 py-2.5 text-slate-400 font-medium">Reasoning</th>
            </tr>
          </thead>
          <tbody>
            {#each Object.entries($rates.models) as [model, rate]}
              <tr class="border-b border-slate-700/50 hover:bg-slate-700/30 transition-colors">
                <td class="px-4 py-2 font-mono text-slate-300">{model}</td>
                <td class="px-4 py-2 text-right tabular-nums text-slate-300">{rate.input}</td>
                <td class="px-4 py-2 text-right tabular-nums text-slate-400">{rate.cached_input}</td>
                <td class="px-4 py-2 text-right tabular-nums text-slate-300">{rate.output}</td>
                <td class="px-4 py-2 text-right tabular-nums text-slate-400">{rate.reasoning}</td>
              </tr>
            {/each}
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
