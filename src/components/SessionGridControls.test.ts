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
    await user.click(screen.getByRole('checkbox', { name: 'Cached' }));
    await user.click(screen.getByRole('checkbox', { name: 'Group by repository' }));
    await user.click(screen.getByRole('checkbox', { name: 'Color by model provider' }));

    expect(sessionGridStore.columnIds).not.toContain('cached');
    expect(sessionGridStore.groupByRepository).toBe(true);
    expect(sessionGridStore.colorByModelProvider).toBe(true);
    expect(localStorage.getItem('sessionGridPreferences.v1')).toContain('groupByRepository');

    await user.click(screen.getByRole('button', { name: 'Reset grid columns to defaults' }));
    expect(screen.getByRole('checkbox', { name: 'Cached' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Group by repository' })).not.toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Color by model provider' })).not.toBeChecked();
  });

  it('shows the current persisted column order after a move', async () => {
    const user = userEvent.setup();
    const { container } = render(SessionGridControls);
    await user.click(screen.getByText('Columns'));

    await user.click(screen.getByRole('button', { name: 'Move Repository left' }));

    const order = [...container.querySelectorAll<HTMLElement>('[data-column-id]')]
      .map((element) => element.dataset.columnId);
    expect(order.slice(0, 4)).toEqual(['name', 'repository', 'started', 'model']);
  });

  describe('detail pane toggle', () => {
    it('is hidden in narrow layouts, where the collapse has nothing to control', () => {
      render(SessionGridControls, { props: { isWide: false } });
      expect(screen.queryByRole('button', { name: /details/i })).toBeNull();
    });

    it('starts closed, opens on click, and persists the choice', async () => {
      const user = userEvent.setup();
      render(SessionGridControls, { props: { isWide: true } });

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
      render(SessionGridControls, { props: { isWide: true } });

      await user.click(screen.getByRole('button', { name: 'Show details' }));
      await user.click(screen.getByRole('button', { name: 'Hide details' }));

      expect(sessionDetailPaneStore.open).toBe(false);
      expect(screen.getByRole('button', { name: 'Show details' })).toHaveAttribute('aria-expanded', 'false');
      expect(sessionGridStore.groupByRepository).toBe(false);
    });
  });
});
