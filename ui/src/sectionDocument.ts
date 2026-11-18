// The Workbench document view (SPEC-15 slice 7, ADR-0064, AC-0669): the
// host's types and the three commands the view uses. Nothing here decides a
// state or a kind: the section table, the stored drafts and the curated
// read are the host's.
import { create } from 'zustand';
import type { CuratedRead } from './store';
import { invokeOr } from './tauri';

export type SectionKindMark = 'P' | 'A' | 'P+A' | 'H';
export type DraftDecision = 'accepted' | 'rejected' | 'returned';
export type DraftClaimKind = 'implemented' | 'documented_intent' | 'interpretation';
export type CuratedDraftState = 'eligible' | 'superseded' | 'unresolvable' | 'stale' | 'conflicted';

export interface SectionAddress {
  catalogue_version: string;
  document: string;
  section: string;
  /** `system`, or the citation id of the subject. */
  scope: string;
}

export interface DraftClaim {
  statement: string;
  kind: DraftClaimKind;
  citations: string[];
}

/** A stored draft with its current review, as the draft store holds it. */
export interface StoredSectionDraft {
  schema_version: number;
  draft_id: string;
  draft: { address: SectionAddress; claims: DraftClaim[]; cited_hashes: Record<string, string> };
  origin: { role: string; role_definition: string; job_id: number; graph_content_hash: string };
  replaces: string | null;
  review_revision: number;
  review_decision: DraftDecision | null;
  review_note: string | null;
  created_at: string;
  reviewed_at: string | null;
}

export interface DocumentSection {
  number: string;
  title: string;
  kind: SectionKindMark;
  drafted: boolean;
  subject: string | null;
  drafts: StoredSectionDraft[];
}

export interface SectionDocument {
  catalogue_version: string;
  document: string;
  sections: DocumentSection[];
  drafts_truncated: boolean;
}

/** An accepted draft's state in the curated read; it carries no claim. */
export interface CuratedDraftRow {
  draft_id: string;
  address: SectionAddress;
  role: string;
  replaces: string | null;
  cited_facts: number;
  state: CuratedDraftState;
  reason: string;
  reason_sentence: string;
}

export interface CuratedDraftCounts {
  accepted: number;
  eligible: number;
  superseded: number;
  unresolvable: number;
  stale: number;
  conflicted: number;
}

export const SYSTEM_SCOPE = 'system';

/** What each kind mark means, in the catalogue's words. */
export const KIND_MEANING: Record<SectionKindMark, string> = {
  P: 'Projected: compiled from recovered facts. No model.',
  A: 'Authored: prose a role drafts from admitted facts.',
  'P+A': 'Projected and authored: a compiled frame, then drafted prose.',
  H: 'Human: only your team can supply it.',
};

/** The host's state of each accepted draft, by draft id. Empty when the
 *  curated read is absent or was not computed: no state is then stated. */
export function curatedDraftStates(read: CuratedRead | null | undefined): Map<string, CuratedDraftRow> {
  const rows = read?.status === 'computed' ? read.drafts ?? [] : [];
  return new Map(rows.map((row) => [row.draft_id, row]));
}

/** Whether the curated read can state the state of an accepted draft. */
export function curatedReadStatesDrafts(read: CuratedRead | null | undefined): boolean {
  return read?.status === 'computed' && Array.isArray(read.drafts);
}

export function scopeLabel(scope: string): string {
  return scope === SYSTEM_SCOPE ? 'Whole system' : `Subject ${scope}`;
}

export async function loadSectionDocument(document = 'D1'): Promise<SectionDocument | null> {
  return invokeOr<SectionDocument | null>('section_document', null, { document });
}

/** Record a decision at the revision this draft was read at. The host
 *  refuses a stale revision; the caller then reloads the document. */
export async function reviewSectionDraft(
  draft: StoredSectionDraft,
  decision: DraftDecision,
  note: string | null,
): Promise<StoredSectionDraft> {
  const result = await invokeOr<StoredSectionDraft | null>('review_section_draft', null, {
    draftId: draft.draft_id,
    expectedRevision: draft.review_revision,
    decision,
    note,
  });
  if (!result || result.draft_id !== draft.draft_id || result.review_decision !== decision ||
      result.review_revision !== draft.review_revision + 1) {
    throw new Error('The core did not confirm this review. Reload the document before retrying.');
  }
  return result;
}

/** The one question that starts a drafting run for the address. */
export async function sectionDraftQuestion(address: SectionAddress): Promise<string> {
  const question = await invokeOr<string | null>('section_draft_question', null, {
    catalogueVersion: address.catalogue_version,
    document: address.document,
    section: address.section,
    scope: address.scope,
  });
  if (!question) throw new Error('The core did not return a drafting request.');
  return question;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export interface SectionDocumentState {
  document: SectionDocument | null;
  busy: boolean;
  error: string | null;
  /** Read the document view. A newer read wins over one still in flight. */
  load: () => Promise<void>;
  /** Record the decision, then read the document again whatever the
   *  outcome: a refused review means the draft moved on. Resolves to
   *  whether the review was recorded, so the caller can read the curated
   *  projection again. */
  review: (draft: StoredSectionDraft, decision: DraftDecision, note: string | null) => Promise<boolean>;
}

let generation = 0;

/** The document view's state. The Workbench loads it on entry and after
 *  each review. */
export const useSectionDocumentStore = create<SectionDocumentState>((set, get) => ({
  document: null,
  busy: false,
  error: null,

  load: async () => {
    const mine = ++generation;
    set({ busy: true });
    try {
      const document = await loadSectionDocument();
      if (mine === generation) set({ document, error: null, busy: false });
    } catch (caught) {
      if (mine === generation) set({ error: message(caught), busy: false });
    }
  },

  review: async (draft, decision, note) => {
    set({ busy: true });
    let failed: string | null = null;
    try {
      await reviewSectionDraft(draft, decision, note);
    } catch (caught) {
      failed = message(caught);
    }
    await get().load();
    if (failed) set({ error: failed });
    return failed === null;
  },
}));
