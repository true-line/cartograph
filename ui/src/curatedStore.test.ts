import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { curatedRecordStatuses } from './curated';
import {
  CURATED_REVISION, ELIGIBLE_RECORD, NOT_COMPUTED_READ, STALE_RECORD, curatedReadFixture,
} from './curatedFixtures';
import { useAppStore, type CuratedRead, type SpecBundle, type StagedProposal } from './store';

// SPEC-12 slice 3, AC-0655: the Workbench shows one curated revision. The
// host returns the curated read inside the `export_spec_with_views` response,
// made from the projection the bundle's `curated_basis` came from, so the
// store publishes both from one response and makes no second read.

const bundle = (revision?: string): SpecBundle => ({
  mode: 'best-effort', artifacts: [], assertion_count: 0, gap_count: 0, drift_count: 0, security_count: 0,
  ...(revision ? { curated_basis: { revision, counts: curatedReadFixture().counts } } : {}),
});

/** A core that answers each spec compile from a queue, in call order. */
function core(responses: { bundle: SpecBundle; curated?: unknown }[]) {
  const commands: string[] = [];
  mockIPC((command) => {
    commands.push(command);
    if (command === 'export_spec_with_views') return { ...responses.shift(), views: null };
    throw new Error(`unexpected ${command}`);
  });
  return commands;
}

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ specBundle: null, specViews: null, specCurated: null, specMode: 'best-effort', specError: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

describe('curated read carried with the bundle (AC-0655)', () => {
  it('publishes the bundle and its curated read from one response, with no second read', async () => {
    const commands = core([{ bundle: bundle(CURATED_REVISION), curated: curatedReadFixture() }]);
    await useAppStore.getState().reloadSpec();
    const state = useAppStore.getState();
    expect(state.specCurated).toEqual(curatedReadFixture());
    expect(state.specBundle?.curated_basis?.revision).toBe((state.specCurated as CuratedRead & { revision: string }).revision);
    expect(commands).toEqual(['export_spec_with_views']);
    expect(state.specBusy).toBe(false);
  });

  it('publishes a read that was not computed with its reason', async () => {
    core([{ bundle: bundle(), curated: NOT_COMPUTED_READ }]);
    await useAppStore.getState().reloadSpec();
    expect(useAppStore.getState().specCurated).toEqual(NOT_COMPUTED_READ);
    expect(useAppStore.getState().specBundle?.curated_basis).toBeUndefined();
  });

  it('leaves the section unread when the response carries no read or a malformed one', async () => {
    core([{ bundle: bundle(CURATED_REVISION) }, { bundle: bundle(CURATED_REVISION), curated: { status: 'other' } }]);
    await useAppStore.getState().reloadSpec();
    expect(useAppStore.getState().specCurated).toBeNull();
    expect(useAppStore.getState().specBundle?.curated_basis?.revision).toBe(CURATED_REVISION);
    await useAppStore.getState().reloadSpec();
    expect(useAppStore.getState().specCurated).toBeNull();
  });

  it('replaces the read with the one for the new mode when the export mode changes', async () => {
    const commands = core([
      { bundle: bundle(CURATED_REVISION), curated: curatedReadFixture() },
      { bundle: { ...bundle(CURATED_REVISION), mode: 'verified-only' }, curated: curatedReadFixture('verified-only') },
    ]);
    await useAppStore.getState().reloadSpec();
    await useAppStore.getState().setSpecMode('verified-only');
    const state = useAppStore.getState();
    expect(state.specMode).toBe('verified-only');
    expect(state.specCurated).toMatchObject({ export_mode: 'verified-only', items: [], items_omitted: 1 });
    expect(commands).toEqual(['export_spec_with_views', 'export_spec_with_views']);
  });

  it('reloads the views after a proposal review is recorded', async () => {
    const saved: StagedProposal = {
      schema_version: 1, proposal_id: STALE_RECORD.record_id, gap_id: 'gap', source_id: 'source', target_id: 'target',
      edge_label: 'CALLS', annotation: 'Saved rationale.', basis_hash: 'b'.repeat(64),
      evidence_binding: 'working_tree_unverified', context_status: 'awaiting_reconciliation',
      review_revision: 0, review_decision: null, review_note: null, created_at: 'created', reviewed_at: null,
      provenance: { tier: 'Agentic', confidence_tier: 'InferredWeak', evidence: [], extractor_id: 'broker', content_hash: 'c'.repeat(64) },
    };
    const commands: string[] = [];
    mockIPC((command) => {
      commands.push(command);
      if (command === 'record_agent_decision') {
        return { ...saved, review_revision: 1, review_decision: 'accepted', reviewed_at: 'reviewed' };
      }
      return { bundle: bundle(CURATED_REVISION), views: null, curated: curatedReadFixture() };
    });
    expect(await useAppStore.getState().recordProposalDecision(saved, 'accepted')).toBe(true);
    await vi.waitFor(() => expect(useAppStore.getState().specCurated).toEqual(curatedReadFixture()));
    expect(commands).toEqual(['record_agent_decision', 'export_spec_with_views']);
    // The record's state for the history and the review dialog is the host's.
    expect(curatedRecordStatuses(useAppStore.getState().specCurated).get(saved.proposal_id)).toEqual({
      state: 'stale', reason_sentence: STALE_RECORD.reason_sentence,
    });
  });
});

