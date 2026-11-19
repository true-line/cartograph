import type { InvestigationResult } from './investigationTypes';

// The curation review of a saved finding (ADR-0074, AC-0846). It is a
// second review, separate from the follow-up review: its own commands, its
// own rows and its own event. A finding with no row has no curation review.

export type CurationDecision = 'accepted' | 'rejected';
export interface FindingCuration {
  subject_id: string;
  finding_id: string;
  review_revision: number;
  review_decision: CurationDecision;
  reviewed_at: string;
}
export interface FindingCurations {
  schema_version: 1;
  investigation_id: string;
  result_id: string;
  reviews: FindingCuration[];
}
export interface FindingCurationChanged {
  investigation_id: string;
  result_id: string;
  subject_id: string;
  review_revision: number;
}

const SUBJECT = /^investigation-finding-review-v1:[0-9a-f]{64}(?![\s\S])/;
const TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z(?![\s\S])/;
const fields = (value: object, keys: string[]) =>
  Object.keys(value).length === keys.length && Object.keys(value).every((key) => keys.includes(key));

export function validCuration(value: FindingCuration | null, result: InvestigationResult): value is FindingCuration {
  return !!value && typeof value === 'object' &&
    fields(value, ['subject_id', 'finding_id', 'review_revision', 'review_decision', 'reviewed_at']) &&
    typeof value.subject_id === 'string' && SUBJECT.test(value.subject_id) &&
    result.findings.filter((finding) => finding.finding_id === value.finding_id).length === 1 &&
    Number.isSafeInteger(value.review_revision) && value.review_revision > 0 &&
    (value.review_decision === 'accepted' || value.review_decision === 'rejected') &&
    typeof value.reviewed_at === 'string' && TIME.test(value.reviewed_at) &&
    Number.isFinite(Date.parse(value.reviewed_at));
}

/** A read of one result's curation reviews: at most one row per finding of
 *  that result, and none for a finding that was never reviewed. */
export function validCurations(value: FindingCurations | null, result: InvestigationResult): value is FindingCurations {
  if (!value || typeof value !== 'object' ||
    !fields(value, ['schema_version', 'investigation_id', 'result_id', 'reviews']) ||
    value.schema_version !== 1 || value.investigation_id !== result.investigation_id ||
    value.result_id !== result.result_id || !Array.isArray(value.reviews)) return false;
  const seen = new Set<string>();
  return value.reviews.every((row) => {
    if (!validCuration(row, result) || seen.has(row.finding_id)) return false;
    seen.add(row.finding_id);
    return true;
  });
}

export function validCurationChanged(value: FindingCurationChanged | null, result: InvestigationResult): value is FindingCurationChanged {
  return !!value && typeof value === 'object' &&
    fields(value, ['investigation_id', 'result_id', 'subject_id', 'review_revision']) &&
    value.investigation_id === result.investigation_id && value.result_id === result.result_id &&
    typeof value.subject_id === 'string' && SUBJECT.test(value.subject_id) &&
    Number.isSafeInteger(value.review_revision) && value.review_revision > 0;
}

/**
 * The newer observation of each finding. A curation review is never
 * deleted, so a row the incoming read lacks is kept, and a delayed read
 * cannot replace a later write. Two different observations of one revision
 * cannot both be true.
 */
export function mergeCurations(current: FindingCuration[], incoming: FindingCuration[]): FindingCuration[] {
  const merged = new Map(current.map((row) => [row.finding_id, row]));
  for (const row of incoming) {
    const old = merged.get(row.finding_id);
    if (!old) { merged.set(row.finding_id, row); continue; }
    if (old.subject_id !== row.subject_id) throw new Error('Curation subject changed');
    if (old.review_revision === row.review_revision &&
      (old.review_decision !== row.review_decision || old.reviewed_at !== row.reviewed_at)) {
      throw new Error('Conflicting curation revision');
    }
    if (row.review_revision > old.review_revision) merged.set(row.finding_id, row);
  }
  return [...merged.values()];
}
