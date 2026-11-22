import { describe, expect, it } from 'vitest';
import supportedWire from './fixtures/completion-v7-supported.json';
import queryHistoryWire from './fixtures/completion-v9-supported.json';
import terminalWire from './fixtures/completion-v7-terminal.json';
import insufficientWire from './fixtures/completion-v7-insufficient.json';
import type { InvestigationDetail, InvestigationObligationObservation, InvestigationResult } from './investigationTypes';
import { queryHistoryDetail, queryHistoryResult, expandedCompletionCatalog, completionDetail, completionResult,
  analyst27Correction, gapTriagerDetail, gapTriagerResult, specWriterDetail, specWriterResult, specWriterV2Detail, specWriterV2Result } from './completionFixtures';
import { validCompletionDetail, validCompletionResult, validCompletionSummary } from './investigationCompletion';

describe('V7 completion wire coherence (AC-0266)', () => {
  it.each([['supported', supportedWire], ['insufficient', insufficientWire]])('accepts actual durable worker %s wire projections', (_name, wire) => {
    const detail = wire.detail as unknown as InvestigationDetail;
    const result = wire.result as unknown as InvestigationResult;
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionResult(result.completion, result)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
    const changed = structuredClone(result); changed.completion!.request_hash = 'f'.repeat(64);
    expect(validCompletionDetail(detail.completion, detail, changed)).toBe(false);
  });
  it('accepts actual durable worker terminal observations without inventing a result', () => {
    const detail = terminalWire.detail as unknown as InvestigationDetail;
    expect(terminalWire.result).toBeNull();
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, completionResult())).toBe(false);
    const changed = structuredClone(detail); changed.completion!.refusal_count = 1;
    expect(validCompletionDetail(changed.completion, changed, null)).toBe(false);
  });
  it('keeps execution, accepted outcome and actual finishing assessment bound together', () => {
    const detail = completionDetail(); const result = completionResult();
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionResult(result.completion, result)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
    for (const field of ['request_hash', 'obligations_hash', 'finishing_attempts_hash'] as const) {
      const changed = structuredClone(result); changed.completion![field] = 'f'.repeat(64);
      expect(validCompletionDetail(detail.completion, detail, changed)).toBe(false);
    }
  });
  it('accepts insufficient results after unavailable attempts without inventing acquisition', () => {
    const detail = completionDetail(); const result = completionResult();
    detail.completion_outcome = 'insufficient_evidence'; result.completion!.outcome = 'insufficient_evidence';
    result.findings = []; result.knowledge_completeness = 'insufficient_evidence';
    detail.completion!.assessment.forEach((row) => { row.acquired = false; row.exactly_supplied = false; row.last_attempt_outcome = 'unavailable'; });
    result.completion!.assessment = structuredClone(detail.completion!.assessment);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
    result.completion!.assessment[0].attempted = false;
    expect(validCompletionResult(result.completion, result)).toBe(false);
  });
  it('refuses accepted supported claims with omitted evidence, mismatched context or missing result', () => {
    const detail = completionDetail(); const result = completionResult();
    const omitted = structuredClone(result); omitted.completion!.assessment[0].exactly_supplied = false;
    expect(validCompletionResult(omitted.completion, omitted)).toBe(false);
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(false);
    const inventory = detail.completion!.obligations.obligations.find((item) => item.kind === 'inventory')!;
    if (inventory.kind === 'inventory') inventory.target.context_id = `context-v2:${'f'.repeat(64)}`;
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(false);
  });
  // AC-1430 (T-1430): a capacity-disclosed row is accepted only on a supported
  // result that names it in a top-level limitation, and only as a never-read gap.
  it('accepts a capacity-disclosed obligation only when the limitations name it', () => {
    const detail = completionDetail(); const result = completionResult();
    const disclose = (row: InvestigationObligationObservation) => {
      row.attempted = false; row.acquired = false; row.exactly_supplied = false; row.unmet = null;
      delete row.last_attempt_outcome; row.capacity_disclosed = true;
    };
    const unnamed = structuredClone(result); disclose(unnamed.completion!.assessment[0]);
    expect(validCompletionResult(unnamed.completion, unnamed)).toBe(false);
    const named = structuredClone(unnamed);
    named.limitations = [...named.limitations, `${named.completion!.assessment[0].obligation_id.slice(0, 12)} was not read (capacity).`];
    expect(validCompletionResult(named.completion, named)).toBe(true);
    const shown = structuredClone(detail); disclose(shown.completion!.assessment[0]);
    expect(validCompletionDetail(shown.completion, shown, named)).toBe(true);
    const attempted = structuredClone(named);
    attempted.completion!.assessment[0].attempted = true; attempted.completion!.assessment[0].last_attempt_outcome = 'unavailable';
    expect(validCompletionResult(attempted.completion, attempted)).toBe(false);
    const flagFalse = structuredClone(named);
    (flagFalse.completion!.assessment[0] as unknown as Record<string, unknown>).capacity_disclosed = false;
    expect(validCompletionResult(flagFalse.completion, flagFalse)).toBe(false);
  });
  it('retains the actual first-refusal assessment while the next step is pending', () => {
    const detail = completionDetail('inv-completion', { status: 'awaiting_consent', has_result: false, completion_outcome: undefined, obligation_status: 'unmet' });
    detail.completion!.refusal_count = 1;
    detail.completion!.assessment[0].unmet = 'acquired_not_supplied';
    detail.completion!.assessment[0].exactly_supplied = false;
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
    detail.status = 'running'; expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
    detail.completion!.refusal_count = 0; expect(validCompletionDetail(detail.completion, detail, null)).toBe(false);
    detail.completion!.refusal_count = 2; expect(validCompletionDetail(detail.completion, detail, null)).toBe(false);
    detail.completion!.refusal_count = 1; detail.status = 'failed';
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(false);
  });
  it('keeps host exhaustion result-free and separate from ordinary cancellation', () => {
    const detail = completionDetail('inv-completion', { status: 'failed', has_result: false, completion_outcome: 'obligations_unmet', obligation_status: 'unmet' });
    detail.completion!.refusal_count = 2;
    detail.completion!.assessment[0].unmet = 'acquired_not_supplied'; detail.completion!.assessment[0].exactly_supplied = false;
    detail.completion!.assessment_basis = { kind: 'terminal_observation', finishing_attempts_hash: 'e'.repeat(64), last_refusal_id: 'f'.repeat(64),
      last_refusal_basis: { step_id: 'finish:2', input_ledger_hash: 'c'.repeat(64), disclosure_hash: `investigation-disclosure-v4:${'d'.repeat(64)}`, finish_hash: 'f'.repeat(64) } };
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, completionResult())).toBe(false);
    detail.status = 'cancelled'; expect(validCompletionSummary(detail)).toBe(false);
    delete detail.completion_outcome; detail.completion!.refusal_count = 1;
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
  });
  it('rejects unknown/aliased completion fields and preserves not-assessed running state', () => {
    const detail = completionDetail('inv-completion', { status: 'running', has_result: false, completion_outcome: undefined, obligation_status: 'not_assessed' });
    detail.completion!.assessment_basis = { kind: 'not_assessed' }; detail.completion!.assessment = [];
    expect(validCompletionDetail(detail.completion, detail, null)).toBe(true);
    expect(validCompletionSummary({ ...detail, inspection_status: 'unmet' })).toBe(false);
    expect(validCompletionSummary(Object.assign(structuredClone(detail), { completion_obligations: detail.completion!.obligations }))).toBe(false);
    expect(validCompletionDetail(Object.assign(structuredClone(detail.completion!), { extra: true }), detail, null)).toBe(false);
    const wrongFamily = completionResult(); wrongFamily.result_id = `investigation-result-v4:${'7'.repeat(64)}`;
    expect(validCompletionResult(wrongFamily.completion, wrongFamily)).toBe(false);
    const result = completionResult(); Object.assign(result.completion!, { extra: true });
    expect(validCompletionResult(result.completion, result)).toBe(false);
  });
});

