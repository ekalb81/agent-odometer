/** Synthetic private organization; browser state is not durable native proof. */
import type { AnnotationEdit, AnnotationIdentity, OrganizationSummary, SavedSearch, SavedSearchDefinition, SessionAnnotation, RecordBookmark } from '../lib/types';
const annotations = new Map<string, SessionAnnotation>();
const searches = new Map<number, SavedSearch>();
const tags = new Set<string>();
const bookmarks = new Map<string, Map<string, RecordBookmark>>();
let nextId = 1;
export function organizationIdentity(key: string): AnnotationIdentity {
  return { session_key: key, fingerprint: `synthetic:${key}`, anchor: '' };
}
export function organizationSummary(key: string): OrganizationSummary {
  return annotations.get(key)?.summary ?? { identity: organizationIdentity(key), revision: 0, pinned: false, has_note: false, tags: [] };
}
export function mockOrganization(command: string, payload: Record<string, unknown>, recoveryUnrestored = false): unknown {
  switch (command) {
    case 'get_record_bookmarks': {
      const key = payload.sessionKey as string;
      return { identity: organizationIdentity(key), bookmarks: [...(bookmarks.get(key)?.values() ?? [])], recovery_backup_unrestored: recoveryUnrestored };
    }
    case 'edit_record_bookmark': {
      const edit = payload.edit as RecordBookmark;
      const rows = bookmarks.get(edit.identity.session_key) ?? new Map<string, RecordBookmark>();
      if ((rows.get(edit.identity.anchor)?.revision ?? 0) !== edit.revision) throw new Error('Bookmark changed; reload before saving');
      const next = { ...edit, revision: edit.revision + 1 };
      rows.set(edit.identity.anchor, next); bookmarks.set(edit.identity.session_key, rows); return next;
    }
    case 'get_organization_summaries': return (payload.keys as string[]).map(organizationSummary);
    case 'get_session_annotation': {
      const identity = payload.identity as AnnotationIdentity;
      return { ...(annotations.get(identity.session_key) ?? { summary: organizationSummary(identity.session_key), note: '' }), recovery_backup_unrestored: recoveryUnrestored };
    }
    case 'edit_session_annotation': {
      const edit = payload.edit as AnnotationEdit;
      if (edit.revision !== organizationSummary(edit.identity.session_key).revision) throw new Error('Organization changed; reload before saving');
      const value = { summary: { identity: edit.identity, revision: edit.revision + 1, pinned: edit.pinned, has_note: !!edit.note, tags: edit.tags, outcome: edit.outcome ?? organizationSummary(edit.identity.session_key).outcome }, note: edit.note };
      annotations.set(edit.identity.session_key, value); edit.tags.forEach(tag => tags.add(tag)); return value;
    }
    case 'list_organization_tags': return [...tags].sort();
    case 'change_organization_tag': {
      const label = payload.label as string; const replacement = payload.replacement as string | null;
      tags.delete(label); if (replacement) tags.add(replacement);
      for (const value of annotations.values()) if (value.summary.tags.includes(label)) {
        value.summary = { ...value.summary, revision: value.summary.revision + 1, tags: value.summary.tags.flatMap(tag => tag !== label ? [tag] : replacement ? [replacement] : []) };
      }
      return null;
    }
    case 'list_saved_searches': return [...searches.values()];
    case 'save_search': {
      const id = payload.id as number | null; const revision = payload.revision as number;
      if (id !== null && searches.get(id)?.revision !== revision) throw new Error('Saved search changed');
      const value = { id: id ?? nextId++, revision: revision + 1, definition: payload.definition as SavedSearchDefinition };
      searches.set(value.id, value); return value;
    }
    case 'delete_saved_search': searches.delete(payload.id as number); return null;
  }
  throw new Error('Unknown synthetic organization command');
}
