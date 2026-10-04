import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import OrganizationToolbar from './OrganizationToolbar.svelte';
import { defaultFilters } from '../lib/sessionProjection';
import { savedSummarySearch } from '../lib/organization';
const mocks = vi.hoisted(() => ({ getOrganizationRecoveryState:vi.fn(),listSavedSearches:vi.fn(),listOrganizationTags:vi.fn(),saveSearch:vi.fn(),deleteSavedSearch:vi.fn(),changeOrganizationTag:vi.fn(),getOrganizationSummaries:vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const saved = {id:7,revision:2,definition:{...savedSummarySearch('Review','claude_code',defaultFilters(),true,['Review']),query:'summary phrase',from:'2026-01-01T00:00:00.123Z',to:'2026-01-01T23:59:59.999Z'}};
beforeEach(() => {vi.resetAllMocks();mocks.getOrganizationRecoveryState.mockResolvedValue(false);mocks.listSavedSearches.mockResolvedValue([saved]);mocks.listOrganizationTags.mockResolvedValue(['Review']);mocks.getOrganizationSummaries.mockResolvedValue([]);});
async function open() {
  const onrestore=vi.fn();const onchange=vi.fn();
  render(OrganizationToolbar,{scope:'all',filters:defaultFilters(),scopes:['all','codex','claude_code'],onrestore,onchange});
  await userEvent.click(screen.getByText('Organize'));
  await waitFor(() => expect(screen.getByLabelText('Saved search')).not.toBeDisabled());
  return {onrestore,onchange};
}
describe('saved query management', () => {
  it('discloses preserved but unrestored searches after recovery', async () => {
    mocks.getOrganizationRecoveryState.mockResolvedValue(true);
    await open();
    expect(screen.getByText(/Earlier organization and saved searches were preserved/)).toBeInTheDocument();
  });
  it('restores provider, exact date bounds and organization choices together', async () => {
    const {onrestore}=await open();
    await userEvent.selectOptions(screen.getByLabelText('Saved search'),'7');
    await userEvent.click(screen.getByRole('button',{name:'Run saved search'}));
    expect(onrestore).toHaveBeenCalledWith('claude_code',expect.objectContaining({search:'summary phrase',utcBounds:{from:saved.definition.from,to:saved.definition.to}}),true,['Review']);
  });
  it('never converts unavailable transcript scope into a summary search', async () => {
    mocks.listSavedSearches.mockResolvedValue([{...saved,definition:{...saved.definition,content_scope:'session_content',session_key:'codex:thread:synthetic',fingerprint:'lineage'}}]);
    const {onrestore}=await open();
    await userEvent.selectOptions(screen.getByLabelText('Saved search'),'7');
    await userEvent.click(screen.getByRole('button',{name:'Run saved search'}));
    expect(await screen.findByRole('alert')).toHaveTextContent('unavailable in the summary view');
    expect(onrestore).not.toHaveBeenCalled();
  });
  it('requires review when saved tag labels were deleted', async () => {
    mocks.listOrganizationTags.mockResolvedValue([]);
    const {onrestore}=await open();
    await userEvent.selectOptions(screen.getByLabelText('Saved search'),'7');
    await userEvent.click(screen.getByRole('button',{name:'Run saved search'}));
    expect(await screen.findByRole('alert')).toHaveTextContent('deleted or renamed tag');
    expect(onrestore).not.toHaveBeenCalled();
  });
  it('preserves a failed rename for explicit retry rather than reporting success', async () => {
    await open();
    await userEvent.selectOptions(screen.getByLabelText('Saved search'),'7');
    await userEvent.type(screen.getByLabelText('Rename selected search'),'New name');
    mocks.saveSearch.mockRejectedValue(new Error('Saved search changed; reload before editing'));
    await userEvent.click(screen.getByRole('button',{name:'Rename search'}));
    expect(await screen.findByRole('alert')).toHaveTextContent('Saved search changed');
    expect(screen.getByLabelText('Rename selected search')).toHaveValue('New name');
    expect(mocks.saveSearch).toHaveBeenCalledWith(7,2,{...saved.definition,name:'New name'});
  });
});
