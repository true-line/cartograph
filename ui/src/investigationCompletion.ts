import type { InvestigationCompletionDetail, InvestigationCompletionResult, InvestigationDetail,
  InvestigationObligationObservation, InvestigationResult, InvestigationSummary } from './investigationTypes';
import { validObligationEnvelope } from './investigationObligations';

const hash = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{64}$/.test(v);
const step = (v: unknown): v is string => typeof v === 'string' && /^[A-Za-z0-9_:-]{1,128}$/.test(v);
const ledger = hash;
const disclosure = (v: unknown) => typeof v === 'string' && /^investigation-disclosure-v4:[0-9a-f]{64}$/.test(v);
const fields = (v: object, required: string[], optional: string[] = []) => required.every((key) => key in v) &&
  Object.keys(v).every((key) => required.includes(key) || optional.includes(key));
const observations = (rows: InvestigationObligationObservation[]) => Array.isArray(rows) && rows.length <= 12 &&
  new Set(rows.map((row) => row.obligation_id)).size === rows.length && rows.every((row) => row &&
    fields(row, ['obligation_id', 'kind', 'attempted', 'acquired', 'exactly_supplied', 'unmet'], ['last_attempt_outcome']) &&
    hash(row.obligation_id) && ['source', 'inventory'].includes(row.kind) &&
    [row.attempted, row.acquired, row.exactly_supplied].every((v) => typeof v === 'boolean') &&
    (!row.exactly_supplied || row.acquired) &&
    (row.unmet === null || ['not_attempted', 'acquired_not_supplied', 'redacted', 'hash_mismatch', 'unavailable'].includes(row.unmet)) &&
    (row.last_attempt_outcome === undefined ? !row.attempted : row.attempted &&
      ['acquired', 'unavailable', 'firewall_refused'].includes(row.last_attempt_outcome)));

export function validCompletionResult(value: InvestigationCompletionResult | undefined, result: InvestigationResult): boolean {
  if (result.schema_version !== 5 || !/^investigation-result-v5:[0-9a-f]{64}$/.test(result.result_id) || !hash(result.input_ledger_hash) ||
      !/^context-v1:[0-9a-f]{64}$/.test(result.graph_snapshot_id) || !fields(result, ['schema_version', 'completion', 'coverage', 'investigation_id', 'result_id', 'input_ledger_hash', 'graph_snapshot_id', 'invocation_step_id', 'disclosure_hash', 'findings', 'knowledge_completeness', 'limitations', 'observed_response_model', 'created_at'], ['inspection_assessment']) ||
      !value || !fields(value, ['schema_version', 'outcome', 'request_hash', 'obligations_hash', 'finishing_attempts_hash', 'assessment']) ||
      value.schema_version !== 1 || !hash(value.request_hash) || !hash(value.obligations_hash) || !hash(value.finishing_attempts_hash) ||
      !observations(value.assessment) || !value.assessment.length || value.assessment.some((row) => row.unmet !== null)) return false;
  const inspection = result.inspection_assessment;
  if (inspection !== undefined && (!inspection || !fields(inspection, ['schema_version', 'requirements_hash', 'input_ledger_hash', 'finishing_step_id', 'disclosure_hash', 'requirements']) ||
      inspection.schema_version !== 1 || !hash(inspection.requirements_hash) || inspection.input_ledger_hash !== result.input_ledger_hash ||
      inspection.finishing_step_id !== result.invocation_step_id || inspection.disclosure_hash !== result.disclosure_hash ||
      !Array.isArray(inspection.requirements) || !inspection.requirements.length || inspection.requirements.length > 12 ||
      new Set(inspection.requirements.map((row) => row.target_id)).size !== inspection.requirements.length ||
      inspection.requirements.some((row) => !fields(row, ['target_id', 'target', 'status', 'acquired', 'supplied', 'cited']) ||
        row.target_id !== row.target.target_id || !hash(row.target_id) || row.target.graph_snapshot_id !== result.graph_snapshot_id ||
        [row.acquired, row.supplied, row.cited].some((v) => typeof v !== 'boolean') ||
        !(row.status === 'supplied' ? row.supplied && row.acquired : row.status === 'unmet' && !row.supplied)))) return false;
  if (value.outcome === 'supported') return result.findings.length > 0 && result.knowledge_completeness !== 'insufficient_evidence' &&
    value.assessment.every((row) => row.exactly_supplied);
  return value.outcome === 'insufficient_evidence' && result.findings.length === 0 && result.knowledge_completeness === 'insufficient_evidence' &&
    result.limitations.length > 0 && value.assessment.every((row) => row.attempted);
}

