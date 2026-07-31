import { addDefenderExclusions } from '../ipc';
import type { DefenderExclusionReceipt } from '../types';
import { config } from './config';

export type DefenderActionOrigin = 'banner' | 'settings';
export type DefenderActionPhase = 'idle' | 'pending' | 'success' | 'error';

type RequestReceipt = () => Promise<DefenderExclusionReceipt>;
type ApplyReceipt = (receipt: DefenderExclusionReceipt) => void;

function applyReceiptToConfig(receipt: DefenderExclusionReceipt): void {
  config.update((current) => ({ ...current, defender_exclusion_receipt: receipt }));
}

export function createDefenderActionStore(
  requestReceipt: RequestReceipt = addDefenderExclusions,
  applyReceipt: ApplyReceipt = applyReceiptToConfig,
) {
  let phase = $state<DefenderActionPhase>('idle');
  let origin = $state<DefenderActionOrigin | null>(null);
  let error = $state<string | null>(null);
  let inFlight: Promise<DefenderExclusionReceipt> | null = null;

  function request(nextOrigin: DefenderActionOrigin): Promise<DefenderExclusionReceipt> {
    if (inFlight) return inFlight;

    phase = 'pending';
    origin = nextOrigin;
    error = null;
    const pending = requestReceipt()
      .then((receipt) => {
        applyReceipt(receipt);
        phase = 'success';
        return receipt;
      })
      .catch((cause: unknown) => {
        phase = 'error';
        error = String(cause);
        throw cause;
      })
      .finally(() => {
        if (inFlight === pending) inFlight = null;
      });
    inFlight = pending;
    return pending;
  }

  function clearFeedback(): void {
    if (phase === 'pending') return;
    phase = 'idle';
    origin = null;
    error = null;
  }

  return {
    get phase() { return phase; },
    get origin() { return origin; },
    get error() { return error; },
    request,
    clearFeedback,
  };
}

export const defenderActionStore = createDefenderActionStore();
