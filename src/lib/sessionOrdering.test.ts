import { describe, expect, it } from 'vitest';
import {
  costIsUnmeasured,
  disambiguateSiblingNames,
  orderSessionsForDisplay,
  type OrderableSession,
} from './sessionProjection';

interface Row extends OrderableSession {
  parent: string | null;
  cost: number;
}

const DAY = 86_400_000;

function row(id: string, startedMs: number, cost: number, parent: string | null = null): Row {
  return { storage_id: id, startedMs, cost, parent };
}

const parentOf = (session: Row) => session.parent;
/** Descending cost — the sort the grid applies before ordering. */
const byCost = (rows: Row[]) => [...rows].sort((a, b) => b.cost - a.cost);

describe('orderSessionsForDisplay', () => {
  // The long-lived-parent case: a cheap parent opened weeks ago with expensive
  // short subagent runs beneath it.
  const parent = row('parent', 0, 1);
  const cheapChild = row('child-cheap', 10 * DAY, 2, 'parent');
  const richChild = row('child-rich', 20 * DAY, 500, 'parent');
  const otherRoot = row('other', 5 * DAY, 100);
  const corpus = [parent, cheapChild, richChild, otherRoot];

  it('keeps children under their parent in tree mode even when they outrank it', () => {
    const { list } = orderSessionsForDisplay(byCost(corpus), { parentOf });

    expect(list.map((r) => r.storage_id)).toEqual(['other', 'parent', 'child-rich', 'child-cheap']);
  });

  it('ranks subagents against every thread in flat mode', () => {
    const { list } = orderSessionsForDisplay(byCost(corpus), { parentOf, flat: true });

    // The expensive subagent now outranks the unrelated root and its own parent.
    expect(list.map((r) => r.storage_id)).toEqual(['child-rich', 'other', 'child-cheap', 'parent']);
  });

  it('anchors each row to its own start time in flat mode, not the parent day', () => {
    const tree = orderSessionsForDisplay(byCost(corpus), { parentOf });
    const flat = orderSessionsForDisplay(byCost(corpus), { parentOf, flat: true });

    // Tree: the subagent inherits the parent's day group, hiding when it ran.
    expect(tree.anchorMs.get('child-rich')).toBe(parent.startedMs);
    expect(flat.anchorMs.get('child-rich')).toBe(richChild.startedMs);
  });

  it('hides children of a collapsed parent, and ignores collapse when flat', () => {
    const collapsed = new Set(['parent']);

    const tree = orderSessionsForDisplay(byCost(corpus), { parentOf, collapsed });
    expect(tree.list.map((r) => r.storage_id)).toEqual(['other', 'parent']);

    const flat = orderSessionsForDisplay(byCost(corpus), { parentOf, collapsed, flat: true });
    expect(flat.list).toHaveLength(4);
  });

  it('promotes an orphaned child to a root rather than dropping it', () => {
    // The parent is filtered out of view; its child must still appear.
    const { list } = orderSessionsForDisplay(byCost([richChild, otherRoot]), { parentOf });

    expect(list.map((r) => r.storage_id)).toEqual(['child-rich', 'other']);
  });

  it('nests grandchildren beneath their own parent', () => {
    const grandchild = row('grandchild', 21 * DAY, 900, 'child-rich');
    const { list } = orderSessionsForDisplay(byCost([...corpus, grandchild]), { parentOf });

    expect(list.map((r) => r.storage_id)).toEqual([
      'other',
      'parent',
      'child-rich',
      'grandchild',
      'child-cheap',
    ]);
  });

  it('terminates on a cyclic parent chain instead of recursing forever', () => {
    // Corrupt lineage metadata: two rows claiming each other as parent.
    const a = { storage_id: 'a', startedMs: 0, cost: 1, parent: 'b' };
    const b = { storage_id: 'b', startedMs: 0, cost: 2, parent: 'a' };

    const { list } = orderSessionsForDisplay([b, a], { parentOf });

    expect(list.map((r) => r.storage_id).sort()).toEqual(['a', 'b']);
  });
});

describe('disambiguateSiblingNames', () => {
  const TEMPLATE =
    'You are a read-only telemetry analyst. Analyze the local Codex diagnostics database at ';

  it('reveals the divergent span of templated fan-out names', () => {
    const out = disambiguateSiblingNames([
      `${TEMPLATE}C:/data/alpha and report retries`,
      `${TEMPLATE}C:/data/beta and report retries`,
    ]);

    expect(out).toEqual(['…C:/data/alpha and report retries', '…C:/data/beta and report retries']);
  });

  it('leaves a single name alone', () => {
    expect(disambiguateSiblingNames([`${TEMPLATE}x`])).toEqual([`${TEMPLATE}x`]);
  });

  it('keeps a short shared prefix, which is still informative', () => {
    const names = ['Fix login bug', 'Fix logout bug'];
    expect(disambiguateSiblingNames(names)).toEqual(names);
  });

  it('leaves wholly identical names alone so the ordinal disambiguates instead', () => {
    const names = [`${TEMPLATE}same`, `${TEMPLATE}same`];
    expect(disambiguateSiblingNames(names)).toEqual(names);
  });

  it('snaps the cut to a word boundary rather than mid-word', () => {
    const out = disambiguateSiblingNames([
      'Analyze the local diagnostics database alpha',
      'Analyze the local diagnostics database beta',
    ]);

    // Cut lands before "alpha"/"beta", not inside a shared word.
    expect(out).toEqual(['…alpha', '…beta']);
  });

  it('cuts both at the same point when one name extends another', () => {
    const out = disambiguateSiblingNames([
      `${TEMPLATE}C:/data`,
      `${TEMPLATE}C:/data and report`,
    ]);

    // A shared cut keeps the pair comparable; cutting each at its own
    // divergence would strip the shorter one to nothing.
    expect(out).toEqual(['…C:/data', '…C:/data and report']);
  });

  it('never returns an empty label', () => {
    // No space in the shared prefix, so the cut lands at its full length and
    // the name equal to the prefix would otherwise render as a bare ellipsis.
    const out = disambiguateSiblingNames(['abcdefghijklmnopqrstuvwxyz', 'abcdefghijklmnopqrstuvwxyz1']);

    expect(out[0]).toBe('abcdefghijklmnopqrstuvwxyz');
    expect(out.every((name) => name.replace('…', '').length > 0)).toBe(true);
  });
});

describe('costIsUnmeasured', () => {
  it('flags a zero total that comes from having no published rate', () => {
    expect(costIsUnmeasured(['gpt-5.3-codex-spark'], 0)).toBe(true);
  });

  it('leaves a genuine zero alone when every model is priced', () => {
    expect(costIsUnmeasured([], 0)).toBe(false);
    expect(costIsUnmeasured(undefined, 0)).toBe(false);
  });

  it('does not claim a partially priced session is unmeasured', () => {
    // Some usage priced, some not: the total is a floor, not an absence, and
    // the existing unpriced marker already communicates that.
    expect(costIsUnmeasured(['gpt-5.3-codex-spark'], 12.5)).toBe(false);
  });
});
