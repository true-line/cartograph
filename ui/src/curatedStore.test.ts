import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { curatedRecordStatuses } from './curated';
import {
  CURATED_REVISION, ELIGIBLE_RECORD, NOT_COMPUTED_READ, STALE_RECORD, curatedReadFixture,
} from './curatedFixtures';
import { useAppStore, type CuratedRead, type SpecBundle, type StagedProposal } from './store';

// SPEC-12 slice 3, AC-0655: the Workbench shows one curated revision. The
// bundle's `curated_basis` is its source; a curated read is published only
// when it reports that revision.

const bundle = (revision?: string): SpecBundle => ({
  mode: 'best-effort', artifacts: [], assertion_count: 0, gap_count: 0, drift_count: 0, security_count: 0,
  ...(revision ? { curated_basis: { revision, counts: curatedReadFixture().counts } } : {}),
});

/** A core that answers the two spec reads from queues, in call order. */
function core(bundles: SpecBundle[], reads: (CuratedRead | Error)[]) {
  const commands: string[] = [];
  mockIPC((command) => {
    commands.push(command);
    if (command === 'export_spec_with_views') return { bundle: bundles.shift(), views: null };
    if (command === 'read_curated_projection') {
      const read = reads.shift();
      if (read instanceof Error) throw read;
      return read;
    }
    throw new Error(`unexpected ${command}`);
  });
  return commands;
}

const SPEC_READS = ['export_spec_with_views', 'read_curated_projection'];

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ specBundle: null, specViews: null, specCurated: null, specMode: 'best-effort', specError: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

describe('curated read pairing (AC-0655)', () => {
  it('publishes a read that reports the revision the bundle carries', async () => {
    const commands = core([bundle(CURATED_REVISION)], [curatedReadFixture()]);
    await useAppStore.getState().reloadSpec();
    const state = useAppStore.getState();
    expect(state.specCurated).toEqual(curatedReadFixture());
    expect(state.specBundle?.curated_basis?.revision).toBe(CURATED_REVISION);
    expect(commands).toEqual(SPEC_READS);
    expect(state.specBusy).toBe(false);
  });

  it('reads the pair once more when a review lands between the two reads', async () => {
    const moved = `curated-revision-v1:${'1'.repeat(64)}`;
    const commands = core(
      [bundle(CURATED_REVISION), bundle(moved)],
      [{ ...curatedReadFixture(), revision: moved }, { ...curatedReadFixture(), revision: moved }],
    );
    await useAppStore.getState().reloadSpec();
    const state = useAppStore.getState();
    expect(state.specBundle?.curated_basis?.revision).toBe(moved);
    expect(state.specCurated).toMatchObject({ status: 'computed', revision: moved });
    expect(commands).toEqual([...SPEC_READS, ...SPEC_READS]);
  });

  it('never shows a read beside a bundle of another revision, and does not poll', async () => {
    const other = `curated-revision-v1:${'2'.repeat(64)}`;
    const commands = core(
      [bundle(CURATED_REVISION), bundle(CURATED_REVISION)],
      [{ ...curatedReadFixture(), revision: other }, { ...curatedReadFixture(), revision: other }],
    );
    await useAppStore.getState().reloadSpec();
    const state = useAppStore.getState();
    expect(state.specCurated).toEqual({ status: 'changed_while_reading' });
    expect(state.specBundle?.curated_basis?.revision).toBe(CURATED_REVISION);
    expect(commands).toEqual([...SPEC_READS, ...SPEC_READS]);
  });

  it('pairs a read that was not computed only with a bundle that has no curated basis', async () => {
    core([bundle()], [NOT_COMPUTED_READ]);
    await useAppStore.getState().reloadSpec();
    expect(useAppStore.getState().specCurated).toEqual(NOT_COMPUTED_READ);

    core([bundle(CURATED_REVISION), bundle(CURATED_REVISION)], [NOT_COMPUTED_READ, NOT_COMPUTED_READ]);
    await useAppStore.getState().reloadSpec();
    expect(useAppStore.getState().specCurated).toEqual({ status: 'changed_while_reading' });
  });

  it('keeps the bundle and leaves the section unread when the curated read fails', async () => {
    const commands = core([bundle(CURATED_REVISION)], [new Error('core busy')]);
    await useAppStore.getState().reloadSpec();
    const state = useAppStore.getState();
    expect(state.specCurated).toBeNull();
    expect(state.specBundle?.curated_basis?.revision).toBe(CURATED_REVISION);
    expect(state.specError).toBeNull();
    expect(commands).toEqual(SPEC_READS);
  });

  it('re-reads the pair for the new mode when the export mode changes', async () => {
    const commands = core([{ ...bundle(CURATED_REVISION), mode: 'verified-only' }], [curatedReadFixture('verified-only')]);
    await useAppStore.getState().setSpecMode('verified-only');
    const state = useAppStore.getState();
    expect(state.specMode).toBe('verified-only');
    expect(state.specCurated).toMatchObject({ export_mode: 'verified-only', items: [], items_omitted: 1 });
    expect(commands).toEqual(SPEC_READS);
  });

  it('re-reads the pair after a proposal review is recorded', async () => {
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
      if (command === 'export_spec_with_views') return { bundle: bundle(CURATED_REVISION), views: null };
      return curatedReadFixture();
    });
    expect(await useAppStore.getState().recordProposalDecision(saved, 'accepted')).toBe(true);
    await vi.waitFor(() => expect(useAppStore.getState().specCurated).toEqual(curatedReadFixture()));
    expect(commands).toEqual(['record_agent_decision', ...SPEC_READS]);
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
    expect(curatedRecordStatuses({ status: 'changed_while_reading' }).size).toBe(0);
    expect(curatedRecordStatuses(null).size).toBe(0);
  });
});
