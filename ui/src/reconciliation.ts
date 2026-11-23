/** Explicit reconciliation of a staged proposal (ADR-0072 slice 3, AC-1074).
 *  The host derives the state and records each result; nothing here is
 *  stored on the proposal or decides its review. */

export const MAX_RECONCILIATION_HISTORY = 64;

export type ReconciliationState = 'awaiting_reconciliation' | 'reconciled' | 'changed' |
  'missing' | 'unverifiable' | 'reconciliation_stale';
export type ReconciliationOutcome = 'reconciled' | 'changed' | 'missing' | 'unverifiable';

export type ItemOutcome =
  | { kind: 'retained_match' }
  | { kind: 'recapture_corroborated' }
  | { kind: 'changed'; reason: 'span_bytes_differ' | 'fact_digest_differs' }
  | { kind: 'missing'; reason: 'fact_absent' | 'path_not_captured' | 'span_out_of_bounds' }
  | { kind: 'unverifiable'; reason: UnverifiableReason };

export type UnverifiableReason = 'no_participating_receipt' | 'lossy_text' |
  'summary_procedure_unknown' | 'validation_byte_limit';

export type CandidateOutcome =
  | { kind: 'matched' }
  | { kind: 'changed'; reason: 'label_differs' | 'summary_differs' }
  | { kind: 'missing' }
  | { kind: 'unverifiable'; reason: UnverifiableReason };

export interface ReconciliationResult {
  schema_version: 1;
  result_id: string;
  proposal_id: string;
  proposal_schema_version: 1 | 2;
  recovery: { job_id: number; graph_snapshot_id: string; captures: { source_id: string; capture_id: string }[] };
  method: 'legacy_text_v1' | 'captured_span_v2';
  slot: {
    source: 'present' | 'missing';
    gap: 'present' | 'missing' | 'gap_resolved';
    relation: 'present' | 'missing';
  };
  evidence: { evidence_id: string; outcome: ItemOutcome }[];
  candidates: { node_id: string; outcome: CandidateOutcome }[];
  outcome: ReconciliationOutcome;
}

export interface ReconciliationHistory {
  proposal_id: string;
  state: ReconciliationState;
  results: { result: ReconciliationResult; recorded_at: string }[];
}

export interface ReconciliationView {
  proposalId: string;
  requestGeneration: number;
  loading: boolean;
  /** True while an explicit reconcile job runs, not a history read. */
  reconciling: boolean;
  history: ReconciliationHistory | null;
  error: string | null;
}

export const RECONCILIATION_UNAVAILABLE =
  'Reconciliation history could not be read. No proposal or review was changed.';
export const RECONCILE_FAILED =
  'The current recovery could not be read, so nothing was recorded. No proposal or review was changed.';

const STATES: ReconciliationState[] = ['awaiting_reconciliation', 'reconciled', 'changed',
  'missing', 'unverifiable', 'reconciliation_stale'];
const OUTCOMES: ReconciliationOutcome[] = ['reconciled', 'changed', 'missing', 'unverifiable'];
const UNVERIFIABLE: UnverifiableReason[] = ['no_participating_receipt', 'lossy_text',
  'summary_procedure_unknown', 'validation_byte_limit'];
const ITEM_REASONS: Record<string, readonly string[] | null> = {
  retained_match: null,
  recapture_corroborated: null,
  changed: ['span_bytes_differ', 'fact_digest_differs'],
  missing: ['fact_absent', 'path_not_captured', 'span_out_of_bounds'],
  unverifiable: UNVERIFIABLE,
};
const CANDIDATE_REASONS: Record<string, readonly string[] | null> = {
  matched: null,
  changed: ['label_differs', 'summary_differs'],
  missing: null,
  unverifiable: UNVERIFIABLE,
};

const isString = (value: unknown): value is string => typeof value === 'string';

function outcomeIn(outcome: unknown, reasons: Record<string, readonly string[] | null>): boolean {
  if (!outcome || typeof outcome !== 'object') return false;
  const { kind, reason } = outcome as { kind?: unknown; reason?: unknown };
  if (!isString(kind) || !(kind in reasons)) return false;
  const allowed = reasons[kind];
  return allowed === null ? reason === undefined : isString(reason) && allowed.includes(reason);
}

