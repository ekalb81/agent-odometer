import { beforeEach, describe, expect, it, vi } from 'vitest';
import { organizationStore } from './organization.svelte';
import type { OrganizationSummary } from '../types';
const mocks = vi.hoisted(() => ({ getOrganizationSummaries: vi.fn(), listOrganizationTags: vi.fn(), getOrganizationRecoveryState: vi.fn() }));
vi.mock('../ipc', () => mocks);
const row: OrganizationSummary = {identity:{session_key:'codex:thread:synthetic',fingerprint:'initial',anchor:''},revision:1,pinned:false,has_note:false,tags:[]};
beforeEach(() => { vi.resetAllMocks(); organizationStore.invalidate('reset'); mocks.listOrganizationTags.mockResolvedValue([]); mocks.getOrganizationRecoveryState.mockResolvedValue(false); });

describe('private organization metadata requests', () => {
  it('marks recovery omissions explicitly even when replacement rows contain no pins', async () => {
    mocks.getOrganizationSummaries.mockResolvedValue([row]);
    mocks.getOrganizationRecoveryState.mockResolvedValue(true);
    await organizationStore.load([row.identity.session_key]);
    expect(organizationStore.recoveryUnrestored).toBe(true);
  });
  it('rejects a late response after purge/invalidation', async () => {
    let finish!: (rows: OrganizationSummary[]) => void;
    mocks.getOrganizationSummaries.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const pending = organizationStore.load([row.identity.session_key]);
    organizationStore.invalidate('History unavailable');
    finish([row]); await pending;
    expect(organizationStore.summaries).toEqual({});
    expect(organizationStore.error).toBe('History unavailable');
  });
  it('does not overwrite a completed edit with an earlier metadata read', async () => {
    let finish!: (rows: OrganizationSummary[]) => void;
    mocks.getOrganizationSummaries.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const pending = organizationStore.load([row.identity.session_key]);
    const edited = { ...row, revision:2,pinned:true,has_note:true,tags:['Review'] };
    organizationStore.update(edited);
    finish([row]); await pending;
    expect(organizationStore.summaries[row.identity.session_key]).toEqual(edited);
    expect(organizationStore.tagLabels).toContain('Review');
    expect(organizationStore.summaries[row.identity.session_key]).not.toHaveProperty('note');
  });
  it('clears metadata on a failed refresh instead of using stale filter membership', async () => {
    mocks.getOrganizationSummaries.mockResolvedValue([row]);
    await organizationStore.load([row.identity.session_key]);
    mocks.getOrganizationSummaries.mockRejectedValue(new Error('Ledger unavailable'));
    await organizationStore.load([row.identity.session_key]);
    expect(organizationStore.summaries).toEqual({});
    expect(organizationStore.error).toContain('Ledger unavailable');
  });
});
