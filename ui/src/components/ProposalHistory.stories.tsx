import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ProposalHistory } from './ProposalHistory';
import type { StagedProposal } from '../store';
import { mixedTaskBasis } from '../taskBasisFixtures';
import { STALE_RECORD } from '../curatedFixtures';

const PENDING: StagedProposal = {
  proposal_id: 'proposal:pending',
  gap_id: 'gap:sync',
  source_id: 'sym:capture',
  target_id: 'sym:send',
  edge_label: 'CALLS',
  annotation: 'The captured task suggests this call target; review the cited evidence.',
  basis_hash: 'b'.repeat(64),
  review_revision: 0,
  review_decision: null,
  review_note: null,
  evidence_binding: 'working_tree_unverified',
  context_status: 'awaiting_reconciliation',
  created_at: '2026-09-10T12:00:00Z',
  reviewed_at: null,
  provenance: {
    tier: 'Agentic', confidence_tier: 'InferredWeak', evidence: [],
    extractor_id: 't3.agent', content_hash: 'c'.repeat(64),
  },
};

const meta = {
  title: 'Surfaces/ProposalHistory',
  component: ProposalHistory,
  args: {
    proposals: [PENDING, {
      ...PENDING, proposal_id: 'proposal:reviewed', annotation: 'An earlier reviewed proposal.',
      review_revision: 1, review_decision: 'accepted', reviewed_at: '2026-09-10T13:00:00Z',
    }],
    loading: false, error: null, hasMore: true,
    onRefresh: fn(), onLoadMore: fn(), onOpen: fn(),
  },
} satisfies Meta<typeof ProposalHistory>;
export default meta;
type Story = StoryObj<typeof meta>;

export const PendingAndReviewedHistory: Story = {
  // AC-0129: persisted pending and reviewed records share one honest review surface.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Pending review')).toBeInTheDocument();
    // AC-0655: without a curated read for the record, no state is made up.
    await expect(canvas.getByText('Accepted')).toBeInTheDocument();
    await expect(canvas.getByTestId('proposal-curated-state')).toHaveTextContent(
      'Its state is listed under Curated items in the Spec Workbench.',
    );
    await expect(canvas.queryByText(/reconciliation/i)).not.toBeInTheDocument();
    await expect(canvas.getAllByText('Inferred (weak)')).toHaveLength(2);
    await userEvent.click(canvas.getByRole('button', { name: 'Review proposal' }));
    await expect(args.onOpen).toHaveBeenCalledWith(PENDING);
    await userEvent.click(canvas.getByRole('button', { name: 'Load more proposals' }));
    await expect(args.onLoadMore).toHaveBeenCalledOnce();
    await userEvent.click(canvas.getByRole('button', { name: 'Refresh proposal history' }));
    await expect(args.onRefresh).toHaveBeenCalledOnce();
  },
};

export const FailedHistoryReadRetainsLoadedRows: Story = {
  // AC-0129: read failures are visible and never masquerade as an empty successful page.
  args: { error: 'Proposal history could not be loaded. Retry the read.' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('could not be loaded');
    await expect(canvas.getByText('Pending review')).toBeInTheDocument();
    await expect(canvas.queryByText('No saved proposals yet.')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Load more proposals' }));
    await expect(args.onLoadMore).toHaveBeenCalledOnce();
  },
};

export const MixedHistoryPreservesLegacyAndAcceptedOrigins: Story = {
  // AC-0179: current receipt coverage never relabels an older proposal's source.
  args: { proposals: [PENDING, { ...PENDING, schema_version: 2, proposal_id: 'proposal:captured',
    source_basis: mixedTaskBasis, evidence_binding: 'per_item', review_decision: 'accepted', review_revision: 1 }], hasMore: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Source binding unverified · legacy evidence')).toBeInTheDocument();
    await expect(canvas.getByText(/1 retained parser span\(s\) · 1 working-tree span\(s\), unverified/)).toBeInTheDocument();
    await expect(canvas.getByText('Selection limits or omissions apply.')).toBeInTheDocument();
    await expect(canvas.getByText('Accepted')).toBeInTheDocument();
    await expect(canvas.getAllByText('Inferred (weak)')).toHaveLength(2);
  },
};

export const AcceptedRecordShowsItsCuratedState: Story = {
  // AC-0655: an accepted record shows the state and reason sentence the
  // host's curated read gave it; a pending record shows none.
  args: {
    proposals: [PENDING, {
      ...PENDING, proposal_id: STALE_RECORD.record_id, review_revision: 1, review_decision: 'accepted',
      reviewed_at: '2026-09-10T13:00:00Z',
    }],
    recordStates: new Map([[STALE_RECORD.record_id, {
      state: STALE_RECORD.state, reason_sentence: STALE_RECORD.reason_sentence,
    }]]),
    hasMore: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Accepted · stale')).toBeInTheDocument();
    await expect(canvas.getAllByTestId('proposal-curated-state')).toHaveLength(1);
    await expect(canvas.getByTestId('proposal-curated-state')).toHaveTextContent(
      'The graph has changed since the proposal was made.',
    );
    // R-INT-2: the record keeps its InferredWeak badge whatever its state.
    await expect(canvas.getAllByText('Inferred (weak)')).toHaveLength(2);
  },
};
