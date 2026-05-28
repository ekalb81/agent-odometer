<script lang="ts">
  import { onDestroy } from 'svelte';
  import type { Session } from '../lib/types';
  import { rates } from '../lib/stores/rates';
  import { computeSessionCredits, formatCredits, tokensCost } from '../lib/credits';
  import { revealInFileManager } from '../lib/ipc';
  import Sparkline from './Sparkline.svelte';

  interface Props {
    session: Session | null;
    onclose: () => void;
  }

  let { session, onclose }: Props = $props();

  const numFmt = new Intl.NumberFormat();
  const pctFmt = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
  const fmtCredit = (amount: number) => formatCredits(amount, $rates?.currency ?? 'credits');

  function fmt(n: number): string {
    return numFmt.format(n);
  }

  function fmtDatetime(iso: string): string {
    return new Date(iso).toLocaleString();
  }

  function fmtTime(iso: string | null): string {
    return iso ? new Date(iso).toLocaleTimeString() : '—';
  }

  function fmtDuration(ms: number | null): string {
    if (ms == null) return '—';
    if (ms < 1000) return `${ms} ms`;
    const s = ms / 1000;
    if (s < 60) return `${s.toFixed(1)}s`;
    const m = Math.floor(s / 60);
    const rem = Math.round(s % 60);
    return `${m}m ${rem}s`;
  }

  // Newest turn first.
  const turnsDesc = $derived(session ? [...session.turns].sort((a, b) => b.index - a.index) : []);

  let expandedTurn = $state<string | null>(null);
  function toggleTurn(id: string) {
    expandedTurn = expandedTurn === id ? null : id;
  }
  // Collapse expanded turn when switching sessions.
  $effect(() => {
    if (session) expandedTurn = null;
  });

  let copied = $state(false);
  function copyId() {
    if (!session) return;
    navigator.clipboard.writeText(session.id).then(() => {
      copied = true;
      setTimeout(() => (copied = false), 1500);
    });
  }

  let promptExpanded = $state(false);
  const PROMPT_LIMIT = 240;

  // Reset expanded state when the session changes.
  $effect(() => {
    if (session) promptExpanded = false;
  });

  function handleReveal() {
    if (!session) return;
    revealInFileManager(session.file_path).catch(() => {});
  }

  // Context window usage.
  const ctxPercent = $derived(
    session && session.context_window
      ? (session.tokens_total.total_tokens / session.context_window) * 100
      : null,
  );

  const ctxBarWidth = $derived(ctxPercent !== null ? Math.min(ctxPercent, 100) : 0);

  // Credit computation for the open session.
  const sessionCredits = $derived(
    session && $rates ? computeSessionCredits(session, $rates) : null,
  );

  // Escape-key handler — attached only while drawer is open.
  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      onclose();
    }
  }

  $effect(() => {
    if (session) {
      window.addEventListener('keydown', handleKeydown);
    } else {
      window.removeEventListener('keydown', handleKeydown);
    }
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKeydown);
  });
</script>

