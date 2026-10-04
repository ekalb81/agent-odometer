import type { Harness, RateCard } from './types';

/** Display label only. All cost calculation and model resolution live in Rust. */
export function harnessCurrency(rates: Pick<RateCard, 'currency' | 'currencies'>, harness: Harness): string {
  return rates.currencies?.[harness] ?? rates.currency;
}

const ISO_CURRENCY = /^[A-Z]{3}$/;

/** Format a backend-priced amount without changing its currency or value. */
export function formatCredits(amount: number, currency: string): string {
  // Preserve sub-cent amounts instead of rounding them to an apparent zero.
  const subCent = amount !== 0 && Math.abs(amount) < 0.005;
  if (ISO_CURRENCY.test(currency)) {
    return new Intl.NumberFormat('en-US', {
      style: 'currency',
      currency,
      minimumFractionDigits: 2,
      maximumFractionDigits: subCent ? 4 : 2,
    }).format(amount);
  }
  const num = new Intl.NumberFormat('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: subCent ? 4 : 2,
  }).format(amount);
  return `${num} ${currency === 'credits' ? 'legacy credits' : currency}`;
}
/** Supported offline FX money units; entitlement labels are excluded. */
export const FX_CURRENCIES = ['USD', 'EUR', 'GBP', 'CAD', 'AUD', 'JPY', 'CHF', 'NZD', 'CNY', 'INR', 'BRL', 'MXN', 'SGD', 'HKD', 'KRW', 'SEK', 'NOK', 'DKK', 'PLN', 'CZK', 'ZAR'];
