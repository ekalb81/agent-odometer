import { writable } from 'svelte/store';
import type { Config } from '../types';

// Holds the current app config, loaded on startup in Phase 3.
export const config = writable<Config>({
  session_roots: [],
  archive_roots: [],
  session_index_path: '',
  claude_session_roots: [],
});
