import { writable } from 'svelte/store';
import type { RateCard } from '../types';

// Holds the active rate card, loaded on startup in Phase 3.
export const rates = writable<RateCard | null>(null);