<!-- Backdrop -->
{#if session}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div
    class="fixed inset-0 bg-black/50 z-40 transition-opacity"
    onclick={onclose}
    aria-hidden="true"
  ></div>
{/if}

<!-- Drawer panel -->
<div
  class="fixed top-0 right-0 h-full w-[480px] max-w-full bg-slate-900 border-l border-slate-700 shadow-2xl z-50 flex flex-col
         transition-transform duration-300 ease-in-out
         {session ? 'translate-x-0' : 'translate-x-full'}"
  role="dialog"
  aria-modal="true"
  aria-label="Session details"
>
  {#if session}
    <!-- Drawer header -->
    <div class="flex items-start justify-between gap-3 px-5 py-4 border-b border-slate-700 flex-shrink-0">
      <div class="min-w-0">
        <div class="flex items-center gap-2 flex-wrap">
          <h2 class="text-base font-semibold text-slate-100 truncate">
            {session.thread_name ?? session.first_user_message?.slice(0, 60) ?? session.id.slice(0, 8)}
          </h2>
          {#if session.archived}
            <span class="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-slate-600 text-slate-300">archived</span>
          {/if}
        </div>
        <div class="flex items-center gap-1.5 mt-1">
          <span class="font-mono text-xs text-slate-400 break-all">{session.id}</span>
          <button
            onclick={copyId}
            class="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-slate-700 hover:bg-slate-600 text-slate-300 transition-colors"
            aria-label="Copy session ID"
            title={copied ? 'Copied!' : 'Copy ID'}
          >
            {copied ? '✓' : 'Copy'}
          </button>
        </div>
      </div>
      <button
        onclick={onclose}
        class="flex-shrink-0 p-1 rounded hover:bg-slate-700 text-slate-400 hover:text-slate-100 transition-colors"
        aria-label="Close drawer"
      >
        <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12"/>
        </svg>
      </button>
    </div>

    <!-- Scrollable body -->
    <div class="flex-1 overflow-y-auto px-5 py-4 space-y-6 text-sm">

      <!-- Identity card -->
      <section>
        <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Identity</h3>
        <dl class="space-y-2">
          {#if session.working_directory}
            <div>
              <dt class="text-xs text-slate-500">Working directory</dt>
              <dd class="flex items-center gap-2 mt-0.5">
                <span class="font-mono text-xs text-slate-300 break-all">{session.working_directory}</span>
                <button
                  onclick={handleReveal}
                  class="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-slate-700 hover:bg-slate-600 text-slate-300 transition-colors whitespace-nowrap"
                  title="Open in file manager"
                  aria-label="Reveal in file manager"
                >
                  Reveal
                </button>
              </dd>
            </div>
          {/if}

          {#if session.originator || session.source || session.cli_version}
            <div class="flex flex-wrap gap-x-6 gap-y-1">
              {#if session.originator}
                <div>
                  <dt class="text-xs text-slate-500">Originator</dt>
                  <dd class="text-slate-300">{session.originator}</dd>
                </div>
              {/if}
              {#if session.source}
                <div>
                  <dt class="text-xs text-slate-500">Source</dt>
                  <dd class="text-slate-300">{session.source}</dd>
                </div>
              {/if}
              {#if session.cli_version}
                <div>
                  <dt class="text-xs text-slate-500">CLI version</dt>
                  <dd class="text-slate-300 font-mono">{session.cli_version}</dd>
                </div>
              {/if}
            </div>
          {/if}

          {#if session.model_provider || session.model}
            <div class="flex flex-wrap gap-x-6 gap-y-1">
              {#if session.model_provider}
                <div>
                  <dt class="text-xs text-slate-500">Provider</dt>
                  <dd class="text-slate-300">{session.model_provider}</dd>
                </div>
              {/if}
              {#if session.model}
                <div>
                  <dt class="text-xs text-slate-500">Model</dt>
                  <dd class="text-slate-300 font-mono">{session.model}</dd>
                </div>
              {/if}
            </div>
          {/if}

          {#if session.plan_type}
            <div>
              <dt class="text-xs text-slate-500">Plan</dt>
              <dd class="text-slate-300">{session.plan_type}</dd>
            </div>
          {/if}
        </dl>
      </section>

      <!-- Credits indicator -->
      {#if session.credits_unlimited !== null || session.credits_balance !== null}
        <section>
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Credits</h3>
          {#if session.credits_unlimited}
            <span class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-green-900/50 text-green-300 border border-green-700/50">
              Unlimited plan
              {#if session.plan_type} · {session.plan_type}{/if}
            </span>
          {:else if session.credits_balance !== null}
            <span class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-slate-700 text-slate-300">
              Balance: {session.credits_balance}
            </span>
          {/if}
        </section>
      {/if}

      <!-- Token table with credit breakdown -->
      <section>
        <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Tokens by model</h3>
        <div class="overflow-x-auto">
          <table class="w-full text-xs border-collapse">
            <thead>
              <tr class="border-b border-slate-700">
                <th class="text-left py-1.5 pr-3 text-slate-400 font-medium">Model</th>
                <th class="text-right py-1.5 px-2 text-slate-400 font-medium">Input</th>
                <th class="text-right py-1.5 px-2 text-slate-400 font-medium">Cached</th>
                <th class="text-right py-1.5 px-2 text-slate-400 font-medium">Output</th>
                <th class="text-right py-1.5 px-2 text-slate-400 font-medium">Reasoning</th>
                <th class="text-right py-1.5 pl-2 text-slate-400 font-medium">Total</th>
                <th class="text-right py-1.5 pl-2 text-slate-400 font-medium">Rate source</th>
                <th class="text-right py-1.5 pl-2 text-slate-400 font-medium">Cost</th>
              </tr>
            </thead>
            <tbody>
              {#each Object.entries(session.tokens_by_model) as [modelName, t]}
                {@const modelCredit = sessionCredits?.byModel.find((mc) => mc.model === modelName)}
                <tr class="border-b border-slate-700/50">
                  <td class="py-1.5 pr-3 font-mono text-slate-300 max-w-[120px] truncate" title={modelName}>{modelName}</td>
                  <td class="py-1.5 px-2 text-right tabular-nums text-slate-300">{fmt(t.input_tokens)}</td>
                  <td class="py-1.5 px-2 text-right tabular-nums text-slate-400">{fmt(t.cached_input_tokens)}</td>
                  <td class="py-1.5 px-2 text-right tabular-nums text-slate-300">{fmt(t.output_tokens)}</td>
                  <td class="py-1.5 px-2 text-right tabular-nums text-slate-400">{fmt(t.reasoning_output_tokens)}</td>
                  <td class="py-1.5 pl-2 text-right tabular-nums font-medium text-slate-100">{fmt(t.total_tokens)}</td>
                  <!-- Rate source: show fallback model name when fallback was used -->
                  <td class="py-1.5 pl-2 text-right font-mono text-slate-400 max-w-[100px]">
                    {#if modelCredit?.fallbackUsed}
                      <span class="text-amber-400" title="Fallback rate used — model not in rate card">
                        → {$rates?.fallback_model ?? '—'}
                      </span>
                    {:else}
                      <span class="text-slate-400">{modelName}</span>
                    {/if}
                  </td>
                  <!-- Per-model cost -->
                  <td class="py-1.5 pl-2 text-right tabular-nums text-slate-300">
                    {modelCredit ? fmtCredit(modelCredit.cost) : '—'}
                  </td>
                </tr>
              {/each}
              <!-- Totals row -->
              <tr class="border-t border-slate-600 bg-slate-800/50 font-medium">
                <td class="py-1.5 pr-3 text-slate-200">Total</td>
                <td class="py-1.5 px-2 text-right tabular-nums text-slate-200">{fmt(session.tokens_total.input_tokens)}</td>
                <td class="py-1.5 px-2 text-right tabular-nums text-slate-300">{fmt(session.tokens_total.cached_input_tokens)}</td>
                <td class="py-1.5 px-2 text-right tabular-nums text-slate-200">{fmt(session.tokens_total.output_tokens)}</td>
                <td class="py-1.5 px-2 text-right tabular-nums text-slate-300">{fmt(session.tokens_total.reasoning_output_tokens)}</td>
                <td class="py-1.5 pl-2 text-right tabular-nums text-slate-100">{fmt(session.tokens_total.total_tokens)}</td>
                <td class="py-1.5 pl-2"></td>
                <!-- Total cost cell: dash for unlimited, dollar amount otherwise -->
                <td class="py-1.5 pl-2 text-right tabular-nums text-slate-100">
                  {#if session.credits_unlimited === true}
                    —
                  {:else}
                    {sessionCredits ? fmtCredit(sessionCredits.total) : '—'}
                  {/if}
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- Reference cost line for unlimited sessions -->
        {#if session.credits_unlimited === true && sessionCredits}
          <p class="mt-2 text-xs text-slate-500">
            Reference: {fmtCredit(sessionCredits.total)} à-la-carte equivalent
          </p>
        {/if}
      </section>

      <!-- Context usage -->
      {#if ctxPercent !== null}
        <section>
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Context window usage</h3>
          <div class="space-y-1.5">
            <div class="flex justify-between text-xs text-slate-400">
              <span>{fmt(session.tokens_total.total_tokens)} / {fmt(session.context_window!)} tokens</span>
              <span class={ctxPercent > 100 ? 'text-amber-400' : 'text-slate-300'}>
                {pctFmt.format(ctxPercent)}%
              </span>
            </div>
            <div class="w-full bg-slate-700 rounded-full h-2 overflow-hidden">
              <div
                class="h-2 rounded-full transition-all {ctxPercent > 90 ? 'bg-amber-500' : 'bg-blue-500'}"
                style="width: {ctxBarWidth}%"
              ></div>
            </div>
          </div>
        </section>
      {/if}

      <!-- Tokens over time sparkline -->
      {#if session.tokens_history && session.tokens_history.length > 0}
        <section>
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Tokens over time</h3>
          <div class="text-slate-400">
            <Sparkline points={session.tokens_history} width={432} height={56} />
          </div>
          <div class="flex justify-between text-xs text-slate-500 mt-1">
            <span>{new Date(session.tokens_history[0].timestamp).toLocaleTimeString()}</span>
            <span>{new Date(session.tokens_history[session.tokens_history.length - 1].timestamp).toLocaleTimeString()}</span>
          </div>
        </section>
      {/if}

      <!-- Lifecycle -->
      <section>
        <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">Lifecycle</h3>
        <dl class="space-y-2">
          <div class="flex flex-wrap gap-x-6 gap-y-1">
            <div>
              <dt class="text-xs text-slate-500">Started</dt>
              <dd class="text-slate-300">{fmtDatetime(session.started_at)}</dd>
            </div>
            <div>
              <dt class="text-xs text-slate-500">Last activity</dt>
              <dd class="text-slate-300">{fmtDatetime(session.last_event_at)}</dd>
            </div>
          </div>
          <div>
            <dt class="text-xs text-slate-500">Total turns</dt>
            <dd class="text-slate-300">{session.total_turns}</dd>
          </div>
          {#if session.forked_from_id}
            <div>
              <dt class="text-xs text-slate-500">Forked from</dt>
              <dd class="font-mono text-xs text-slate-400 break-all">{session.forked_from_id}</dd>
            </div>
          {/if}
        </dl>
      </section>

      <!-- Turns -->
      {#if session.turns && session.turns.length > 0}
        <section>
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">
            Turns ({session.turns.length})
          </h3>
          <ul class="space-y-2">
            {#each turnsDesc as turn (turn.turn_id)}
              {@const credit = $rates ? tokensCost(turn.tokens, turn.model, $rates) : null}
              {@const isOpen = expandedTurn === turn.turn_id}
              <li class="bg-slate-800 rounded-lg border border-slate-700/60 overflow-hidden">
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <button
                  type="button"
                  class="w-full text-left px-3 py-2 hover:bg-slate-700/40 transition-colors"
                  onclick={() => toggleTurn(turn.turn_id)}
                  aria-expanded={isOpen}
                >
                  <div class="flex items-center justify-between gap-2">
                    <span class="text-xs font-medium text-slate-300">
                      #{turn.index}
                      <span class="text-slate-500 font-normal">· {fmtTime(turn.started_at)}</span>
                    </span>
                    <span class="flex items-center gap-2 flex-shrink-0">
                      {#if turn.model}
                        <span class="text-xs font-mono text-slate-400">{turn.model}</span>
                      {/if}
                      <span class="text-xs tabular-nums text-slate-300">{fmt(turn.tokens.total_tokens)} tok</span>
                      {#if credit}
                        <span class="text-xs tabular-nums text-emerald-400">{fmtCredit(credit.cost)}</span>
                        {#if credit.fallbackUsed}
                          <span class="text-amber-400" title="Fallback rate used (model not in rate card)">⚠</span>
                        {/if}
                      {/if}
                    </span>
                  </div>
                  {#if turn.user_message}
                    <p class="text-xs text-slate-400 truncate mt-1" title={turn.user_message}>
                      {turn.user_message}
                    </p>
                  {/if}
                </button>

                {#if isOpen}
                  <div class="px-3 py-2 border-t border-slate-700/60 space-y-2 text-xs">
                    <!-- Token breakdown -->
                    <div class="grid grid-cols-2 gap-x-4 gap-y-0.5 text-slate-400">
                      <span>Input <span class="text-slate-300 tabular-nums float-right">{fmt(turn.tokens.input_tokens)}</span></span>
                      <span>Cached <span class="text-slate-300 tabular-nums float-right">{fmt(turn.tokens.cached_input_tokens)}</span></span>
                      <span>Output <span class="text-slate-300 tabular-nums float-right">{fmt(turn.tokens.output_tokens)}</span></span>
                      <span>Reasoning <span class="text-slate-300 tabular-nums float-right">{fmt(turn.tokens.reasoning_output_tokens)}</span></span>
                    </div>
                    <!-- Timing -->
                    <div class="flex flex-wrap gap-x-4 gap-y-0.5 text-slate-400">
                      <span>Duration <span class="text-slate-300">{fmtDuration(turn.duration_ms)}</span></span>
                      {#if turn.time_to_first_token_ms != null}
                        <span>TTFT <span class="text-slate-300">{fmtDuration(turn.time_to_first_token_ms)}</span></span>
                      {/if}
                      {#if turn.completed_at}
                        <span>Ended <span class="text-slate-300">{fmtTime(turn.completed_at)}</span></span>
                      {/if}
                    </div>
                    <!-- Full prompt -->
                    {#if turn.user_message}
                      <div>
                        <div class="text-slate-500 mb-0.5">Prompt</div>
                        <p class="whitespace-pre-wrap text-slate-300 bg-slate-900/50 rounded px-2 py-1.5">{turn.user_message}</p>
                      </div>
                    {/if}
                    <!-- Agent reply -->
                    {#if turn.last_agent_message}
                      <div>
                        <div class="text-slate-500 mb-0.5">Final reply</div>
                        <p class="whitespace-pre-wrap text-slate-300 bg-slate-900/50 rounded px-2 py-1.5">{turn.last_agent_message}</p>
                      </div>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      <!-- First prompt -->
      {#if session.first_user_message}
        <section>
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-500 mb-3">First prompt</h3>
          <div class="bg-slate-800 rounded-lg border-l-2 border-blue-600 px-4 py-3 text-slate-300 text-xs leading-relaxed">
            {#if session.first_user_message.length <= PROMPT_LIMIT || promptExpanded}
              <span class="whitespace-pre-wrap">{session.first_user_message}</span>
            {:else}
              <span class="whitespace-pre-wrap">{session.first_user_message.slice(0, PROMPT_LIMIT)}…</span>
            {/if}

            {#if session.first_user_message.length > PROMPT_LIMIT}
              <button
                onclick={() => (promptExpanded = !promptExpanded)}
                class="mt-2 block text-blue-400 hover:text-blue-300 transition-colors"
              >
                {promptExpanded ? 'Show less' : 'Show more'}
              </button>
            {/if}
          </div>
        </section>
      {/if}

    </div>
  {/if}
</div>
