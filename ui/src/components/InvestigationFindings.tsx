import type { CurationDecision, FindingCuration } from '../findingCurations';
import type { FindingReview, FindingReviewDecision } from '../findingReviews';
import type {
  AdvisoryAudit, InvestigationCitation, InvestigationCitationRead, InvestigationFinding, InvestigationResult,
} from '../investigationTypes';
import { TierBadge } from './TierBadge';
import { InvestigationMessageMeta } from './InvestigationMessageMeta';

const CLAIMS: Record<InvestigationFinding['claim_kind'], string> = {
  implemented_behavior: 'Implemented behavior', documented_intent: 'Documented intent',
  inferred_interpretation: 'Interpretation', proposed_design: 'Proposed design',
};
const READ_STATUS: Record<InvestigationCitationRead['status'], string> = {
  available: 'Retained historical source', metadata_only: 'Historical graph metadata only',
  working_tree_unverified: 'Working-tree input was unverified and is not retained',
  unavailable: 'Retained source is unavailable', invalid: 'Retained source did not validate',
  operational_failure: 'Historical source could not be checked',
};

export interface InvestigationFindingsProps {
  advisoryAudit?: AdvisoryAudit;
  readOnly?: boolean;
  producerId?: string;
  sectionId?: string;
  reviews?: FindingReview[];
  reviewLoading?: boolean;
  reviewBusy?: boolean;
  reviewError?: string | null;
  onRefreshReviews?: () => void;
  onReview?: (id: string, decision: FindingReviewDecision) => void;
  /** Curation reviews (ADR-0074): a second review, separate from follow-up.
   *  The controls are drawn only when `onCurate` is given. */
  curations?: FindingCuration[];
  curationLoading?: boolean;
  curationBusy?: boolean;
  curationError?: string | null;
  onRefreshCurations?: () => void;
  onCurate?: (id: string, decision: CurationDecision) => void;
  result: InvestigationResult | null;
  citations: InvestigationCitation[];
  citationId: string | null;
  read: InvestigationCitationRead | null;
  loading: boolean;
  error: string | null;
  onRead: (id: string) => void;
  onClose: () => void;
}

