/// <reference types="node" />
// Node types are referenced for this file alone rather than added to
// `tsconfig.json`'s global `types`: `src/` is browser code, and giving every
// module ambient `process`/`fs` would let node-only APIs type-check their way
// into the app. Only this test regenerates a fixture on disk.
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { apiCostFromBuckets, creditsFromBuckets } from './credits';
import type { RateCard, TierBucket } from './types';

/**
 * Cross-engine pricing conformance, desktop half (issue #47).
 *
 * Odometer prices usage twice: here in `credits.ts` for the desktop, and in
 * `src-tauri/src/query.rs` for the CLI and MCP server. #47's DRY boundary
 * says one query service should serve every surface, and its first
 * acceptance criterion is that equivalent queries reconcile across them.
 * Nothing enforced that, so the two could drift silently — which is why
 * AGENTS.md carries a "keep both in sync" rule that relies on people
 * remembering.
 *
 * This file and `src-tauri/tests/pricing_conformance.rs` read the *same*
 * fixture and assert against the *same* `expected` block. Agreement with
 * that file is therefore agreement with each other, without either test
 * having to invoke the other language.
 *
 * The expectations are generated from this engine, deliberately: the
 * desktop is what users see today, so it defines current behaviour and the
 * Rust side is measured against it. That makes this test a regression guard
 * for the desktop and a conformance report for the CLI — not a claim that
 * this engine is correct.
 *
 * Regenerate after an intended pricing change:
 *
 *   UPDATE_PRICING_CONFORMANCE=1 npx vitest run src/lib/pricingConformance.test.ts
 *
 * Review the diff. A changed number here is a changed number in the app.
 */

// Resolved from the vitest root rather than `import.meta.url`: under this
// config the module URL is not a file: URL, so `fileURLToPath` throws.
const FIXTURE = resolve('tests/conformance/pricing-cases.json');

interface Case {
  name: string;
  harness: string;
  table: 'plan' | 'api';
  buckets: TierBucket[];
  expected?: CaseExpectation;
}

interface CaseExpectation {
  /** Null when the engine reports "not answerable", distinct from a zero cost. */
  total: number | null;
  by_model: { model: string; cost: number; basis: string; unpriced: boolean }[];
  missing_models: string[];
  unpriced_models: string[];
}

interface Fixture {
  rate_card: RateCard;
  cases: Case[];
}

const fixture: Fixture = JSON.parse(readFileSync(FIXTURE, 'utf8'));

/** Rounded to a fixed precision so a float's last bits cannot fail a cross-language comparison. */
function round(value: number): number {
  return Number(value.toFixed(9));
}

function evaluate(testCase: Case): CaseExpectation {
  const credits =
    testCase.table === 'api'
      ? apiCostFromBuckets(testCase.buckets, fixture.rate_card, testCase.harness as never)
      : creditsFromBuckets(testCase.buckets, fixture.rate_card, testCase.harness as never);

  // `apiCostFromBuckets` returns null when no API table is configured, which
  // callers use to hide the column rather than render a zero.
  if (credits === null) {
    return { total: null, by_model: [], missing_models: [], unpriced_models: [] };
  }

  return {
    total: round(credits.total),
    by_model: credits.byModel
      .map((entry) => ({
        model: entry.model,
        cost: round(entry.cost),
        basis: entry.basis,
        unpriced: entry.unpriced,
      }))
      .sort((a, b) => a.model.localeCompare(b.model)),
    missing_models: [...credits.missingModels].sort(),
    unpriced_models: [...credits.unpricedModels].sort(),
  };
}

describe('pricing conformance fixture (issue #47)', () => {
  if (process.env.UPDATE_PRICING_CONFORMANCE === '1') {
    it('regenerates expectations from the desktop engine', () => {
      const updated = {
        ...fixture,
        cases: fixture.cases.map((testCase) => ({ ...testCase, expected: evaluate(testCase) })),
      };
      writeFileSync(FIXTURE, `${JSON.stringify(updated, null, 2)}\n`, 'utf8');
      expect(updated.cases.every((testCase) => testCase.expected)).toBe(true);
    });
    return;
  }

  it('covers every pricing path the engines can take', () => {
    // A fixture that silently lost cases would still pass every assertion
    // below, so the shape of the corpus is pinned too.
    const bases = new Set(
      fixture.cases.flatMap((testCase) => (testCase.expected?.by_model ?? []).map((m) => m.basis)),
    );
    for (const required of ['direct', 'aliased', 'floating_alias', 'fallback', 'unavailable']) {
      expect(bases, `no case exercises the ${required} basis`).toContain(required);
    }
    expect(fixture.cases.length).toBeGreaterThanOrEqual(20);
  });

  for (const testCase of fixture.cases) {
    it(testCase.name, () => {
      expect(
        testCase.expected,
        'fixture has no expectation; run with UPDATE_PRICING_CONFORMANCE=1',
      ).toBeDefined();
      expect(evaluate(testCase)).toEqual(testCase.expected);
    });
  }
});
