import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ActivitySummary from './ActivitySummary.svelte';

const mocks = vi.hoisted(() => ({ write: vi.fn() }));
vi.mock('../lib/ipc', () => ({ publishActivitySummaryExport: mocks.write }));
const props = {
  svg: '<svg xmlns="http://www.w3.org/2000/svg"><title>Exact snapshot</title></svg>', markdown: '# Exact Markdown\n', onclose: vi.fn(),
  request: { session_ids: ['codex:synthetic'], days: [{ from: '2026-01-01T00:00:00Z', to: '2026-01-01T23:59:59.999Z', tokens: 321, tool_calls: 4 }], coverage_complete: true },
};
beforeEach(() => {
  mocks.write.mockReset().mockResolvedValue(true);
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
});

describe('activity summary preview', () => {
  it('saves exactly the displayed SVG through the native picker and copies the shown Markdown', async () => {
    const user = userEvent.setup();
    const copy = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
    render(ActivitySummary, props);
    expect(decodeURIComponent(screen.getByAltText('Exact local SVG activity summary preview').getAttribute('src')!.split(',')[1])).toBe(props.svg);
    expect(screen.getByLabelText('Companion Markdown')).toHaveValue(props.markdown);
    await user.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(mocks.write).toHaveBeenCalledWith({ ...props.request, svg: props.svg }, 'odometer-activity.svg');
    expect(await screen.findByRole('status')).toHaveTextContent('Saved the exact SVG preview');
    await user.click(screen.getByRole('button', { name: 'Copy Markdown' }));
    expect(copy).toHaveBeenCalledWith(props.markdown);
  });
  it('reports cancellation, write failure, and clipboard failure without exposing destination data', async () => {
    const user = userEvent.setup();
    vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('secret clipboard data'));
    mocks.write.mockResolvedValueOnce(false).mockRejectedValueOnce('private/path');
    render(ActivitySummary, props);
    await user.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(screen.getByRole('status')).toHaveTextContent('Save canceled');
    await user.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(screen.getByRole('status')).toHaveTextContent('could not be saved');
    expect(screen.queryByText('private/path')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Copy Markdown' }));
    expect(screen.getByRole('status')).toHaveTextContent('Copy unavailable');
  });
  it('ignores a late native completion after the preview is closed', async () => {
    let finish!: (value: boolean) => void;
    mocks.write.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const view = render(ActivitySummary, props);
    await userEvent.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeDisabled();
    view.unmount(); finish(true);
    await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument());
  });
  it('reports a rejected post-picker accounting proof without publishing through an unchecked fallback', async () => {
    mocks.write.mockRejectedValue(new Error('accounting_identity_ambiguous: private archive location'));
    render(ActivitySummary, props);
    await userEvent.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(screen.getByRole('status')).toHaveTextContent(/ambiguous accounting identities/);
    expect(screen.queryByText(/private archive location/)).not.toBeInTheDocument();
    expect(mocks.write).toHaveBeenCalledTimes(1);
  });
});
