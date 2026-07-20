import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

type Phase = 'idle' | 'checking' | 'installing' | 'error';
/** Outcome of the most recent MANUAL check, for Settings feedback. */
type LastManualResult = 'none' | 'uptodate' | 'available' | 'failed';

/**
 * Shared auto-update state: the App banner and the Settings "check now"
 * button drive the same underlying update. Auto checks fail silently
 * (offline, dev builds); manual checks surface their outcome.
 */
function createUpdaterStore() {
  let available = $state<Update | null>(null);
  let phase = $state<Phase>('idle');
  let progress = $state(0);
  let total = $state(0);
  let lastManualResult = $state<LastManualResult>('none');
  let error = $state<string | null>(null);

  async function checkNow(manual = false): Promise<void> {
    if (phase === 'installing' || phase === 'checking') return;
    if (available && !manual) return; // banner already showing
    phase = 'checking';
    if (manual) lastManualResult = 'none';
    try {
      const update = await check();
      if (update) {
        available = update;
        if (manual) lastManualResult = 'available';
      } else if (manual) {
        lastManualResult = 'uptodate';
      }
      phase = 'idle';
    } catch (e) {
      phase = 'idle';
      if (manual) {
        lastManualResult = 'failed';
        error = String(e);
      } else {
        console.debug('update check skipped:', e);
      }
    }
  }

  async function install(): Promise<void> {
    if (!available || phase === 'installing') return;
    phase = 'installing';
    progress = 0;
    total = 0;
    error = null;
    try {
      await available.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          total = event.data.contentLength ?? 0;
        } else if (event.event === 'Progress') {
          progress += event.data.chunkLength;
        }
      });
      // Windows exits into the installer on its own; elsewhere relaunch now.
      await relaunch();
    } catch (e) {
      console.error('update install failed:', e);
      error = String(e);
      phase = 'error';
    }
  }

  return {
    get available() {
      return available;
    },
    get phase() {
      return phase;
    },
    get progress() {
      return progress;
    },
    get total() {
      return total;
    },
    get lastManualResult() {
      return lastManualResult;
    },
    get error() {
      return error;
    },
    checkNow,
    install,
  };
}

export const updaterStore = createUpdaterStore();
