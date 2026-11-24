import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type StagedProposal } from './store';
import { HISTORY_INCOMPLETE, RECONCILE_FAILED, requireReconciliationHistory } from './reconciliation';
import { reconciliationHistory, reconciliationResult } from './reconciliationFixtures';

const proposal = (id: string): StagedProposal => ({
  schema_version: 1, proposal_id: id, gap_id: 'gap', source_id: 'source', target_id: 'target',
  edge_label: 'CALLS', annotation: 'Saved rationale.', basis_hash: 'b'.repeat(64),
  evidence_binding: 'working_tree_unverified', context_status: 'awaiting_reconciliation',
  review_revision: 0, review_decision: null, review_note: null, created_at: 'original', reviewed_at: null,
  provenance: { tier: 'Agentic', confidence_tier: 'InferredWeak', evidence: [], extractor_id: 'broker', content_hash: 'c'.repeat(64) },
});

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((success) => { resolve = success; });
  return { promise, resolve };
}

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ escalation: null, stagedProposals: [] });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState({ escalation: null, stagedProposals: [] });
});

describe('staged proposal reconciliation (AC-1074)', () => {
  it('reconciles as one job, then reads every recorded result', async () => {
    const calls: [string, unknown][] = [];
    const older = reconciliationResult('a', 'reconciled', 3);
    const newer = reconciliationResult('b', 'changed', 9);
    mockIPC((command, args) => {
      calls.push([command, args]);
      return command === 'reconcile_staged'
        ? reconciliationHistory('changed', [newer])
        : reconciliationHistory('changed', [newer, older]);
    });
    const saved = proposal('proposal:fixture');
    const immutable = JSON.stringify(saved);
    useAppStore.getState().openStagedProposal(saved);
    await useAppStore.getState().reconcileStaged();
    expect(calls).toEqual([
      ['reconcile_staged', { proposalId: 'proposal:fixture' }],
      ['staged_reconciliation_history', { proposalId: 'proposal:fixture', limit: 64 }],
    ]);
    const view = useAppStore.getState().escalation?.reconciliation;
    expect(view?.history?.state).toBe('changed');
    expect(view?.history?.results.map((entry) => entry.result.recovery.job_id)).toEqual([9, 3]);
    expect(view?.reconciling).toBe(false);
    expect(JSON.stringify(useAppStore.getState().escalation?.proposal)).toBe(immutable);
  });

  it('keeps the new result and says the rest is missing when the follow-up read fails', async () => {
    mockIPC((command) => {
      if (command === 'reconcile_staged') return reconciliationHistory('reconciled', [reconciliationResult('a')]);
      throw new Error('history unavailable');
    });
    useAppStore.getState().openStagedProposal(proposal('proposal:fixture'));
    await useAppStore.getState().reconcileStaged();
    const view = useAppStore.getState().escalation?.reconciliation;
    expect(view?.history?.results).toHaveLength(1);
    expect(view?.error).toBe(HISTORY_INCOMPLETE);
  });

  it('a failed reconcile shows no earlier result as current', async () => {
    let calls = 0;
    mockIPC(() => {
      if (++calls === 1) return reconciliationHistory('reconciled', [reconciliationResult('a')]);
      throw new Error('private path must not reach the view');
    });
    useAppStore.getState().openStagedProposal(proposal('proposal:fixture'));
    await useAppStore.getState().loadReconciliation();
    expect(useAppStore.getState().escalation?.reconciliation?.history?.state).toBe('reconciled');
    await useAppStore.getState().reconcileStaged();
    const view = useAppStore.getState().escalation?.reconciliation;
    expect(view?.history).toBeNull();
    expect(view?.error).toBe(RECONCILE_FAILED);
  });

  it('drops a late answer for another selection', async () => {
    const first = deferred<unknown>();
    let calls = 0;
    mockIPC(() => ++calls === 1 ? first.promise
      : { ...reconciliationHistory('missing'), proposal_id: 'second' });
    useAppStore.getState().openStagedProposal(proposal('proposal:fixture'));
    const earlier = useAppStore.getState().loadReconciliation();
    useAppStore.getState().openStagedProposal(proposal('second'));
    await useAppStore.getState().loadReconciliation();
    first.resolve(reconciliationHistory('reconciled'));
    await earlier;
    const view = useAppStore.getState().escalation?.reconciliation;
    expect(view?.proposalId).toBe('second');
    expect(view?.history?.state).toBe('missing');
  });

  it('refuses an answer for another proposal or with an unknown outcome', () => {
    expect(() => requireReconciliationHistory(reconciliationHistory('reconciled'), 'other')).toThrow();
    const bad = reconciliationHistory('reconciled', [reconciliationResult('a')]);
    (bad.results[0].result.evidence[0].outcome as { kind: string }).kind = 'certified';
    expect(() => requireReconciliationHistory(bad, 'proposal:fixture')).toThrow();
    const reasonless = reconciliationHistory('changed', [reconciliationResult('a', 'changed')]);
    delete (reasonless.results[0].result.evidence[0].outcome as { reason?: string }).reason;
    expect(() => requireReconciliationHistory(reasonless, 'proposal:fixture')).toThrow();
    expect(requireReconciliationHistory(reconciliationHistory('reconciliation_stale'), 'proposal:fixture').state)
      .toBe('reconciliation_stale');
  });

  it('reads a result recorded for a v3 proposal (AC-1086)', () => {
    const v3 = { ...reconciliationResult('a'), proposal_schema_version: 3 as const };
    expect(requireReconciliationHistory(reconciliationHistory('reconciled', [v3]), 'proposal:fixture')
      .results[0].result.proposal_schema_version).toBe(3);
    const unknown = { ...reconciliationResult('a'), proposal_schema_version: 4 } as unknown as typeof v3;
    expect(() => requireReconciliationHistory(reconciliationHistory('reconciled', [unknown]), 'proposal:fixture'))
      .toThrow();
  });
});
