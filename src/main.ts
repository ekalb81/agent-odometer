import './app.css';
import App from './App.svelte';
import { mount } from 'svelte';

async function start() {
  // In a plain browser (`npm run dev` without the Tauri shell), install a
  // fixture IPC mock so the UI is workable. Dead code in production builds.
  if (import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)) {
    await import('./dev-mock');
  }
  mount(App, { target: document.getElementById('app')! });
}

void start();
