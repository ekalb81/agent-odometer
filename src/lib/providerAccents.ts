/** Id-keyed accent lookup for the two builtin providers. Descriptors carry no
 * color information, so the tab fill / CSS accent-scoping class stays a
 * small static map here rather than living on the wire type. Any provider id
 * outside the map (a future third provider) gets the same neutral fill the
 * non-provider views ('all', 'instructions', 'settings') already use today,
 * and no accent-scoping class of its own. */
export interface ProviderAccent {
  /** Tailwind fill class for the segmented-control tab button. */
  tabFill: string;
  /** CSS class that scopes the `--accent*` custom properties (app.css); null
   * when the id has no dedicated accent — callers fall back to whatever
   * accent class already wraps the page. */
  accentClass: 'accent-codex' | 'accent-claude' | null;
}

const NEUTRAL_ACCENT: ProviderAccent = { tabFill: 'bg-ink text-app!', accentClass: null };

const PROVIDER_ACCENTS: Record<string, ProviderAccent> = {
  codex: { tabFill: 'bg-[#2b58c9]', accentClass: 'accent-codex' },
  claude_code: { tabFill: 'bg-[#e8935a]', accentClass: 'accent-claude' },
};

export function providerAccent(id: string): ProviderAccent {
  return PROVIDER_ACCENTS[id] ?? NEUTRAL_ACCENT;
}
