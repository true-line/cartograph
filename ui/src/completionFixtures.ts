import { qualifiedCatalog, qualifiedDetail, qualifiedResult, qualifiedCoverageContext, inspectionTarget } from './investigationFixtures';
import type { InvestigationCatalog, InvestigationCompletionDetail, InvestigationDetail, InvestigationObligation,
  InvestigationObligationChoices, InvestigationResult, InvestigationSpecialist } from './investigationTypes';

/** Scripted UI examples only; these never establish provider acceptance. */
export const completionCatalog: InvestigationCatalog = { ...qualifiedCatalog, specialists: [...qualifiedCatalog.specialists,
  ...(['domain-analyst@7', 'evidence-auditor@7'] as const).map((id, index) => ({ ...qualifiedCatalog.specialists[index], id,
    version: 7, name: index ? 'Completion evidence auditor' : 'Completion domain analyst', prompt_fingerprint: `fixture-${id}` })),
  { ...qualifiedCatalog.specialists[0], id: 'spec-writer@1', version: 1, name: 'Spec Writer',
    purpose: 'Draft a cited proposed specification from the scoped recovered context.',
    operations: ['query_context', 'read_evidence', 'read_coverage_inventory', 'finish'] as InvestigationSpecialist['operations'],
    prompt_fingerprint: 'fixture-spec-writer@1' },
  { ...qualifiedCatalog.specialists[0], id: 'spec-writer@2', version: 2, name: 'Spec Writer',
    purpose: 'Write a cited proposed specification from the scoped recovered context.',
    operations: ['query_context', 'read_evidence', 'read_coverage_inventory', 'finish'] as InvestigationSpecialist['operations'],
    prompt_fingerprint: 'fixture-spec-writer@2' },
  { ...qualifiedCatalog.specialists[0], id: 'gap-triager@1', version: 1, name: 'Gap Triager',
    purpose: 'Recommend a cited next investigation step for a recorded gap.',
    operations: ['query_context', 'read_evidence', 'read_coverage_inventory', 'finish'] as InvestigationSpecialist['operations'],
    prompt_fingerprint: 'fixture-gap-triager@1' }] };
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

export function specWriterDetail(id = 'inv-spec-writer'): InvestigationDetail {
  return completionDetail(id, { specialist_id: 'spec-writer@1',
    specialist: completionCatalog.specialists.find((item) => item.id === 'spec-writer@1')! });
}

export function specWriterResult(id = 'inv-spec-writer'): InvestigationResult {
  const result = completionResult(id);
  result.findings = result.findings.slice(0, 1).map((finding) => ({ ...finding, claim_kind: 'proposed_design',
    title: 'Reject limited inputs before guarded processing',
    statement: 'Reject limited inputs before guarded processing. Acceptance criterion: when limited is true, the operation returns false. Runtime policy and broader behavior remain unresolved.' }));
  return result;
}

/** New immutable UI fixture; the original @1 definition/result above stay unchanged. */
export function specWriterV2Detail(id = 'inv-spec-writer-v2'): InvestigationDetail {
  return completionDetail(id, { specialist_id: 'spec-writer@2',
    specialist: completionCatalog.specialists.find((item) => item.id === 'spec-writer@2')! });
}

export function specWriterV2Result(id = 'inv-spec-writer-v2'): InvestigationResult {
  const result = specWriterResult(id);
  return { ...result, schema_version: 8, result_id: `investigation-result-v8:${'8'.repeat(64)}`,
    narrative: { text: 'Validate the caller limit before invoking processing; refuse an oversized request without starting the operation.',
      finding_ids: result.findings.map((finding) => finding.finding_id) } };
}

/** Advertised opt-in example; production keeps this unavailable until registered. */
export const expandedCompletionCatalog: InvestigationCatalog = {
  ...completionCatalog,
  specialists: [...completionCatalog.specialists, ...completionCatalog.specialists.filter((item) => item.version === 7).map((item) => ({
    ...item, id: item.id.replace('@7', '@8') as import('./investigationTypes').SpecialistId, version: 8,
  }))],
  policies: [{ limits: { ...completionCatalog.limits, profile: 'investigation-v2', model_invocations: 64, tool_actions: 56,
    evidence_requests: 64, evidence_items: 56, evidence_bytes: 65536, generated_token_reservations: 131072 },
    request_schema_version: 5, envelope_schema_version: 2, envelope_contract: 'investigation-completion-obligations@2',
    max_completion_obligations: 56, request_bytes: 65536, envelope_bytes: 65536,
    specialist_ids: ['domain-analyst@8', 'evidence-auditor@8'] }],
};

