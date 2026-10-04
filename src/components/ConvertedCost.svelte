<script lang="ts">
  import type { ConvertedTotal } from '../lib/types';
  import { formatCredits } from '../lib/currency';
  let { value, incomplete = false }: { value?: ConvertedTotal | null; incomplete?: boolean } = $props();
</script>
{#if value}
  <p class="text-xs text-ink-faint">Display restatement: <span class="font-mono">{formatCredits(value.amount, value.target_currency)}</span> from {value.from_currency}; rate {value.rate} · {value.as_of} · {value.source}. User-supplied, not a live quote.{incomplete ? ' Unpriced usage remains excluded.' : ''}</p>
{/if}
