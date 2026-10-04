import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SessionOrganizationEditor from './SessionOrganizationEditor.svelte';
import { organizationStore } from '../lib/stores/organization.svelte';
import type { SessionAnnotation } from '../lib/types';
const mocks = vi.hoisted(() => ({ getOrganizationSummaries:vi.fn(),getSessionAnnotation:vi.fn(),editSessionAnnotation:vi.fn(),listOrganizationTags:vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const annotation: SessionAnnotation = {summary:{identity:{session_key:'codex:thread:synthetic',fingerprint:'lineage',anchor:''},revision:2,pinned:false,has_note:false,tags:[]},note:''};
beforeEach(() => {
  vi.resetAllMocks(); organizationStore.invalidate('reset');
  mocks.getOrganizationSummaries.mockResolvedValue([annotation.summary]);
  mocks.getSessionAnnotation.mockResolvedValue(annotation);
});
async function open() {
  const view=render(SessionOrganizationEditor,{sessionKey:annotation.summary.identity.session_key});
  await userEvent.click(await screen.findByRole('button',{name:'Edit organization'}));return view;
}
describe('private session editing', () => {
  it('saves notes only through private IPC and exposes only metadata to the list store', async () => {
    await open();
    await userEvent.type(screen.getByLabelText('Private note'),'PRIVATE_SENTINEL_252');
    await userEvent.type(screen.getByLabelText('Tags (comma separated)'),'Review, Next');
    await userEvent.click(screen.getByLabelText('Pin this session'));
    mocks.editSessionAnnotation.mockImplementation(async edit => ({summary:{...annotation.summary,revision:3,pinned:edit.pinned,has_note:true,tags:edit.tags},note:edit.note}));
    await userEvent.click(screen.getByRole('button',{name:'Save organization'}));
    await waitFor(() => expect(mocks.editSessionAnnotation).toHaveBeenCalledWith({identity:annotation.summary.identity,revision:2,pinned:true,note:'PRIVATE_SENTINEL_252',tags:['Review','Next']}));
    await screen.findByRole('button',{name:'Edit organization'});
    expect(JSON.stringify(organizationStore.summaries)).not.toContain('PRIVATE_SENTINEL_252');
    expect(screen.queryByText('PRIVATE_SENTINEL_252')).not.toBeInTheDocument();
  });
  it('keeps unsaved text visible when a stale revision is refused', async () => {
    await open();
    await userEvent.type(screen.getByLabelText('Private note'),'Keep my unsaved note');
    mocks.editSessionAnnotation.mockRejectedValue(new Error('Organization changed; reload before saving'));
    await userEvent.click(screen.getByRole('button',{name:'Save organization'}));
    expect(await screen.findByRole('alert')).toHaveTextContent('Organization changed');
    expect(screen.getByLabelText('Private note')).toHaveValue('Keep my unsaved note');
    expect(mocks.editSessionAnnotation).toHaveBeenCalledTimes(1);
  });
  it('reports unavailable history explicitly and retries without fabricating empty annotations', async () => {
    mocks.getOrganizationSummaries.mockResolvedValue([]);
    render(SessionOrganizationEditor,{sessionKey:annotation.summary.identity.session_key});
    expect(await screen.findByRole('alert')).toHaveTextContent('Organization unavailable');
    expect(screen.queryByRole('button',{name:'Edit organization'})).not.toBeInTheDocument();
    mocks.getOrganizationSummaries.mockResolvedValue([annotation.summary]);
    await userEvent.click(screen.getByRole('button',{name:'Reload organization'}));
    await screen.findByRole('button',{name:'Edit organization'});
  });
});