// AC-0404: authored messages require the supported saved-result contract and cited proposals.
it('accepts only a supported Spec Writer v2 narrative bound to its cited proposal', () => {
  const detail = specWriterV2Detail(); const result = specWriterV2Result();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionResult(result.completion, result)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);

  const missing = structuredClone(result); delete missing.narrative;
  expect(validCompletionResult(missing.completion, missing)).toBe(false);
  const foreign = structuredClone(result); foreign.narrative!.finding_ids = ['foreign-finding'];
  expect(validCompletionResult(foreign.completion, foreign)).toBe(false);
  const duplicate = structuredClone(result); duplicate.narrative!.finding_ids = [result.findings[0].finding_id, result.findings[0].finding_id];
  expect(validCompletionResult(duplicate.completion, duplicate)).toBe(false);
  const uncited = structuredClone(result); uncited.findings[0].citation_ids = [];
  expect(validCompletionResult(uncited.completion, uncited)).toBe(false);

  const insufficient = structuredClone(result);
  insufficient.completion!.outcome = 'insufficient_evidence'; insufficient.findings = [];
  insufficient.knowledge_completeness = 'insufficient_evidence';
  expect(validCompletionResult(insufficient.completion, insufficient)).toBe(false);
  delete insufficient.narrative;
  expect(validCompletionResult(insufficient.completion, insufficient)).toBe(true);

  const oldRoleNarrative = { ...specWriterResult(), narrative: { text: 'unsupported', finding_ids: ['F1'] } };
  expect(validCompletionResult(oldRoleNarrative.completion, oldRoleNarrative)).toBe(false);
});

