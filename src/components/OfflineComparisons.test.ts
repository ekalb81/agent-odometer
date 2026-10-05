import { render, screen, waitFor, fireEvent } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import OfflineComparisons from './OfflineComparisons.svelte';
import { organizationStore } from '../lib/stores/organization.svelte';
import { mockExperiments } from '../dev-mock/experiments';
import type { CuratedDataset, ExperimentReport, FreezePreview, FreezeRequest } from '../lib/types';
const mocks=vi.hoisted(()=>({getCuratedDataset:vi.fn(),getOfflineExperiments:vi.fn(),getOfflineExperiment:vi.fn(),previewOfflineExperiment:vi.fn(),commitOfflineExperiment:vi.fn(),previewOfflineImport:vi.fn(),commitOfflineImport:vi.fn(),removeOfflineExperiment:vi.fn(),exportOfflineExperiment:vi.fn()}));
vi.mock('../lib/ipc',()=>mocks);
const dataset:CuratedDataset={format_version:1,revision:4,earliest_change_revision:1,recovery_backup_unrestored:false,changes:[],cases:[{id:7,version:4,session_key:'synthetic-key',fingerprint:'synthetic-fingerprint',content_hash:'synthetic-hash',content:{name:'Synthetic accepted case',rubric:'Correctness',expected_outcome:'accepted',source_provider:'codex',source_fingerprint_at_capture:'synthetic-fingerprint',source_records:['synthetic-record'],captured_at:'2026-01-01T00:00:00Z',redactions:1,blocks:[{role:'user',text:'<script>escaped synthetic input</script>',truncated:true}]}}]};
const request:FreezeRequest={dataset_revision:4,name:'Synthetic experiment',rubric:'Correctness',redact_phrases:[],variants:['a','b'].map(id=>({id,prompt_id:`prompt-${id}`,prompt_version:'1',prompt:`Synthetic ${id} instruction`,provider:'codex',model:'gpt-5.5',service_tier:'standard'}))};
let preview:FreezePreview, report:ExperimentReport;
beforeEach(()=>{
  vi.resetAllMocks();organizationStore.invalidate('test reset');
  preview=mockExperiments('preview_offline_experiment',{request},dataset) as FreezePreview;
  report=mockExperiments('commit_offline_experiment',{token:preview.token,reviewed:true},dataset) as ExperimentReport;
  report.current_selected_pricing_differs=true;report.current_dataset_revision=5;
  mocks.getCuratedDataset.mockResolvedValue(dataset);mocks.getOfflineExperiments.mockResolvedValue([]);mocks.getOfflineExperiment.mockResolvedValue(report);
  mocks.previewOfflineExperiment.mockResolvedValue(preview);mocks.commitOfflineExperiment.mockResolvedValue(report);mocks.removeOfflineExperiment.mockResolvedValue(undefined);mocks.exportOfflineExperiment.mockResolvedValue(true);
  Object.defineProperty(HTMLDialogElement.prototype,'showModal',{configurable:true,value:function(this:HTMLDialogElement){this.setAttribute('open','');}});
});
async function fillFreeze(expectPreview=true){
 await screen.findByText('No frozen comparisons yet. At most eight comparisons can be stored.');
 await fireEvent.input(screen.getByRole('textbox',{name:'Comparison name'}),{target:{value:request.name}});
 await fireEvent.input(screen.getByRole('textbox',{name:'Outcome review rubric'}),{target:{value:request.rubric}});
 for(const variant of request.variants){for(const [label,key] of [['Prompt identifier','prompt_id'],['Prompt version','prompt_version'],['Prompt text','prompt']] as const){await fireEvent.input(screen.getByRole('textbox',{name:`${label} ${variant.id}`}),{target:{value:variant[key]}});}}
 await userEvent.click(screen.getByRole('button',{name:'Build frozen comparison preview'}));
 if(expectPreview) await screen.findByRole('region',{name:'Exact offline comparison preview'});
}
async function openReport(){
 mocks.getOfflineExperiments.mockResolvedValue([{id:1,revision:1,name:request.name,dataset_revision:4,expected_cases:1,captured_at:report.manifest.captured_at}]);
 render(OfflineComparisons,{onclose:vi.fn()});await userEvent.click(await screen.findByRole('button',{name:'Synthetic experiment · dataset 4 · 1 cases'}));await screen.findByRole('region',{name:'Offline comparison report'});
}
it('freezes only a reviewed backend token, invalidates edits, and preserves frozen conditions in explicit exports',async()=>{
 render(OfflineComparisons,{onclose:vi.fn()});await fillFreeze();
 expect(mocks.previewOfflineExperiment).toHaveBeenCalledWith(request);
 expect(screen.getByRole('button',{name:'Save reviewed frozen comparison'})).toBeDisabled();
 expect(screen.getByRole('region',{name:'Exact offline comparison preview'}).querySelector('script')).toBeNull();
 await userEvent.click(screen.getByRole('checkbox',{name:/I reviewed all included/}));
 await userEvent.type(screen.getByRole('textbox',{name:'Prompt text a'}),' edited');expect(screen.queryByRole('region',{name:'Exact offline comparison preview'})).not.toBeInTheDocument();
 await userEvent.click(screen.getByRole('button',{name:'Build frozen comparison preview'}));await userEvent.click(await screen.findByRole('checkbox',{name:/I reviewed all included/}));await userEvent.click(screen.getByRole('button',{name:'Save reviewed frozen comparison'}));
 await screen.findByText('Reviewed offline comparison saved locally.');expect(mocks.commitOfflineExperiment).toHaveBeenCalledWith(preview.token,true);
 expect(screen.getByText(/current selected pricing differs; frozen estimates remain unchanged/)).toBeInTheDocument();expect(screen.getAllByText(/0 completed; 0 failed; 1 missing \/ 1/, {exact:false})).toHaveLength(2);
 await userEvent.click(screen.getByRole('button',{name:'Export frozen comparison JSON'}));expect(mocks.exportOfflineExperiment).toHaveBeenCalledWith(1,1,report.export_digest);expect(mocks.exportOfflineExperiment.mock.calls[0]).toHaveLength(3);expect(report.manifest.mode).toBe('imported_offline');expect(report.manifest.active_replay).toBe('no_go');
});
it('reviews minimized imported outputs and keeps missing and unavailable results separate from zeros',async()=>{
 await openReport();await userEvent.click(screen.getByRole('button',{name:'Insert empty import template'}));const rows=JSON.parse((screen.getByRole('textbox',{name:'Imported result JSON'}) as HTMLTextAreaElement).value);expect(rows[0].observed_model).toBe('');expect(rows[0].actual_cost_usd).toBeNull();
 const imported=mockExperiments('preview_offline_import',{request:{experiment_id:1,revision:1,rows,redact_phrases:[]}},dataset);
 mocks.previewOfflineImport.mockResolvedValue(imported);mocks.commitOfflineImport.mockResolvedValue({...report,revision:2});
 await userEvent.click(screen.getByRole('button',{name:'Build minimized import preview'}));expect(mocks.previewOfflineImport).toHaveBeenCalledWith({experiment_id:1,revision:1,rows,redact_phrases:[]});
 await userEvent.click(await screen.findByRole('checkbox',{name:/I reviewed all included/}));await userEvent.click(screen.getByRole('button',{name:'Save reviewed offline results'}));await screen.findByText('Reviewed offline comparison saved locally.');expect(mocks.commitOfflineImport).toHaveBeenCalledWith('synthetic-import-preview',true);expect(screen.getAllByText(/mean Unavailable · n=0/).length).toBeGreaterThan(0);
});
it('rejects delayed save publication after purge',async()=>{
 let finish!:(r:ExperimentReport)=>void;mocks.commitOfflineExperiment.mockReturnValue(new Promise(done=>{finish=done;}));
 render(OfflineComparisons,{onclose:vi.fn()});await fillFreeze();await userEvent.click(screen.getByRole('checkbox',{name:/I reviewed all included/}));await userEvent.click(screen.getByRole('button',{name:'Save reviewed frozen comparison'}));
 mocks.getCuratedDataset.mockResolvedValue({...dataset,revision:7,cases:[]});organizationStore.invalidate('purged after valid backend commit');await screen.findByText(/Dataset 7 · 0 cases/);finish(report);await waitFor(()=>expect(screen.getByRole('button',{name:'Reload comparisons'})).toBeEnabled());expect(screen.queryByRole('region',{name:'Offline comparison report'})).not.toBeInTheDocument();expect(screen.queryByText('Reviewed offline comparison saved locally.')).not.toBeInTheDocument();
});
it('removes reviewed copied content using revision and clears stale data on unavailable history',async()=>{
 await openReport();await userEvent.click(screen.getByRole('button',{name:'Review comparison removal'}));expect(mocks.removeOfflineExperiment).not.toHaveBeenCalled();await userEvent.click(screen.getByRole('button',{name:'Cancel removal'}));await userEvent.click(screen.getByRole('button',{name:'Review comparison removal'}));mocks.getOfflineExperiments.mockResolvedValue([]);await userEvent.click(screen.getByRole('button',{name:'Remove reviewed comparison'}));await screen.findByText(/Comparison removed, including its copied inputs/);expect(mocks.removeOfflineExperiment).toHaveBeenCalledWith(1,1);expect(screen.queryByRole('region',{name:'Offline comparison report'})).not.toBeInTheDocument();
 mocks.getCuratedDataset.mockRejectedValue(new Error('PRIVATE_PATH'));organizationStore.invalidate('unavailable');await screen.findByText('Offline comparisons unavailable. Reload history before retrying.');expect(screen.queryByText(/PRIVATE_PATH/)).not.toBeInTheDocument();
});
it('reports unavailable previews, stale saves, invalid JSON and export failures without diagnostic contents',async()=>{
 render(OfflineComparisons,{onclose:vi.fn()});mocks.previewOfflineExperiment.mockRejectedValue(new Error('PRIVATE_BODY'));await fillFreeze(false);await screen.findByText(/Freeze preview unavailable/);expect(screen.queryByText(/PRIVATE_BODY/)).not.toBeInTheDocument();
 mocks.previewOfflineExperiment.mockResolvedValue(preview);await userEvent.click(screen.getByRole('button',{name:'Build frozen comparison preview'}));await userEvent.click(await screen.findByRole('checkbox',{name:/I reviewed all included/}));mocks.commitOfflineExperiment.mockRejectedValue(new Error('stale'));await userEvent.click(screen.getByRole('button',{name:'Save reviewed frozen comparison'}));await screen.findByText(/Comparison was not saved/);
 mocks.getOfflineExperiments.mockResolvedValue([{id:1,revision:1,name:request.name,dataset_revision:4,expected_cases:1,captured_at:report.manifest.captured_at}]);await userEvent.click(screen.getByRole('button',{name:'Reload comparisons'}));await userEvent.click(await screen.findByRole('button',{name:'Synthetic experiment · dataset 4 · 1 cases'}));await userEvent.type(screen.getByRole('textbox',{name:'Imported result JSON'}),'invalid JSON');await userEvent.click(screen.getByRole('button',{name:'Build minimized import preview'}));await screen.findByText(/Import preview unavailable/);
 mocks.exportOfflineExperiment.mockRejectedValue(new Error('PRIVATE_DESTINATION'));await userEvent.click(screen.getByRole('button',{name:'Export frozen comparison JSON'}));await screen.findByText(/Comparison export could not be saved/);expect(screen.queryByText(/PRIVATE_DESTINATION/)).not.toBeInTheDocument();
});
