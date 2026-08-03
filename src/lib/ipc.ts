// Thin typed wrappers around @tauri-apps/api invoke + event.listen.
// All IPC between the Svelte frontend and Rust backend goes through this module.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Session, SessionSummary, RangeTotals, ScanStatus, Config, RateCard, ExternalEvent, CorrelationQuery, CorrelationResult, GitOutcome, PerformanceStatus, ToolImpactResult, ToolImpactTarget, ToolImpactTargetKind, InstructionInventory, InstructionScanProgress, InstructionContent, ProviderDescriptor, TurnReceiptIntegrationStatus, DefenderExclusionReceipt, SubscriptionUsageEntry } from './types';

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

export function listSessions(): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>('list_sessions');
}

/** Full session (turns + token history) for the detail drawer. */
export function getSessionDetails(sessionId: string): Promise<Session | null> {
  return invoke<Session | null>('get_session_details', { sessionId });
}

/** Date-scoped rollups for all sessions, one result map per requested window.
 *  Bounds are inclusive UTC ISO strings; null = open bound. Sessions with no
 *  usage in a window are omitted from that window's map. */
export function sessionsInRanges(
  ranges: { from: string | null; to: string | null }[],
  sessionIds?: string[],
): Promise<Record<string, RangeTotals>[]> {
  return invoke<Record<string, RangeTotals>[]>('sessions_in_ranges', {
    ranges,
    sessionIds: sessionIds ?? null,
  });
}

/** Most-recent provider-reported subscription-usage snapshot per harness.
 *  Omits harnesses with no rate-limit history (Claude Code transcripts, today). */
export function getSubscriptionUsage(): Promise<SubscriptionUsageEntry[]> {
  return invoke<SubscriptionUsageEntry[]>('get_subscription_usage');
}

export function listToolImpactTargets(
  from: string | null,
  to: string | null,
  sessionIds?: string[],
): Promise<ToolImpactTarget[]> {
  return invoke<ToolImpactTarget[]>('list_tool_impact_targets', {
    query: { from, to, session_ids: sessionIds ?? null },
  });
}

export function compareToolImpact(
  targetKind: ToolImpactTargetKind,
  targetKey: string,
  from: string | null,
  to: string | null,
  sessionIds?: string[],
): Promise<ToolImpactResult> {
  return invoke<ToolImpactResult>('compare_tool_impact', {
    query: {
      target_kind: targetKind,
      target_key: targetKey,
      from,
      to,
      session_ids: sessionIds ?? null,
    },
  });
}

/** Current bulk-scan progress (call once on mount, then follow events). */
export function getScanStatus(): Promise<ScanStatus> {
  return invoke<ScanStatus>('get_scan_status');
}

/** Windows only: opens the UAC flow to exclude session folders from Defender scanning. */
export function addDefenderExclusions(): Promise<DefenderExclusionReceipt> {
  return invoke<DefenderExclusionReceipt>('add_defender_exclusions');
}

export function getConfig(): Promise<Config> {
  return invoke<Config>('get_config');
}

/** Registered providers with display names and capability flags. */
export function listProviders(): Promise<ProviderDescriptor[]> {
  return invoke<ProviderDescriptor[]>('list_providers');
}

export function setConfig(config: Config): Promise<void> {
  return invoke<void>('set_config', { config });
}

export function listInstructionFiles(): Promise<InstructionInventory> {
  return invoke<InstructionInventory>('list_instruction_files');
}

export function cancelInstructionScan(): Promise<number> {
  return invoke<number>('cancel_instruction_scan');
}

export function readInstructionFile(path: string): Promise<InstructionContent> {
  return invoke<InstructionContent>('read_instruction_file', { path });
}

export function openInstructionFile(path: string): Promise<void> {
  return invoke<void>('open_instruction_file', { path });
}

export function getTurnReceiptStatus(): Promise<TurnReceiptIntegrationStatus> {
  return invoke<TurnReceiptIntegrationStatus>('get_turn_receipt_status');
}

export function repairTurnReceiptIntegrations(): Promise<TurnReceiptIntegrationStatus> {
  return invoke<TurnReceiptIntegrationStatus>('repair_turn_receipt_integrations');
}

