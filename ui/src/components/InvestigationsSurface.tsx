import { isCompletionSpecialist, isExpandedSpecialist } from '../investigationFamilies';
import { SPECIALIST_TABLE } from '../generated/specialistTable';
import { ROLE_LABELS, currentVersion, isRetired, offeredRoles, offeredVersions, profileFor, roleOf } from '../specialistRoles';
import { useRef, useState } from 'react';
import { RunLimitFooter } from './RunLimitFooter';
import type { InvestigationState } from '../investigationStore';
import type { GraphNode } from '../store';
import type { InvestigationProviderMode, InvestigationScope, InvestigationStatus, SpecialistActivityItem, SpecialistId } from '../investigationTypes';
import { InvestigationActivity } from './InvestigationActivity';
import { InvestigationProgress } from './InvestigationProgress';
import { InvestigationFindings } from './InvestigationFindings';
import { EgressConsentDialog } from './EgressConsentDialog';
import { InspectionAssessment } from './InspectionAssessment';
import { CompletionAssessment } from './CompletionAssessment';
import { investigationPolicy, obligationEnvelope } from '../investigationObligations';
import { InvestigationCoverage } from './InvestigationCoverage';
import { InvestigationArtifactReads } from './InvestigationArtifactReads';
import { useArtifactReads } from '../investigationArtifactReads';
import { InvestigationChatTranscript } from './InvestigationChatTranscript';
import type { SpecialistActivityState } from '../specialistActivityStore';

export const INVESTIGATION_STATUS: Record<InvestigationStatus, string> = {
  queued: 'Queued', preparing: 'Preparing context', running: 'Running', awaiting_consent: 'Awaiting cloud consent',
  completed: 'Execution completed', failed: 'Failed', cancelled: 'Cancelled',
  interrupted: 'Interrupted', outcome_unknown: 'Model outcome unknown',
};

export interface InvestigationsSurfaceProps {
  state: InvestigationState;
  activity?: Pick<SpecialistActivityState, 'status' | 'tasks' | 'error' | 'refreshing'>;
  onRefreshActivity?: () => void;
  anchors: GraphNode[];
  canStart: boolean;
}

const EMPTY_ACTIVITY: Pick<SpecialistActivityState, 'status' | 'tasks' | 'error' | 'refreshing'> = {
  status: 'idle', tasks: [], error: null, refreshing: false,
};

