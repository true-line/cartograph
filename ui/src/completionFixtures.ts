import { qualifiedCatalog, qualifiedDetail, qualifiedResult, qualifiedCoverageContext, inspectionTarget } from './investigationFixtures';
import type { InvestigationCatalog, InvestigationCompletionDetail, InvestigationDetail, InvestigationObligation,
  InvestigationObligationChoices, InvestigationResult } from './investigationTypes';

/** Scripted UI examples only; these never establish provider acceptance. */
export const completionCatalog: InvestigationCatalog = { ...qualifiedCatalog, specialists: [...qualifiedCatalog.specialists,
  ...(['domain-analyst@7', 'evidence-auditor@7'] as const).map((id, index) => ({ ...qualifiedCatalog.specialists[index], id,
    version: 7, name: index ? 'Completion evidence auditor' : 'Completion domain analyst', prompt_fingerprint: `fixture-${id}` }))] };
export const sourceObligation: Extract<InvestigationObligation, { kind: 'source' }> = { kind: 'source', obligation_id: '1'.repeat(64), target: { ...inspectionTarget, graph_snapshot_id: `context-v1:${'8'.repeat(64)}` } };
export const inventoryObligation: Extract<InvestigationObligation, { kind: 'inventory' }> = { kind: 'inventory', obligation_id: '2'.repeat(64), target: {
  context_id: qualifiedCoverageContext.context_id, repo: 'acme/api', inventory_hash: '3'.repeat(64) } };
export const completionChoices: InvestigationObligationChoices = { schema_version: 1, graph_snapshot_id: sourceObligation.target.graph_snapshot_id,
  context_id: qualifiedCoverageContext.context_id, inventories: [
    { repo: inventoryObligation.target.repo, status: 'recorded', obligation: inventoryObligation },
    { repo: 'example/stale', status: 'stale' }, { repo: 'example/unrecorded', status: 'unrecorded' }] };
export function completionDetail(id = 'inv-completion', overrides: Partial<InvestigationDetail> = {}): InvestigationDetail {
  const completion: InvestigationCompletionDetail = { schema_version: 1, request_hash: 'a'.repeat(64), obligations_hash: 'b'.repeat(64),
    obligations: { schema_version: 1, contract: 'investigation-completion-obligations@1', obligations: structuredClone([inventoryObligation, sourceObligation]) },
    refusal_count: 0, assessment_basis: { kind: 'finishing_disclosure', step_id: 'finish:1', input_ledger_hash: 'c'.repeat(64),
      disclosure_hash: `investigation-disclosure-v4:${'d'.repeat(64)}`, finishing_attempts_hash: 'e'.repeat(64) },
    assessment: [inventoryObligation, sourceObligation].map((item) => ({ obligation_id: item.obligation_id, kind: item.kind,
      attempted: true, acquired: true, exactly_supplied: true, unmet: null, last_attempt_outcome: 'acquired' })) };
  return qualifiedDetail(id, { schema_version: 4, specialist_id: 'domain-analyst@7',
    specialist: completionCatalog.specialists.find((item) => item.id === 'domain-analyst@7')!,
    graph_snapshot_id: sourceObligation.target.graph_snapshot_id, completion_outcome: 'supported', obligation_status: 'satisfied', completion, ...overrides });
}
export function completionResult(id = 'inv-completion', overrides: Partial<InvestigationResult> = {}): InvestigationResult {
  const detail = completionDetail(id);
  return qualifiedResult(id, { schema_version: 5, result_id: `investigation-result-v5:${'7'.repeat(64)}`,  graph_snapshot_id: detail.graph_snapshot_id!, input_ledger_hash: 'c'.repeat(64),
    invocation_step_id: 'finish:1', disclosure_hash: `investigation-disclosure-v4:${'d'.repeat(64)}`,
    completion: { schema_version: 1, outcome: 'supported', request_hash: 'a'.repeat(64), obligations_hash: 'b'.repeat(64),
      finishing_attempts_hash: 'e'.repeat(64), assessment: detail.completion!.assessment }, ...overrides });
}
