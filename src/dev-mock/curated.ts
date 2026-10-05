/** Synthetic browser fixture only. Native Rust owns projection and persistence. */
import { organizationSummary } from './organization';
import { transcriptFixture } from './transcript';
import { redactTranscriptText } from '../lib/transcriptExport';
import type { CuratedContent, CuratedDataset, CuratedPreview, CuratedRequest, TranscriptRequest } from '../lib/types';
let dataset:CuratedDataset={export_digest:'synthetic-reviewed-dataset-digest',format_version:1,revision:0,earliest_change_revision:null,cases:[],changes:[],recovery_backup_unrestored:false};
const previews=new Map<string,{value:CuratedPreview,request:CuratedRequest}>();let nextId=1;
export function mockCurated(command:string,payload:Record<string,unknown>,recovery=false):unknown{
  if(command==='get_curated_dataset')return {...dataset,recovery_backup_unrestored:recovery};
  if(command==='get_curated_candidates'){
    const page=transcriptFixture({session_id:payload.sessionKey as string,cursor:payload.cursor as TranscriptRequest['cursor']});
    return {records:page.records.flatMap(row=>{
      const view=row.presentation;if(!view||!['user','assistant'].includes(view.role??''))return[];
      const text=view.blocks.filter(b=>b.kind==='text').map(b=>b.text).join('\n\n');
      return text?[{record_id:row.id,role:view.role,excerpt:redactTranscriptText(text,[]).text.slice(0,160)}]:[];
    }),next_cursor:page.next_cursor,availability:page.availability};
  }
  if(command==='preview_curated_case'){
    const request=payload.request as CuratedRequest;const summary=organizationSummary(request.identity.session_key);
    if(!['accepted','rejected'].includes(summary.outcome?.label??'')||request.dataset_revision!==dataset.revision)throw new Error('Synthetic source changed');
    let redactions=0;const clean=(text:string)=>{const v=redactTranscriptText(text,request.redact_phrases);redactions+=v.count;return v.text;};
    const blocks=request.record_ids.map(id=>{
      const row=transcriptFixture({session_id:request.identity.session_key,record_id:id}).records[0];
      if(!row||row.id!==id)throw new Error('Synthetic anchor changed');
      return {role:row.presentation?.role??'record',text:clean((row.presentation?.blocks??[]).filter(b=>b.kind==='text').map(b=>b.text).join('\n\n')),truncated:false};
    });
    const content:CuratedContent={name:clean(request.name),rubric:clean(request.rubric),expected_outcome:summary.outcome!.label,source_provider:'codex',source_fingerprint_at_capture:request.identity.fingerprint,source_records:request.record_ids,captured_at:'2026-07-29T15:30:00Z',blocks,redactions};
    const token=`synthetic-${previews.size+1}`;const value={token,content,replacing_case:request.case_id};previews.set(token,{value,request});return value;
  }
  if(command==='commit_curated_case'){
    const pending=previews.get(payload.token as string);if(!pending||!payload.reviewed||pending.request.dataset_revision!==dataset.revision)throw new Error('Synthetic preview changed');
    const id=pending.request.case_id??nextId++;const change=pending.request.case_id?'edited':'added';const revision=dataset.revision+1;
    const value={id,version:revision,session_key:pending.request.identity.session_key,fingerprint:pending.request.identity.fingerprint,content_hash:'synthetic-checksum',content:pending.value.content};
    dataset={...dataset,revision,earliest_change_revision:dataset.earliest_change_revision??revision,cases:[...dataset.cases.filter(c=>c.id!==id),value],changes:[...dataset.changes,{revision,case_id:id,change,content_hash:value.content_hash}]};previews.delete(payload.token as string);return dataset;
  }
  if(command==='remove_curated_case'){
    if(payload.revision!==dataset.revision)throw new Error('Synthetic dataset changed');
    const id=payload.caseId as number;const row=dataset.cases.find(c=>c.id===id);if(!row)throw new Error('Synthetic example missing');
    const revision=dataset.revision+1;dataset={...dataset,revision,cases:dataset.cases.filter(c=>c.id!==id),changes:[...dataset.changes,{revision,case_id:id,change:'removed',content_hash:row.content_hash}]};return dataset;
  }
  throw new Error('Unknown synthetic curated command');
}
