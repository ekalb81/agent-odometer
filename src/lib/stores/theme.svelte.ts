export type ThemePreference = 'system' | 'dark' | 'light';

const STORAGE_KEY = 'themePreference';

function loadPreference(): ThemePreference {
  const v = localStorage.getItem(STORAGE_KEY);
  return v === 'dark' || v === 'light' || v === 'system' ? v : 'system';
}

/** Theme preference: follows the OS by default, manual override in Settings. */
function createThemeStore() {
  let preference = $state<ThemePreference>(loadPreference());
  const media = window.matchMedia('(prefers-color-scheme: dark)');
  let mediaDark = $state(media.matches);

  const resolved = $derived<'dark' | 'light'>(
    preference === 'system' ? (mediaDark ? 'dark' : 'light') : preference,
  );

  // data-theme drives every CSS token; applied imperatively so the store
  // works without a component context.
  function apply() {
    const t = preference === 'system' ? (media.matches ? 'dark' : 'light') : preference;
    document.documentElement.dataset.theme = t;
  }

  media.addEventListener('change', (e) => {
    mediaDark = e.matches;
    apply();
  });
  apply();

  return {
    get preference() {
      return preference;
    },
    get resolved() {
      return resolved;
    },
    set(next: ThemePreference) {
      preference = next;
      localStorage.setItem(STORAGE_KEY, next);
      apply();
    },
  };
}

export const themeStore = createThemeStore();
