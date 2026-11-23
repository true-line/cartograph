import { useEffect } from 'react';
import type { EscalationState } from '../store';
import type { CuratedRecordStatus } from '../curated';
import type { EgressPreview } from './EgressConsentDialog';
import { EgressConsentDialog } from './EgressConsentDialog';
import { TierBadge } from './TierBadge';
import { ProposalBasisAssessment } from './ProposalBasisAssessment';
import { ProposalReconciliation } from './ProposalReconciliation';
import { TaskEvidence } from './TaskEvidence';

export interface ResolutionStrategyModalProps {
  state: EscalationState;
  /** The state the host's curated read gave the open proposal, when that
   *  read returned it (SPEC-12 slice 3, AC-0655). */
  recordState?: CuratedRecordStatus;
  onRun: (strategyId: 'local-slm' | 'cloud-opus') => void;
  onConsent: (preview: EgressPreview) => void;
  onDismissPreview: () => void;
  onDecide: (decision: 'accepted' | 'rejected') => void;
  onClose: () => void;
  onAssessBasis?: () => void;
  /** Read the open proposal's reconciliation results (AC-1074). */
  onLoadReconciliation?: () => void;
  /** Reconcile the open proposal against the current recovery (AC-1074). */
  onReconcile?: () => void;
  /** Reject an accepted record, withdrawing it from curated content. */
  onWithdraw?: () => void;
  /** Run the record's Gap again on the current source, for a new proposal. */
  onResolveAgain?: (gapId: string) => void;
}

/** Resolution Strategy modal (handoff §Resolution Strategy, screenshot 11):
 *  one component reused for every gap. Shows the escalation ladder, why T0
 *  stopped, and runnable strategy cards; a run yields a staged proposal for
 *  Accept/Reject. An accepted record gets its state from the curated
 *  projection (ADR-0061); the dialog states it when the host has read it. */