/** Props/callbacks only: the coordinator and dedicated store own task lifecycle. */
export function InvestigationsSurface({ state, activity = EMPTY_ACTIVITY, onRefreshActivity, anchors, canStart }: InvestigationsSurfaceProps) {
  const [historySearchText, setHistorySearchText] = useState(state.historySearch);
  const { specialistId, providerMode, question, scopeType, anchor, hops, parent } = state.composer;
  // ADR-0071: one choice; the version fixes its profile, so no separate budget picker.
  const chooseSpecialist = (id: SpecialistId) => state.updateComposer({ specialistId: id, limitProfile: profileFor(state.catalog, id) });
  const setProviderMode = (providerMode: InvestigationProviderMode) => state.updateComposer({ providerMode });
  const setQuestion = (question: string) => state.updateComposer({ question });
  const setScopeType = (scopeType: 'all' | 'neighborhood') => state.updateComposer({ scopeType });
  const setAnchor = (anchor: string) => state.updateComposer({ anchor });
  const setHops = (hops: number) => state.updateComposer({ hops });
  const setParent = (parent: typeof state.composer.parent) => state.updateComposer({ parent });
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const provider = state.catalog?.providers.find((item) => item.mode === providerMode);
  const summary = state.detail ?? state.history.find((item) => item.investigation_id === state.selectedId);
  // AC-1816: only a specialist that offers read_artifact has saved pages to show.
  const readsArtifacts = state.detail?.specialist.operations.includes('read_artifact') ?? false;
  const artifactReads = useArtifactReads(state.detail?.investigation_id ?? null, state.detail?.revision ?? 0, readsArtifacts);
  const conversationSelected = state.conversationPinned?.investigation_id === state.selectedId &&
    state.conversationPinned.conversation_id === state.conversationId ? state.conversationPinned :
    summary?.investigation_id === state.selectedId && summary.conversation_id === state.conversationId ? summary : null;
  const conversationRows = state.conversationItems;
  const selectedIsOnPage = conversationSelected && state.conversationItems.some((item) => item.investigation_id === conversationSelected.investigation_id);
  const questionBytes = new TextEncoder().encode(question).length;
  const locked = state.starting || state.pendingStart !== null;
  const requirements = state.draftInspectionRequirements;
  const matchedActivityTasks = state.catalog ? activity.tasks.filter((task) =>
    state.catalog?.specialists.some((specialist) => specialist.id === task.summary.specialist_id)) : [];
  const activityIncomplete = activity.status === 'incomplete' || matchedActivityTasks.length !== activity.tasks.length;
  const showActivityRows = activity.status !== 'idle' && activity.status !== 'loading' &&
    activity.status !== 'preview' && activity.status !== 'error';
  const activityGroups = state.catalog && showActivityRows ? state.catalog.specialists.map((specialist) => ({
    specialist,
    tasks: matchedActivityTasks.filter((task) => task.summary.specialist_id === specialist.id),
  })).filter(({ tasks }) => tasks.length > 0) : [];
  const activityCounts = matchedActivityTasks.reduce((counts, task) => {
    const status = task.summary.status;
    if (status === 'queued' || status === 'preparing' || status === 'running' || status === 'awaiting_consent') {
      counts[status] += 1;
      counts.total += 1;
      if (task.summary.cancel_requested) counts.cancelRequested += 1;
    }
    return counts;
  }, { queued: 0, preparing: 0, running: 0, awaiting_consent: 0, cancelRequested: 0, total: 0 });
  const activityCountLabel = activity.refreshing
    ? activityIncomplete ? 'Last observed partial activity counts' : 'Last observed activity counts'
    : activityIncomplete ? 'Partial activity counts' : 'Shown activity counts';
  const activityCountMessage = showActivityRows && !state.catalogLoading
    ? `${activityCountLabel}: ` +
      `queued ${activityCounts.queued}, preparing ${activityCounts.preparing}, running ${activityCounts.running}, ` +
      `awaiting consent ${activityCounts.awaiting_consent}; cancellation requested ${activityCounts.cancelRequested} of ${activityCounts.total} active tasks shown.` +
      (activityIncomplete ? ' More active tasks may exist.' : '')
    : null;
  const activityRefreshMessage = activityIncomplete
    ? matchedActivityTasks.length > 0 ? 'Activity incomplete · refreshing; last observed matching tasks are shown.' : 'Activity incomplete · refreshing; no idle status is inferred.'
    : matchedActivityTasks.length > 0 ? 'Refreshing activity; last observed tasks are shown.' : 'Checking for active tasks…';
  const completionVersion = isCompletionSpecialist(specialistId);
  const roles = offeredRoles(state.catalog);
  const selectedRole = roleOf(specialistId);
  const profile = state.composer.limitProfile ?? 'investigation-v1';
  const policy = investigationPolicy(state.catalog, profile, specialistId);
  const selectedSpecialistAvailable = Boolean(policy && state.catalog?.specialists.some((item) => item.id === specialistId));
  const limits = policy?.limits ?? (profile === 'investigation-v1' ? state.catalog?.limits : undefined);
  const requestLimit = policy?.request_bytes ?? 8192;
  const choices = state.obligationChoices;
  const obligations = obligationEnvelope(state.draftObligations, policy);
  const completionBound = Boolean(choices && obligations.obligations.length > 0 && policy && obligations.obligations.length <= policy.max_completion_obligations &&
    obligations.obligations.every((item) => item.kind === 'source' ? item.target.graph_snapshot_id === choices.graph_snapshot_id : item.target.context_id === choices.context_id));
  const inspectionVersion = specialistId.endsWith('@5');
  const coverageVersion = specialistId.endsWith('@6');
  const supportsRequirements = inspectionVersion || coverageVersion;
  const scope: InvestigationScope = scopeType === 'all' ? { type: 'all' } : { type: 'neighborhood', anchor, hops };
  const requestBytes = completionVersion ? new TextEncoder().encode(JSON.stringify({ schema_version: policy?.request_schema_version ?? 4, request_nonce: '0'.repeat(36),
    specialist_id: specialistId, obligations, question, scope, provider_mode: providerMode, limit_profile: profile,
    expected_graph_revision: choices?.graph_snapshot_id ?? null, expected_context_id: choices?.context_id ?? '',
    conversation_id: parent?.conversation_id ?? null, parent_id: parent?.parent_id ?? null })).length : new TextEncoder().encode(JSON.stringify({ schema_version: coverageVersion ? 3 : inspectionVersion ? 2 : 1,
    request_nonce: '0'.repeat(36), specialist_id: specialistId, question, scope, provider_mode: providerMode,
    limit_profile: profile, ...parent,
    ...(supportsRequirements && requirements.length ? { inspection_requirements: requirements,
      expected_graph_revision: requirements[0].graph_snapshot_id } : {}) })).length;
  const startEnabled = canStart && !locked && !state.catalogLoading && Boolean(provider?.available) &&
    selectedSpecialistAvailable && Boolean(policy) && question.trim().length > 0 &&
    questionBytes <= 2048 && (scopeType === 'all' || anchor.trim().length > 0) &&
    (completionVersion ? completionBound && requirements.length === 0 :
      state.draftObligations.length === 0 && (inspectionVersion ? requirements.length > 0 : coverageVersion || requirements.length === 0)) && requestBytes <= requestLimit;
  const refresh = () => { void state.loadCatalog(); void state.loadHistory(); onRefreshActivity?.(); };

  return <section className="investigations-surface" aria-label="Investigations">
    <header className="investigation-heading"><div><h2>Investigations</h2>
      <p className="muted">Ask a specialist to inspect recovered context and evidence. Findings remain T3 / InferredWeak.</p></div>
      <button type="button" className="secondary-button" onClick={refresh} disabled={state.catalogLoading || state.historyLoading}>Refresh investigations</button>
    </header>
    <form className="investigation-compose" aria-label="Start an investigation" onSubmit={(event) => {
      event.preventDefault();
      if (startEnabled && state.catalog) void state.start({ specialist_id: specialistId, question,
        scope, provider_mode: providerMode, limit_profile: profile, ...parent,
        ...(completionVersion ? { obligations } : supportsRequirements && requirements.length ? { inspection_requirements: requirements } : {}) });
    }}>
      <fieldset disabled={locked}><legend>Specialist</legend>
        <div className="investigation-specialists">{roles.map((role) => {
          const current = currentVersion(state.catalog, role)!;
          return <label key={role} className={`investigation-specialist${selectedRole === role ? ' selected' : ''}`}>
            <input type="radio" name="specialist" checked={selectedRole === role && selectedSpecialistAvailable}
              onChange={() => chooseSpecialist(current.id)} />
            <span><strong>{ROLE_LABELS[role]}</strong><span className="muted">{current.purpose}</span>
              <code>T3 / InferredWeak</code></span>
          </label>;
        })}</div>
        {(state.catalog || state.pendingStart) && <div className="investigation-version">
          <label>Specialist version<select value={specialistId} onChange={(event) => chooseSpecialist(event.target.value as SpecialistId)}>
            {!selectedSpecialistAvailable && <option value={specialistId}>{specialistId} · {isRetired(specialistId) ? 'retired' : 'saved selection'}, unavailable for new starts</option>}
            {roles.map((role) => <optgroup key={role} label={ROLE_LABELS[role]}>
              {offeredVersions(state.catalog, role).map((item) => <option key={item.id} value={item.id}>
                {item.id}{SPECIALIST_TABLE[item.id].availability === 'current' ? ' · current' : ''}{isExpandedSpecialist(item.id) ? ' · Expanded budget' : ''}</option>)}
            </optgroup>)}
          </select></label>
          <p className="muted">Each role runs its current version by default. Choose another version only to compare with earlier runs.</p>
        </div>}
      </fieldset>
      {limits && <p className="muted">{profile === 'investigation-v1' ? 'Standard' : 'Expanded'} budget · {limits.tool_actions} tool actions, {limits.model_invocations} model calls</p>}
      {profile !== 'investigation-v1' && <p>Expanded investigations allow more evidence and model calls, which can increase runtime and provider cost. Completion is not guaranteed.</p>}
      {state.catalog && <section className="specialist-activity-list" aria-label="Active specialist tasks">
        {activityCountMessage && <p className="specialist-activity-counts">{activityCountMessage}</p>}
        {showActivityRows && activityGroups.map(({ specialist, tasks }) => <section key={specialist.id} className="specialist-activity-group">
          <h3>{specialist.name} <code>{specialist.id}</code></h3>
          <SpecialistTaskActivity tasks={tasks} onOpen={(id) => { void state.open(id); }} />
        </section>)}
        {activity.status === 'error' && <p className="error-text" role="status">
          {activity.refreshing ? 'Activity unknown; refreshing after an earlier error.' : 'Activity unknown'}
        </p>}
        {activity.status === 'preview' && <p className="muted" role="status">Activity shown in desktop app</p>}
        {activity.status === 'idle' && <p className="muted" role="status">Activity not checked</p>}
        {activity.status === 'loading' && <p className="muted" role="status">Checking activity…</p>}
        {showActivityRows && activity.refreshing && <p role="status">{activityRefreshMessage}</p>}
        {showActivityRows && !activity.refreshing && activityIncomplete && <p role="status">
          The active task list is incomplete. No idle status is inferred.
        </p>}
        {showActivityRows && !activity.refreshing && !activityIncomplete && !state.catalogLoading && matchedActivityTasks.length === 0 &&
          <p role="status">No active tasks</p>}
      </section>}
      {state.catalog === null && activity.status !== 'loading' && <p role="status">The catalog is unavailable; activity cannot be matched to exact specialist versions.</p>}
      {state.catalog && !state.catalogLoading && !selectedSpecialistAvailable && <p role="status">
        Selected specialist <code>{specialistId}</code> is unavailable for this investigation budget. Choose an available specialist explicitly to continue.
        {parent && ' The saved parent keeps its original specialist version.'}
      </p>}
      {state.catalogLoading && <p role="status">Loading specialists…</p>}
      {state.catalogError && <p className="error-text" role="alert">{state.catalogError}</p>}
      {!state.catalog && !state.catalogLoading && <p className="muted">The app core supplies available specialists and provider limits.</p>}
      {completionVersion && <fieldset disabled={locked || state.obligationChoicesLoading} aria-label="Required completion evidence"><legend>Required completion evidence</legend>
        <p>Select captured sources in the evidence drawer or choose recorded inventories below. Viewing evidence does not discharge an obligation.</p>
        <p>Choose 1–{policy?.max_completion_obligations ?? 12} combined obligations within the complete {requestLimit.toLocaleString()}-byte request. Follow-ups require fresh explicit selections.</p>
        <button type="button" className="secondary-button" onClick={() => void state.loadObligationChoices(scope)}>Refresh completion choices</button>
        {state.obligationChoicesLoading && <p role="status">Resolving exact completion context…</p>}
        {choices && <><ul>{choices.inventories.map((item) => <li key={item.repo}>
          <strong>{item.repo}</strong> · {item.status}
          <button type="button" className="secondary-button" disabled={!item.obligation || state.draftObligations.some((selected) => selected.obligation_id === item.obligation?.obligation_id)}
            onClick={() => item.obligation && state.addObligation(item.obligation)} aria-label={`Require inventory ${item.repo}`}>Require inventory</button>
          {item.status !== 'recorded' && <span> Only recorded inventory from this exact context can be selected.</span>}
        </li>)}</ul><details><summary>Selection context</summary><code>{choices.context_id}</code></details></>}
        <ul>{state.draftObligations.map((item) => <li key={item.obligation_id}>
          {item.kind === 'source' ? `${item.target.source.repo}/${item.target.source.path}` : `${item.target.repo} inventory`}
          <button type="button" className="secondary-button" aria-label={`Remove obligation ${item.obligation_id}`} onClick={() => state.removeObligation(item.obligation_id)}>Remove</button>
        </li>)}</ul>
        {!completionBound && <p role="status">Select at least one obligation resolved from the current context. Refresh and remove stale selections if the context changed.</p>}
        {requirements.length > 0 && <><p>Earlier source requirements are separate from V7 obligations.</p>
          <button type="button" className="secondary-button" onClick={() => {
            void (async () => { for (const target of requirements) if (!await state.addSourceObligation(target)) return; state.clearInspectionRequirements(); })();
          }}>Require selected sources for completion</button></>}
        {state.obligationDraftError && <p role="alert" className="error-text">{state.obligationDraftError}</p>}
      </fieldset>}
      {!completionVersion && state.draftObligations.length > 0 && <p role="status">Completion obligations require an explicit V7 specialist. Remove them before using another version.
        <button type="button" onClick={state.clearObligations} disabled={locked}>Remove completion obligations</button></p>}
      {!completionVersion && <fieldset disabled={locked} aria-label="Required source inspections"><legend>Required source inspections</legend>
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
      </fieldset>}
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
          placeholder={specialistId.startsWith('spec-writer@') ? 'What specification should the cited evidence support, and what remains unresolved?' : specialistId.startsWith('gap-triager@') ? 'Which recorded gap should be investigated next, and what evidence would reduce it?' : specialistId.startsWith('domain-analyst@') ? 'What business behavior is evidenced in this scope, and what remains unknown?' : 'Which claims have supporting evidence, and where is that evidence incomplete?'}
          onChange={(event) => setQuestion(event.target.value)} />
      </label>
      <p className={questionBytes > 2048 ? 'error-text' : 'muted'}>{questionBytes} / 2,048 UTF-8 bytes. Scope uses the recovered graph at preparation time.</p>
      {requestBytes > requestLimit && <p role="alert" className="error-text">This request exceeds the {requestLimit.toLocaleString()}-byte limit. Shorten the question or remove required sources.</p>}
      {parent && <p className="muted">New task following <code>{parent.parent_id}</code>; earlier findings remain T3 history.
        <button type="button" className="secondary-button" disabled={locked} onClick={() => setParent(null)}>Start a separate conversation</button></p>}
      {limits && <RunLimitFooter limits={limits}
        usesSavedLimits={state.catalog?.configured_limit_specialists?.includes(specialistId) ?? false} />}
      <button type="submit" disabled={!startEnabled}>{state.starting ? 'Saving investigation…' : parent ? 'Start follow-up' : 'Start investigation'}</button>
      {state.startError && <div role="alert" className="error-text"><p>{state.startError}</p>
        {state.pendingStart && <button type="button" className="secondary-button" disabled={state.starting} onClick={() => void state.retryStart()}>Retry same start request</button>}</div>}
    </form>

    <div className="investigation-layout">
      <aside className="investigation-history" aria-label="Investigation history">
        <h3>{state.historyActiveOnly ? 'Active investigations' : 'Saved investigations'}</h3>
        <p className="muted">Questions, activity and findings survive restart and job cleanup.</p>
        <div role="group" aria-label="Investigation history view">
          <button type="button" className="secondary-button" aria-pressed={!state.historyActiveOnly}
            onClick={() => void state.selectHistoryActive(false)}>All investigations</button>
          <button type="button" className="secondary-button" aria-pressed={state.historyActiveOnly}
            onClick={() => void state.selectHistoryActive(true)}>Active investigations</button>
        </div>
        <form aria-label="Search saved investigations" onSubmit={(event) => {
          event.preventDefault();
          void state.searchHistory(historySearchText);
        }}>
          <label>Search saved questions, specialist names or IDs, and answers
            <input type="search" value={historySearchText} onChange={(event) => setHistorySearchText(event.target.value)} />
          </label>
          <button type="submit" className="secondary-button">Search history</button>
          {(historySearchText || state.historySearch) && <button type="button" className="secondary-button" onClick={() => {
            setHistorySearchText('');
            void state.searchHistory('');
          }}>Clear search</button>}
        </form>
        {state.historyLoading && <p role="status">Loading history…</p>}
        {state.historyError && <p role="alert" className="error-text">{state.historyError}</p>}
        {!state.historyLoading && state.history.length === 0 && <p className="muted">
          {state.historyActiveOnly
            ? (/^[\t\n\v\f\r ]+$/.test(state.historySearch) || !state.historySearch
              ? 'No active investigations.' : 'No active investigations match this search.')
            : (/^[\t\n\v\f\r ]+$/.test(state.historySearch) || !state.historySearch
              ? 'No saved investigations loaded.' : 'No saved investigations match this search.')}
        </p>}
        <ul>{state.history.map((item) => <li key={item.investigation_id}>
          <button type="button" className={`investigation-history-item${item.investigation_id === state.selectedId ? ' selected' : ''}`}
            aria-pressed={item.investigation_id === state.selectedId} onClick={() => void state.open(item.investigation_id)}>
            <strong>{item.question}</strong>{state.historyAnswerMatchIds.includes(item.investigation_id) && <span>Answer match</span>}{state.historyNameMatchIds.includes(item.investigation_id) && <span>Name match</span>}<span>{item.specialist_id}{isRetired(item.specialist_id) && ' · retired'}</span><span>{INVESTIGATION_STATUS[item.status]}</span>
            {state.historyActiveOnly && <><span>Provider: {item.provider_mode}</span><time>Updated {item.updated_at}</time></>}
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
            <p className="muted">Specialist: <code>{summary.specialist_id}</code>{isRetired(summary.specialist_id) &&
              ' · retired: this run reopens read-only, and a follow-up needs a version offered today'}</p>
            <p className="muted">Scope: {summary.scope.type === 'all' ? 'Recovered system' : `${summary.scope.anchor} · ${summary.scope.hops} hop(s)`}</p>
            <p className="muted">Prepared graph: <code>{summary.graph_snapshot_id ?? 'Pending preparation'}</code></p>
            <div className="investigation-actions">
              {[4, 5, 6, 7].includes(summary.schema_version) && <p>{summary.completion_outcome === 'obligations_unmet' ? 'Completion obligations unmet; no result admitted.' :
                summary.completion_outcome === 'findings_withheld_after_correction' ? 'Some findings were withheld because their claims could not be verified.' :
                summary.completion_outcome === 'insufficient_evidence' ? 'Admitted insufficient-evidence completion.' : summary.completion_outcome === 'supported' ? 'Supported completion.' : 'No completion outcome recorded.'}</p>}
              {summary.actions.can_cancel && <button type="button" className="danger-button" disabled={state.actionBusy}
                onClick={() => void state.cancel()}>Request cancellation</button>}
              {summary.conversation_id && <button type="button" className="secondary-button"
                aria-expanded={state.conversationId === summary.conversation_id}
                onClick={() => state.conversationId === summary.conversation_id ? state.closeConversation() : void state.openConversation(summary.conversation_id)}>
                {state.conversationId === summary.conversation_id ? 'Close conversation turns' : 'Open conversation turns'}
              </button>}
              {summary.actions.can_follow_up && <button type="button" className="secondary-button" disabled={locked || state.catalogLoading || !state.catalog} onClick={() => {
                if (state.catalogLoading || !state.catalog) return;
                state.clearInspectionRequirements();
                state.clearObligations();
                setParent({ conversation_id: summary.conversation_id, parent_id: summary.investigation_id });
                setScopeType(summary.scope.type); if (summary.scope.type === 'neighborhood') { setAnchor(summary.scope.anchor); setHops(summary.scope.hops); }
                state.updateComposer({ specialistId: summary.specialist_id, limitProfile: isExpandedSpecialist(summary.specialist_id) ? 'investigation-v2' : 'investigation-v1' });
                setProviderMode(summary.provider_mode); setQuestion(''); questionRef.current?.focus();
              }}>Ask a follow-up</button>}
              {state.consent && <button type="button" disabled={state.actionBusy || state.loading} onClick={state.showConsent}>Review next cloud action</button>}
            </div>
          </>}
          {state.conversationId && <section className="investigation-section" aria-label="Conversation turns">
            <h4>Conversation turns</h4>
            <p className="muted">Saved investigation tasks in this conversation, newest first. Each task keeps its own result and citations.</p>
            {conversationSelected && !selectedIsOnPage && <div className="muted" aria-label="Selected conversation turn">
              <strong>Selected turn (pinned):</strong> {conversationSelected.question} · <code>{conversationSelected.specialist_id}</code>
              <span> · {INVESTIGATION_STATUS[conversationSelected.status]} · {conversationSelected.updated_at}</span>
              {conversationSelected.parent_id && <><span> · follow-up to {conversationSelected.parent_id}</span>
                <button type="button" className="secondary-button" onClick={() => void state.open(conversationSelected.parent_id!)}>Open parent turn</button></>}
            </div>}
            {state.conversationLoading && <p role="status">Loading conversation turns…</p>}
            {state.conversationError && <p role="alert" className="error-text">{state.conversationError}</p>}
            {!state.conversationLoading && conversationRows.length === 0 && !conversationSelected && <p className="muted">No saved turns in this conversation.</p>}
            <button type="button" className="secondary-button" disabled={state.conversationLoading}
              onClick={() => void state.loadConversation()}>Refresh conversation turns</button>
            <ol>{conversationRows.map((item) => <li key={item.investigation_id}>
              <button type="button" className="investigation-history-item" aria-current={item.investigation_id === state.selectedId ? 'true' : undefined}
                onClick={() => void state.open(item.investigation_id)}>
                <strong>{item.question}</strong><span><code>{item.specialist_id}</code></span>
                <span>{INVESTIGATION_STATUS[item.status]}</span><time>{item.updated_at}</time>
                {item.parent_id && <span>Follow-up to {item.parent_id}</span>}
              </button>
              {item.parent_id && <button type="button" className="secondary-button" onClick={() => void state.open(item.parent_id!)}>
                Open parent turn
              </button>}
            </li>)}</ol>
            {state.conversationCursor && <button type="button" className="secondary-button" disabled={state.conversationLoading}
              onClick={() => void state.loadConversation(true)}>Load older turns</button>}
          </section>}
          {state.loading && !state.detail && <p role="status">Loading saved investigation…</p>}
          {state.error && <p className="error-text" role="alert">{state.error}</p>}
          {state.actionError && <p className="error-text" role="alert">{state.actionError}</p>}
          {state.detail && <InvestigationProgress status={state.detail.status} cancelRequested={state.detail.cancel_requested}
            events={state.events} usage={state.detail.usage} limits={state.detail.limits} />}
          {state.detail && <>
            {[3, 4, 5, 6, 7].includes(state.detail.schema_version) && <InvestigationCoverage context={state.detail.coverage_context}
              attestation={state.result?.coverage} />}
            {readsArtifacts && <InvestigationArtifactReads reads={artifactReads.reads} error={artifactReads.error} />}
            {[4, 5, 6, 7].includes(state.detail.schema_version) && state.detail.completion && <CompletionAssessment completion={state.detail.completion} outcome={state.detail.completion_outcome} />}
            {![4, 5, 6, 7].includes(state.detail.schema_version) && <InspectionAssessment requirements={state.detail.inspection_requirements ?? []}
              awaitingAssessment={!['completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'].includes(state.detail.status) || state.detail.invocation_pending}
              assessment={state.detail.inspection_assessment} location={state.inspectionLocation}
              locationTargetId={state.inspectionLocationTargetId} locationLoading={state.inspectionLocationLoading}
              locationError={state.inspectionLocationError} onLocation={(target) => void state.readInspectionLocation(target)} />}
            <details className="investigation-section"><summary>Specialist, provider and consumed limits</summary>
              <p>{state.detail.specialist.name} · <code>{state.detail.specialist.id}</code> · T3 / InferredWeak</p>
              <p>Prompt fingerprint: <code>{state.detail.specialist.prompt_fingerprint}</code></p>
              <p>Requested model (saved configuration): <code>{state.detail.provider.provider_id} / {state.detail.provider.model}</code></p>
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
          {state.detail && <InvestigationChatTranscript chat={state.chat} status={state.chatStatus}
            result={state.result} citations={state.detail.investigation_id === state.chat?.task_id &&
              state.result?.investigation_id === state.chat?.task_id ? state.detail.citations : undefined}
            error={state.chatError} onRefresh={() => void state.refreshSelected()} />}
          <InvestigationActivity events={state.events} />
          <InvestigationFindings advisoryAudit={state.detail?.investigation_id === state.result?.investigation_id ? state.detail?.advisory_audit : undefined} reviews={state.findingReviews} reviewLoading={state.reviewLoading} reviewBusy={state.reviewBusy} reviewError={state.reviewError}
            onRefreshReviews={() => void state.refreshReviews()} onReview={(id, decision) => void state.reviewFinding(id, decision)}
            curations={state.findingCurations} curationLoading={state.curationLoading} curationBusy={state.curationBusy} curationError={state.curationError}
            onRefreshCurations={() => void state.refreshCurations()} onCurate={(id, decision) => void state.curateFinding(id, decision)} producerId={state.detail?.specialist_id} result={state.result} citations={state.detail?.citations ?? []}
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

function SpecialistTaskActivity({ tasks, onOpen }: {
  tasks: SpecialistActivityItem[];
  onOpen: (investigationId: string) => void;
}) {
  return <div className="specialist-task-activity">
    {tasks.map((task) => <SpecialistTaskRow key={task.summary.investigation_id} task={task} onOpen={onOpen} />)}
  </div>;
}

const ACTIVITY_STATUS_LABELS = {
  queued: 'Queued', preparing: 'Preparing context', running: 'Running', awaiting_consent: 'Awaiting cloud consent',
} as const;

function SpecialistTaskRow({ task, onOpen }: { task: SpecialistActivityItem; onOpen: (investigationId: string) => void }) {
  const summary = task.summary;
  const status = `${ACTIVITY_STATUS_LABELS[summary.status as keyof typeof ACTIVITY_STATUS_LABELS] ?? 'Unrecognized status'}${summary.cancel_requested ? ' · Cancellation requested' : ''}`;
  return <article className="specialist-task-row">
    <p className="specialist-task-question">{summary.question}</p>
    <p className="muted">{status} · Saved provider: {task.saved_provider.mode} / {task.saved_provider.provider_id} · {task.saved_provider.model}</p>
    <p className="muted">{task.reported_usage
      ? `Reported tokens so far: input ${task.reported_usage.input_tokens ?? 'Unknown'}; output ${task.reported_usage.output_tokens ?? 'Unknown'}.`
      : 'Reported tokens unavailable.'} Provider-reported cache-read tokens so far: {task.reported_cache_read_tokens === undefined
        ? 'Unavailable' : task.reported_cache_read_tokens ?? 'Unknown'}. May be incomplete; not a cost estimate.</p>
    <button type="button" className="secondary-button" aria-label={`Open investigation: ${summary.question}`}
      onClick={() => onOpen(summary.investigation_id)}>Open investigation</button>
  </article>;
}
