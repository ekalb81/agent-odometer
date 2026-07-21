<script lang="ts">
  import type { Session } from '../lib/types';
  import { rates } from '../lib/stores/rates';
  import { computeSessionApiCost, computeSessionCredits, fallbackModelName, formatCredits, harnessCurrency, tokensCost } from '../lib/credits';
  import { openTaskInChatGPT, revealInFileManager } from '../lib/ipc';
  import Sparkline from './Sparkline.svelte';

  interface Props {
    session: Session | null;
    /** Subagent sessions spawned by this one (for the "N subagents" pill). */
    childCount?: number;
    onclose: () => void;
  }

  let { session, childCount = 0, onclose }: Props = $props();

  const numFmt = new Intl.NumberFormat();
  const pctFmt = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });
  const fmtCredit = (amount: number) =>
    formatCredits(
      amount,
      session && $rates ? harnessCurrency($rates, session.harness) : ($rates?.currency ?? 'credits'),
    );

  function fmt(n: number): string {
    return numFmt.format(n);
  }

  function fmtK(n: number): string {
    return n >= 1000 ? `${Math.round(n / 1000)}k` : String(n);
  }

  function fmtDatetime(iso: string): string {
    return new Date(iso).toLocaleString();
  }

  function fmtTime(iso: string | null): string {
    if (!iso) return '—';
    const d = new Date(iso);
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }

  function fmtDurationMs(ms: number | null): string {
    if (ms == null) return '—';
    if (ms < 1000) return `${ms} ms`;
    const s = ms / 1000;
    if (s < 60) return `${s.toFixed(1)}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ${Math.round(s % 60)}s`;
    const h = Math.floor(m / 60);
    return `${h}h ${m % 60}m`;
  }

  const sessionDuration = $derived(
    session
      ? fmtDurationMs(new Date(session.last_event_at).getTime() - new Date(session.started_at).getTime())
      : '—',
  );

  const isSubagent = $derived(
    Boolean(session && (session.parent_thread_id || session.agent_path || session.source === 'subagent')),
  );

  // Newest turn first.
  const turnsDesc = $derived(session ? [...session.turns].sort((a, b) => b.index - a.index) : []);
  const turnsAsc = $derived(session ? [...session.turns].sort((a, b) => a.index - b.index) : []);

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

  function handleRevealWorkspace() {
    if (!session?.working_directory) return;
    revealInFileManager(session.working_directory).catch(() => {});
  }

  function handleRevealTranscript() {
    if (!session) return;
    revealInFileManager(session.file_path).catch(() => {});
  }

  function handleOpenTask() {
    if (!session) return;
    openTaskInChatGPT(session.parent_thread_id ?? session.id).catch(() => {});
  }

  // Context window usage: the LAST call's context fill, not cumulative
  // session throughput (which exceeds the window many times over).
  const ctxPercent = $derived(
    session && session.context_window && session.latest_context_tokens != null
      ? (session.latest_context_tokens / session.context_window) * 100
      : null,
  );
  const ctxBarWidth = $derived(ctxPercent !== null ? Math.min(ctxPercent, 100) : 0);

  const sessionCredits = $derived(
    session && $rates ? computeSessionCredits(session, $rates) : null,
  );

  // What the same usage would cost à la carte at OpenAI API rates —
  // informational for subscription users; codex sessions only.
  const sessionApiCost = $derived(
    session && session.harness === 'codex' && $rates
      ? computeSessionApiCost(session, $rates)
      : null,
  );

  // The headline money figure: Codex shows the API-rate estimate, Claude the
  // Anthropic-rate cost.
  const heroCost = $derived(
    session?.harness === 'codex'
      ? (sessionApiCost ? { label: 'Est. API', text: formatCredits(sessionApiCost.total, 'USD') } :
         sessionCredits ? { label: 'Credits', text: fmtCredit(sessionCredits.total) } : null)
      : (sessionCredits ? { label: 'Cost', text: fmtCredit(sessionCredits.total) } : null),
  );

  // Per-turn costs for the mini bar chart (turn order, oldest → newest).
  const turnCosts = $derived((() => {
    if (!session || !$rates) return [];
    return turnsAsc.map((t) => ({
      index: t.index,
      cost: tokensCost(t.tokens, t.model, $rates!, t.service_tier, session!.harness).cost,
    }));
  })());
  const maxTurnCost = $derived(turnCosts.reduce((m, t) => Math.max(m, t.cost), 0));

  function turnCost(turnId: string): { cost: number; fallbackUsed: boolean } | null {
    if (!session || !$rates) return null;
    const t = session.turns.find((x) => x.turn_id === turnId);
    if (!t) return null;
    return tokensCost(t.tokens, t.model, $rates, t.service_tier, session.harness);
  }

  const fmtMoney = $derived((n: number) =>
    session?.harness === 'codex' && sessionApiCost ? formatCredits(n, 'USD') : fmtCredit(n),
  );
