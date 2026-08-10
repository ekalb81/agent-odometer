const STORAGE_KEY = 'sessionDetailPaneOpen.v1';

function loadPreference(): boolean {
  return localStorage.getItem(STORAGE_KEY) === 'true';
}

/**
 * Whether SessionsView's wide-layout persistent detail pane is expanded.
 * Defaults closed so an empty selection doesn't reserve 410px of the session
 * grid for a "Select a session to see its details" placeholder (#wide layout
 * only — the narrow layout's overlay drawer is unaffected and always keys
 * off the selection itself).
 *
 * Selecting a session opens the pane; collapsing it again is a pure UI
 * toggle that leaves the selection (and the fetched session) intact, unlike
 * deselecting. See SessionsView's `selectSession` and the wide-layout
 * `DetailPane`'s `onclose` wiring.
 */
function createSessionDetailPaneStore() {
  let open = $state(loadPreference());

  function setOpen(next: boolean) {
    open = next;
    localStorage.setItem(STORAGE_KEY, String(open));
  }

  function toggle() {
    setOpen(!open);
  }

  return {
    get open() {
      return open;
    },
    setOpen,
    toggle,
  };
}

export const sessionDetailPaneStore = createSessionDetailPaneStore();
