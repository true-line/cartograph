import type {
  InvestigationCapturedLocation, InvestigationInspectionAssessment, InvestigationInspectionTarget,
} from '../investigationTypes';

export function inspectionAssessmentLabel(assessment: InvestigationInspectionAssessment | null | undefined, requirementCount = 0, awaitingAssessment = true): string {
  if (!assessment) return requirementCount > 0
    ? awaitingAssessment ? 'Source inspections: Awaiting assessment' : 'Source inspections: Assessment unavailable'
    : 'Source inspections: Not assessed';
  return `Source inspections: ${assessment.requirements.filter((item) => item.supplied).length} / ${assessment.requirements.length} available when the specialist finished`;
}

export interface InspectionAssessmentProps {
  requirements: InvestigationInspectionTarget[];
  assessment: InvestigationInspectionAssessment | null | undefined;
  location: InvestigationCapturedLocation | null;
  locationTargetId: string | null;
  locationLoading: boolean;
  locationError: string | null;
  onLocation: (target: InvestigationInspectionTarget) => void;
  awaitingAssessment?: boolean;
}

/** These observations come from the host's saved finishing disclosure, never viewer reads. */
export function InspectionAssessment({ requirements, assessment, location, locationTargetId, locationLoading, locationError, onLocation, awaitingAssessment = true }: InspectionAssessmentProps) {
  const targets = assessment?.requirements.map((item) => item.target) ?? requirements;
  return <section className="investigation-section" aria-label="Source inspection obligations">
    <h3>Source inspection obligations</h3>
    <p>{inspectionAssessmentLabel(assessment, requirements.length, awaitingAssessment)}</p>
    <p className="muted">These checks track source inspection. Conclusions still need review.
      Uncited source is not shown as support for a finding.</p>
    {!assessment && <p className="muted">{requirements.length > 0
      ? awaitingAssessment ? 'These selected sources will be assessed when the specialist finishes.' : 'This task ended without a source inspection assessment.'
      : 'This task has no recorded source inspection requirements or assessment.'}</p>}
    <ul>{targets.map((target) => {
      const observed = assessment?.requirements.find((item) => item.target_id === target.target_id);
      const selected = locationTargetId === target.target_id;
      return <li key={target.target_id}>
        <strong>{target.source.repo}/{target.source.path}</strong> · bytes {target.captured.byte_start}–{target.captured.byte_end}
        {' · '}{target.role.replaceAll('_', ' ')}
        {observed && <>
          <p>{observed.status === 'supplied' ? 'Requirement met: exact source was available to the specialist when it finished.' : 'Requirement unmet: exact source was not available to the specialist when it finished.'}</p>
          <p>Read by specialist: {observed.acquired ? 'yes' : 'no'} · Available when specialist finished: {observed.supplied ? 'yes' : 'no'} · Cited: {observed.cited ? 'yes' : 'no'}</p>
          {observed.acquired && !observed.supplied && <p className="muted">Reading alone does not satisfy this requirement. Source removed or redacted before the answer remains unmet.</p>}
        </>}
        <button type="button" className="secondary-button" disabled={selected && locationLoading}
          onClick={() => onLocation(target)}>Show captured line/column</button>
        {selected && locationLoading && <p role="status">Checking exact retained coordinates…</p>}
        {selected && locationError && <p role="alert">{locationError}</p>}
        {selected && location?.target_id === target.target_id && <p>
          Captured location: line {location.start_line}, column {location.start_column} to line {location.end_line}, column {location.end_column}
          {' '} (1-based; UTF-8 byte columns; end exclusive). Bytes {location.byte_start}–{location.byte_end}.
        </p>}
        <details><summary>Immutable target identity</summary>
          <p>Graph <code>{target.graph_snapshot_id}</code> · fact digest <code>{target.fact_digest}</code></p>
          <p>Target <code>{target.target_id}</code> · role <code>{target.role}</code> · index {target.index}</p>
          <p>Receipt <code>{target.receipt_id}</code> · occurrence {target.receipt_inventory_index}</p>
          <p>Captured file <code>{target.captured.file.capture_id}</code> · digest <code>{target.captured.file.digest}</code></p>
        </details>
      </li>;
    })}</ul>
    {assessment && <details><summary>Saved assessment identity</summary>
      <p>Requirements <code>{assessment.requirements_hash}</code> · input ledger <code>{assessment.input_ledger_hash}</code></p>
      <p>Final step <code>{assessment.finishing_step_id}</code> · disclosure <code>{assessment.disclosure_hash}</code></p>
    </details>}
  </section>;
}