describe('record states from a curated read (AC-0655)', () => {
  it('lists only the records the host returned', () => {
    const best = curatedRecordStatuses(curatedReadFixture());
    expect(best.get(ELIGIBLE_RECORD.record_id)?.state).toBe('eligible');
    expect(best.size).toBe(4);
    // A verified-only read returns no item, so no state is made up for it.
    const verified = curatedRecordStatuses(curatedReadFixture('verified-only'));
    expect(verified.has(ELIGIBLE_RECORD.record_id)).toBe(false);
    expect(verified.size).toBe(3);
    expect(curatedRecordStatuses(NOT_COMPUTED_READ).size).toBe(0);
    expect(curatedRecordStatuses(null).size).toBe(0);
  });
});

describe('withdrawing an accepted proposal (AC-1020)', () => {
  const accepted: StagedProposal = {
    schema_version: 1, proposal_id: STALE_RECORD.record_id, gap_id: 'gap', source_id: 'source', target_id: 'target',
    edge_label: 'CALLS', annotation: 'Saved rationale.', basis_hash: 'b'.repeat(64),
    evidence_binding: 'working_tree_unverified', context_status: 'awaiting_reconciliation',
    review_revision: 1, review_decision: 'accepted', review_note: null, created_at: 'created', reviewed_at: 'reviewed',
    provenance: { tier: 'Agentic', confidence_tier: 'InferredWeak', evidence: [], extractor_id: 'broker', content_hash: 'c'.repeat(64) },
  };

  it('rejects the opened accepted record at its current revision and reloads the views', async () => {
    const calls: { command: string; args: unknown }[] = [];
    mockIPC((command, args) => {
      calls.push({ command, args });
      if (command === 'record_agent_decision') {
        return { ...accepted, review_revision: 2, review_decision: 'rejected' };
      }
      return { bundle: bundle(CURATED_REVISION), views: null, curated: curatedReadFixture() };
    });
    useAppStore.getState().openStagedProposal(accepted);
    await useAppStore.getState().withdrawProposal();
    await vi.waitFor(() => expect(calls.map((call) => call.command)).toEqual(['record_agent_decision', 'export_spec_with_views']));
    expect(calls[0].args).toMatchObject({ proposalId: accepted.proposal_id, expectedRevision: 1, decision: 'rejected' });
    const escalation = useAppStore.getState().escalation;
    expect(escalation?.decided).toBe('rejected');
    expect(escalation?.reviewing).toBe(false);
  });

  it('does nothing for a record that is not accepted', async () => {
    const calls: string[] = [];
    mockIPC((command) => {
      calls.push(command);
      return null;
    });
    useAppStore.getState().openStagedProposal({ ...accepted, review_revision: 0, review_decision: null, reviewed_at: null });
    await useAppStore.getState().withdrawProposal();
    expect(calls).toEqual([]);
  });
});