function validResult(result: unknown, proposalId: string): result is ReconciliationResult {
  if (!result || typeof result !== 'object') return false;
  const r = result as Partial<ReconciliationResult>;
  const slot = r.slot;
  const recovery = r.recovery;
  return r.schema_version === 1 && isString(r.result_id) && r.proposal_id === proposalId &&
    (r.proposal_schema_version === 1 || r.proposal_schema_version === 2) &&
    (r.method === 'legacy_text_v1' || r.method === 'captured_span_v2') &&
    !!recovery && Number.isSafeInteger(recovery.job_id) && isString(recovery.graph_snapshot_id) &&
    Array.isArray(recovery.captures) &&
    recovery.captures.every((c) => !!c && isString(c.source_id) && isString(c.capture_id)) &&
    !!slot && ['present', 'missing'].includes(slot.source) &&
    ['present', 'missing', 'gap_resolved'].includes(slot.gap) &&
    ['present', 'missing'].includes(slot.relation) &&
    Array.isArray(r.evidence) &&
    r.evidence.every((item) => !!item && isString(item.evidence_id) && outcomeIn(item.outcome, ITEM_REASONS)) &&
    Array.isArray(r.candidates) &&
    r.candidates.every((item) => !!item && isString(item.node_id) && outcomeIn(item.outcome, CANDIDATE_REASONS)) &&
    !!r.outcome && OUTCOMES.includes(r.outcome);
}

/** The host's answer, checked before it is shown; anything else is refused. */
export function requireReconciliationHistory(value: unknown, proposalId: string): ReconciliationHistory {
  const history = value as Partial<ReconciliationHistory> | null;
  if (!history || history.proposal_id !== proposalId || !history.state ||
      !STATES.includes(history.state) || !Array.isArray(history.results) ||
      history.results.length > MAX_RECONCILIATION_HISTORY ||
      history.results.some((entry) => !entry || !isString(entry.recorded_at) ||
        !validResult(entry.result, proposalId))) {
    throw new Error(RECONCILIATION_UNAVAILABLE);
  }
  return history as ReconciliationHistory;
}

export const stateLabels: Record<ReconciliationState, string> = {
  awaiting_reconciliation: 'Not yet reconciled',
  reconciled: 'Reconciled with the current recovery',
  changed: 'Changed since it was staged',
  missing: 'Missing from the current recovery',
  unverifiable: 'Could not be compared',
  reconciliation_stale: 'Reconciled, but the recovery has moved since',
};

const reasonLabels: Record<string, string> = {
  span_bytes_differ: 'the cited bytes differ',
  fact_digest_differs: 'the cited fact differs',
  fact_absent: 'the cited fact is no longer recovered',
  path_not_captured: 'the cited path is in no current capture',
  span_out_of_bounds: 'no current range covers the cited span',
  no_participating_receipt: 'its producer records no receipt',
  lossy_text: 'the current bytes are not valid UTF-8',
  summary_procedure_unknown: 'its summary procedure is unknown',
  validation_byte_limit: 'checking it would pass the byte limit',
  label_differs: 'its label differs',
  summary_differs: 'its summary differs',
};

const kindLabels: Record<string, string> = {
  retained_match: 'Matches its retained bytes',
  recapture_corroborated: 'Same text in the recapture (working-tree origin)',
  matched: 'Matches',
  changed: 'Changed',
  missing: 'Missing',
  unverifiable: 'Not compared',
};

/** One sentence for an item or candidate outcome. */
export function outcomeSentence(outcome: ItemOutcome | CandidateOutcome): string {
  const reason = 'reason' in outcome ? reasonLabels[outcome.reason] : undefined;
  return reason ? `${kindLabels[outcome.kind]}: ${reason}` : kindLabels[outcome.kind];
}

/** One sentence for the slot, naming only what is not present. */
export function slotSentence(slot: ReconciliationResult['slot']): string {
  const parts: string[] = [];
  if (slot.source === 'missing') parts.push('the source is no longer recovered');
  if (slot.gap === 'missing') parts.push('the gap is no longer recovered');
  if (slot.gap === 'gap_resolved') parts.push('deterministic recovery now resolves the gap');
  if (slot.relation === 'missing') parts.push('the source no longer relates to the gap');
  return parts.length === 0 ? 'Source, gap and their relation are present.' :
    `${parts.join('; ').replace(/^./, (c) => c.toUpperCase())}.`;
}
