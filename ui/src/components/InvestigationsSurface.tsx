import { useRef } from 'react';
import type { InvestigationState } from '../investigationStore';
import type { GraphNode } from '../store';
import type { InvestigationProviderMode, InvestigationScope, InvestigationStatus, SpecialistId } from '../investigationTypes';
import { InvestigationActivity } from './InvestigationActivity';
import { InvestigationFindings } from './InvestigationFindings';
import { EgressConsentDialog } from './EgressConsentDialog';
import { InspectionAssessment } from './InspectionAssessment';
import { InvestigationCoverage } from './InvestigationCoverage';

export const INVESTIGATION_STATUS: Record<InvestigationStatus, string> = {
  queued: 'Queued', preparing: 'Preparing context', running: 'Running', awaiting_consent: 'Awaiting cloud consent',
  completed: 'Execution completed', failed: 'Failed', cancelled: 'Cancelled',
  interrupted: 'Interrupted', outcome_unknown: 'Model outcome unknown',
};

export interface InvestigationsSurfaceProps {
  state: InvestigationState;
  anchors: GraphNode[];
  canStart: boolean;
}

/** Props/callbacks only: the coordinator and dedicated store own task lifecycle. */
export function InvestigationsSurface({ state, anchors, canStart }: InvestigationsSurfaceProps) {
  const { specialistId, providerMode, question, scopeType, anchor, hops, parent } = state.composer;
  const setSpecialistId = (specialistId: SpecialistId) => state.updateComposer({ specialistId });
  const setProviderMode = (providerMode: InvestigationProviderMode) => state.updateComposer({ providerMode });
  const setQuestion = (question: string) => state.updateComposer({ question });
  const setScopeType = (scopeType: 'all' | 'neighborhood') => state.updateComposer({ scopeType });
  const setAnchor = (anchor: string) => state.updateComposer({ anchor });
  const setHops = (hops: number) => state.updateComposer({ hops });
  const setParent = (parent: typeof state.composer.parent) => state.updateComposer({ parent });
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const provider = state.catalog?.providers.find((item) => item.mode === providerMode);
  const summary = state.detail ?? state.history.find((item) => item.investigation_id === state.selectedId);
  const questionBytes = new TextEncoder().encode(question).length;
  const locked = state.starting || state.pendingStart !== null;
  const requirements = state.draftInspectionRequirements;
  const inspectionVersion = specialistId.endsWith('@5');
  const coverageVersion = specialistId.endsWith('@6');
  const supportsRequirements = inspectionVersion || coverageVersion;
  const scope: InvestigationScope = scopeType === 'all' ? { type: 'all' } : { type: 'neighborhood', anchor, hops };
  const requestBytes = new TextEncoder().encode(JSON.stringify({ schema_version: coverageVersion ? 3 : inspectionVersion ? 2 : 1,
    request_nonce: '0'.repeat(36), specialist_id: specialistId, question, scope, provider_mode: providerMode,
    limit_profile: state.catalog?.limits.profile ?? 'investigation-v1', ...parent,
    ...(supportsRequirements && requirements.length ? { inspection_requirements: requirements,
      expected_graph_revision: requirements[0].graph_snapshot_id } : {}) })).length;
  const startEnabled = canStart && !locked && Boolean(provider?.available) &&
    Boolean(state.catalog?.specialists.some((item) => item.id === specialistId)) && question.trim().length > 0 &&
    questionBytes <= 2048 && (scopeType === 'all' || anchor.trim().length > 0) &&
    (inspectionVersion ? requirements.length > 0 : coverageVersion || requirements.length === 0) && requestBytes <= 8192;
  const refresh = () => { void state.loadCatalog(); void state.loadHistory(); };

  return <section className="investigations-surface" aria-label="Investigations">
    <header className="investigation-heading"><div><h2>Investigations</h2>
      <p className="muted">Ask a specialist to inspect recovered context and evidence. Findings remain T3 / InferredWeak.</p></div>
      <button type="button" className="secondary-button" onClick={refresh} disabled={state.catalogLoading || state.historyLoading}>Refresh investigations</button>
    </header>
    <form className="investigation-compose" aria-label="Start an investigation" onSubmit={(event) => {
      event.preventDefault();
      if (startEnabled && state.catalog) void state.start({ specialist_id: specialistId, question,
        scope, provider_mode: providerMode, limit_profile: state.catalog.limits.profile, ...parent,
        ...(supportsRequirements && requirements.length ? { inspection_requirements: requirements } : {}) });
    }}>
      <fieldset disabled={locked}><legend>Specialist</legend>
        <div className="investigation-specialists">{state.catalog?.specialists.map((specialist) =>
          <label key={specialist.id} className={`investigation-specialist${specialistId === specialist.id ? ' selected' : ''}`}>
            <input type="radio" name="specialist" checked={specialistId === specialist.id} onChange={() => setSpecialistId(specialist.id)} />
            <span><strong>{specialist.name}</strong><span className="muted">{specialist.purpose}</span>
              <code>{specialist.id} · T3 / InferredWeak</code></span>
          </label>)}</div>
      </fieldset>
      {state.catalogLoading && <p role="status">Loading specialists…</p>}
      {state.catalogError && <p className="error-text" role="alert">{state.catalogError}</p>}
      {!state.catalog && !state.catalogLoading && <p className="muted">The app core supplies available specialists and provider limits.</p>}
      <fieldset disabled={locked} aria-label="Required source inspections"><legend>Required source inspections</legend>
        <p className="muted">Choose a captured range in the evidence drawer and select “Require this source in an investigation”.
          Viewing source does not satisfy a specialist requirement. Follow-ups do not inherit earlier inspections.</p>
        <p className="muted">Up to 12 required sources, within the 8,192-byte total request limit; long paths/questions reduce how many fit.</p>
        {requirements.length === 0 ? <p>No source requirements selected.</p> : <>
          <ul>{requirements.map((target) => <li key={target.target_id}>
            <strong>{target.source.repo}/{target.source.path}</strong> · bytes {target.captured.byte_start}–{target.captured.byte_end}
            {' · '}{target.role.replaceAll('_', ' ')}
            <button type="button" className="secondary-button" aria-label={`Remove required source ${target.source.path}`}
              onClick={() => state.removeInspectionRequirement(target.target_id)}>Remove</button>
            <details><summary>Exact source binding</summary><p>Graph <code>{target.graph_snapshot_id}</code></p>
              <p>Fact <code>{target.fact.kind === 'node' ? target.fact.id : `${target.fact.source} ${target.fact.label} ${target.fact.destination}`}</code></p>
              <p>Role <code>{target.role}</code> · index {target.index}; receipt occurrence {target.receipt_inventory_index}</p>
              <p>Target <code>{target.target_id}</code> · digest <code>{target.fact_digest}</code></p>
            </details>
          </li>)}</ul>
          {!supportsRequirements && <p role="status">Select an explicit V5 or V6 specialist to use these source requirements, or remove them to use the selected specialist.</p>}
        </>}
        {inspectionVersion && requirements.length === 0 && <p role="status">V5 requires at least one explicitly selected captured source.</p>}
        {coverageVersion && <p className="muted">V6 includes saved ingest coverage. Source inspection requirements are optional and stay separate.</p>}
        {state.inspectionDraftError && <p role="alert" className="error-text">{state.inspectionDraftError}</p>}
      </fieldset>
      <div className="investigation-form-row">
        <label>Scope<select value={scopeType} disabled={locked} onChange={(event) => setScopeType(event.target.value as 'all' | 'neighborhood')}>
          <option value="all">Recovered system</option><option value="neighborhood">Fact neighborhood</option>
        </select></label>
        <label>Provider<select value={providerMode} disabled={locked} onChange={(event) => setProviderMode(event.target.value as InvestigationProviderMode)}>
          <option value="local">Local</option><option value="cloud">Cloud · consent for every step</option>
        </select></label>
        {scopeType === 'neighborhood' && <>
          <label className="investigation-anchor">Anchor fact ID<input value={anchor} list="investigation-anchor-options" disabled={locked}
            onChange={(event) => setAnchor(event.target.value)} maxLength={2048} />
            <datalist id="investigation-anchor-options">{anchors.slice(0, 200).map((node) => <option key={node.id} value={node.id}>{node.label}</option>)}</datalist></label>
          <label>Hops<select value={hops} disabled={locked} onChange={(event) => setHops(Number(event.target.value))}>
            <option value={1}>1</option><option value={2}>2</option><option value={3}>3</option>
          </select></label>
        </>}
      </div>
      {scopeType === 'neighborhood' && <p className="muted">Suggestions show up to 200 loaded facts. The host validates the exact anchor and neighborhood.</p>}
      {provider && <p className={provider.available ? 'muted' : 'error-text'}>
        {provider.provider_id} · {provider.model}{!provider.available && ` — ${provider.unavailable_reason ?? 'Unavailable'}`}
      </p>}
      <label className="investigation-question">{parent ? 'Follow-up question' : 'Question'}
        <textarea ref={questionRef} value={question} disabled={locked} maxLength={2048} rows={3}
          placeholder={specialistId.startsWith('domain-analyst@') ? 'What business behavior is evidenced in this scope, and what remains unknown?' : 'Which claims have supporting evidence, and where is that evidence incomplete?'}
          onChange={(event) => setQuestion(event.target.value)} />
      </label>
      <p className={questionBytes > 2048 ? 'error-text' : 'muted'}>{questionBytes} / 2,048 UTF-8 bytes. Scope uses the recovered graph at preparation time.</p>
      {requestBytes > 8192 && <p role="alert" className="error-text">This request exceeds the 8,192-byte limit. Shorten the question or remove required sources.</p>}
      {parent && <p className="muted">New task following <code>{parent.parent_id}</code>; earlier findings remain T3 history.
        <button type="button" className="secondary-button" disabled={locked} onClick={() => setParent(null)}>Start a separate conversation</button></p>}
      {state.catalog && <p className="muted">Up to {state.catalog.limits.model_invocations} model invocations and {state.catalog.limits.tool_actions} tool actions;
        {' '}{state.catalog.limits.active_seconds}s active execution. Source and byte limits apply. Missing evidence is not proof of absence.</p>}
      <button type="submit" disabled={!startEnabled}>{state.starting ? 'Saving investigation…' : parent ? 'Start follow-up' : 'Start investigation'}</button>
      {state.startError && <div role="alert" className="error-text"><p>{state.startError}</p>
        {state.pendingStart && <button type="button" className="secondary-button" disabled={state.starting} onClick={() => void state.retryStart()}>Retry same start request</button>}</div>}
    </form>

    <div className="investigation-layout">
      <aside className="investigation-history" aria-label="Investigation history">
        <h3>Saved investigations</h3><p className="muted">Questions, activity and findings survive restart and job cleanup.</p>
        {state.historyLoading && <p role="status">Loading history…</p>}
        {state.historyError && <p role="alert" className="error-text">{state.historyError}</p>}
        {!state.historyLoading && state.history.length === 0 && <p className="muted">No saved investigations loaded.</p>}
        <ul>{state.history.map((item) => <li key={item.investigation_id}>
          <button type="button" className={`investigation-history-item${item.investigation_id === state.selectedId ? ' selected' : ''}`}
            aria-pressed={item.investigation_id === state.selectedId} onClick={() => void state.open(item.investigation_id)}>
            <strong>{item.question}</strong><span>{item.specialist_id}</span><span>{INVESTIGATION_STATUS[item.status]}</span>
            {item.schema_version === 3 && <span>Coverage-qualified · saved basis shown on open</span>}
            <span>Source inspections: {item.inspection_status === 'pending' ? 'Awaiting assessment' : item.inspection_status === 'supplied'
              ? 'Required sources available' : item.inspection_status === 'unmet' ? 'Required sources missing' : 'Not assessed'}
              {item.inspection_requirement_count !== undefined && ` · ${item.inspection_requirement_count} required`}</span>
            <time>{item.created_at}</time>{item.cancel_requested && <span>Cancellation requested</span>}
          </button></li>)}</ul>
        {state.nextCursor && <button type="button" className="secondary-button" disabled={state.historyLoading} onClick={() => void state.loadHistory(true)}>Load older investigations</button>}
      </aside>
      <div className="investigation-detail" aria-label="Selected investigation">
        {!state.selectedId ? <p className="muted">Start an investigation or select its saved history.</p> : <>
          <div className="investigation-heading"><div><h3>{summary?.question ?? 'Loading investigation'}</h3><code>{state.selectedId}</code></div>
            <button type="button" className="secondary-button" disabled={state.loading} onClick={() => void state.refreshSelected()}>Refresh selected investigation</button></div>
          {summary && <>
            <p role="status">{INVESTIGATION_STATUS[summary.status]}{summary.cancel_requested && ' · Cancellation requested'}</p>
            {summary.invocation_pending && <p className="investigation-notice">A model request is outstanding. Its outcome is pending;
              cancellation does not immediately terminate remote inference. A valid completed answer can still be retained.</p>}
            {summary.status === 'outcome_unknown' && <p className="investigation-notice">The previous model outcome is unknown. This task will not be replayed automatically.</p>}
            {summary.status === 'interrupted' && <p className="investigation-notice">The original worker input is no longer available. A follow-up uses a new task and basis.</p>}
            <p className="muted">Scope: {summary.scope.type === 'all' ? 'Recovered system' : `${summary.scope.anchor} · ${summary.scope.hops} hop(s)`}</p>
            <p className="muted">Prepared graph: <code>{summary.graph_snapshot_id ?? 'Pending preparation'}</code></p>
            <div className="investigation-actions">
              {summary.actions.can_cancel && <button type="button" className="danger-button" disabled={state.actionBusy}
                onClick={() => void state.cancel()}>Request cancellation</button>}
              {summary.actions.can_follow_up && <button type="button" className="secondary-button" disabled={locked || state.catalogLoading || !state.catalog} onClick={() => {
                if (state.catalogLoading || !state.catalog) return;
                state.clearInspectionRequirements();
                setParent({ conversation_id: summary.conversation_id, parent_id: summary.investigation_id });
                setScopeType(summary.scope.type); if (summary.scope.type === 'neighborhood') { setAnchor(summary.scope.anchor); setHops(summary.scope.hops); }
                setSpecialistId(state.catalog?.specialists.some((item) => item.id === summary.specialist_id)
                  ? summary.specialist_id : summary.specialist_id.startsWith('evidence-auditor@') ? 'evidence-auditor@3' : 'domain-analyst@3');
                setProviderMode(summary.provider_mode); setQuestion(''); questionRef.current?.focus();
              }}>Ask a follow-up</button>}
              {state.consent && <button type="button" disabled={state.actionBusy || state.loading} onClick={state.showConsent}>Review next cloud action</button>}
            </div>
          </>}
          {state.loading && !state.detail && <p role="status">Loading saved investigation…</p>}
          {state.error && <p className="error-text" role="alert">{state.error}</p>}
          {state.actionError && <p className="error-text" role="alert">{state.actionError}</p>}
          {state.detail && <>
            {state.detail.schema_version === 3 && <InvestigationCoverage context={state.detail.coverage_context}
              attestation={state.result?.coverage} />}
            <InspectionAssessment requirements={state.detail.inspection_requirements ?? []}
              awaitingAssessment={!['completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'].includes(state.detail.status) || state.detail.invocation_pending}
              assessment={state.detail.inspection_assessment} location={state.inspectionLocation}
              locationTargetId={state.inspectionLocationTargetId} locationLoading={state.inspectionLocationLoading}
              locationError={state.inspectionLocationError} onLocation={(target) => void state.readInspectionLocation(target)} />
            <details className="investigation-section"><summary>Specialist, provider and consumed limits</summary>
              <p>{state.detail.specialist.name} · <code>{state.detail.specialist.id}</code> · T3 / InferredWeak</p>
              <p>Prompt fingerprint: <code>{state.detail.specialist.prompt_fingerprint}</code></p>
              <p>Configured model: <code>{state.detail.provider.provider_id} / {state.detail.provider.model}</code></p>
              <p>Model invocations used: {state.detail.usage.model_invocations} / {state.detail.limits.model_invocations};
                tool actions used: {state.detail.usage.tool_actions} / {state.detail.limits.tool_actions}.</p>
              <p>Admitted facts: {state.detail.usage.selected_facts}; copied evidence: {state.detail.usage.evidence_items} items / {state.detail.usage.evidence_bytes} bytes.</p>
              <p>Captured-file validation: {state.detail.usage.captured_validation_bytes} / {state.detail.limits.captured_validation_bytes} bytes.</p>
              <p>Generated-token reservations: {state.detail.usage.generated_token_reservations} / {state.detail.limits.generated_token_reservations}.</p>
              <p>Reported input/output tokens: {state.detail.usage.reported_input_tokens ?? 'Unavailable'} / {state.detail.usage.reported_output_tokens ?? 'Unavailable'}.</p>
              <p className="muted">Reservations are limits, not measured token use or a monetary guarantee.</p>
            </details>
            {state.detail.error && <p className="error-text" role="alert">{state.detail.error}</p>}
          </>}
          <InvestigationActivity events={state.events} />
          <InvestigationFindings result={state.result} citations={state.detail?.citations ?? []}
            citationId={state.citationId} read={state.citationRead} loading={state.citationLoading} error={state.citationError}
            onRead={(id) => void state.readCitation(id)} onClose={state.clearCitation} />
        </>}
      </div>
    </div>
    {state.consentOpen && state.consent && state.consent.investigation_id === state.selectedId && <EgressConsentDialog
      preview={state.consent.preview} busy={state.actionBusy} cancelLabel="Decline this action"
      onCancel={() => void state.decline()} onConsent={() => void state.approve()}
      onClose={state.hideConsent} closeLabel="Review later"
      additionalDetails={<><p>Investigation <code>{state.consent.investigation_id}</code> · step <code>{state.consent.step_id}</code></p>
        <p>Model <code>{state.consent.provider.model}</code> · endpoint <code>{state.consent.provider.endpoint}</code></p>
        <p>Deployment: {state.consent.provider.deployment ?? 'Not supplied'} · expires {state.consent.expires_at}</p>
        <p>Declared step limits: {state.consent.completion_limits.max_output_tokens} generated tokens;
          {' '}{state.consent.completion_limits.request_timeout_ms} ms request timeout. The actual deadline may narrow as task time elapses.</p>
        <details><summary>Exact provider profile and step limits</summary>
          <pre>{JSON.stringify({ profile: state.consent.provider_profile, limits: state.consent.completion_limits }, null, 2)}</pre></details>
        <p>Approval covers this step only. Declining does not switch to a local provider; reviewing later leaves this task waiting.</p></>} />}
  </section>;
}
