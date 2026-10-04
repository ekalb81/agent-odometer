<script lang="ts">
  import type { CurrencyConversion } from '../lib/types';
  import { FX_CURRENCIES } from '../lib/currency';
  let { conversion = $bindable<CurrencyConversion | null>(null), originalCurrency, onchange }: { conversion: CurrencyConversion | null; originalCurrency: string; onchange: () => void } = $props();
  const currencies = FX_CURRENCIES;
  let enabled = $state(false);
  let from = $state('USD');
  let target = $state('EUR');
  let rate = $state('');
  let asOf = $state('');
  let source = $state('');
  let lastEmitted: CurrencyConversion | null | undefined;
  $effect(() => {
    if (conversion === lastEmitted) return;
    enabled = conversion !== null;
    from = conversion?.from_currency ?? (currencies.includes(originalCurrency) ? originalCurrency : '');
    target = conversion?.target_currency ?? 'EUR';
    rate = conversion ? String(conversion.rate) : '';
    asOf = conversion?.as_of.replace(/Z$/, '') ?? '';
    source = conversion?.source ?? '';
  });
  function changed() {
    const parsed = new Date(`${asOf}Z`);
    const timestamp = Number.isNaN(parsed.getTime()) ? '' : parsed.toISOString();
    conversion = enabled ? { from_currency: from || null, target_currency: target, rate: Number(rate), as_of: timestamp, source } : null;
    lastEmitted = conversion;
    onchange();
  }
</script>

<fieldset class="border border-edge rounded-lg p-3 mb-4 space-y-3 text-xs">
  <legend class="font-semibold text-ink-2 px-1">Offline display currency</legend>
  <label class="flex items-center gap-2"><input type="checkbox" checked={enabled} onchange={(event) => { enabled = event.currentTarget.checked; changed(); }} />Use a user-supplied FX rate</label>
  <p class="text-ink-faint">No live FX source is used. Original money remains visible; plan credits and unlike original currencies are never converted or combined. Save the rate card to apply these settings.</p>
  {#if enabled}
    <div class="flex flex-wrap gap-3">
      <label>Original money currency<select class="block bg-app border border-edge rounded-sm p-1.5 mt-1" value={from} onchange={(event) => { from = event.currentTarget.value; changed(); }}><option value="">Select currency</option>{#each currencies as currency}<option>{currency}</option>{/each}</select></label>
      <label>Display currency<select class="block bg-app border border-edge rounded-sm p-1.5 mt-1" value={target} onchange={(event) => { target = event.currentTarget.value; changed(); }}>{#each currencies as currency}<option>{currency}</option>{/each}</select></label>
      <label>Display units per original unit<input class="block w-44 bg-app border border-edge rounded-sm p-1.5 mt-1" type="text" inputmode="decimal" value={rate} oninput={(event) => { rate = event.currentTarget.value; changed(); }} /></label>
      <label>Rate timestamp (UTC)<input class="block bg-app border border-edge rounded-sm p-1.5 mt-1" type="text" placeholder="2026-10-01T12:30:00" value={asOf} oninput={(event) => { asOf = event.currentTarget.value; changed(); }} /></label>
      <label class="flex-1 min-w-0">User-supplied source<input class="block w-full bg-app border border-edge rounded-sm p-1.5 mt-1" maxlength="256" value={source} oninput={(event) => { source = event.currentTarget.value; changed(); }} placeholder="Source or reference you checked" /></label>
    </div>
    {#if !from || !Number.isFinite(Number(rate)) || Number(rate) <= 0 || !asOf || !source.trim()}<p role="status" class="text-amber-500">Choose a monetary source, positive rate, UTC timestamp and source. Incomplete settings cannot be saved.</p>{/if}
  {/if}
</fieldset>
