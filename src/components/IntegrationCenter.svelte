<script lang="ts">
  import { getIntegrationStatus, previewIntegrationChange, applyIntegrationChange, testIntegrationClient, openIntegrationConfiguration } from '../lib/ipc';
  import type { IntegrationClient, IntegrationScope, IntegrationChange, IntegrationCenterReport, IntegrationPreview, IntegrationVerifyReport, IntegrationClientCard } from '../lib/types';

  let scope = $state<IntegrationScope>('user');
  let project = $state('');
  let report = $state<IntegrationCenterReport | null>(null);
  let preview = $state<IntegrationPreview | null>(null);
  let tests = $state<Partial<Record<IntegrationClient, IntegrationVerifyReport>>>({});
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let freshSince = $state<Partial<Record<IntegrationClient, string>>>({});
  let epoch = 0;
  const names: Record<IntegrationClient, string> = { codex: 'Codex', claude_code: 'Claude Code' };
  const projectPath = $derived(scope === 'project' ? project.trim() || null : null);

  async function refresh(selectedScope: IntegrationScope, selectedProject: string | null, reset = true) {
    const request = ++epoch;
    preview = null; if (reset) tests = {}; error = null; report = null;
    if (selectedScope === 'project' && !selectedProject) { busy = false; return; }
    busy = true;
    try { const response = await getIntegrationStatus(selectedScope, selectedProject); if (request === epoch) { report = response; tests = Object.fromEntries(Object.entries(tests).filter(([client]) => response.cards.some((card) => card.client === client && card.configured))); } }
    catch (cause) { if (request === epoch) error = String(cause); }
    finally { if (request === epoch) busy = false; }
  }
  $effect(() => { void refresh(scope, projectPath); });

  async function prepare(client: IntegrationClient, action: IntegrationChange) {
    const request = ++epoch; busy = true; error = null; notice = null; preview = null;
    try { const response = await previewIntegrationChange(client, scope, action, projectPath); if (request === epoch) preview = response; }
    catch (cause) { if (request === epoch) error = String(cause); }
    finally { if (request === epoch) busy = false; }
  }
  async function apply() {
    if (!preview) return;
    const request = ++epoch; const id = preview.id; busy = true; error = null;
    try {
      const response = await applyIntegrationChange(id);
      if (request !== epoch) return;
      preview = null; freshSince = {}; notice = `Configuration saved. Restart the client and start a fresh task.${response.backup_path ? ` Original backup: ${response.backup_path}` : ''}`;
      await refresh(scope, projectPath);
    } catch (cause) { if (request === epoch) error = String(cause); }
    finally { if (request === epoch) busy = false; }
  }
  async function test(client: IntegrationClient) {
    const request = ++epoch; busy = true; error = null;
    try { const response = await testIntegrationClient(client, scope, projectPath); if (request === epoch) tests = { ...tests, [client]: response }; }
    catch (cause) { if (request === epoch) error = String(cause); }
    finally { if (request === epoch) busy = false; }
  }
  async function copy(value: string) {
    try { await navigator.clipboard.writeText(value); notice = 'Copied.'; }
    catch { error = 'Clipboard unavailable. Select and copy the displayed text.'; }
  }
  async function openConfiguration(client: IntegrationClient) {
    try { await openIntegrationConfiguration(client, scope, projectPath); }
    catch (cause) { error = String(cause); }
  }
  function check(client: IntegrationClient, id: string) { return tests[client]?.checks.find((check) => check.id === id); }
  function latestUse(client: IntegrationClient, fresh = false) {
    const since = fresh ? freshSince[client] : undefined;
    return report?.activity.filter((entry) => entry.client === client && entry.success && (!since || (entry.timestamp >= since && entry.initialized_at >= since))).at(-1);
  }
  function freshTask(card: IntegrationClientCard) {
    freshSince = { ...freshSince, [card.client]: new Date().toISOString() };
    notice = `Restart ${names[card.client]} and start a new task with the sample instructions. Refresh activity afterward. Self-reported client metadata is observational evidence, not authenticated identity.`;
  }
</script>

