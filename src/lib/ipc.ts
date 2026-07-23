// Thin typed wrappers around @tauri-apps/api invoke + event.listen.
// All IPC between the Svelte frontend and Rust backend goes through this module.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Session, SessionSummary, RangeTotals, ScanStatus, Config, RateCard } from './types';

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
): Promise<Record<string, RangeTotals>[]> {
  return invoke<Record<string, RangeTotals>[]>('sessions_in_ranges', { ranges });
}

/** Current bulk-scan progress (call once on mount, then follow events). */
export function getScanStatus(): Promise<ScanStatus> {
  return invoke<ScanStatus>('get_scan_status');
}

/** Windows only: opens the UAC flow to exclude session folders from Defender scanning. */
export function addDefenderExclusions(): Promise<void> {
  return invoke<void>('add_defender_exclusions');
}

export function getConfig(): Promise<Config> {
  return invoke<Config>('get_config');
}

export function setConfig(config: Config): Promise<void> {
  return invoke<void>('set_config', { config });
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

export function onRatesUpdated(cb: (rates: RateCard) => void): Promise<UnlistenFn> {
  return listen<RateCard>('rates-updated', (event) => cb(event.payload));
}

export function onConfigUpdated(cb: (config: Config) => void): Promise<UnlistenFn> {
  return listen<Config>('config-updated', (event) => cb(event.payload));
}
