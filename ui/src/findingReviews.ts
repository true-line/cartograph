import type { InvestigationResult } from './investigationTypes';
export type FindingReviewDecision = 'accepted_for_follow_up' | 'rejected_for_follow_up';
export interface FindingReviewSubject { investigation_id: string; result_id: string; finding_id: string; subject_id: string }
export interface FindingReview { subject: FindingReviewSubject; revision: number; decision: FindingReviewDecision | null; reviewed_at: string | null }
export interface FindingReviewChanged { investigation_id: string; result_id: string; subject_id: string; review_revision: number }
export function validReviewChanged(value: FindingReviewChanged | null, result: InvestigationResult): value is FindingReviewChanged {
  return !!value && fields(value, ['investigation_id', 'result_id', 'subject_id', 'review_revision']) &&
    value.investigation_id === result.investigation_id && value.result_id === result.result_id &&
    typeof value.subject_id === 'string' && /^investigation-finding-review-v1:[0-9a-f]{64}(?![\s\S])/.test(value.subject_id) &&
    Number.isSafeInteger(value.review_revision) && value.review_revision > 0;
}
export interface FindingReviews { schema_version: 1; investigation_id: string; result_id: string; reviews: FindingReview[] }
const fields = (value: object, keys: string[]) => Object.keys(value).length === keys.length && Object.keys(value).every((key) => keys.includes(key));
export function sameReviewSubject(a: FindingReviewSubject, b: FindingReviewSubject): boolean {
  return a.investigation_id === b.investigation_id && a.result_id === b.result_id && a.finding_id === b.finding_id && a.subject_id === b.subject_id;
}
export function validReview(value: FindingReview, result: InvestigationResult): boolean {
  if (!value || !fields(value, ['subject', 'revision', 'decision', 'reviewed_at']) || !value.subject) return false;
  const s = value.subject;
  return fields(s, ['investigation_id', 'result_id', 'finding_id', 'subject_id']) &&
    s.investigation_id === result.investigation_id && s.result_id === result.result_id &&
    result.findings.filter((f) => f.finding_id === s.finding_id).length === 1 &&
    typeof s.subject_id === 'string' && /^investigation-finding-review-v1:[0-9a-f]{64}(?![\s\S])/.test(s.subject_id) &&
    Number.isSafeInteger(value.revision) && value.revision >= 0 &&
    (value.revision === 0 ? value.decision === null && value.reviewed_at === null :
      (value.decision === 'accepted_for_follow_up' || value.decision === 'rejected_for_follow_up') &&
      typeof value.reviewed_at === 'string' && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(value.reviewed_at) && Number.isFinite(Date.parse(value.reviewed_at)) && new Date(value.reviewed_at).toISOString() === value.reviewed_at.replace('Z', '.000Z') && new TextEncoder().encode(value.reviewed_at).length <= 32);
}
export function validReviews(value: FindingReviews | null, result: InvestigationResult): value is FindingReviews {
  return !!value && fields(value, ['schema_version', 'investigation_id', 'result_id', 'reviews']) && value.schema_version === 1 &&
    value.investigation_id === result.investigation_id && value.result_id === result.result_id && Array.isArray(value.reviews) &&
    value.reviews.length === result.findings.length && value.reviews.every((row, index) => validReview(row, result) && row.subject.finding_id === result.findings[index].finding_id);
}
export function mergeReviews(current: FindingReview[], incoming: FindingReview[]): FindingReview[] {
  return incoming.map((row) => {
    const old = current.find((r) => r.subject.finding_id === row.subject.finding_id);
    if (!old) return row;
    if (!sameReviewSubject(old.subject, row.subject)) throw new Error('Review subject changed');
    if (old.revision === row.revision && (old.decision !== row.decision || old.reviewed_at !== row.reviewed_at)) throw new Error('Conflicting review revision');
    return old.revision > row.revision ? old : row;
  });
}
