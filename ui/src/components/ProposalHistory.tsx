import type { StagedProposal } from '../store';
import type { CuratedRecordStatus } from '../curated';
import { TierBadge } from './TierBadge';
import { TaskEvidence } from './TaskEvidence';

export interface ProposalHistoryProps {
  proposals: StagedProposal[];
  /** The state the host's curated read gave each accepted record, by
   *  proposal id (SPEC-12 slice 3, AC-0655). A record with no entry was not
   *  returned by that read; no state is made up for it. */
  recordStates?: ReadonlyMap<string, CuratedRecordStatus>;
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  onRefresh: () => void;
  onLoadMore: () => void;
  onOpen: (proposal: StagedProposal) => void;
}

/** Durable review history; all data and actions come from the host-backed store. */
export function ProposalHistory({
  proposals, recordStates, loading, error, hasMore, onRefresh, onLoadMore, onOpen,
}: ProposalHistoryProps) {
  return (
    <section className="proposal-history" aria-label="Proposal history">
      <header className="proposal-history-heading">
        <div>
          <h3>Pending and reviewed proposals</h3>
          <p className="muted">Saved proposals remain available after restart and job cleanup.</p>
        </div>
        <button type="button" className="secondary-button" disabled={loading} onClick={onRefresh}>
          Refresh proposal history
        </button>
      </header>
      <p className="muted">
        An accepted proposal gets one state each time it is read. Only an eligible one is a
        curated item; every other is listed with its reason. Review preserves T3 / InferredWeak
        and changes no recovered fact. Evidence origins describe the saved input, not its current availability.
      </p>
      {error && <p className="error-text" role="alert">{error}</p>}
      {loading && <p className="muted" role="status">Loading saved proposals…</p>}
      {!loading && !error && proposals.length === 0 && (
        <p className="muted">No saved proposals yet.</p>
      )}
      {proposals.length > 0 && (
        <>
          <p className="muted">{proposals.length} loaded</p>
          <ul className="proposal-history-list" aria-label="Saved proposals">
            {proposals.map((proposal) => (
              <li key={proposal.proposal_id}>
                <div className="proposal-history-heading">
                  <strong>
                    {proposal.review_decision === 'accepted'
                      ? `Accepted${recordStates?.get(proposal.proposal_id) ? ` · ${recordStates.get(proposal.proposal_id)?.state}` : ''}`
                      : proposal.review_decision === 'rejected' ? 'Rejected' : 'Pending review'}
                  </strong>
                  <TierBadge tier={proposal.provenance.confidence_tier} />
                </div>
                <p><code>{proposal.source_id}</code> —{proposal.edge_label}→ <code>{proposal.target_id}</code></p>
                {proposal.review_decision === 'accepted' && (
                  <p className="curated-record-state" data-testid="proposal-curated-state">
                    {recordStates?.get(proposal.proposal_id)?.reason_sentence ??
                      'Its state is listed under Curated items in the Spec Workbench.'}
                  </p>
                )}
                <p className="proposal-annotation">{proposal.annotation}</p>
                <TaskEvidence basis={proposal.source_basis} compact />
                <p className="muted">Review revision {proposal.review_revision}</p>
                <button type="button" onClick={() => onOpen(proposal)}>
                  {proposal.review_decision ? 'View reviewed proposal' : 'Review proposal'}
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      {hasMore && (
        <button type="button" className="register-show-more" disabled={loading} onClick={onLoadMore}>
          Load more proposals
        </button>
      )}
    </section>
  );
}
