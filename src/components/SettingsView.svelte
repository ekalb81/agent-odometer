<script lang="ts">
  import { config } from '../lib/stores/config';
  import { rates } from '../lib/stores/rates';
</script>

<div class="flex flex-col gap-6 p-6 overflow-auto h-full">
  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Config</h2>
    <pre class="bg-slate-800 rounded-lg p-4 text-xs text-slate-300 overflow-auto">{JSON.stringify($config, null, 2)}</pre>
  </section>

  <section>
    <h2 class="text-sm font-semibold uppercase tracking-wider text-slate-400 mb-2">Rate Card</h2>
    <pre class="bg-slate-800 rounded-lg p-4 text-xs text-slate-300 overflow-auto">{JSON.stringify($rates, null, 2)}</pre>
  </section>
</div>
