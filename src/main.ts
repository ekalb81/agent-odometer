import './app.css';
import App from './App.svelte';
import { mount } from 'svelte';

async function start() {
  // In a plain browser (`npm run dev` without the Tauri shell), install a
  // fixture IPC mock so the UI is workable. Visual CI uses the same fixture
  // layer against a Vite production build, but never inside the native shell.
  const browserFixtureMode = !('__TAURI_INTERNALS__' in window) && (
    import.meta.env.DEV || import.meta.env.VITE_VISUAL_TEST === '1'
  );
  if (browserFixtureMode) {
    await import('./dev-mock');
    // Expose the store so dev tooling can simulate live-update flushes.
    const { sessionsStore } = await import('./lib/stores/sessions.svelte');
    (window as unknown as Record<string, unknown>).__sessionsStore = sessionsStore;
  }
  mount(App, { target: document.getElementById('app')! });
}

void start();
