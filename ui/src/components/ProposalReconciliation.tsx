import {
  outcomeSentence, slotSentence, stateLabels, type ReconciliationView,
} from '../reconciliation';

export interface ProposalReconciliationProps {
  state?: ReconciliationView;
  onLoad?: () => void;
  onReconcile?: () => void;
}

/** The host's reconciliation of the selected proposal against the current
 *  recovery (ADR-0072 slice 3, AC-1074). Each result is recorded apart from
 *  the proposal; neither the proposal nor its review changes. */
export function ProposalReconciliation({ state, onLoad, onReconcile }: ProposalReconciliationProps) {
  const history = state?.history;
  const busy = state?.loading ?? false;
  return (
    <section className="egress-section" aria-label="Reconciliation">
      <h3>Reconciliation</h3>
      <p className="muted">
        Compare each saved evidence item, candidate and the gap slot with the current recovery,
        and record the result. The proposal and its review stay as saved.
      </p>
      <footer className="egress-actions">
        {onLoad && (
          <button type="button" className="secondary-button" disabled={busy} onClick={onLoad}>
            {state && !state.reconciling && busy ? 'Reading results…' : 'Show results'}
          </button>
        )}
        {onReconcile && (
          <button type="button" className="secondary-button" disabled={busy} onClick={onReconcile}>
            {state?.reconciling ? 'Reconciling…' : 'Reconcile now'}
          </button>
        )}
      </footer>
      {!state && <p className="muted">Not read for this selection.</p>}
      {state?.error && <p className="error-text" role="alert">{state.error}</p>}
      {history && (
        <>
          <p data-testid="reconciliation-state">
            <strong>{stateLabels[history.state]}</strong>
          </p>
          {history.results.length === 0 ? (
            <p className="muted">No reconciliation has been recorded for this proposal.</p>
          ) : (
            <ol className="consent-notes" aria-label="Recorded reconciliations">
              {history.results.map(({ result, recorded_at }) => (
                <li key={result.result_id}>
                  <p>
                    <strong>{result.outcome}</strong>{' · recorded '}{recorded_at}
                    {' · job '}{result.recovery.job_id}
                  </p>
                  <p className="muted">{slotSentence(result.slot)}</p>
                  <details>
                    <summary>
                      Evidence ({result.evidence.length}) and candidates ({result.candidates.length})
                    </summary>
                    <ul>
                      {result.evidence.map((item) => (
                        <li key={item.evidence_id}><code>{item.evidence_id}</code> · {outcomeSentence(item.outcome)}</li>
                      ))}
                      {result.candidates.map((item) => (
                        <li key={item.node_id}><code>{item.node_id}</code> · {outcomeSentence(item.outcome)}</li>
                      ))}
                    </ul>
                    <p className="muted">
                      Graph <code>{result.recovery.graph_snapshot_id}</code>
                      {' · '}{result.recovery.captures.length} capture(s) read
                    </p>
                  </details>
                </li>
              ))}
            </ol>
          )}
        </>
      )}
    </section>
  );
}
