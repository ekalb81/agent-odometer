import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ProjectManagement from './ProjectManagement.svelte';
import type { ProjectInfo } from '../lib/types';

// Issue #41's residual is that the alias/merge backend existed with no UI
// calling it. So the assertions that matter here are the ones about what
// actually reaches the backend: the right command, with the right
// arguments, and only when the user asked for it. A regression that
// rendered the table but wired a button to nothing would look completely
// fine on screen.
const { resolveProjects, setProjectAlias, mergeProjects, unmergeProject } = vi.hoisted(() => ({
  resolveProjects: vi.fn(),
  setProjectAlias: vi.fn().mockResolvedValue(undefined),
  mergeProjects: vi.fn().mockResolvedValue(undefined),
  unmergeProject: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../lib/ipc', () => ({
  resolveProjects,
  setProjectAlias,
  mergeProjects,
  unmergeProject,
}));

function project(overrides: Partial<ProjectInfo> & Pick<ProjectInfo, 'project_key'>): ProjectInfo {
  return {
    label: overrides.project_key,
    provenance: 'repository_root',
    member_keys: [overrides.project_key],
    session_count: 1,
    ...overrides,
  };
}

const ODOMETER = project({ project_key: 'repo:odometer', label: 'agent-odometer', session_count: 42 });
const SCRATCH = project({ project_key: 'path:scratch', label: 'scratch', provenance: 'fallback_path_identity' });

async function renderWith(...projects: ProjectInfo[]) {
  resolveProjects.mockResolvedValue(projects);
  render(ProjectManagement);
  // The store loads asynchronously; wait for the table rather than a timer.
  if (projects.length > 0) {
    await screen.findByText(projects[0].label);
  }
}

/** The table row containing `label`, so an action click cannot hit another project's button. */
function rowFor(label: string): HTMLElement {
  const cell = screen.getByText(label);
  const row = cell.closest('tr');
  if (!row) throw new Error(`no row for ${label}`);
  return row;
}

describe('ProjectManagement (issue #41)', () => {
  beforeEach(() => {
    resolveProjects.mockReset();
    setProjectAlias.mockClear().mockResolvedValue(undefined);
    mergeProjects.mockClear().mockResolvedValue(undefined);
    unmergeProject.mockClear().mockResolvedValue(undefined);
  });

  it('lists each project with how its identity was resolved', async () => {
    await renderWith(ODOMETER, SCRATCH);

    expect(screen.getByText('agent-odometer')).toBeInTheDocument();
    expect(screen.getByText('42')).toBeInTheDocument();
    // Provenance is shown rather than hidden: "Path identity" is the case a
    // user most needs to recognise, because it is the one that fragments
    // when a directory moves.
    expect(screen.getByText('Path identity')).toBeInTheDocument();
    expect(screen.getByText('Repository root')).toBeInTheDocument();
  });

  it('saves a renamed project as a local alias', async () => {
    await renderWith(ODOMETER, SCRATCH);

    await userEvent.click(within(rowFor('agent-odometer')).getByRole('button', { name: 'Rename' }));
    const input = screen.getByLabelText('Local label');
    await userEvent.clear(input);
    await userEvent.type(input, 'Odometer');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(setProjectAlias).toHaveBeenCalledWith('repo:odometer', 'Odometer'));
  });

  it('clears the alias rather than saving an empty name', async () => {
    // An empty box means "no local label", not a project literally named "".
    await renderWith(ODOMETER);

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
    await userEvent.clear(screen.getByLabelText('Local label'));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(setProjectAlias).toHaveBeenCalledWith('repo:odometer', null));
  });

  it('does not call the backend when the name is unchanged', async () => {
    await renderWith(ODOMETER);

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect(setProjectAlias).not.toHaveBeenCalled();
  });

  it('does not call the backend when a rename is cancelled', async () => {
    await renderWith(ODOMETER);

    await userEvent.click(screen.getByRole('button', { name: 'Rename' }));
    await userEvent.type(screen.getByLabelText('Local label'), 'Discarded');
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(setProjectAlias).not.toHaveBeenCalled();
  });

  it('merges a project into the one the user picked', async () => {
    await renderWith(ODOMETER, SCRATCH);

    await userEvent.click(within(rowFor('scratch')).getByRole('button', { name: /Merge/ }));
    await userEvent.selectOptions(screen.getByLabelText(/Show/), 'repo:odometer');
    await userEvent.click(screen.getByRole('button', { name: 'Merge' }));

    // Source first, canonical second — reversing these would silently fold
    // the wrong project away.
    await waitFor(() => expect(mergeProjects).toHaveBeenCalledWith('path:scratch', 'repo:odometer'));
  });

  it('does not merge until a target is chosen', async () => {
    await renderWith(ODOMETER, SCRATCH);

    await userEvent.click(within(rowFor('scratch')).getByRole('button', { name: /Merge/ }));
    expect(screen.getByRole('button', { name: 'Merge' })).toBeDisabled();
    expect(mergeProjects).not.toHaveBeenCalled();
  });

  it('offers undo only for a project that is actually merged, and reverses it', async () => {
    const combined = project({
      project_key: 'repo:odometer',
      label: 'agent-odometer',
      member_keys: ['repo:odometer', 'path:scratch'],
      session_count: 43,
    });
    await renderWith(combined);

    const undo = screen.getByRole('button', { name: 'Undo merge' });
    await userEvent.click(undo);

    await waitFor(() => expect(unmergeProject).toHaveBeenCalledWith('repo:odometer'));
  });

  it('hides undo for an unmerged project', async () => {
    await renderWith(ODOMETER, SCRATCH);

    expect(screen.queryByRole('button', { name: 'Undo merge' })).not.toBeInTheDocument();
  });

  it('surfaces a rejected merge instead of failing silently', async () => {
    // The backend rejects a self-merge and any merge that would create a
    // cycle. Swallowing that would leave the user looking at a table that
    // did not change, with no reason given.
    mergeProjects.mockRejectedValue('cannot merge a project into itself');
    await renderWith(ODOMETER, SCRATCH);

    await userEvent.click(within(rowFor('scratch')).getByRole('button', { name: /Merge/ }));
    await userEvent.selectOptions(screen.getByLabelText(/Show/), 'repo:odometer');
    await userEvent.click(screen.getByRole('button', { name: 'Merge' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('cannot merge a project into itself');
  });

  it('explains itself when there are no projects yet', async () => {
    resolveProjects.mockResolvedValue([]);
    render(ProjectManagement);

    expect(await screen.findByText(/No projects yet/)).toBeInTheDocument();
  });
});
