import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, expect, it } from 'vitest';
import ConvertedCost from './ConvertedCost.svelte';

afterEach(cleanup);

it('renders the backend restatement and its offline provenance with excluded usage visible', () => {
  render(ConvertedCost, { value: { from_currency: 'USD', target_currency: 'EUR', amount: 19.73, rate: .9, as_of: '2026-10-01T12:30:00Z', source: 'Synthetic reference' }, incomplete: true });
  expect(screen.getByText('€19.73')).toBeInTheDocument();
  expect(screen.getByText(/from USD; rate 0.9/)).toHaveTextContent('2026-10-01T12:30:00Z');
  expect(screen.getByText(/Synthetic reference/)).toHaveTextContent('User-supplied, not a live quote. Unpriced usage remains excluded.');
});

it('does not invent a converted amount when no backend restatement is available', () => {
  const { container } = render(ConvertedCost, { value: null });
  expect(container).toHaveTextContent('');
  expect(screen.queryByText(/Display restatement/)).not.toBeInTheDocument();
});