it('accepts Spec Writer only as its exact schema-four completion identity (AC-0382)', () => {
  const detail = specWriterDetail();
  const result = specWriterResult();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionResult(result.completion, result)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  expect(result.findings.every((finding) => finding.claim_kind === 'proposed_design')).toBe(true);
  expect(validCompletionSummary({ ...detail, specialist_id: 'spec-writer@3' as InvestigationDetail['specialist_id'] })).toBe(false);
  expect(validCompletionSummary({ ...detail, schema_version: 5 })).toBe(false);
  expect(validCompletionSummary({ ...detail, specialist_id: 'domain-analyst@8' })).toBe(false);
});

it('accepts Gap Triager only as its exact schema-four completion identity (AC-0384)', () => {
  const detail = gapTriagerDetail();
  const result = gapTriagerResult();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionResult(result.completion, result)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  expect(result.findings.every((finding) => finding.claim_kind === 'proposed_design')).toBe(true);
  expect(validCompletionSummary({ ...detail, specialist_id: 'gap-triager@10' as InvestigationDetail['specialist_id'] })).toBe(false);
  expect(validCompletionSummary({ ...detail, schema_version: 5 })).toBe(false);
  expect(validCompletionSummary({ ...detail, specialist_id: 'domain-analyst@8' })).toBe(false);
});


it('binds expanded saved history to its own role, envelope and profile (AC-0281)', () => {
  const detail = completionDetail(undefined, { schema_version: 5, specialist_id: 'domain-analyst@8',
    limits: expandedCompletionCatalog.policies![0].limits, last_event_sequence: 227 });
  detail.completion!.obligations.schema_version = 2;
  detail.completion!.obligations.contract = 'investigation-completion-obligations@2';
  const result = completionResult();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  detail.limits = { ...detail.limits, profile: 'investigation-v1' };
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(false);
  detail.schema_version = 4;
  expect(validCompletionSummary(detail)).toBe(false);
});


