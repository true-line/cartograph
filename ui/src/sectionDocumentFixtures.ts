// Fixtures for the Workbench document view (SPEC-15 slice 7, AC-0669).
import { curatedReadFixture } from './curatedFixtures';
import type { ComputedCuratedRead } from './store';
import type {
  CuratedDraftRow, DraftDecision, SectionDocument, StoredSectionDraft,
} from './sectionDocument';

const ID = (seed: string) => seed.repeat(32);

export const CAPABILITY_FACT = 'F-00000000000000a1';
export const ACTOR_FACT = 'F-00000000000000b2';
export const SUBJECT_SCOPE = 'F-00000000000000c3';

function stored(
  seed: string, section: string, scope: string, statement: string,
  review: DraftDecision | null, extra: Partial<StoredSectionDraft> = {},
): StoredSectionDraft {
  return {
    schema_version: 1,
    draft_id: ID(seed),
    draft: {
      address: { catalogue_version: '1.0', document: 'D1', section, scope },
      claims: [
        { statement, kind: 'implemented', citations: [CAPABILITY_FACT, ACTOR_FACT] },
        { statement: 'The README says refunds are planned.', kind: 'documented_intent', citations: [CAPABILITY_FACT] },
      ],
      cited_hashes: { [CAPABILITY_FACT]: 'h1', [ACTOR_FACT]: 'h2' },
    },
    origin: { role: 'section-writer@1', role_definition: 'fingerprint', job_id: 7, graph_content_hash: 'graph' },
    replaces: null,
    review_revision: review ? 1 : 0,
    review_decision: review,
    review_note: null,
    created_at: '2026-11-17T09:00:00Z',
    reviewed_at: review ? '2026-11-17T09:30:00Z' : null,
    ...extra,
  };
}

export const PENDING_DRAFT = stored('a1', '1.3', 'system', 'An administrator can check out an order.', null);
export const ELIGIBLE_DRAFT = stored('b2', '1.2', 'system', 'The administrator is the only recovered actor.', 'accepted');
export const STALE_DRAFT = stored('c3', '1.3', SUBJECT_SCOPE, 'Checkout validates the cart before payment.', 'accepted');
export const MISSING_FACT = 'F-00000000000000d4';
/** Accepted, and one cited fact is gone from the graph. Not in the default
 *  document: a story adds it. */
export const UNRESOLVABLE_DRAFT = stored('f6', '1.5', 'system', 'An order over the limit is refused.', 'accepted');
export const RETURNED_DRAFT = stored('d4', '1.1', 'system', 'The system sells goods online.', 'returned', {
  review_note: 'Cite the checkout handler.',
});
export const SUCCESSOR_DRAFT = stored('e5', '1.1', 'system', 'The system takes orders and payments.', null, {
  replaces: RETURNED_DRAFT.draft_id,
});

const SECTIONS: [string, string, SectionDocument['sections'][number]['kind'], string | null][] = [
  ['1.1', 'Purpose and scope of the system', 'A', null],
  ['1.2', 'Actors and roles', 'P+A', 'Actor'],
  ['1.3', 'Capabilities', 'P+A', 'Capability'],
  ['1.4', 'Acceptance criteria per capability', 'P+A', 'Capability'],
  ['1.5', 'Business rules', 'P+A', 'BusinessRule'],
  ['1.6', 'Business flows', 'P', null],
  ['1.7', 'Out of scope and non-goals', 'H', null],
  ['1.8', 'Known gaps', 'P', null],
];

/** D1 with the given stored drafts under their sections, as the host
 *  returns it. */
export function sectionDocumentFixture(drafts: StoredSectionDraft[] = [
  SUCCESSOR_DRAFT, RETURNED_DRAFT, ELIGIBLE_DRAFT, PENDING_DRAFT, STALE_DRAFT,
]): SectionDocument {
  return {
    catalogue_version: '1.0',
    document: 'D1',
    drafts_truncated: false,
    sections: SECTIONS.map(([number, title, kind, subject]) => ({
      number, title, kind, subject,
      drafted: kind === 'A' || kind === 'P+A',
      drafts: drafts.filter((draft) => draft.draft.address.section === number),
    })),
  };
}

function row(
  draft: StoredSectionDraft, state: CuratedDraftRow['state'], reason: string, sentence: string,
  facts: Pick<CuratedDraftRow, 'changed' | 'missing'> = { changed: [], missing: [] },
): CuratedDraftRow {
  return {
    draft_id: draft.draft_id,
    address: draft.draft.address,
    role: draft.origin.role,
    replaces: draft.replaces,
    cited_facts: 2,
    state,
    reason,
    reason_sentence: sentence,
    ...facts,
  };
}

export const ELIGIBLE_ROW = row(
  ELIGIBLE_DRAFT, 'eligible', 'all_conditions_hold',
  'Every fact this draft cites is unchanged; a best-effort export prints it.',
);
export const STALE_ROW = row(
  STALE_DRAFT, 'stale', 'cited_fact_changed',
  'A fact this draft cites changed after the draft was written; a best-effort export prints it marked stale.',
  { changed: [CAPABILITY_FACT], missing: [] },
);
export const UNRESOLVABLE_ROW = row(
  UNRESOLVABLE_DRAFT, 'unresolvable', 'cited_fact_missing',
  'A fact this draft cites is not in the current graph.',
  { changed: [ACTOR_FACT], missing: [MISSING_FACT] },
);

/** The curated read that goes with [`sectionDocumentFixture`]. */
export function curatedReadWithDrafts(rows: CuratedDraftRow[] = [ELIGIBLE_ROW, STALE_ROW]): ComputedCuratedRead {
  const count = (state: CuratedDraftRow['state']) => rows.filter((item) => item.state === state).length;
  return {
    ...curatedReadFixture(),
    drafts: rows,
    draft_counts: {
      accepted: rows.length,
      eligible: count('eligible'),
      superseded: count('superseded'),
      unresolvable: count('unresolvable'),
      stale: count('stale'),
      conflicted: count('conflicted'),
    },
  };
}
