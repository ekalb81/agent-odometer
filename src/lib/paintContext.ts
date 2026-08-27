/**
 * UI state that a paint measurement needs in order to be comparable across
 * runs (issue #184).
 *
 * `frontend.session_batch_paint` measures the paint that follows a batch of
 * session mutations, but how much that paint costs depends almost entirely
 * on whether a sessions list is on screen to re-render: field recordings
 * split into two non-overlapping regimes at roughly 110-140 ms with the list
 * open against 5-6 ms with it closed. Runs were previously classified by
 * correlating `session_batch_paint` against nearby `session_list_paint`
 * events within a +/-2 s window — inference over a coincidence of timing,
 * which made an apparent 25x cross-version regression an artifact of which
 * tab happened to be open during each recording.
 *
 * This module lets the component that owns that state publish it, so the
 * paint event carries it as a recorded field instead.
 *
 * Privacy: row counts and a view identifier only, matching the rest of the
 * performance surface — never paths, prompts, project names, or session
 * content. It also adds no IPC: these values ride along on an event that is
 * already emitted.
 */

/**
 * Identity of the `SessionsView` instance whose rows are currently
 * published. Several instances stay mounted at once (one per provider tab,
 * plus "all") so filters and sort survive tab switches, and exactly one is
 * `active`. Tracking which one published lets a deactivating instance clear
 * its own value without clobbering the newly activated one, whose effect may
 * have already run — Svelte does not order effects across components.
 */
let publisher: string | null = null;
let rows: number | null = null;

/** Rows the active session list currently has in the DOM. */
export function publishRenderedSessionRows(view: string, count: number): void {
  publisher = view;
  rows = count;
}

/**
 * Withdraws `view`'s published count, if it is still the current publisher.
 * Called when a list is deactivated or destroyed, so a paint measured while
 * no list is on screen reports 0 rather than a stale count from the tab the
 * user just left.
 */
export function clearRenderedSessionRows(view: string): void {
  if (publisher !== view) return;
  publisher = null;
  rows = null;
}

/**
 * Rows rendered by the active session list, or 0 when no list is on screen.
 *
 * Deliberately 0 rather than null for the closed case: "no list rendering"
 * is a genuine zero rows of list work, not missing data. An event recorded
 * before this existed has no `rows_rendered` key at all, which is how an
 * older export stays distinguishable from a real zero.
 */
export function renderedSessionRows(): number {
  return rows ?? 0;
}

/** Test seam: forget any published rows. */
export function resetPaintContextForTests(): void {
  publisher = null;
  rows = null;
}