it('keeps query-history role, summary, result and disclosure domains distinct (AC-0286)', () => {
  const detail = queryHistoryDetail(); const result = queryHistoryResult();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionResult(result.completion, result, 56)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  expect(validCompletionSummary({ ...detail, specialist_id: 'domain-analyst@8' })).toBe(false);
  expect(validCompletionSummary({ ...detail, schema_version: 5 })).toBe(false);
  expect(validCompletionDetail(detail.completion, detail, completionResult(detail.investigation_id))).toBe(false);
  expect(validCompletionResult(result.completion, { ...result, result_id: result.result_id.replace('-v6:', '-v5:') }, 56)).toBe(false);
  expect(validCompletionResult(result.completion, { ...result, disclosure_hash: result.disclosure_hash!.replace('-v5:', '-v4:') }, 56)).toBe(false);
});


it('accepts actual production mock-worker query-history wire and rejects older disclosure reuse (AC-0286)', () => {
  const detail = queryHistoryWire.detail as unknown as InvestigationDetail;
  const result = queryHistoryWire.result as unknown as InvestigationResult;
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionResult(result.completion, result, 56)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  const changed = structuredClone(detail);
  if (changed.completion!.assessment_basis.kind !== 'finishing_disclosure') throw new Error('Expected supported finishing wire');
  changed.completion!.assessment_basis.disclosure_hash = changed.completion!.assessment_basis.disclosure_hash.replace('-v5:', '-v4:');
  expect(validCompletionDetail(changed.completion, changed, result)).toBe(false);
});

// AC-0287 / #1705: scripted transport validation, not durable host authentication.
it('requires a complete text-free partition for an explicit partial result', () => {
  const result = structuredClone(queryHistoryWire.result) as unknown as InvestigationResult;
  result.schema_version = 7;
  result.result_id = `investigation-result-v7:${'a'.repeat(64)}`;
  result.disclosure_hash = `investigation-disclosure-v6:${'b'.repeat(64)}`;
  result.inspection_assessment!.disclosure_hash = result.disclosure_hash;
  result.knowledge_completeness = 'partial';
  const completion = result.completion!;
  completion.schema_version = 2;
  completion.outcome = 'findings_withheld_after_correction';
  completion.withholding = {
    schema_version: 1, detector_version: 1, disposition: 'findings_withheld_after_correction',
    original_finish_hash: '1'.repeat(64), original_response_hash: '2'.repeat(64),
    request_hash: completion.request_hash, obligations_hash: completion.obligations_hash,
    input_ledger_hash: result.input_ledger_hash, disclosure_hash: result.disclosure_hash,
    prior_correction_witness_hash: '3'.repeat(64), projected_result_hash: '4'.repeat(64),
    original_finding_count: result.findings.length + 1, withheld_indexes: [0],
    survivor_indexes: result.findings.map((_, index) => index + 1),
  };
  expect(validCompletionResult(completion, result)).toBe(true);
  for (const mutate of [
    (r: InvestigationResult) => { delete r.completion!.withholding; },
    (r: InvestigationResult) => { r.completion!.outcome = 'supported'; },
    (r: InvestigationResult) => { r.completion!.withholding!.survivor_indexes = [0]; },
    (r: InvestigationResult) => { r.completion!.withholding!.input_ledger_hash = 'f'.repeat(64); },
    (r: InvestigationResult) => { r.completion!.assessment[0].exactly_supplied = false; },
  ]) {
    const changed = structuredClone(result); mutate(changed);
    expect(validCompletionResult(changed.completion, changed)).toBe(false);
  }
});

// AC-0574: both correction routes project the same authenticated final wire.
describe('desktop analyst27 corrected results', () => {
  it.each([false, true])('accepts corrected result with withholding=%s and rejects changed bindings', (partial) => {
    const { detail, result } = analyst27Correction(partial);
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionResult(result.completion, result)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
    const changed = structuredClone(result); changed.completion!.request_hash = 'f'.repeat(64);
    expect(validCompletionDetail(detail.completion, detail, changed)).toBe(false);
  });
  it('retains saved analyst26 partial-result support without creating it', () => {
    const { detail, result } = analyst27Correction(true);
    detail.specialist_id = 'domain-analyst@26';
    detail.specialist = { ...detail.specialist!, id: 'domain-analyst@26', version: 26 };
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  });
});
