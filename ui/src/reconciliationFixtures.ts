import type { ReconciliationHistory, ReconciliationResult } from './reconciliation';

export const reconciliationResult = (
  id: string, outcome: ReconciliationResult['outcome'] = 'reconciled', job = 7,
): ReconciliationResult => ({
  schema_version: 1, result_id: `reconcile-v1:${id}`, proposal_id: 'proposal:fixture',
  proposal_schema_version: 2,
  recovery: { job_id: job, graph_snapshot_id: 'snapshot:now', captures: [{ source_id: 'src_a', capture_id: 'capture-v1:a' }] },
  method: 'captured_span_v2',
  slot: { source: 'present', gap: 'present', relation: 'present' },
  evidence: outcome === 'changed'
    ? [{ evidence_id: 'E1', outcome: { kind: 'changed', reason: 'span_bytes_differ' } }]
    : [{ evidence_id: 'E1', outcome: { kind: 'retained_match' } }],
  candidates: [{ node_id: 'fn:target', outcome: { kind: 'matched' } }],
  outcome,
});

export const reconciliationHistory = (
  state: ReconciliationHistory['state'], results: ReconciliationResult[] = [],
): ReconciliationHistory => ({
  proposal_id: 'proposal:fixture', state,
  results: results.map((result, index) => ({ result, recorded_at: `2026-11-23 2${index}:00:00` })),
});
