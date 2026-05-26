import { writable } from 'svelte/store';
import type { Session } from '../types';

// Map of session id -> Session, populated by Phase 3.
export const sessions = writable<Map<string, Session>>(new Map());