<section aria-labelledby="integration-heading" class="space-y-3 max-w-3xl">
  <h2 id="integration-heading" class="text-sm font-semibold uppercase tracking-wider text-ink-muted">Integration Center</h2>
  <p class="text-xs text-ink-faint">Connect local agent clients to Odometer’s read-only stdio analytics. Configuration, server verification, and actual client use are separate evidence.</p>
  <div class="flex flex-wrap items-end gap-3">
    <label class="text-xs text-ink-2">Configuration scope
      <select class="block bg-card border border-edge rounded-sm p-2 mt-1" bind:value={scope}>
        <option value="user">User</option><option value="project">Project</option>
      </select>
    </label>
    {#if scope === 'project'}
      <label class="text-xs text-ink-2 flex-1 min-w-0">Project directory
        <input class="block w-full bg-card border border-edge rounded-sm p-2 mt-1" bind:value={project} placeholder="Existing absolute directory" />
      </label>
    {/if}
    <button class="border border-edge rounded-sm px-3 py-2 text-xs" disabled={busy || (scope === 'project' && !projectPath)} onclick={() => refresh(scope, projectPath, false)}>Refresh status and activity</button>
  </div>
  {#if busy}<p class="text-xs text-ink-faint" role="status">Checking integration…</p>{/if}
  {#if error}<p class="text-xs text-red-500 break-words" role="alert">{error}</p>{/if}
  {#if notice}<p class="text-xs text-ink-2 break-words" role="status">{notice}</p>{/if}
  {#if report}
    <p class="text-xs text-ink-faint">Ledger: {report.status.ledger_available ? `${report.status.sessions ?? 'unknown'} recorded sessions` : 'unavailable'} · Scan: {report.status.scan_status.replaceAll('_', ' ')} · Observation age: {report.status.observation.age_seconds === null ? 'unknown' : `${report.status.observation.age_seconds}s`}</p>
    {#if report.status.ledger_available && report.status.coverage_complete !== true}
      <p class="text-xs text-amber-600">{report.status.coverage_complete === false ? 'Partial history — recorded totals exclude unrecovered historical sources.' : 'History coverage unavailable — recorded totals are not verified complete.'}</p>
    {/if}
    {#each report.status.diagnostics as diagnostic}
      <p class="text-xs text-amber-500"><code>{diagnostic.code}</code>: {diagnostic.evidence} {diagnostic.next_action}</p>
    {/each}
    {#each report.cards as card}
      {@const verification = tests[card.client]}
      {@const used = latestUse(card.client)}
      {@const fresh = freshSince[card.client] ? latestUse(card.client, true) : undefined}
      <article aria-label={`${names[card.client]} integration`} class="bg-card border border-edge rounded-lg p-4 space-y-3">
        <div class="flex flex-wrap justify-between gap-2"><h3 class="font-semibold text-sm">{names[card.client]}</h3><span class="text-xs text-ink-faint">{verification?.ok ? 'Server canary verified' : 'Not verified working'}</span></div>
        <dl class="grid grid-cols-2 sm:grid-cols-4 gap-3 text-xs">
          <div><dt class="text-ink-faint">Installed</dt><dd>{card.installed ? card.version : card.executable ? 'Version unverified' : 'Not installed'}</dd></div>
          <div><dt class="text-ink-faint">Configured</dt><dd>{card.configured ? 'Matching entry' : 'Not configured'}</dd></div>
          <div><dt class="text-ink-faint">Launchable</dt><dd>{check(card.client, 'mcp_launch')?.status ?? 'unknown'}</dd></div>
          <div><dt class="text-ink-faint">Connected</dt><dd>{check(card.client, 'mcp_initialize')?.status ?? 'unknown'}</dd></div>
          <div><dt class="text-ink-faint">Tools exposed</dt><dd>{check(card.client, 'mcp_tools')?.status ?? 'unknown'}</dd></div>
          <div><dt class="text-ink-faint">Data ready</dt><dd>{report.status.ledger_available ? check(card.client, 'mcp_query')?.status ?? 'unknown' : 'unavailable'}</dd></div>
          <div><dt class="text-ink-faint">Actively used</dt><dd>{used ? `Unverified; reported call ${used.timestamp}` : 'No successful client call observed'}</dd></div>
          <div><dt class="text-ink-faint">Reported initialization</dt><dd>{fresh ? 'Call reported after new initialization' : freshSince[card.client] ? 'Awaiting reported call after initialization' : 'Not observed'}</dd></div>
        </dl>
        <p class="text-xs text-ink-faint">Launch, handshake, catalog and canary are direct server checks. Actual client loading requires a fresh task. Client identity in activity is self-reported.</p>
        <p class="text-xs text-amber-500">Supported-client agent-task proof: {report.supported_client_task_proof.replaceAll('_', ' ')}. Reported activity cannot authenticate the client or prove an actual agent task.</p>
        {#if used}<p class="text-xs text-ink-faint">Last observed successful client initialization: {used.initialized_at}. Last call: {used.tool}.</p>{/if}
        <div class="flex flex-wrap gap-2 text-xs">
          <button class="border border-edge rounded-sm px-3 py-2" disabled={busy} onclick={() => prepare(card.client, 'install')}>{card.managed ? 'Preview repair' : 'Preview setup'}</button>
          <button class="border border-edge rounded-sm px-3 py-2" disabled={busy || !card.configured} onclick={() => test(card.client)}>Test now</button>
          <button class="border border-edge rounded-sm px-3 py-2" onclick={() => copy(card.manual_command)}>Copy server command</button>
          <button class="border border-edge rounded-sm px-3 py-2" onclick={() => copy(card.instructions)}>Copy instructions</button>
          <button class="border border-edge rounded-sm px-3 py-2" onclick={() => openConfiguration(card.client)}>Open configuration location</button>
          <button class="border border-edge rounded-sm px-3 py-2" onclick={() => copy(`---\nname: odometer-analytics\ndescription: Read local observed agent usage through Odometer MCP tools.\n---\n\n${card.instructions}`)}>Copy optional SKILL.md</button>
          <button class="border border-edge rounded-sm px-3 py-2" disabled={!card.configured} onclick={() => freshTask(card)}>Start fresh-task verification</button>
          <button class="border border-edge rounded-sm px-3 py-2" disabled={busy || !card.managed} onclick={() => prepare(card.client, 'remove')}>Preview removal</button>
          <button class="border border-edge rounded-sm px-3 py-2" disabled={busy || !card.restore_available} onclick={() => prepare(card.client, 'restore')}>Preview restore</button>
        </div>
        {#if card.diagnostic}<p class="text-xs text-amber-500">{card.diagnostic.code}: {card.diagnostic.evidence} {card.diagnostic.next_action}</p>{/if}
        {#if verification}<ul class="space-y-1 text-xs">{#each verification.checks as step}<li>{step.status}: {step.detail}</li>{/each}</ul>{/if}
        <details class="text-xs"><summary class="cursor-pointer text-ink-2">Manual setup, configuration location and instructions</summary>
          <p class="mt-2 break-all">Configuration: {card.configuration_path}</p>
          <pre class="mt-2 whitespace-pre-wrap break-all">{card.manual_command}</pre>
          <p class="mt-2 whitespace-pre-wrap">{card.instructions}</p>
        </details>
      </article>
    {/each}
    <article aria-label="Generic MCP client integration" class="bg-card border border-edge rounded-lg p-4 space-y-2 text-xs">
      <h3 class="font-semibold text-sm">Generic MCP client</h3>
      <p>Use manual stdio configuration with the server command above. Odometer cannot discover this client’s configuration or authenticate its identity. Test the server, then start a fresh task and inspect activity.</p>
      <p>Sample task: Call odometer_status, then usage_report and workflow_metrics for the same UTC week. Explain incomplete coverage before proposing an observational optimization experiment.</p>
    </article>
    {@const canary = report.activity.filter((entry) => entry.success && entry.tool === 'odometer_status' && entry.ledger_available === true && entry.schema_version === report?.status.schema_version).at(-1)}
    <p class="text-xs text-ink-faint">Last successful server status canary: {canary?.timestamp ?? 'not observed'}.</p>
    <details class="text-xs"><summary class="cursor-pointer text-ink-2">Recent local MCP activity</summary>
      <p class="my-2 text-ink-faint">At most 100 calls. No prompts, arguments, session IDs, paths, results or credentials are retained. Verifier calls do not prove client use.</p>
      {#if !report.activity_available}<p role="status">Activity unavailable; absence does not prove no use.</p>
      {:else if report.activity.length === 0}<p>No calls observed.</p>
      {:else}<ul class="space-y-2">{#each [...report.activity].reverse() as entry}<li>{entry.timestamp} · {entry.client} (self-reported) · {entry.tool} · {entry.success ? 'success' : entry.error_code ?? 'failed'} · {entry.duration_ms}ms · {entry.result_bytes} bytes{entry.result_rows === null ? '' : ` · ${entry.result_rows} rows`}{entry.fallback_or_unavailable ? ' · incomplete pricing evidence' : ''}</li>{/each}</ul>{/if}
    </details>
    <p class="text-xs text-ink-faint">{report.status.pricing_authority} {report.status.quota_authority}</p>
  {/if}
  {#if preview}
    <div class="border border-accent rounded-lg p-4 space-y-3" aria-label="Integration change preview">
      <h3 class="text-sm font-semibold">Review {preview.action} for {names[preview.client]}</h3>
      <p class="text-xs break-all">{preview.configuration_path}</p>
      <pre class="text-xs whitespace-pre-wrap break-all">{preview.entry_preview}</pre>
      <p class="text-xs text-amber-500">{preview.warning}</p>
      <button class="border border-edge rounded-sm px-3 py-2 text-xs" disabled={busy} onclick={apply}>Apply reviewed change</button>
      <button class="border border-edge rounded-sm px-3 py-2 text-xs ml-2" disabled={busy} onclick={() => preview = null}>Cancel</button>
    </div>
  {/if}
</section>