export function InvestigationFindings({ advisoryAudit, result, citations, citationId, read, loading, error, onRead, onClose, reviews = [], reviewLoading = false, reviewBusy = false, reviewError, onRefreshReviews, onReview, curations = [], curationLoading = false, curationBusy = false, curationError, onRefreshCurations, onCurate, readOnly = false, producerId, sectionId = 'investigation-findings' }: InvestigationFindingsProps) {
  const selected = citations.find((citation) => citation.citation_id === citationId);
  return <section id={sectionId} className="investigation-section" aria-label="Investigation findings">
    <h3>Findings <TierBadge tier="InferredWeak" /></h3>
    {result && advisoryAudit && <aside aria-label="Advisory model audit">
      <h4>Advisory model audit</h4>
      <p>Model feedback is advisory. It does not verify these findings or change their T3 / InferredWeak status.</p>
      {advisoryAudit.status === 'not_completed'
        ? <p>Audit not completed: {{ transport: 'transport failure', timeout: 'request timed out', length: 'output limit reached', invalid_output: 'invalid model output' }[advisoryAudit.reason]}.</p>
        : <ul>{advisoryAudit.reviews.map(({ subject, assessment }, index) => <li key={index}>
          {subject.kind === 'limitation' ? `Overall limitation ${subject.limitation_index + 1}`
            : subject.kind === 'finding' ? `Finding ${subject.finding_index + 1}`
              : `Finding ${subject.finding_index + 1}, limitation ${subject.limitation_index + 1}`}:
          {' '}{{ pass: 'Pass', contradiction: 'Contradiction', unsupported_scope: 'Unsupported scope', answer_coverage: 'Answer coverage', indeterminate: 'Indeterminate' }[assessment.verdict]} (advisory)
          {'citation_id' in assessment && <button type="button" className="secondary-button"
            disabled={!citations.some((citation) => citation.citation_id === assessment.citation_id)}
            onClick={() => onRead(assessment.citation_id)}>Inspect advisory citation {assessment.citation_id}</button>}
        </li>)}</ul>}
    </aside>}
    <p className="muted">Every specialist finding is T3 / InferredWeak, including statements about implemented behavior.
      Findings do not change confirmed facts, accepted proposals or context. An export lists a finding only after
      you accept it as curated, and then only as a marked InferredWeak statement.</p>
    {!result ? <p className="muted">No admitted result is available yet.</p> : <>
      {result.narrative && <article aria-label="Specialist final message">
        <h4>{producerId === 'gap-triager@2' ? 'Gap Triager' : producerId === 'spec-writer@2' ? 'Spec Writer' : 'Specialist'} final message · {producerId}</h4>
        <p className="muted">Proposed design · T3 / InferredWeak · saved final output</p>
        <InvestigationMessageMeta citations={citations} citationIds={result.narrative.finding_ids.every((id) =>
          result.findings.some((finding) => finding.finding_id === id))
          ? result.findings.filter((finding) => result.narrative!.finding_ids.includes(finding.finding_id)).flatMap((finding) => finding.citation_ids)
          : undefined} />
        <p className="investigation-statement">{result.narrative.text}</p>
        {result.findings.filter((finding) => result.narrative!.finding_ids.includes(finding.finding_id)).map((finding) =>
          <div key={finding.finding_id} aria-label={`Message support ${finding.finding_id}`}>
            <span>{finding.title} · {finding.finding_id}</span>
            {finding.citation_ids.map((id) => <button key={id} type="button" className="secondary-button"
              disabled={!citations.some((citation) => citation.citation_id === id)} onClick={() => onRead(id)}>Inspect message citation {id}</button>)}
          </div>)}
      </article>}
      {result.completion?.outcome === 'findings_withheld_after_correction' && <p role="status">Some findings were withheld because their claims could not be verified.</p>}
      <p>{result.knowledge_completeness === 'insufficient_evidence'
        ? 'Insufficient evidence to support an answer.' : 'Findings cover the inspected scope; knowledge remains partial.'}</p>
      <ul className="investigation-limitations">{result.limitations.map((item, index) => <li key={index}>{item}</li>)}</ul>
      {!readOnly && result.findings.length > 0 && <>
        <p className="muted">Follow-up selection is a human opinion about this historical finding. It does not validate evidence, activate curated facts or authorize execution.</p>
        {onRefreshReviews && <button type="button" className="secondary-button" disabled={reviewLoading || reviewBusy} onClick={onRefreshReviews}>Refresh reviews</button>}
        {reviewLoading && <p role="status">Loading saved review decisions…</p>}
        {reviewError && <p role="alert">{reviewError}</p>}
      </>}
      {!readOnly && onCurate && result.findings.length > 0 && <div aria-label="Curation reviews">
        <p className="muted">Accepting a finding as curated is a second review, separate from follow-up. The finding stays T3 / InferredWeak and
          nothing is written to the graph. A best-effort export, the Spec Workbench and the read-only MCP then show it as a cited statement while
          every fact it cites is unchanged. A verified-only export never shows it.</p>
        {onRefreshCurations && <button type="button" className="secondary-button" disabled={curationLoading || curationBusy} onClick={onRefreshCurations}>Refresh curation reviews</button>}
        {curationLoading && <p role="status">Loading saved curation reviews…</p>}
        {curationError && <p role="alert">{curationError}</p>}
      </div>}
      <ul className="investigation-findings">
        {result.findings.map((finding) => <li key={finding.finding_id}>
          <div className="investigation-heading"><h4>{finding.title}</h4><span className="investigation-claim">{CLAIMS[finding.claim_kind]}</span></div>
          <p className="investigation-statement">{finding.statement}</p>
          <p className="muted">T3 · InferredWeak · <code>{finding.finding_id}</code></p>
          {!readOnly && (() => {
            const review = reviews.find((row) => row.subject.finding_id === finding.finding_id);
            return <div aria-label={`Follow-up review for ${finding.title}`}>
              <p role="status">{review ? review.decision === 'accepted_for_follow_up' ? 'Accepted for follow-up' : review.decision === 'rejected_for_follow_up' ? 'Rejected for follow-up' : 'Pending review' : 'Review state unavailable'}</p>
              {review && <p>Review revision {review.revision}{review.reviewed_at && <> · <time>{review.reviewed_at}</time></>}</p>}
              {onReview && <><button type="button" disabled={!review || reviewLoading || reviewBusy || !!reviewError} onClick={() => onReview(finding.finding_id, 'accepted_for_follow_up')}>Use for follow-up</button>
                <button type="button" className="secondary-button" disabled={!review || reviewLoading || reviewBusy || !!reviewError} onClick={() => onReview(finding.finding_id, 'rejected_for_follow_up')}>Do not use for follow-up</button></>}
            </div>;
          })()}
          {!readOnly && onCurate && (() => {
            const curation = curations.find((row) => row.finding_id === finding.finding_id);
            const subject = reviews.find((row) => row.subject.finding_id === finding.finding_id);
            const blocked = !subject || curationLoading || curationBusy || !!curationError;
            return <div aria-label={`Curation review for ${finding.title}`}>
              <p role="status">{curation ? curation.review_decision === 'accepted' ? 'Accepted as curated' : 'Rejected as curated' : curationLoading ? 'Loading curation review' : curationError ? 'Curation review state unavailable' : 'No curation review'}</p>
              {curation && <p>Curation revision {curation.review_revision} · <time>{curation.reviewed_at}</time></p>}
              <button type="button" disabled={blocked || curation?.review_decision === 'accepted'} onClick={() => onCurate(finding.finding_id, 'accepted')}>Accept as curated</button>
              <button type="button" className="secondary-button" disabled={blocked || curation?.review_decision === 'rejected'} onClick={() => onCurate(finding.finding_id, 'rejected')}>Reject as curated</button>
            </div>;
          })()}
          {finding.limitations.length > 0 && <ul className="investigation-limitations">{finding.limitations.map((item, index) => <li key={index}>{item}</li>)}</ul>}
          <div className="investigation-citations" aria-label={`Citations for ${finding.title}`}>
            {finding.citation_ids.map((id) => {
              const citation = citations.find((item) => item.citation_id === id);
              return <button key={id} type="button" className="secondary-button" disabled={!citation}
                onClick={() => onRead(id)} aria-label={`Inspect citation ${id}`}>
                {id}{citation?.source ? ` · ${citation.source.path}` : ' · graph metadata'}
              </button>;
            })}
          </div>
        </li>)}
      </ul>
      <details><summary>Saved result identity</summary>
        <p>Result <code>{result.result_id}</code></p><p>Input ledger <code>{result.input_ledger_hash}</code></p>
        <p>Graph <code>{result.graph_snapshot_id}</code></p>
        <p>Reported response model: <code>{result.observed_response_model ?? 'Unknown'}</code></p>
      </details>
    </>}
    {selected && <aside className="investigation-citation-inspector" aria-label="Historical citation">
      <div className="investigation-heading"><h4>Citation <code>{selected.citation_id}</code></h4>
        <button type="button" className="secondary-button" onClick={onClose}>Close citation</button></div>
      <p className="muted">This inspection uses the saved citation. Current graph facts and checkout bytes are never substituted.</p>
      <p>Fact <code>{selected.fact.kind === 'node' ? selected.fact.id : `${selected.fact.source} ${selected.fact.label} ${selected.fact.destination}`}</code></p>
      <p>Saved digest <code>{selected.fact_digest}</code></p>
      {selected.source && <p><code>{selected.source.repo}/{selected.source.path}</code>
        {' · bytes '}{selected.source.byte_start}–{selected.source.byte_end}{' · revision '}<code>{selected.source.commit_sha}</code></p>}
      {selected.origin.kind === 'captured_primary_source' && <>
        <p>Receipt <code>{selected.origin.receipt_id}</code> · occurrence {selected.origin.receipt_inventory_index}</p>
        <p className="muted">Retained primary parser input; complete input coverage and business meaning are not established.</p>
      </>}
      {loading && <p role="status">Reading historical citation…</p>}
      {error && <p role="alert" className="error-text">{error}</p>}
      {read?.citation_id === selected.citation_id && <>
        <p role="status">{READ_STATUS[read.status]}</p>
        {read.status === 'metadata_only' && <p className="muted">Original graph properties were not archived; the saved identity and digest remain available.</p>}
        {read.status === 'available' && read.text !== null && <pre>{read.text}</pre>}
      </>}
    </aside>}
  </section>;
}