</script>

<div class="flex flex-col h-full bg-panel overflow-hidden" aria-label="Session details">
  {#if !session}
    <div class="flex-1 flex flex-col items-center justify-center gap-2 text-ink-faint text-xs p-6 text-center">
      <svg width="28" height="28" viewBox="0 0 96 96" class="opacity-40" aria-hidden="true">
        <circle cx="48" cy="48" r="38" fill="none" stroke="currentColor" stroke-width="14" stroke-dasharray="4.6 5.8"/>
        <circle cx="48" cy="48" r="10" fill="currentColor"/>
      </svg>
      <p>Select a session to see its details</p>
    </div>
  {:else}
    <!-- Header -->
    <div class="px-5 py-4 border-b border-edge flex-shrink-0">
      <div class="flex items-start justify-between gap-2">
        <div class="font-semibold text-[15px] text-ink min-w-0 truncate" title={session.thread_name ?? session.first_user_message ?? session.id}>
          {session.thread_name ?? session.first_user_message?.slice(0, 60) ?? session.id.slice(0, 8)}
        </div>
        <button
          onclick={onclose}
          class="flex-shrink-0 p-0.5 rounded text-ink-faint hover:text-ink transition-colors"
          aria-label="Deselect session"
          title="Deselect (Esc)"
        >
          <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12"/>
          </svg>
        </button>
      </div>
      <div class="flex items-center gap-2 mt-[5px] flex-wrap">
        <button
          onclick={copyId}
          class="font-mono text-[11px] text-ink-faint hover:text-ink-2 transition-colors"
          title={copied ? 'Copied!' : `${session.id} — click to copy`}
          aria-label="Copy session ID"
        >{copied ? 'copied ✓' : session.id.slice(0, 18)}</button>
        {#if session.credits_unlimited}
          <span class="text-[10px] font-semibold px-[9px] py-[2px] rounded-full bg-[var(--positive-chip-bg)] text-pos border border-[var(--positive-chip-border)]">
            unlimited{#if session.plan_type}&nbsp;· {session.plan_type}{/if}
          </span>
        {/if}
        {#if childCount > 0}
          <span class="text-[10px] font-semibold px-[9px] py-[2px] rounded-full bg-[var(--subagent-chip-bg)] text-[var(--subagent-chip-fg)]">
            {childCount} {childCount === 1 ? 'subagent' : 'subagents'}
          </span>
        {/if}
        {#if isSubagent}
          <span class="text-[10px] font-semibold px-[9px] py-[2px] rounded-full bg-[var(--subagent-chip-bg)] text-[var(--subagent-chip-fg)]">subagent</span>
        {/if}
        {#if session.archived}
          <span class="text-[10px] font-semibold px-[9px] py-[2px] rounded-full bg-[var(--archived-chip-bg)] text-[var(--archived-chip-fg)]">archived</span>
        {/if}
        {#if session.harness !== 'claude_code'}
          <button
            onclick={handleOpenTask}
            class="ml-auto text-[10px] font-medium px-2 py-[2px] rounded-full border border-edge text-ink-muted hover:text-ink transition-colors whitespace-nowrap"
            aria-label={session.parent_thread_id ? 'Open parent task in ChatGPT' : 'Open task in ChatGPT'}
          >
            {session.parent_thread_id ? 'Open parent ↗' : 'Open in ChatGPT ↗'}
          </button>
        {/if}
      </div>
    </div>

    <!-- 2×2 stat grid -->
    <div class="grid grid-cols-2 gap-px bg-edge border-b border-edge flex-shrink-0">
      <div class="bg-panel px-5 py-2.5">
        <div class="section-label">Tokens</div>
        <div class="font-mono font-semibold mt-0.5 text-ink">{fmt(session.tokens_total.total_tokens)}</div>
      </div>
      <div class="bg-panel px-5 py-2.5">
        <div class="section-label">{heroCost?.label ?? 'Cost'}</div>
        <div class="font-mono font-semibold mt-0.5 text-accent-cost">{heroCost?.text ?? '—'}</div>
      </div>
      <div class="bg-panel px-5 py-2.5">
        <div class="section-label">Turns</div>
        <div class="font-mono font-semibold mt-0.5 text-ink">{session.total_turns}</div>
      </div>
      <div class="bg-panel px-5 py-2.5">
        <div class="section-label">Duration</div>
        <div class="font-mono font-semibold mt-0.5 text-ink">{sessionDuration}</div>
      </div>
    </div>

    <!-- Scrollable body -->
    <div class="flex-1 overflow-y-auto min-h-0">
      <!-- Context bar -->
      {#if ctxPercent !== null}
        <div class="px-5 py-3 border-b border-edge">
          <div class="flex justify-between text-[11px] text-ink-muted mb-1.5">
            <span>Context · last request</span>
            <span class="font-mono text-ink-2">
              {fmtK(session.latest_context_tokens!)} / {fmtK(session.context_window!)} · {pctFmt.format(ctxPercent)}%
            </span>
          </div>
          <div class="h-[6px] bg-track rounded-[3px] overflow-hidden">
            <div
              class="h-[6px] rounded-[3px] {ctxPercent > 90 ? 'bg-amber-500' : 'bg-accent'}"
              style="width: {ctxBarWidth}%"
            ></div>
          </div>
        </div>
      {/if}

      <!-- Cost per turn -->
      {#if turnCosts.length > 0 && maxTurnCost > 0}
        <div class="px-5 py-3 border-b border-edge">
          <div class="flex justify-between items-baseline mb-2">
            <span class="section-label">Cost per turn</span>
            {#if sessionCredits}
              <span class="text-[10px] font-mono text-ink-faint">{fmtMoney(session.harness === 'codex' && sessionApiCost ? sessionApiCost.total : sessionCredits.total)} total</span>
            {/if}
          </div>
          <div class="flex items-end gap-[5px] h-10">
            {#each turnCosts as t (t.index)}
              <div
                class="flex-1 rounded-t-[3px] {t.cost >= maxTurnCost * 0.8 ? 'bg-accent' : 'bg-accent-dim'}"
                style="height: {Math.max(2, Math.round((t.cost / maxTurnCost) * 38))}px"
                title={`#${t.index} · ${fmtMoney(t.cost)}`}
              ></div>
            {/each}
          </div>
          <div class="flex justify-between text-[10px] text-ink-faint font-mono mt-1">
            <span>#{turnCosts[0].index} · {fmtMoney(turnCosts[0].cost)}</span>
            <span>#{turnCosts[turnCosts.length - 1].index} · {fmtMoney(turnCosts[turnCosts.length - 1].cost)}</span>
          </div>
        </div>
      {/if}

      <!-- Turns, newest first -->
      {#if turnsDesc.length > 0}
        <div class="px-5 py-3">
          <div class="section-label mb-2">Turns · newest first</div>
          <div class="flex flex-col gap-1.5">
            {#each turnsDesc as turn (turn.turn_id)}
              {@const credit = turnCost(turn.turn_id)}
              {@const isOpen = expandedTurn === turn.turn_id}
              <div class="bg-card border border-edge rounded-lg overflow-hidden">
                <button
                  type="button"
                  class="w-full text-left px-3 py-2 hover:bg-[var(--row-hover)] transition-colors"
                  onclick={() => toggleTurn(turn.turn_id)}
                  aria-expanded={isOpen}
                >
                  <div class="flex items-center justify-between gap-2 text-[11px]">
                    <span class="font-semibold text-ink">
                      #{turn.index}
                      <span class="text-ink-faint font-normal">{fmtTime(turn.started_at)}</span>
                      {#if turn.status !== 'completed'}
                        <span class="ml-1 font-medium px-1.5 py-px rounded-full
                          {turn.status === 'aborted'
                            ? 'bg-amber-500/15 text-amber-500'
                            : turn.status === 'rolled_back'
                              ? 'bg-[var(--archived-chip-bg)] text-[var(--archived-chip-fg)]'
                              : 'bg-accent-chipbg text-accent-chipfg'}">
                          {turn.status.replace('_', ' ')}
                        </span>
                      {/if}
                    </span>
                    <span class="flex items-center gap-1.5 flex-shrink-0">
                      {#if credit}
                        <span class="font-mono text-pos">{fmtMoney(credit.cost)}</span>
                        {#if credit.fallbackUsed && turn.tokens.total_tokens > 0}
                          <span class="text-amber-500" title="Fallback rate used (model not in rate card)">⚠</span>
                        {/if}
                      {:else}
                        <span class="font-mono text-ink-muted">{fmt(turn.tokens.total_tokens)} tok</span>
                      {/if}
                    </span>
                  </div>
                  {#if turn.user_message}
                    <p class="text-[11px] text-ink-muted truncate mt-0.5" title={turn.user_message}>
                      {turn.user_message}
                    </p>
                  {/if}
                </button>

                {#if isOpen}
                  <div class="px-3 py-2 border-t border-edge space-y-2 text-[11px]">
                    <!-- Token breakdown -->
                    <div class="grid grid-cols-2 gap-x-4 gap-y-0.5 text-ink-muted">
                      <span>Input <span class="text-ink-2 font-mono float-right">{fmt(turn.tokens.input_tokens)}</span></span>
                      <span>Cached <span class="text-ink-2 font-mono float-right">{fmt(turn.tokens.cached_input_tokens)}</span></span>
                      <span>Output <span class="text-ink-2 font-mono float-right">{fmt(turn.tokens.output_tokens)}</span></span>
                      <span>Reasoning <span class="text-ink-2 font-mono float-right">{fmt(turn.tokens.reasoning_output_tokens)}</span></span>
                    </div>
                    <!-- Timing / metadata -->
                    <div class="flex flex-wrap gap-x-4 gap-y-0.5 text-ink-muted">
                      <span>Total <span class="text-ink-2 font-mono">{fmt(turn.tokens.total_tokens)} tok</span></span>
                      <span>Duration <span class="text-ink-2">{fmtDurationMs(turn.duration_ms)}</span></span>
                      {#if turn.time_to_first_token_ms != null}
                        <span>TTFT <span class="text-ink-2">{fmtDurationMs(turn.time_to_first_token_ms)}</span></span>
                      {/if}
                      {#if turn.model}
                        <span>Model <span class="text-ink-2 font-mono">{turn.model}</span></span>
                      {/if}
                      {#if turn.reasoning_effort}
                        <span>Effort <span class="text-ink-2">{turn.reasoning_effort}</span></span>
                      {/if}
                      {#if turn.collaboration_mode}
                        <span>Mode <span class="text-ink-2">{turn.collaboration_mode}</span></span>
                      {/if}
                      {#if turn.service_tier}
                        <span>Tier <span class="text-ink-2">{turn.service_tier}</span></span>
                      {/if}
                    </div>
                    {#if turn.abort_reason}
                      <p class="text-amber-500">Stopped: {turn.abort_reason}</p>
                    {/if}
                    {#if turn.user_message}
                      <div>
                        <div class="text-ink-faint mb-0.5">Prompt</div>
                        <p class="whitespace-pre-wrap text-ink-2 bg-app rounded px-2 py-1.5">{turn.user_message}</p>
                      </div>
                    {/if}
                    {#if turn.last_agent_message}
                      <div>
                        <div class="text-ink-faint mb-0.5">Final reply</div>
                        <p class="whitespace-pre-wrap text-ink-2 bg-app rounded px-2 py-1.5">{turn.last_agent_message}</p>
                      </div>
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        </div>
      {/if}

      <!-- Everything the compact layout doesn't surface, tucked away but kept. -->
      <details class="px-5 py-3 border-t border-edge group">
        <summary class="section-label cursor-pointer select-none list-none flex items-center gap-1">
          <span class="inline-block transition-transform group-open:rotate-90">▸</span> More details
        </summary>
        <div class="mt-3 space-y-4 text-xs">
          <!-- Tokens by model -->
          <div>
            <div class="section-label mb-2">Tokens by model</div>
            <table class="w-full text-[11px] border-collapse">
              <thead>
                <tr class="border-b border-edge text-ink-muted">
                  <th class="text-left py-1 pr-2 font-medium">Model</th>
                  <th class="text-right py-1 px-1 font-medium">Input</th>
                  <th class="text-right py-1 px-1 font-medium">Cached</th>
                  <th class="text-right py-1 px-1 font-medium">Output</th>
                  <th class="text-right py-1 pl-1 font-medium">Total</th>
                  <th class="text-right py-1 pl-1 font-medium">Cost</th>
                </tr>
              </thead>
              <tbody>
                {#each Object.entries(session.tokens_by_model) as [modelName, t]}
                  {@const modelCredit = sessionCredits?.byModel.find((mc) => mc.model === modelName)}
                  <tr class="border-b border-edgerow">
                    <td class="py-1 pr-2 font-mono text-ink-2 max-w-[110px] truncate" title={modelName}>
                      {modelName}
                      {#if modelCredit?.fallbackUsed}
                        <span class="text-amber-500" title="Fallback rate used ({$rates ? fallbackModelName($rates, session.harness) : '—'})">⚠</span>
                      {/if}
                    </td>
                    <td class="py-1 px-1 text-right font-mono text-ink-2">{fmt(t.input_tokens)}</td>
                    <td class="py-1 px-1 text-right font-mono text-ink-muted">{fmt(t.cached_input_tokens)}</td>
                    <td class="py-1 px-1 text-right font-mono text-ink-2">{fmt(t.output_tokens)}</td>
                    <td class="py-1 pl-1 text-right font-mono text-ink">{fmt(t.total_tokens)}</td>
                    <td class="py-1 pl-1 text-right font-mono text-ink-2">{modelCredit ? fmtCredit(modelCredit.cost) : '—'}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
            {#if session.credits_unlimited === true && sessionCredits}
              <p class="mt-1.5 text-[11px] text-ink-faint">
                Reference: {fmtCredit(sessionCredits.total)} à-la-carte equivalent
              </p>
            {/if}
            {#if sessionApiCost}
              <p class="mt-1.5 text-[11px] text-ink-faint">
                Est. API cost: <span class="text-accent-cost">{formatCredits(sessionApiCost.total, 'USD')}</span>
                at OpenAI API rates{#if session.plan_type}&nbsp;— informational on the {session.plan_type} plan{/if}
              </p>
            {/if}
          </div>

          <!-- Tokens over time -->
          {#if session.tokens_history && session.tokens_history.length > 0}
            <div>
              <div class="section-label mb-2">Tokens over time</div>
              <div class="text-accent">
                <Sparkline points={session.tokens_history} width={368} height={48} />
              </div>
              <div class="flex justify-between text-[10px] text-ink-faint font-mono mt-1">
                <span>{fmtTime(session.tokens_history[0].timestamp)}</span>
                <span>{fmtTime(session.tokens_history[session.tokens_history.length - 1].timestamp)}</span>
              </div>
            </div>
          {/if}

          <!-- Identity / lifecycle -->
          <dl class="space-y-1.5 text-[11px]">
            {#if session.working_directory}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Workspace</dt>
                <dd class="font-mono text-ink-2 break-all min-w-0">{session.working_directory}</dd>
                <button onclick={handleRevealWorkspace} class="flex-shrink-0 text-ink-faint hover:text-ink transition-colors" title="Open in file manager">Reveal</button>
              </div>
            {/if}
            <div class="flex items-start gap-2">
              <dt class="text-ink-faint w-20 flex-shrink-0">Transcript</dt>
              <dd class="font-mono text-ink-2 break-all min-w-0">{session.file_path}</dd>
              <button onclick={handleRevealTranscript} class="flex-shrink-0 text-ink-faint hover:text-ink transition-colors" title="Reveal in file manager">Reveal</button>
            </div>
            {#if session.agent_path || session.agent_nickname}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Agent</dt>
                <dd class="text-ink-2">{session.agent_nickname ?? session.agent_path}</dd>
              </div>
            {/if}
            {#if session.model}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Model</dt>
                <dd class="font-mono text-ink-2">
                  {session.model}{#if session.service_tier}&nbsp;· {session.service_tier}{/if}
                  {#if session.model_provider}&nbsp;({session.model_provider}){/if}
                </dd>
              </div>
            {/if}
            {#if session.plan_type}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Plan</dt>
                <dd class="text-ink-2">{session.plan_type}{#if session.credits_balance !== null}&nbsp;· balance {session.credits_balance}{/if}</dd>
              </div>
            {/if}
            {#if session.originator || session.source || session.cli_version}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Source</dt>
                <dd class="text-ink-2">
                  {[session.originator, session.source].filter(Boolean).join(' · ')}
                  {#if session.cli_version}<span class="font-mono">&nbsp;· {session.cli_version}</span>{/if}
                </dd>
              </div>
            {/if}
            <div class="flex items-start gap-2">
              <dt class="text-ink-faint w-20 flex-shrink-0">Started</dt>
              <dd class="text-ink-2">{fmtDatetime(session.started_at)}</dd>
            </div>
            <div class="flex items-start gap-2">
              <dt class="text-ink-faint w-20 flex-shrink-0">Last event</dt>
              <dd class="text-ink-2">{fmtDatetime(session.last_event_at)}</dd>
            </div>
            {#if session.forked_from_id}
              <div class="flex items-start gap-2">
                <dt class="text-ink-faint w-20 flex-shrink-0">Forked from</dt>
                <dd class="font-mono text-ink-muted break-all">{session.forked_from_id}</dd>
              </div>
            {/if}
          </dl>

          <!-- First prompt -->
          {#if session.first_user_message}
            <div>
              <div class="section-label mb-2">First prompt</div>
              <p class="whitespace-pre-wrap text-ink-2 text-[11px] leading-relaxed bg-card border border-edge rounded-lg px-3 py-2">{session.first_user_message}</p>
            </div>
          {/if}
        </div>
      </details>
    </div>
  {/if}
</div>
