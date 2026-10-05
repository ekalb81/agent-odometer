import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SessionOrganizationEditor from './SessionOrganizationEditor.svelte';
import { organizationStore } from '../lib/stores/organization.svelte';
import type { SessionAnnotation } from '../lib/types';
const mocks = vi.hoisted(() => ({ getOrganizationSummaries:vi.fn(),getSessionAnnotation:vi.fn(),editSessionAnnotation:vi.fn(),listOrganizationTags:vi.fn(),getOrganizationRecoveryState:vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const annotation: SessionAnnotation = {summary:{identity:{session_key:'codex:thread:synthetic',fingerprint:'lineage',anchor:''},revision:2,pinned:false,has_note:false,tags:[]},note:''};
beforeEach(() => {
  vi.resetAllMocks(); organizationStore.invalidate('reset');
  mocks.getOrganizationSummaries.mockResolvedValue([annotation.summary]);
  mocks.getSessionAnnotation.mockResolvedValue(annotation);
  mocks.listOrganizationTags.mockResolvedValue([]);
  mocks.getOrganizationRecoveryState.mockResolvedValue(false);
});
async function open() {
  const view=render(SessionOrganizationEditor,{sessionKey:annotation.summary.identity.session_key});
  await userEvent.click(await screen.findByRole('button',{name:'Edit organization'}));return view;
}
describe('private session editing', () => {
  it('saves explicit outcomes and optional effort with private repair notes', async () => {
    await open();
    expect(screen.getByLabelText('Task outcome')).toHaveValue('not_rated');
    await userEvent.selectOptions(screen.getByLabelText('Task outcome'),'accepted');
    await userEvent.type(screen.getByLabelText('User-reported repair minutes (optional)'),'0');
    await userEvent.selectOptions(screen.getByLabelText('Accepted on first pass (explicit report)'),'true');
    await userEvent.type(screen.getByLabelText('Private note'),'PRIVATE_REPAIR_268');
    mocks.editSessionAnnotation.mockImplementation(async edit => ({ summary: { ...annotation.summary, revision:edit.revision + 1, outcome:edit.outcome }, note:edit.note }));
    await userEvent.click(screen.getByRole('button',{name:'Save organization'}));
    await waitFor(() => expect(mocks.editSessionAnnotation).toHaveBeenCalledWith(expect.objectContaining({
      outcome:{label:'accepted',repair_minutes:0,first_pass_accepted:true},note:'PRIVATE_REPAIR_268',
    })));
    expect(JSON.stringify(organizationStore.summaries)).not.toContain('PRIVATE_REPAIR_268');
    await screen.findByRole('button',{name:'Edit organization'});
    await userEvent.click(screen.getByRole('button',{name:'Edit organization'}));
    await userEvent.selectOptions(screen.getByLabelText('Task outcome'),'rejected');
    expect(screen.getByLabelText('Accepted on first pass (explicit report)')).toHaveValue('');
    await userEvent.click(screen.getByRole('button',{name:'Discard changes'}));
    expect(organizationStore.summaries[annotation.summary.identity.session_key].outcome?.label).toBe('accepted');
    await userEvent.click(screen.getByRole('button',{name:'Edit organization'}));
    expect(screen.getByLabelText('Task outcome')).toHaveValue('accepted');
    expect(screen.getByLabelText('User-reported repair minutes (optional)')).toHaveValue(0);
    await userEvent.selectOptions(screen.getByLabelText('Accepted on first pass (explicit report)'),'false');
    await userEvent.selectOptions(screen.getByLabelText('Task outcome'),'unresolved');
    expect(screen.getByLabelText('Accepted on first pass (explicit report)')).toHaveValue('false');
    await userEvent.selectOptions(screen.getByLabelText('Task outcome'),'not_rated');
    expect(screen.getByLabelText('Accepted on first pass (explicit report)')).toHaveValue('');
    expect(screen.getByLabelText('Accepted on first pass (explicit report)')).toBeDisabled();
  });
  it.each(['invalidate', 'replace', 'destroy'] as const)('does not publish a delayed successful save after %s', async action => {
    const view = await open();
    let finish!: (value: SessionAnnotation) => void;
    mocks.editSessionAnnotation.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    await userEvent.click(screen.getByLabelText('Pin this session'));
    await userEvent.click(screen.getByRole('button', { name: 'Save organization' }));
    if (action === 'invalidate') organizationStore.invalidate('History purged');
    if (action === 'replace') {
      mocks.getOrganizationSummaries.mockResolvedValue([{ ...annotation.summary, identity: { ...annotation.summary.identity, fingerprint: 'replacement' } }]);
      await organizationStore.load([annotation.summary.identity.session_key]);
    }
    if (action === 'destroy') view.unmount();
    finish({ ...annotation, summary: { ...annotation.summary, revision: 3, pinned: true, tags: ['Old'] } });
    if (action !== 'destroy') await screen.findByRole('alert');
    else await waitFor(() => expect(organizationStore.summaries[annotation.summary.identity.session_key]?.pinned).toBe(false));
    expect(Object.values(organizationStore.summaries).every(row => !row.pinned && !row.tags.includes('Old'))).toBe(true);
    if (action === 'replace') expect(organizationStore.summaries[annotation.summary.identity.session_key].identity.fingerprint).toBe('replacement');
  });
  it('discloses that a recovered empty note did not restore private backup data', async () => {
    mocks.getSessionAnnotation.mockResolvedValue({ ...annotation, recovery_backup_unrestored:true });
    await open();
    expect(screen.getByText(/Earlier pins, tags, and notes remain/)).toBeInTheDocument();
    expect(screen.getByLabelText('Private note')).toHaveValue('');
  });
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
