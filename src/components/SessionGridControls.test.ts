import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';
import { sessionGridStore } from '../lib/stores/sessionGrid.svelte';
import SessionGridControls from './SessionGridControls.svelte';

describe('SessionGridControls', () => {
  beforeEach(() => {
    localStorage.clear();
    sessionGridStore.reset();
  });

  it('persists user-facing visibility and grouping changes and can reset them', async () => {
    const user = userEvent.setup();
    render(SessionGridControls);

    await user.click(screen.getByText('Columns'));
    await user.click(screen.getByRole('checkbox', { name: 'Cached' }));
    await user.click(screen.getByRole('checkbox', { name: 'Group by repository' }));

    expect(sessionGridStore.columnIds).not.toContain('cached');
    expect(sessionGridStore.groupByRepository).toBe(true);
    expect(localStorage.getItem('sessionGridPreferences.v1')).toContain('groupByRepository');

    await user.click(screen.getByRole('button', { name: 'Reset grid columns to defaults' }));
    expect(screen.getByRole('checkbox', { name: 'Cached' })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: 'Group by repository' })).not.toBeChecked();
  });
});