export function ResolutionStrategyModal({
  state,
  recordState,
  onRun,
  onConsent,
  onDismissPreview,
  onDecide,
  onClose,
  onAssessBasis,
  onLoadReconciliation,
  onReconcile,
  onWithdraw,
  onResolveAgain,
}: ResolutionStrategyModalProps) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  // The one-action consent dialog takes over while a cloud preview waits.
  if (state.preview) {
    return (
      <EgressConsentDialog
        preview={state.preview}
        busy={state.running}
        onCancel={onDismissPreview}
        onConsent={onConsent}
      />
    );
  }

  const { report, proposal } = state;
  return (
    <div className="egress-backdrop" role="presentation">
      <section
        className="egress-dialog resolution-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="resolution-title"
      >
        <header className="egress-heading">
          <div>
            <p className="egress-kicker">Resolution Strategy · propose-only</p>
            <h2 id="resolution-title">{report?.summary ?? state.gapId}</h2>
          </div>
          <button type="button" aria-label="Close resolution strategy" onClick={onClose}>
            ✕
          </button>
        </header>

        {state.loading && <p className="muted">Deriving strategies from provenance…</p>}
        {state.error && <p className="error-text" role="alert">{state.error}</p>}
        {!state.loading && !state.error && !report && !proposal && (
          <p className="muted">
            No strategy report — the core could not assemble context for this gap.
          </p>
        )}

        {report && !proposal && (
          <>
            <p className="evidence-why gap">
              Why deterministic recovery stopped: {report.stop_reason}
            </p>
            <p className="muted ladder-line">
              Escalation ladder: {report.attempted_tiers.join(' → ')} established this gap →
              escalate next. T2/T3 never overwrite T0/T1 (R-INT-1).
            </p>

            <div className="egress-section">
              <h3>Required evidence ({report.required_evidence.length})</h3>
              <ul className="consent-notes">
                {report.required_evidence.map((line) => (
                  <li key={line}>
                    <code>{line}</code>
                  </li>
                ))}
              </ul>
              <p className="muted">
                {report.candidates} allowed candidate targets — the model can never invent one.
              </p>
            </div>

            <TaskEvidence basis={report.source_basis} />

            <div className="strategy-cards">
              {report.strategies.map((strategy) => (
                <div
                  key={strategy.id}
                  className={`strategy-card${strategy.available ? '' : ' unavailable'}`}
                  data-testid={`strategy-${strategy.id}`}
                >
                  <header>
                    <strong>
                      {strategy.locality === 'local' ? 'Local' : 'Cloud'} · {strategy.tier}
                    </strong>
                    <code>{strategy.provider}</code>
                  </header>
                  <ul className="consent-notes">
                    <li>
                      egress: <code>{strategy.egress_bytes} bytes</code>
                      {strategy.est_usd !== null &&
                        ` · ~$${strategy.est_usd.toFixed(4)} per run`}
                    </li>
                    <li>latency: {strategy.latency}</li>
                    <li>privacy: {strategy.privacy}</li>
                  </ul>
                  <p className="muted">{strategy.export_impact}</p>
                  {strategy.available ? (
                    <button
                      type="button"
                      disabled={state.running}
                      onClick={() => onRun(strategy.id)}
                    >
                      {state.running
                        ? 'Running…'
                        : strategy.locality === 'local'
                          ? 'Run locally'
                          : 'Review exact payload…'}
                    </button>
                  ) : (
                    <p className="error-text">{strategy.unavailable_reason}</p>
                  )}
                </div>
              ))}
            </div>
          </>
        )}

        {proposal && (
          <div className="egress-section proposal-card" data-testid="proposal-card">
            <h3>
              Staged proposal <TierBadge tier={proposal.provenance.confidence_tier} />
            </h3>
            <p>
              <code>{proposal.source_id}</code> —{proposal.edge_label}→{' '}
              <code>{proposal.target_id}</code>
            </p>
            <p className="muted">{proposal.annotation}</p>
            <p className="muted">
              Review preserves T3 / InferredWeak and never overwrites T0/T1. An accepted proposal
              gets one state each time it is read: only an eligible one is a curated item, and
              every other is listed with its reason.
            </p>
            <TaskEvidence basis={proposal.source_basis} />
            <details>
              <summary>Cited evidence ({proposal.provenance.evidence.length})</summary>
              <ul className="consent-notes">
                {proposal.provenance.evidence.map((reference, index) => (
                  <li key={index}>
                    <code>{reference.repo}:{reference.path}</code>
                    {' · bytes '}{reference.byte_start}..{reference.byte_end}
                    {' · cited revision '}<code>{reference.commit_sha}</code>
                  </li>
                ))}
              </ul>
            </details>
            {proposal.review_note && <p className="muted">Review note: {proposal.review_note}</p>}
            <ProposalBasisAssessment
              state={state.basisAssessment?.proposalId === proposal.proposal_id ? state.basisAssessment : undefined}
              onAssess={onAssessBasis}
            />
            <ProposalReconciliation
              state={state.reconciliation?.proposalId === proposal.proposal_id ? state.reconciliation : undefined}
              onLoad={onLoadReconciliation}
              onReconcile={onReconcile}
            />
            {state.decided ? (
              <p className="consent-status" data-testid="decision-recorded">
                Decision recorded: {state.decided}.
                {state.decided === 'accepted' && (recordState
                  ? ` State: ${recordState.state}. ${recordState.reason_sentence}`
                  : ' Its state is listed under Curated items in the Spec Workbench.')}
                {proposal.source_basis ? ' Saved evidence origins remain unchanged.' : ' Source binding remains unverified.'}
              </p>
            ) : null}
            {state.decided === 'accepted' && (onWithdraw || onResolveAgain) && (
              <div className="proposal-reconcile" data-testid="proposal-reconcile">
                <p className="muted">
                  To reconcile this record with the current source, resolve its gap again and
                  review the new proposal, then withdraw this one. Withdrawing rejects it, so it
                  leaves curated content at the next revision; its history stays.
                </p>
                <footer className="egress-actions">
                  {onWithdraw && (
                    <button type="button" className="secondary-button" disabled={state.reviewing} onClick={onWithdraw}>
                      {state.reviewing ? 'Saving review…' : 'Withdraw acceptance'}
                    </button>
                  )}
                  {onResolveAgain && (
                    <button type="button" disabled={state.reviewing} onClick={() => onResolveAgain(proposal.gap_id)}>
                      Resolve gap again on current source
                    </button>
                  )}
                </footer>
              </div>
            )}
            {!state.decided && (
              <footer className="egress-actions">
                <button type="button" className="secondary-button" disabled={state.reviewing} onClick={() => onDecide('rejected')}>
                  Reject
                </button>
                <button type="button" disabled={state.reviewing} onClick={() => onDecide('accepted')}>
                  {state.reviewing ? 'Saving review…' : `Accept as ${proposal.provenance.confidence_tier}`}
                </button>
              </footer>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
