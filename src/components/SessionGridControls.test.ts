import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';
import { sessionGridStore } from '../lib/stores/sessionGrid.svelte';
import { sessionDetailPaneStore } from '../lib/stores/sessionDetailPane.svelte';
import SessionGridControls from './SessionGridControls.svelte';

describe('SessionGridControls', () => {
  beforeEach(() => {
    localStorage.clear();
    sessionGridStore.reset();
    sessionDetailPaneStore.setOpen(false);
  });

  it('persists user-facing visibility and grouping changes and can reset them', async () => {
    const user = userEvent.setup();
    render(SessionGridControls);

    await user.click(screen.getByText('Columns'));
    await user.click(screen.getByRole('checkbox', { name: 'Cost' }));
    await user.click(screen.getByRole('checkbox', { name: 'Cached' }));
    await user.click(screen.getByRole('checkbox', { name: 'Group by repository' }));
    await user.click(screen.getByRole('checkbox', { name: 'Color by model provider' }));

    expect(sessionGridStore.columnIds).not.toContain('cost');
    expect(sessionGridStore.columnIds).toContain('cached');
    expect(screen.getByRole('checkbox', { name: 'Cached' })).toBeChecked();
    expect(sessionGridStore.groupByRepository).toBe(true);
    expect(sessionGridStore.colorByModelProvider).toBe(true);
    expect(localStorage.getItem('sessionGridPreferences.v1')).toContain('groupByRepository');

    await user.click(screen.getByRole('button', { name: 'Reset grid columns to defaults' }));
    expect(screen.getByRole('checkbox', { name: 'Cost' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Cached' })).not.toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Group by repository' })).not.toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Color by model provider' })).not.toBeChecked();
  });

  it('shows the current persisted column order after a move', async () => {
    const user = userEvent.setup();
    const { container } = render(SessionGridControls);
    await user.click(screen.getByText('Columns'));

    await user.click(screen.getByRole('button', { name: 'Move Model left' }));

    const order = [...container.querySelectorAll<HTMLElement>('[data-column-id]')]
      .map((element) => element.dataset.columnId);
    expect(order.slice(0, 4)).toEqual(['name', 'cost', 'model', 'total']);
  });

  describe('detail pane toggle', () => {
    it('does not open an empty pane even when the open preference was saved', () => {
      sessionDetailPaneStore.setOpen(true);
      render(SessionGridControls, { props: { isWide: true, hasSelection: false } });
      expect(screen.getByRole('button', { name: 'Show details' })).toBeDisabled();
      expect(screen.getByRole('button', { name: 'Show details' })).toHaveAttribute('aria-expanded', 'false');
    });

    it('clamps and persists finite pane widths without losing the last good width', () => {
      sessionDetailPaneStore.setWidth(20);
      expect(sessionDetailPaneStore.width).toBe(410);
      sessionDetailPaneStore.setWidth(900);
      expect(sessionDetailPaneStore.width).toBe(800);
      expect(localStorage.getItem('sessionDetailPaneWidth.v1')).toBe('800');
      sessionDetailPaneStore.setWidth(NaN);
      expect(sessionDetailPaneStore.width).toBe(800);
      sessionDetailPaneStore.setWidth(560);
    });
    it('is hidden in narrow layouts, where the collapse has nothing to control', () => {
      render(SessionGridControls, { props: { isWide: false } });
      expect(screen.queryByRole('button', { name: /details/i })).toBeNull();
    });

    it('starts closed, opens on click, and persists the choice', async () => {
      const user = userEvent.setup();
      render(SessionGridControls, { props: { isWide: true, hasSelection: true } });

      const toggle = screen.getByRole('button', { name: 'Show details' });
      expect(toggle).toHaveAttribute('aria-expanded', 'false');
      expect(toggle).toHaveAttribute('aria-controls', 'session-detail-pane');

      await user.click(toggle);

      expect(sessionDetailPaneStore.open).toBe(true);
      expect(screen.getByRole('button', { name: 'Hide details' })).toHaveAttribute('aria-expanded', 'true');
      expect(localStorage.getItem('sessionDetailPaneOpen.v1')).toBe('true');
    });

    it('closes again on a second click without touching grid preferences', async () => {
      const user = userEvent.setup();
      render(SessionGridControls, { props: { isWide: true, hasSelection: true } });

      await user.click(screen.getByRole('button', { name: 'Show details' }));
      await user.click(screen.getByRole('button', { name: 'Hide details' }));

      expect(sessionDetailPaneStore.open).toBe(false);
      expect(screen.getByRole('button', { name: 'Show details' })).toHaveAttribute('aria-expanded', 'false');
      expect(sessionGridStore.groupByRepository).toBe(false);
    });
  });
});
