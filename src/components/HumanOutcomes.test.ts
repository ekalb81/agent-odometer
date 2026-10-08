import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import HumanOutcomes from './HumanOutcomes.svelte';
import { organizationStore } from '../lib/stores/organization.svelte';
import type { SessionSummary } from '../lib/types';
const mocks = vi.hoisted(() => ({ writeExport:vi.fn(),getOrganizationSummaries:vi.fn(),listOrganizationTags:vi.fn(),getOrganizationRecoveryState:vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const session = { storage_id:'task',id:'task',harness:'codex',parent_thread_id:null,agent_path:null,source:'cli' } as unknown as SessionSummary;
beforeEach(() => { vi.resetAllMocks(); organizationStore.invalidate('History unavailable'); mocks.writeExport.mockResolvedValue(true); });
it('keeps unavailable history separate from an empty set of ratings', async () => {
  render(HumanOutcomes,{sessions:[session]});
  await userEvent.click(screen.getByText('Recorded task outcomes · whole-task ratings for selected sessions'));
  expect(screen.getByRole('alert')).toHaveTextContent('Recorded task outcomes unavailable');
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
});
it('exports only explicit structured metadata and reports denominators without note text', async () => {
  mocks.getOrganizationSummaries.mockResolvedValue([{ identity:{session_key:'task',fingerprint:'lineage',anchor:''},revision:1,pinned:true,tags:['Private'],has_note:true,
    outcome:{label:'accepted',repair_minutes:0,first_pass_accepted:true},note:'PRIVATE_SENTINEL_268' }]);
  mocks.listOrganizationTags.mockResolvedValue([]); mocks.getOrganizationRecoveryState.mockResolvedValue(false);
  await organizationStore.load(['task']);
  render(HumanOutcomes,{sessions:[session]});
  await userEvent.click(screen.getByText('Recorded task outcomes · whole-task ratings for selected sessions'));
  expect(screen.getByText(/1 labelled \/ 1 tasks/)).toBeInTheDocument();
  expect(screen.getByText(/1 \/ 1 explicitly reported/)).toBeInTheDocument();
  await userEvent.click(screen.getByRole('button',{name:'Export recorded task outcomes JSON'}));
  const [name,format,content] = mocks.writeExport.mock.calls[0];
  expect([name,format]).toEqual(['odometer-human-outcomes.json','json']);
  expect(content).not.toMatch(/PRIVATE_SENTINEL_268|Private|has_note|fingerprint/);
  expect(JSON.parse(content).rows).toEqual([{session_key:'task',label:'accepted',repair_minutes:0,first_pass_accepted:true,metadata_available:true}]);
  mocks.writeExport.mockRejectedValue(new Error('Save refused'));
  await userEvent.click(screen.getByRole('button',{name:'Export recorded task outcomes JSON'}));
  expect(await screen.findByRole('alert')).toHaveTextContent('Save refused');
});