/** Completion axes never infer a result from a terminal execution state. */
export function validCompletionSummary(value: InvestigationSummary): boolean {
  if (!fields(value, ['schema_version', 'investigation_id', 'conversation_id', 'parent_id', 'job_id', 'specialist_id', 'question', 'scope', 'provider_mode', 'status', 'revision', 'cancel_requested', 'invocation_pending', 'actions', 'graph_snapshot_id', 'last_event_sequence', 'has_result', 'created_at', 'updated_at', 'obligation_status'],
      ['completion_outcome', 'completion', 'coverage_context', 'context_owner', 'origin', 'specialist', 'provider', 'scope_snapshot_id', 'limits', 'usage', 'citations', 'error']) ||
      !['queued', 'preparing', 'running', 'awaiting_consent', 'completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'].includes(value.status) ||
      value.schema_version !== 4 || !['domain-analyst@7', 'evidence-auditor@7'].includes(value.specialist_id) ||
      !['not_assessed', 'satisfied', 'unmet'].includes(value.obligation_status ?? '') ||
      ['inspection_status', 'inspection_requirement_count', 'inspection_requirements', 'inspection_assessment'].some((key) => key in value)) return false;
  if (value.completion_outcome === undefined) return !value.has_result && value.status !== 'completed';
  if (value.completion_outcome === 'obligations_unmet') return value.status === 'failed' && !value.has_result && value.obligation_status === 'unmet';
  return ['supported', 'insufficient_evidence'].includes(value.completion_outcome) && value.status === 'completed' && value.has_result &&
    value.obligation_status === 'satisfied';
}
export function validCompletionDetail(value: InvestigationCompletionDetail | undefined, detail: InvestigationDetail,
  result: InvestigationResult | null): boolean {
  if (!['completion', 'context_owner', 'origin', 'specialist', 'provider', 'scope_snapshot_id', 'limits', 'usage', 'citations', 'error'].every((key) => key in detail) ||
      !value || !fields(value, ['schema_version', 'request_hash', 'obligations_hash', 'obligations', 'refusal_count', 'assessment_basis', 'assessment']) ||
      value.schema_version !== 1 || !hash(value.request_hash) || !hash(value.obligations_hash) || !validObligationEnvelope(value.obligations) ||
      !Number.isSafeInteger(value.refusal_count) || value.refusal_count < 0 || value.refusal_count > 2 || !observations(value.assessment)) return false;
  if (value.obligations.obligations.some((item) => item.kind === 'source'
    ? detail.graph_snapshot_id !== null && item.target.graph_snapshot_id !== detail.graph_snapshot_id
    : detail.coverage_context !== undefined && (item.target.context_id !== detail.coverage_context.context_id ||
      !detail.coverage_context.coverage.repos.some((repo) => repo.repo === item.target.repo && repo.status === 'recorded' && repo.inventory_hash === item.target.inventory_hash)))) return false;
  if (detail.obligation_status === 'satisfied' && value.assessment.some((row) => row.unmet !== null)) return false;
  if (detail.obligation_status === 'unmet' && !value.assessment.some((row) => row.unmet !== null)) return false;
  if (result?.inspection_assessment) {
    const sources = value.obligations.obligations.filter((item) => item.kind === 'source');
    if (result.inspection_assessment.requirements.length !== sources.length || sources.some((item) =>
      !result.inspection_assessment!.requirements.some((row) => JSON.stringify(row.target) === JSON.stringify(item.target)))) return false;
  }
  const basis = value.assessment_basis;
  if (!basis || typeof basis !== 'object') return false;
  if (basis.kind === 'not_assessed') return fields(basis, ['kind']) && !detail.has_result &&
    detail.completion_outcome === undefined && detail.obligation_status === 'not_assessed' && value.refusal_count === 0 && value.assessment.length === 0;
  if (value.assessment.length !== value.obligations.obligations.length || value.assessment.some((row, i) =>
      row.obligation_id !== value.obligations.obligations[i].obligation_id || row.kind !== value.obligations.obligations[i].kind)) return false;
  if (basis.kind === 'finishing_disclosure' && result === null) return     fields(basis, ['kind', 'step_id', 'input_ledger_hash', 'disclosure_hash', 'finishing_attempts_hash']) &&
    step(basis.step_id) && ledger(basis.input_ledger_hash) && disclosure(basis.disclosure_hash) && hash(basis.finishing_attempts_hash) &&
    !detail.has_result && detail.completion_outcome === undefined &&
    ['queued', 'preparing', 'running', 'awaiting_consent'].includes(detail.status) &&
    value.refusal_count === 1 && detail.obligation_status === 'unmet' && value.assessment.some((row) => row.unmet !== null);
  if (basis.kind === 'finishing_disclosure') return fields(basis, ['kind', 'step_id', 'input_ledger_hash', 'disclosure_hash', 'finishing_attempts_hash']) &&
    step(basis.step_id) && ledger(basis.input_ledger_hash) && disclosure(basis.disclosure_hash) && hash(basis.finishing_attempts_hash) &&
    Boolean(result && result.completion && result.schema_version === 5 && validCompletionResult(result.completion, result) &&
      result.completion?.outcome === detail.completion_outcome && result.completion.request_hash === value.request_hash &&
      result.completion.obligations_hash === value.obligations_hash && result.completion.finishing_attempts_hash === basis.finishing_attempts_hash &&
      result.invocation_step_id === basis.step_id && result.input_ledger_hash === basis.input_ledger_hash && result.disclosure_hash === basis.disclosure_hash &&
      JSON.stringify(result.completion.assessment) === JSON.stringify(value.assessment));
  if (basis.kind !== 'terminal_observation' || !fields(basis, ['kind', 'finishing_attempts_hash', 'last_refusal_id', 'last_refusal_basis']) ||
      !hash(basis.finishing_attempts_hash) || detail.has_result || result !== null ||
      !['failed', 'cancelled', 'interrupted', 'outcome_unknown'].includes(detail.status)) return false;
  if (basis.last_refusal_id === null) {
    if (basis.last_refusal_basis !== null || value.refusal_count !== 0) return false;
  } else {
    const refusal = basis.last_refusal_basis;
    if (!hash(basis.last_refusal_id) || !refusal || !fields(refusal, ['step_id', 'input_ledger_hash', 'disclosure_hash', 'finish_hash']) ||
        !step(refusal.step_id) || !ledger(refusal.input_ledger_hash) || !disclosure(refusal.disclosure_hash) || !hash(refusal.finish_hash) || value.refusal_count === 0) return false;
  }
  if (detail.completion_outcome === 'obligations_unmet') return value.refusal_count === 2 && value.assessment.some((row) => row.unmet !== null);
  return detail.completion_outcome === undefined && value.refusal_count < 2;
}
