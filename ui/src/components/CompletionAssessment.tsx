import type { InvestigationCompletionDetail, InvestigationCompletionOutcome } from '../investigationTypes';

const OUTCOME: Record<InvestigationCompletionOutcome, string> = {
  findings_withheld_after_correction: 'Some findings were withheld because their claims could not be verified.',
  supported: 'Supported completion', insufficient_evidence: 'Completed with insufficient evidence',
  obligations_unmet: 'Completion obligations unmet — no result was admitted',
};
const REASON = {
  not_attempted: 'No completed read attempt', acquired_not_supplied: 'Acquired earlier, omitted from this finish',
  redacted: 'Representation was redacted', hash_mismatch: 'Representation did not match the original', unavailable: 'Requested evidence was unavailable',
};

/** Saved host observations; viewing an inventory or source never discharges an obligation. */
export function CompletionAssessment({ completion, outcome }: {
  completion: InvestigationCompletionDetail; outcome: InvestigationCompletionOutcome | undefined;
}) {
  const basis = completion.assessment_basis;
  return <section className="investigation-section" aria-label="Completion obligations">
    <h3>Completion obligations</h3>
    <p>{outcome ? OUTCOME[outcome] : 'No completion outcome recorded.'}</p>
    <p className="muted">These checks record evidence exposure. Findings remain T3 / InferredWeak and still need review.
      Insufficient evidence can be an admitted empty result after completed read attempts; it does not mean every source was acquired.</p>
    <p>{basis.kind === 'finishing_disclosure' ? 'Assessed against the actual finishing disclosure.' :
      basis.kind === 'terminal_observation' ? 'Durable terminal observations; no model finishing disclosure or result is implied.' :
        'Not assessed yet. Execution status is shown separately.'}</p>
    <p>Finish refusals: {completion.refusal_count} / 2.</p>
    <ul>{completion.obligations.obligations.map((item) => {
      const observed = completion.assessment.find((row) => row.obligation_id === item.obligation_id);
      return <li key={`${item.kind}:${item.obligation_id}`}>
        <strong>{item.kind === 'source' ? `${item.target.source.repo}/${item.target.source.path}` : `${item.target.repo} inventory`}</strong>
        {item.kind === 'source' && <> · bytes {item.target.captured.byte_start}–{item.target.captured.byte_end}</>}
        {!observed ? <p>No assessment recorded.</p> : <>
          <p>Completed attempt: {observed.attempted ? 'Yes' : 'No'} · Acquired: {observed.acquired ? 'Yes' : 'No'} · Exactly supplied: {observed.exactly_supplied ? 'Yes' : 'No'}</p>
          {observed.last_attempt_outcome && <p>Last completed attempt: {observed.last_attempt_outcome === 'acquired' ? 'Acquired evidence' :
            observed.last_attempt_outcome === 'unavailable' ? 'Evidence unavailable' : 'Firewall refused the representation'}.</p>}
          {observed.unmet && <p>{REASON[observed.unmet]}.</p>}
          {!observed.unmet && observed.acquired && !observed.exactly_supplied && <p>Acquired earlier, not exactly supplied at finish.</p>}
          {observed.capacity_disclosed && <p>Not read: excluded by prompt capacity. Disclosed in the result's limitations, not met.</p>}
        </>}
        <details><summary>Exact obligation identity</summary><code>{item.obligation_id}</code>
          {item.kind === 'inventory' && <><p>Context <code>{item.target.context_id}</code></p><p>Inventory <code>{item.target.inventory_hash}</code></p></>}
        </details>
      </li>;
    })}</ul>
    <details><summary>Saved completion binding</summary>
      <p>Request <code>{completion.request_hash}</code></p><p>Obligations <code>{completion.obligations_hash}</code></p>
      {basis.kind === 'finishing_disclosure' && <><p>Assessed step <code>{basis.step_id}</code></p><p>Disclosure <code>{basis.disclosure_hash}</code></p></>}
      {basis.kind === 'terminal_observation' && basis.last_refusal_id && <p>Last refused finish <code>{basis.last_refusal_id}</code></p>}
    </details>
  </section>;
}
