import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import CuratedDataset from './CuratedDataset.svelte';
import { organizationStore } from '../lib/stores/organization.svelte';
import type { CuratedDataset as Dataset, CuratedPreview, OrganizationSummary } from '../lib/types';
const mocks = vi.hoisted(() => ({ getCuratedDataset:vi.fn(),getCuratedCandidates:vi.fn(),previewCuratedCase:vi.fn(),commitCuratedCase:vi.fn(),removeCuratedCase:vi.fn(),getOrganizationSummaries:vi.fn(),exportCuratedDataset:vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const summary:OrganizationSummary = {identity:{session_key:'synthetic',fingerprint:'fingerprint',anchor:''},revision:2,pinned:false,tags:['PRIVATE_TAG'],has_note:true,outcome:{label:'accepted',repair_minutes:null,first_pass_accepted:null}};
const empty:Dataset={export_digest:'reviewed-dataset-digest',format_version:1,revision:0,earliest_change_revision:null,cases:[],changes:[],recovery_backup_unrestored:false};
const preview:CuratedPreview={token:'server-bound-preview',replacing_case:null,content:{name:'Synthetic case',rubric:'Review correctness',expected_outcome:'accepted',source_provider:'codex',source_fingerprint_at_capture:'fingerprint',source_records:['exact-record'],captured_at:'2026-10-04T12:00:00Z',blocks:[{role:'user',text:'<script>escaped synthetic text</script> [credential field redacted]',truncated:true}],redactions:1}};
const saved:Dataset={...empty,revision:1,earliest_change_revision:1,cases:[{id:1,version:1,session_key:'synthetic',fingerprint:'fingerprint',content_hash:'content-checksum',content:preview.content}],changes:[{revision:1,case_id:1,change:'added',content_hash:'content-checksum'}]};
beforeEach(()=>{
  vi.resetAllMocks();organizationStore.invalidate('test reset');
  mocks.getCuratedDataset.mockResolvedValue(empty);mocks.getOrganizationSummaries.mockResolvedValue([summary]);
  mocks.getCuratedCandidates.mockResolvedValue({records:[{record_id:'exact-record',role:'user',excerpt:'Synthetic source excerpt'}],next_cursor:null,availability:'available'});
  mocks.previewCuratedCase.mockResolvedValue(preview);mocks.commitCuratedCase.mockResolvedValue(saved);mocks.removeCuratedCase.mockResolvedValue({...empty,revision:2,earliest_change_revision:1,changes:[...saved.changes,{revision:2,case_id:1,change:'removed',content_hash:'content-checksum'}]});mocks.exportCuratedDataset.mockResolvedValue(true);
  Object.defineProperty(HTMLDialogElement.prototype,'showModal',{configurable:true,value:function(this:HTMLDialogElement){this.setAttribute('open','');}});
});
async function prepare(){
  await userEvent.click(await screen.findByRole('checkbox',{name:/Record 1/}));
  await userEvent.type(screen.getByRole('textbox',{name:'Example name'}),'Synthetic case');
  await userEvent.type(screen.getByRole('textbox',{name:'Optional review rubric'}),'Review correctness');
  await userEvent.click(screen.getByRole('button',{name:'Build exact minimized preview'}));
  await screen.findByRole('region',{name:'Exact curated example preview'});
}
it('stores only the reviewed backend token, escapes preview text, and exports an explicit version without organization notes',async()=>{
  render(CuratedDataset,{sessionKey:'synthetic',onclose:vi.fn()});await prepare();
  expect(mocks.previewCuratedCase).toHaveBeenCalledWith(expect.objectContaining({identity:summary.identity,outcome_revision:2,dataset_revision:0,record_ids:['exact-record']}));
  expect(screen.getByRole('region',{name:'Exact curated example preview'}).querySelector('script')).toBeNull();
  expect(screen.getByText('user · shortened excerpt')).toBeInTheDocument();
  expect(screen.getByRole('button',{name:'Add reviewed example'})).toBeDisabled();
  await userEvent.click(screen.getByRole('checkbox',{name:/I reviewed every included/}));
  await userEvent.type(screen.getByRole('textbox',{name:'Optional review rubric'}),' edited');
  expect(screen.queryByRole('region',{name:'Exact curated example preview'})).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button',{name:'Build exact minimized preview'}));
  await userEvent.click(await screen.findByRole('checkbox',{name:/I reviewed every included/}));
  await userEvent.click(screen.getByRole('button',{name:'Add reviewed example'}));
  await screen.findByText('Reviewed example saved locally.');expect(mocks.commitCuratedCase).toHaveBeenCalledWith('server-bound-preview',true);
  await userEvent.click(screen.getByRole('button',{name:'Export this dataset version JSON'}));
  expect(mocks.exportCuratedDataset).toHaveBeenCalledWith(1,'reviewed-dataset-digest');expect(JSON.stringify(mocks.exportCuratedDataset.mock.calls)).not.toMatch(/PRIVATE_TAG|has_note|repair_minutes/);
});
it('requires a reviewed removal, uses dataset revision, preserves content-free change history and edits stable IDs',async()=>{
  mocks.getCuratedDataset.mockResolvedValue(saved);render(CuratedDataset,{onclose:vi.fn()});
  await userEvent.click(await screen.findByText(/Example 1 · Synthetic case/));
  await userEvent.click(screen.getByRole('button',{name:'Edit example 1'}));
  await screen.findByRole('button',{name:'Build exact minimized preview'});
  expect(screen.getByRole('textbox',{name:'Example name'})).toHaveValue('Synthetic case');
  await userEvent.click(screen.getByRole('button',{name:'Build exact minimized preview'}));
  expect(mocks.previewCuratedCase).toHaveBeenCalledWith(expect.objectContaining({case_id:1,dataset_revision:1,record_ids:['exact-record']}));
  await userEvent.click(screen.getByRole('button',{name:'Review removal of example 1'}));
  expect(mocks.removeCuratedCase).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole('button',{name:'Cancel removal'}));
  await userEvent.click(screen.getByRole('button',{name:'Review removal of example 1'}));
  await userEvent.click(screen.getByRole('button',{name:'Remove reviewed example'}));
  await screen.findByText(/Example removed. Its content is gone/);expect(mocks.removeCuratedCase).toHaveBeenCalledWith(1,1);expect(screen.queryByText(/Example 1 · Synthetic case/)).not.toBeInTheDocument();expect(screen.getByText('No curated examples yet.')).toBeInTheDocument();
});
it('rejects late preview/source responses and clears cached case content when history is invalidated',async()=>{
  let resolve!:(value:CuratedPreview)=>void;mocks.previewCuratedCase.mockReturnValue(new Promise(done=>{resolve=done;}));
  const view=render(CuratedDataset,{sessionKey:'synthetic',onclose:vi.fn()});
  await userEvent.click(await screen.findByRole('checkbox',{name:/Record 1/}));await userEvent.type(screen.getByRole('textbox',{name:'Example name'}),'Synthetic case');await userEvent.click(screen.getByRole('button',{name:'Build exact minimized preview'}));
  view.unmount();resolve(preview);await waitFor(()=>expect(screen.queryByRole('region',{name:'Exact curated example preview'})).not.toBeInTheDocument());
  mocks.getCuratedDataset.mockResolvedValue(saved);render(CuratedDataset,{onclose:vi.fn()});await screen.findByText(/Example 1 · Synthetic case/);
  mocks.getCuratedDataset.mockRejectedValue(new Error('PRIVATE_HISTORY_PATH'));organizationStore.invalidate('purged');
  await screen.findByText('Dataset unavailable. Reload history before retrying.');expect(screen.queryByText(/Example 1 · Synthetic case/)).not.toBeInTheDocument();expect(screen.queryByText(/PRIVATE_HISTORY_PATH/)).not.toBeInTheDocument();
});
it('cannot republish a delayed successful save after a purge reload',async()=>{
  let resolve!:(value:Dataset)=>void;
  mocks.commitCuratedCase.mockReturnValue(new Promise(done=>{resolve=done;}));
  render(CuratedDataset,{sessionKey:'synthetic',onclose:vi.fn()});await prepare();
  await userEvent.click(screen.getByRole('checkbox',{name:/I reviewed every included/}));
  await userEvent.click(screen.getByRole('button',{name:'Add reviewed example'}));
  expect(mocks.commitCuratedCase).toHaveBeenCalledOnce();
  mocks.getCuratedDataset.mockResolvedValue({...empty,revision:2});
  mocks.getOrganizationSummaries.mockResolvedValue([]);
  organizationStore.invalidate('purged after backend commit');
  await screen.findByText('Version 2 · 0 examples');
  resolve(saved);
  await waitFor(()=>expect(screen.getByRole('button',{name:'Reload dataset'})).toBeEnabled());
  expect(screen.queryByText(/Example 1 · Synthetic case/)).not.toBeInTheDocument();
  expect(screen.queryByText('Reviewed example saved locally.')).not.toBeInTheDocument();
  expect(screen.getByText('Version 2 · 0 examples')).toBeInTheDocument();
});
it('keeps unrated sources ineligible and exposes unavailable/recovery/export failures honestly',async()=>{
  mocks.getOrganizationSummaries.mockResolvedValue([{...summary,outcome:{label:'not_rated',repair_minutes:null,first_pass_accepted:null}}]);mocks.getCuratedDataset.mockResolvedValue({...empty,recovery_backup_unrestored:true});
  render(CuratedDataset,{sessionKey:'synthetic',onclose:vi.fn()});await screen.findByText(/Use Edit organization to enter Accepted or Rejected/);expect(screen.queryByRole('button',{name:'Build exact minimized preview'})).not.toBeInTheDocument();expect(screen.getByText(/Earlier examples remain in the preserved recovery backup/)).toBeInTheDocument();
  mocks.exportCuratedDataset.mockRejectedValue(new Error('PRIVATE_DESTINATION'));await userEvent.click(screen.getByRole('button',{name:'Export this dataset version JSON'}));await screen.findByText(/Dataset export could not be saved/);expect(screen.queryByText(/PRIVATE_DESTINATION/)).not.toBeInTheDocument();
});
