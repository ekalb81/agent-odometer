// Thin typed wrappers around @tauri-apps/api invoke + event.listen.
// All IPC between the Svelte frontend and Rust backend goes through this module.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Session, Config, RateCard } from './types';

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

export function listSessions(): Promise<Session[]> {
  return invoke<Session[]>('list_sessions');
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

export function setRates(rates: RateCard): Promise<void> {
  return invoke<void>('set_rates', { rates });
}

export function revealInFileManager(path: string): Promise<void> {
  return invoke<void>('reveal_in_file_manager', { path });
}

// ---------------------------------------------------------------------------
// Events  (Phase 3 will emit these from the watcher)
// ---------------------------------------------------------------------------

export function onSessionUpdated(cb: (session: Session) => void): Promise<UnlistenFn> {
  return listen<Session>('session-updated', (event) => cb(event.payload));
}

export function onSessionRemoved(cb: (sessionId: string) => void): Promise<UnlistenFn> {
  return listen<string>('session-removed', (event) => cb(event.payload));
}

export function onRatesUpdated(cb: (rates: RateCard) => void): Promise<UnlistenFn> {
  return listen<RateCard>('rates-updated', (event) => cb(event.payload));
}
