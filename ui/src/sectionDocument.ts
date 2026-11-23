// The Workbench document view (SPEC-15 slice 7, ADR-0064, AC-0669): the
// host's types and the three commands the view uses. Nothing here decides a
// state or a kind: the section table, the stored drafts and the curated
// read are the host's.
import { create } from 'zustand';
import type { CuratedRead } from './store';
import { invokeOr } from './tauri';
import type { EgressPreview } from './components/EgressConsentDialog';

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

/** What a draft changes in the draft it replaces, as the host compared
 *  the two stored drafts (SPEC-17, AC-0675). */
export interface SectionChange {
  claims_added: DraftClaim[];
  claims_removed: DraftClaim[];
  claims_kept: DraftClaim[];
  facts_added: string[];
  facts_removed: string[];
  /** Cited by both drafts with another recorded hash. */
  facts_changed: string[];
}

export interface SectionDocument {
  catalogue_version: string;
  document: string;
  sections: DocumentSection[];
  drafts_truncated: boolean;
  /** By the id of a draft that replaces a stored draft; absent from a
   *  host that predates the comparison. */
  changes?: Record<string, SectionChange>;
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
  /** Cited ids whose fact changed since the draft recorded it, sorted
   *  (ADR-0045, AC-0671); absent from a host that predates them. */
  changed?: string[];
  /** Cited ids that resolve to no fact now, sorted. */
  missing?: string[];
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

/** What a composition staged (#1991, AC-0862): a draft for review, or the
 *  closed reason there is none, with how many claims were staged and left
 *  out. */
export type ComposeOutcome =
  | { outcome: 'staged'; draft_id: string; claims: number; left_out: number; gated?: GatedStats }
  | { outcome: 'no_draft'; reason: string; claims: number; left_out: number; gated?: GatedStats };

/** What a gated composition (`draft-rules@1`, #2083, AC-0980) did. */
export interface GatedStats {
  chunks: number;
  rows: number;
  /** Gap rows left out before drafting (#1748). */
  gap_rows_skipped: number;
  /** Rows left out because an earlier row holds the same rule (#1748). */
  repeated_rows_skipped: number;
  rows_covered: number;
  calls: number;
  first_try_passed: number;
  retries: number;
  sent_back: number;
  audit_dropped: number;
  audit_failed: number;
  failures: Record<string, number>;
}

/** Why a composition staged nothing, by its closed code. */
export const COMPOSE_REFUSAL: Record<string, string> = {
  composition_system_scope_only: 'This section is composed for the whole system only.',
  composition_d1_only: 'Only sections of the product specification (D1) are composed.',
  section_not_compiled: 'The export compiled no such section.',
  no_citable_fact: 'The compiled section cites no fact, so there is nothing to compose from.',
  reply_not_the_schema: 'The model did not reply with the composition schema. Nothing was staged.',
  no_admissible_claim: 'No claim in the reply kept the citation and statement rules.',
  statement_names_source: 'Every claim named a source file or code symbol, so none was staged.',
  route_not_cited: 'Every claim named a route that none of its cited facts shows, so none was staged.',
  number_not_cited: 'Every claim stated a number or version that none of its cited facts shows, so none was staged.',
  endpoint_not_cited: 'Every claim was about endpoints or routes without citing an endpoint, so none was staged.',
  claim_not_supported: 'Every claim asserted more than its cited facts show, so none was staged.',
  rule_capability_not_cited: 'Every claim stated a rule without citing the capability it belongs to, so none was staged.',
  claim_overreaches: 'The review found every claim said more than its cited facts show, so none was staged.',
  graph_changed: 'The graph changed while the section was being composed. Compose it again.',
  no_claim_admitted: 'No drafted claim passed the gates, so nothing was staged.',
  gated_composition_section_unsupported: 'Only section 1.5 is drafted through the gated workflow.',
  area_not_found: 'No current area has that scope. Re-run the knowledge step, then compose again.',
  area_not_in_section: 'The compiled section lists nothing under that area.',
  run_stopped: 'The run stopped here, so this draft and any after it were not staged.',
  part_failed: 'Drafting this part failed, so it was not staged; the other parts were still drafted.',
};

/** The exact payload a cloud composition of the address would send. */
export async function sectionComposePreview(address: SectionAddress): Promise<EgressPreview> {
  const preview = await invokeOr<EgressPreview | null>('section_compose_preview', null, {
    catalogueVersion: address.catalogue_version,
    document: address.document,
    section: address.section,
    scope: address.scope,
  });
  if (!preview) throw new Error('The core did not return a composition preview.');
  return preview;
}

/** Compose the address in the cloud for the payload the user approved. */
export async function runSectionCompose(
  address: SectionAddress,
  preview: EgressPreview,
): Promise<ComposeOutcome> {
  const outcome = await invokeOr<ComposeOutcome | null>('run_section_compose', null, {
    catalogueVersion: address.catalogue_version,
    document: address.document,
    section: address.section,
    scope: address.scope,
    mode: 'cloud',
    approvedPayloadHash: preview.payload_hash,
  });
  if (!outcome) throw new Error('The core did not return a composition outcome.');
  return outcome;
}

/** One draft of a gated run: its scope, the area it is of (none for the
 *  system draft), its outcome and, on a `part_failed` part, why it failed:
 *  the failed model call's class and the part's error (#1748). */
export type GatedDraft = ComposeOutcome & {
  scope: string;
  area: string | null;
  failure?: { class: string; message: string };
};

/** Draft §1.5 through the gated workflow on the configured local model
 *  (#2083, AC-0980). Nothing leaves the machine, so there is no consent.
 *  At the system scope the host drafts each area separately, plus one
 *  draft of the rules that are no area's. */
export async function runSectionComposeGated(address: SectionAddress): Promise<GatedDraft[]> {
  const drafts = await invokeOr<GatedDraft[] | null>('run_section_compose_gated', null, {
    catalogueVersion: address.catalogue_version,
    document: address.document,
    section: address.section,
    scope: address.scope,
  });
  if (!drafts) throw new Error('The core did not return a composition outcome.');
  return drafts;
}

/** One line for the user about a gated run: one draft's outcome, or the
 *  sum over the area drafts and the areas that staged nothing. */
export function gatedRunText(drafts: GatedDraft[]): string {
  if (drafts.length === 1) return composeOutcomeText(drafts[0]);
  const staged = drafts.filter((draft) => draft.outcome === 'staged');
  const claims = staged.reduce((sum, draft) => sum + draft.claims, 0);
  const sum = (key: 'rows' | 'rows_covered' | 'calls') =>
    drafts.reduce((total, draft) => total + (draft.gated?.[key] ?? 0), 0);
  const empty = drafts
    .filter((draft) => draft.outcome === 'no_draft')
    .map((draft) => draft.area ?? 'rules outside an area');
  const nothing = empty.length > 0 ? ` Nothing was staged for ${empty.join(', ')}.` : '';
  return `Composed ${claims} ${claims === 1 ? 'claim' : 'claims'} in ${staged.length} `
    + `${staged.length === 1 ? 'draft' : 'drafts'} for review, by area.${nothing} `
    + `${sum('rows_covered')} of ${sum('rows')} rule rows covered in ${sum('calls')} local calls.`;
}

/** One line for the user about a composition's outcome. */
export function composeOutcomeText(outcome: ComposeOutcome): string {
  const left = outcome.left_out > 0
    ? ` ${outcome.left_out} ${outcome.left_out === 1 ? 'claim was' : 'claims were'} left out.`
    : '';
  const gated = outcome.gated
    ? ` ${outcome.gated.rows_covered} of ${outcome.gated.rows} rule rows covered in ${outcome.gated.calls} local calls.`
    : '';
  if (outcome.outcome === 'staged') {
    return `Composed ${outcome.claims} ${outcome.claims === 1 ? 'claim' : 'claims'} for review.${left}${gated}`;
  }
  return `${COMPOSE_REFUSAL[outcome.reason] ?? outcome.reason}${left}${gated}`;
}

/** Why the host refuses a re-draft, by its closed code (SPEC-17). */
export const REDRAFT_REFUSAL: Record<string, string> = {
  unknown_draft: 'This draft is no longer stored. Reload the sections.',
  draft_not_accepted: 'Only an accepted draft is re-drafted.',
  draft_not_stale: 'Every fact this draft cites is unchanged, so there is nothing to re-draft.',
  successor_pending: 'A re-draft of this draft is already stored. Review it first.',
};

/** The question that starts a re-draft of one stale or unresolvable
 *  accepted draft. The host checks the state; a refusal is its closed code
 *  put into words. */
export async function sectionRedraftQuestion(draft: StoredSectionDraft): Promise<string> {
  let question: string | null;
  try {
    question = await invokeOr<string | null>('section_redraft_question', null, {
      draftId: draft.draft_id,
    });
  } catch (error) {
    const code = message(error);
    throw new Error(REDRAFT_REFUSAL[code] ?? code, { cause: error });
  }
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