export function getRates(): Promise<RateCard> {
  return invoke<RateCard>('get_rates');
}

export function getBundledRates(): Promise<RateCard> {
  return invoke<RateCard>('get_bundled_rates');
}

export function setRates(rates: RateCard): Promise<void> {
  return invoke<void>('set_rates', { rates });
}

export function revealInFileManager(path: string): Promise<void> {
  return invoke<void>('reveal_in_file_manager', { path });
}

export function openTaskInChatGPT(sessionId: string): Promise<void> {
  return invoke<void>('open_task_in_chatgpt', { sessionId });
}

/** Opens a backend-owned native save dialog and writes only its selected path. */
export function writeExport(
  defaultName: string,
  format: 'csv' | 'json',
  content: string,
): Promise<boolean> {
  return invoke<boolean>('write_export', { defaultName, format, content });
}

export function listExternalEvents(): Promise<ExternalEvent[]> {
  return invoke<ExternalEvent[]>('list_external_events');
}

export function correlateEvents(query: CorrelationQuery): Promise<CorrelationResult> {
  return invoke<CorrelationResult>('correlate_events', { query });
}

export function scanGitOutcomes(postWindowHours = 24): Promise<GitOutcome[]> {
  return invoke<GitOutcome[]>('scan_git_outcomes', { postWindowHours });
}

export interface TrayTotals {
  tokens: string;
  codex_credits: string;
  codex_api_usd: string;
  claude_usd: string;
}

export function setTrayTotals(totals: TrayTotals): Promise<void> {
  return invoke<void>('set_tray_totals', { totals });
}

export function getPerformanceStatus(): Promise<PerformanceStatus> {
  return invoke<PerformanceStatus>('get_performance_status');
}

export function recordFrontendPerformance(
  operation: string,
  durationMs: number,
  success: boolean,
  metadata: Record<string, string>,
): Promise<void> {
  return invoke<void>('record_frontend_performance', { operation, durationMs, success, metadata });
}

export function exportPerformanceData(format: 'jsonl' | 'csv'): Promise<boolean> {
  return invoke<boolean>('export_performance_data', { format });
}

export function onOpenSettings(cb: () => void): Promise<UnlistenFn> {
  return listen('open-settings', cb);
}

// ---------------------------------------------------------------------------
// Events  (Phase 3 will emit these from the watcher)
// ---------------------------------------------------------------------------

export function onSessionUpdated(cb: (session: SessionSummary) => void): Promise<UnlistenFn> {
  return listen<SessionSummary>('session-updated', (event) => cb(event.payload));
}

export function onSessionRemoved(cb: (sessionId: string) => void): Promise<UnlistenFn> {
  return listen<string>('session-removed', (event) => cb(event.payload));
}

export function onScanProgress(cb: (status: ScanStatus) => void): Promise<UnlistenFn> {
  return listen<ScanStatus>('scan-progress', (event) => cb(event.payload));
}

export function onInstructionScanProgress(
  cb: (status: InstructionScanProgress) => void,
): Promise<UnlistenFn> {
  return listen<InstructionScanProgress>('instruction-scan-progress', (event) => cb(event.payload));
}

/** Fresh inventory from a background rescan behind a stale-while-revalidate read. */
export function onInstructionInventoryUpdated(
  cb: (inventory: InstructionInventory) => void,
): Promise<UnlistenFn> {
  return listen<InstructionInventory>('instruction-inventory-updated', (event) => cb(event.payload));
}

/** Terminal failure of a background rescan (never emitted for cancellations). */
export function onInstructionInventoryError(cb: (error: string) => void): Promise<UnlistenFn> {
  return listen<string>('instruction-inventory-error', (event) => cb(event.payload));
}

export function onRatesUpdated(cb: (rates: RateCard) => void): Promise<UnlistenFn> {
  return listen<RateCard>('rates-updated', (event) => cb(event.payload));
}

export function onConfigUpdated(cb: (config: Config) => void): Promise<UnlistenFn> {
  return listen<Config>('config-updated', (event) => cb(event.payload));
}

export function onConfigEvent(cb: (event: ExternalEvent) => void): Promise<UnlistenFn> {
  return listen<ExternalEvent>('config-event', (event) => cb(event.payload));
}
