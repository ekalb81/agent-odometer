import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SessionProjectEditor from './SessionProjectEditor.svelte';
import { projectStore } from '../lib/stores/projects.svelte';
import type { ProjectInfo } from '../lib/types';

const mocks = vi.hoisted(() => ({
  resolveProjects: vi.fn(), reassignSessionProject: vi.fn(), clearSessionProjectOverride: vi.fn(),
}));
vi.mock('../lib/ipc', () => mocks);
const session = { storage_id: 'codex:thread:shared', project_key: 'repo:original', project_label: 'Original' };
const original: ProjectInfo = { project_key: 'repo:original', label: 'Original', provenance: 'repository_root', member_keys: ['repo:original'], session_count: 2 };
const target: ProjectInfo = { project_key: 'repo:target', label: 'Target', provenance: 'repository_root', member_keys: ['repo:target'], session_count: 1 };
const assigned = { ...target, overridden_session_keys: [session.storage_id] };
beforeEach(async () => {
  vi.resetAllMocks();
  mocks.resolveProjects.mockResolvedValue([original, target]);
  mocks.reassignSessionProject.mockResolvedValue(target.project_key);
  mocks.clearSessionProjectOverride.mockResolvedValue(undefined);
  await projectStore.refresh();
});
async function openEditor(value = session) {
  const view = render(SessionProjectEditor, { session: value });
  await userEvent.click(screen.getByRole('button', { name: 'Change project' }));
  return view;
}

describe('session project management', () => {
  it('refreshes destinations when opening after a previously loaded partial list', async () => {
    mocks.resolveProjects.mockResolvedValue([original]);
    await projectStore.refresh();
    render(SessionProjectEditor, { session });
    mocks.resolveProjects.mockResolvedValue([original, target]);
    await userEvent.click(screen.getByRole('button', { name: 'Change project' }));
    expect(await screen.findByRole('option', { name: 'Target' })).toBeInTheDocument();
  });

  it('moves exactly the selected durable session and updates the shared lookup', async () => {
    await openEditor();
    expect(screen.getByRole('button', { name: 'Move session' })).toBeDisabled();
    await userEvent.selectOptions(screen.getByLabelText('Destination project'), target.project_key);
    mocks.resolveProjects.mockResolvedValue([original, assigned]);
    await userEvent.click(screen.getByRole('button', { name: 'Move session' }));
    await waitFor(() => expect(mocks.reassignSessionProject).toHaveBeenCalledWith(session.storage_id, target.project_key));
    await screen.findByRole('button', { name: 'Change project' });
    expect(projectStore.forSession(session)?.label).toBe('Target');
    expect(projectStore.forSession({ ...session, storage_id: 'claude_code:session:shared' })?.label).toBe('Original');
  });

  it('creates a standalone project even when no working directory or other project exists', async () => {
    mocks.resolveProjects.mockResolvedValue([]);
    await projectStore.refresh();
    await openEditor({ ...session, project_key: null, project_label: null } as unknown as typeof session);
    expect(screen.getByText('No project assigned')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Make standalone project' }));
    await waitFor(() => expect(mocks.reassignSessionProject).toHaveBeenCalledWith(session.storage_id, null));
  });

  it('offers undo for a persisted assignment after reopening and restores detected grouping', async () => {
    mocks.resolveProjects.mockResolvedValue([original, assigned]);
    await projectStore.refresh();
    await openEditor();
    mocks.resolveProjects.mockResolvedValue([original, target]);
    await userEvent.click(screen.getByRole('button', { name: 'Restore detected project' }));
    await waitFor(() => expect(mocks.clearSessionProjectOverride).toHaveBeenCalledWith(session.storage_id));
    await screen.findByRole('button', { name: 'Change project' });
    expect(projectStore.forSession(session)?.label).toBe('Original');
  });

  it('cancels without saving and does not offer undo for a detected project', async () => {
    await openEditor();
    expect(screen.queryByRole('button', { name: 'Restore detected project' })).not.toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText('Destination project'), target.project_key);
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(mocks.reassignSessionProject).not.toHaveBeenCalled();
  });

  it('surfaces save errors and keeps the previous assignment', async () => {
    mocks.reassignSessionProject.mockRejectedValue('history unavailable');
    await openEditor();
    await userEvent.click(screen.getByRole('button', { name: 'Make standalone project' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('history unavailable');
    expect(projectStore.forSession(session)?.label).toBe('Original');
  });

  it('retries a failed refresh without repeating the already saved edit', async () => {
    await openEditor();
    mocks.resolveProjects.mockRejectedValueOnce('temporary read failure');
    await userEvent.click(screen.getByRole('button', { name: 'Make standalone project' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Project saved');
    mocks.resolveProjects.mockResolvedValue([assigned]);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
    expect(mocks.reassignSessionProject).toHaveBeenCalledTimes(1);
  });

  it('finishes an old save without changing the newly selected session', async () => {
    let finish!: (key: string) => void;
    mocks.reassignSessionProject.mockReturnValue(new Promise<string>((resolve) => { finish = resolve; }));
    const view = await openEditor();
    await userEvent.click(screen.getByRole('button', { name: 'Make standalone project' }));
    expect(screen.getByRole('button', { name: 'Make standalone project' })).toBeDisabled();
    view.unmount();
    render(SessionProjectEditor, { session: { ...session, storage_id: 'codex:thread:other' } });
    mocks.resolveProjects.mockResolvedValue([original, assigned]);
    finish(target.project_key);
    await waitFor(() => expect(projectStore.hasOverride(session.storage_id)).toBe(true));
    expect(screen.getByText('Original')).toBeInTheDocument();
    expect(mocks.reassignSessionProject).toHaveBeenCalledTimes(1);
  });
});
