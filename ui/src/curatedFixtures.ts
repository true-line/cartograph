import type {
  ComputedCuratedRead, CuratedCounts, CuratedFinding, CuratedFindingListing, CuratedRead,
  CuratedRecord, SpecExportMode,
} from './store';

// Story and test fixtures for the curated read (SPEC-12 slice 3, AC-0655).
// The reason sentences are the ones the host sends (`spec`'s `curated.md`
// sentences); the UI never derives one from a reason code.

export const CURATED_REVISION = `curated-revision-v1:${'9a'.repeat(32)}`;
export const CURATED_GRAPH_BASIS = '4c'.repeat(32);

const record = (
  record_id: string,
  state: CuratedRecord['state'],
  reason: string,
  reason_sentence: string,
  extra: Partial<CuratedRecord> = {},
): CuratedRecord => ({
  record_id,
  gap_id: 'gap:sync',
  source_id: 'sym:capture',
  target_id: 'sym:send',
  edge_label: 'CALLS',
  tier: 'Agentic',
  confidence: 'InferredWeak',
  review_status: 'accepted',
  source_support: { captured: 2, unverified: 0 },
  state,
  reason,
  reason_sentence,
  basis_summary: 'graph unchanged; associations unchanged; evidence available 2, not available 0, unverified 0',
  ...extra,
});

export const ELIGIBLE_RECORD = record(
  'proposal:eligible', 'eligible', 'all_conditions_hold', 'Every condition holds.',
);

export const UNVERIFIED_RECORD = record(
  'proposal:legacy', 'unverified', 'legacy_record',
  'The record predates captured evidence, so nothing ties its evidence to stored source bytes.',
  {
    gap_id: 'gap:queue', source_id: 'sym:enqueue', target_id: 'res:orders-queue', edge_label: 'PUBLISHES_TO',
    source_support: { captured: 0, unverified: 2 }, basis_summary: 'no assessment supplied',
  },
);

export const STALE_RECORD = record(
  'proposal:moved', 'stale', 'graph_changed', 'The graph has changed since the proposal was made.',
  {
    gap_id: 'gap:auth', source_id: 'ep:login', target_id: 'sym:verify',
    basis_summary: 'graph changed; associations unchanged; evidence available 2, not available 0, unverified 0',
  },
);

export const CONFLICTED_RECORD = record(
  'proposal:other-target', 'conflicted', 'different_target_accepted',
  'Another accepted record proposes a different edge for the same Gap. Neither is a curated item while both are accepted.',
  { target_id: 'sym:send-batch' },
);

export const NO_COUNTS: CuratedCounts = {
  accepted: 0, eligible: 0, unverified: 0, unresolvable: 0, stale: 0, already_covered: 0, conflicted: 0,
};

/** A computed read with one item and listed records in three states. In
 *  `verified-only` the item is left out and counted, as the host does. */
export function curatedReadFixture(mode: SpecExportMode = 'best-effort'): ComputedCuratedRead {
  const items = mode === 'best-effort' ? [ELIGIBLE_RECORD] : [];
  return {
    status: 'computed',
    export_mode: mode,
    view: 'curated',
    revision: CURATED_REVISION,
    graph_basis: CURATED_GRAPH_BASIS,
    items,
    items_omitted: 1 - items.length,
    listed: [STALE_RECORD, UNVERIFIED_RECORD, CONFLICTED_RECORD],
    assertion_decisions: {
      digest: `curated-assertion-decisions-v1:${'7e'.repeat(32)}`,
      rejections_applied: 1,
      stale: [{ content_hash: 'd1'.repeat(32), decision: 'rejected', state: 'stale' }],
    },
    counts: { ...NO_COUNTS, accepted: 4, eligible: 1, unverified: 1, stale: 1, conflicted: 1 },
  };
}

/** A computed read over a store with no accepted proposal. */
export function emptyCuratedRead(mode: SpecExportMode = 'best-effort'): ComputedCuratedRead {
  return {
    status: 'computed',
    export_mode: mode,
    view: 'curated',
    revision: `curated-revision-v1:${'00'.repeat(32)}`,
    graph_basis: CURATED_GRAPH_BASIS,
    items: [],
    items_omitted: 0,
    listed: [],
    assertion_decisions: {
      digest: `curated-assertion-decisions-v1:${'11'.repeat(32)}`,
      rejections_applied: 0,
      stale: [],
    },
    counts: NO_COUNTS,
  };
}

export const ELIGIBLE_FINDING: CuratedFinding = {
  finding_ref: `investigation-finding-review-v1:${'1f'.repeat(32)}`,
  investigation_id: 'inv-refunds',
  result_id: 'result:inv-refunds',
  finding_id: 'F1',
  role: 'domain-analyst@2',
  kind: 'implemented_behavior',
  title: 'Refunds need an approved return',
  statement: 'A refund is issued only for an approved return.',
  limitations: ['Partial refunds were not read.'],
  citations: ['F-0123456789abcdef', 'F-fedcba9876543210'],
  tier: 'Agentic',
  confidence: 'InferredWeak',
  review_status: 'accepted',
  source_support: { captured: 1, unverified: 1 },
  state: 'eligible',
  reason: 'all_conditions_hold',
  reason_sentence: 'Every fact the finding cites is as the investigation read it.',
  changed: [],
  missing: [],
};

export const STALE_FINDING: CuratedFindingListing = {
  finding_ref: `investigation-finding-review-v1:${'2f'.repeat(32)}`,
  investigation_id: 'inv-refunds',
  result_id: 'result:inv-refunds',
  finding_id: 'F2',
  role: 'domain-analyst@2',
  cited_facts: 1,
  state: 'stale',
  reason: 'cited_fact_changed',
  reason_sentence: 'A fact the finding cites changed after the investigation read it.',
  changed: ['F-00000000000000c1'],
  missing: [],
};

/** The curated read with one eligible and one stale accepted finding, split
 *  by export mode as the host splits it (ADR-0074 decision 11). */
export function curatedFindingsReadFixture(mode: SpecExportMode = 'best-effort'): ComputedCuratedRead {
  const findings = mode === 'best-effort' ? [ELIGIBLE_FINDING] : [];
  return {
    ...curatedReadFixture(mode),
    findings,
    findings_omitted: 1 - findings.length,
    findings_listed: [STALE_FINDING],
    finding_counts: { accepted: 2, eligible: 1, unresolvable: 0, stale: 1 },
  };
}

export const NOT_COMPUTED_READ: CuratedRead = {
  status: 'not_computed',
  export_mode: 'best-effort',
  reason: 'too_many_accepted',
  limit: 256,
  sentence:
    'More than 256 proposals are accepted, which is the most one export reads. A part of them is never shown as if it were all.',
};
