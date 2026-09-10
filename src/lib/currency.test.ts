import { describe, expect, it } from 'vitest';
import { formatCredits, harnessCurrency } from './currency';

describe('currency presentation', () => {
  it('uses the provider currency and falls back to the card-wide unit', () => {
    const rates = { currency: 'credits', currencies: { codex: 'credits', claude_code: 'USD' } };
    expect(harnessCurrency(rates, 'codex')).toBe('credits');
    expect(harnessCurrency(rates, 'claude_code')).toBe('USD');
    expect(harnessCurrency(rates, 'gemini_cli')).toBe('credits');
    expect(harnessCurrency({ ...rates, currencies: {} }, 'claude_code')).toBe('credits');
  });

  it.each([
    [0, 'USD', '$0.00'],
    [1234.567, 'USD', '$1,234.57'],
    [12.5, 'EUR', '€12.50'],
    [1234.567, 'credits', '1,234.57 credits'],
    [0, 'credits', '0.00 credits'],
    [0.0012, 'USD', '$0.0012'],
    [0.0012, 'credits', '0.0012 credits'],
    [-0.0012, 'USD', '-$0.0012'],
    [0.005, 'USD', '$0.01'],
  ])('formats %s %s as %s without recalculating a price', (amount, currency, expected) => {
    expect(formatCredits(amount, currency)).toBe(expected);
  });
});
