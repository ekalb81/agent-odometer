import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import SessionContextMenu from './SessionContextMenu.svelte';

function props(overrides: Record<string, unknown> = {}) {
  return {
    sessionName: 'Parent session',
    x: 12,
    y: 24,
    descendantCount: 2,
    includeDescendants: false,
    busy: false,
    error: null,
    onincludechange: vi.fn(),
    onexport: vi.fn(),
    onclose: vi.fn(),
    ...overrides,
  };
}

describe('SessionContextMenu', () => {
  it('selects descendants and either export format', async () => {
    const user = userEvent.setup();
    const menuProps = props();
    render(SessionContextMenu, { props: menuProps });

    expect(screen.getByRole('menu', { name: 'Export Parent session' })).toHaveFocus();
    await user.click(screen.getByRole('menuitemcheckbox', { name: 'Include 2 child sessions' }));
    await user.click(screen.getByRole('menuitem', { name: 'Export as JSON' }));
    await user.click(screen.getByRole('menuitem', { name: 'Export as CSV' }));

    expect(menuProps.onincludechange).toHaveBeenCalledWith(true);
    expect(menuProps.onexport).toHaveBeenNthCalledWith(1, 'json');
    expect(menuProps.onexport).toHaveBeenNthCalledWith(2, 'csv');
  });

  it('closes from Escape, right-click, or the backdrop', async () => {
    const user = userEvent.setup();
    const onclose = vi.fn();
    render(SessionContextMenu, { props: props({ onclose }) });

    await user.keyboard('{Escape}');
    expect(onclose).toHaveBeenCalledTimes(1);

    const backdrop = screen.getByTestId('session-context-backdrop');
    await user.pointer({ target: backdrop, keys: '[MouseRight]' });
    await user.click(backdrop);
    expect(onclose).toHaveBeenCalledTimes(3);
  });

  it('hides the child option and reports busy errors', () => {
    render(SessionContextMenu, {
      props: props({ descendantCount: 0, busy: true, error: 'Export failed' }),
    });

    expect(screen.queryByRole('menuitemcheckbox')).not.toBeInTheDocument();
    expect(screen.getByRole('menuitem', { name: 'Export as JSON' })).toBeDisabled();
    expect(screen.getByText('Preparing export…')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('Export failed');
  });
});