/** Current request family, with the same immutable expanded numeric policy. */
export const queryHistoryCatalog: InvestigationCatalog = {
  ...expandedCompletionCatalog,
  specialists: expandedCompletionCatalog.specialists.map((item) => item.version === 8 ? {
    ...item, id: item.id.replace('@8', '@9') as import('./investigationTypes').SpecialistId,
    version: 9, prompt_fingerprint: `fixture-${item.id.replace('@8', '@9')}`,
    operations: [...item.operations, 'replay_query'],
  } : item),
  policies: expandedCompletionCatalog.policies!.map((policy) => ({ ...policy, request_schema_version: 6,
    specialist_ids: ['domain-analyst@9', 'evidence-auditor@9'] })),
};

export function queryHistoryDetail(id = 'inv-query-history'): InvestigationDetail {
  const detail = completionDetail(id, { schema_version: 6, specialist_id: 'domain-analyst@9',
    specialist: queryHistoryCatalog.specialists.find((item) => item.id === 'domain-analyst@9')!,
    limits: queryHistoryCatalog.policies![0].limits });
  detail.completion!.obligations.schema_version = 2;
  detail.completion!.obligations.contract = 'investigation-completion-obligations@2';
  if (detail.completion!.assessment_basis.kind === 'finishing_disclosure') {
    detail.completion!.assessment_basis.disclosure_hash = `investigation-disclosure-v5:${'d'.repeat(64)}`;
  }
  return detail;
}

export function queryHistoryResult(id = 'inv-query-history'): InvestigationResult {
  return completionResult(id, { schema_version: 6, result_id: `investigation-result-v6:${'7'.repeat(64)}`,
    disclosure_hash: `investigation-disclosure-v5:${'d'.repeat(64)}` });
}


/** New starts explicitly advertise @10; old fixture catalogs remain immutable. */
export const admissionCatalog: InvestigationCatalog = {
  ...queryHistoryCatalog,
  specialists: queryHistoryCatalog.specialists.map((item) => item.version === 9 ? {
    ...item, id: item.id.replace('@9', '@10') as import('./investigationTypes').SpecialistId,
    version: 10, prompt_fingerprint: `fixture-${item.id.replace('@9', '@10')}`,
  } : item),
  policies: queryHistoryCatalog.policies!.map((policy) => ({ ...policy, request_schema_version: 7,
    specialist_ids: ['domain-analyst@10', 'evidence-auditor@10'] })),
};
export function admissionDetail(id = 'inv-admission'): InvestigationDetail {
  const detail = queryHistoryDetail(id);
  detail.schema_version = 7; detail.specialist_id = 'domain-analyst@10';
  detail.specialist = admissionCatalog.specialists.find((item) => item.id === 'domain-analyst@10')!;
  if (detail.completion!.assessment_basis.kind === 'finishing_disclosure') {
    detail.completion!.assessment_basis.disclosure_hash = `investigation-disclosure-v6:${'d'.repeat(64)}`;
  }
  return detail;
}
export function admissionResult(id = 'inv-admission'): InvestigationResult {
  return { ...queryHistoryResult(id), schema_version: 7, result_id: `investigation-result-v7:${'7'.repeat(64)}`,
    disclosure_hash: `investigation-disclosure-v6:${'d'.repeat(64)}` };
}

export function gapTriagerDetail(id = 'inv-gap-triager'): InvestigationDetail {
  return completionDetail(id, { specialist_id: 'gap-triager@1',
    specialist: completionCatalog.specialists.find((item) => item.id === 'gap-triager@1')! });
}

export function gapTriagerResult(id = 'inv-gap-triager'): InvestigationResult {
  const result = completionResult(id);
  result.findings = result.findings.slice(0, 1).map((finding) => ({ ...finding, claim_kind: 'proposed_design',
    title: 'Verify the guarded boundary before treating the gap as closed',
    statement: 'Inspect the admitted source behind this recorded gap and add a test for both outcomes before deciding whether the boundary is closed. The gap remains unresolved here.' }));
  return result;
}
