import { createPortal } from 'react-dom';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { DESKTOP_DEFAULT_SPECIALIST, useInvestigationStore, type InvestigationDraft } from '../investigationStore';
import { isCompletionSpecialist } from '../investigationFamilies';
import { ROLE_LABELS, currentVersion, offeredRoles, offeredVersions, profileFor, roleOf } from '../specialistRoles';
import { investigationPolicy, obligationEnvelope, validObligationChoices, validObligationEnvelope } from '../investigationObligations';
import { invokeOr } from '../tauri';
import type { InvestigationFollowUpContext, InvestigationObligation, InvestigationObligationChoices, InvestigationProviderMode, InvestigationScope, SpecialistId } from '../investigationTypes';
import { composerScope, investigationScopeLabel, investigationScopeValid } from '../investigationTypes';
import { isQuickDispatchKey } from '../shortcuts';


export interface InvestigationQuickDispatchProps {
  fallbackEnabled: boolean;
  openRequest: number;
  followUpCancelRequest?: number;
  followUpContext?: InvestigationFollowUpContext | null;
  inlineReply?: { taskId: string; host: HTMLElement } | null;
  onStandalone?: () => void;
  onCloseParent?: (taskId: string) => void;
  onNavigate: (view: 'investigations') => void;
  onAcceptedTask: (taskId: string) => void;
}

export function InvestigationQuickDispatch({ fallbackEnabled, openRequest, followUpCancelRequest = 0, followUpContext = null, inlineReply = null, onStandalone, onCloseParent, onNavigate, onAcceptedTask }: InvestigationQuickDispatchProps) {
  const state = useInvestigationStore();
  const [open, setOpen] = useState(false);
  const [dialogParent, setDialogParent] = useState<InvestigationFollowUpContext | null>(null);
  const [inline, setInline] = useState(false);
  const blockedParent = useRef(false);
  const [question, setQuestion] = useState('');
  const [chosenSpecialistId, setSpecialistId] = useState<SpecialistId>(DESKTOP_DEFAULT_SPECIALIST);
  const [providerMode, setProviderMode] = useState<InvestigationProviderMode>('local');
  const [busy, setBusy] = useState(false);
  const [choices, setChoices] = useState<InvestigationObligationChoices | null>(null);
  const [choicesIdentity, setChoicesIdentity] = useState<object | null>(null);
  const [selectedInventoryIds, setSelectedInventoryIds] = useState<string[]>([]);
  const [choicesError, setChoicesError] = useState<string | null>(null);
  const [choicesErrorIdentity, setChoicesErrorIdentity] = useState<object | null>(null);
  const [choicesRefresh, setChoicesRefresh] = useState(0);
  const previousFocus = useRef<HTMLElement | null>(null);
  const handledOpenRequest = useRef(0);
  const handledFollowUpCancel = useRef(0);
  const catalogRequested = useRef(false);
  const dialogRef = useRef<HTMLElement>(null);
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const choicesGeneration = useRef(0);
  // ADR-0071: quick dispatch offers roles; each runs its current version.
  const specialists = useMemo(() => offeredRoles(state.catalog).map((role) => currentVersion(state.catalog, role)!), [state.catalog]);
  // A choice the catalog no longer offers falls back to the same role's current version.
  const specialistId = dialogParent || !specialists.length || specialists.some((item) => item.id === chosenSpecialistId) ? chosenSpecialistId :
    (specialists.find((item) => roleOf(item.id) === roleOf(chosenSpecialistId)) ?? specialists[0]).id;
  const profile = profileFor(state.catalog, specialistId);
  // A follow-up keeps its saved version while the catalog still offers it (ADR-0071 decision 2).
  const parentRole = dialogParent ? roleOf(dialogParent.specialist_id) : undefined;
  const parentOffered = Boolean(dialogParent && parentRole && offeredVersions(state.catalog, parentRole).some((item) => item.id === dialogParent.specialist_id));
  const specialist = dialogParent ? state.catalog?.specialists.find((item) => item.id === specialistId) : specialists.find((item) => item.id === specialistId);
  const providers = state.catalog?.providers ?? [];
  const provider = providers.find((item) => item.mode === providerMode);
  const supported = dialogParent ? parentOffered : specialists.some((item) => item.id === specialistId);
  const composer = state.composer;
  const fromComposer: InvestigationScope = useMemo(() => composerScope(composer),
    [composer]);
  const scope = dialogParent?.scope ?? fromComposer;
  const scopeKey = JSON.stringify(scope);
  const choicesRequestIdentity = useMemo(() => ({ choicesRefresh, dialogParent, open, scopeKey, specialistId }),
    [choicesRefresh, dialogParent, open, scopeKey, specialistId]);
  const scopeValid = investigationScopeValid(scope);
  const questionBytes = new TextEncoder().encode(question).length;
  const hasPendingStart = state.pendingStart !== null;
  const inFlight = busy || state.starting;
  const locked = inFlight || state.pendingStart !== null;
  const completionRole = isCompletionSpecialist(specialistId);
  const policy = completionRole ? investigationPolicy(state.catalog, profile, specialistId) : undefined;
  const currentChoices = choicesIdentity === choicesRequestIdentity ? choices : null;
  const choicesLoading = open && completionRole && scopeValid && choicesIdentity !== choicesRequestIdentity && choicesErrorIdentity !== choicesRequestIdentity;
  const currentChoicesError = choicesErrorIdentity === choicesRequestIdentity ? choicesError : null;
  const selectedObligations: InvestigationObligation[] = currentChoices?.inventories
    .filter((item) => item.status === 'recorded' && item.obligation && selectedInventoryIds.includes(item.obligation.obligation_id))
    .map((item) => item.obligation!) ?? [];
  const completionObligations = obligationEnvelope(selectedObligations, policy);
  const validCompletionSelection = Boolean(currentChoices && validObligationChoices(currentChoices) && policy &&
    validObligationEnvelope(completionObligations, policy) && completionObligations.obligations.length <= policy.max_completion_obligations);
  const maxCompletionObligations = policy?.max_completion_obligations ?? 0;
  const atCompletionObligationLimit = selectedObligations.length >= maxCompletionObligations;
  const requestBytes = completionRole && policy ? new TextEncoder().encode(JSON.stringify({ schema_version: policy.request_schema_version,
    request_nonce: '0'.repeat(36), specialist_id: specialistId, obligations: completionObligations,
    question, scope, provider_mode: providerMode, limit_profile: profile,
    expected_graph_revision: currentChoices?.graph_snapshot_id ?? null, expected_context_id: currentChoices?.context_id ?? '',
    conversation_id: dialogParent?.conversation_id ?? null, parent_id: dialogParent?.parent_id ?? null })).length : 0;
  const valid = supported && Boolean(provider?.available) && question.trim().length > 0 && questionBytes <= 2048 && scopeValid &&
    (!completionRole || (validCompletionSelection && requestBytes <= (policy?.request_bytes ?? 0)));

  const openDialog = useCallback((parent: InvestigationFollowUpContext | null = null) => {
    if (open && ((!dialogParent && !parent) || inFlight)) return;
    blockedParent.current = false;
    handledFollowUpCancel.current = followUpCancelRequest;
    setInline(Boolean(parent && inlineReply?.taskId === parent.parent_id));
    previousFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDialogParent(parent);
    if (parent) { setSpecialistId(parent.specialist_id); setProviderMode(parent.provider_mode); if (dialogParent) setQuestion(''); }
    else if (dialogParent) setQuestion('');
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    setOpen(true);
  }, [dialogParent, followUpCancelRequest, inFlight, inlineReply, open]);
  const close = useCallback((accepted = false, externalCancel = false) => {
    if (inFlight && !accepted) return;
    if (dialogParent && !accepted) {
      blockedParent.current = true;
      if (!externalCancel && followUpCancelRequest === handledFollowUpCancel.current &&
          followUpContext?.parent_id === dialogParent.parent_id && followUpContext.conversation_id === dialogParent.conversation_id) {
        onCloseParent?.(dialogParent.parent_id);
      }
    }
    ++choicesGeneration.current;
    setOpen(false);
    if (dialogParent) setQuestion('');
    setDialogParent(null);
    catalogRequested.current = false;
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    if (previousFocus.current?.isConnected) previousFocus.current.focus();
  }, [dialogParent, followUpCancelRequest, followUpContext, inFlight, onCloseParent]);
  useEffect(() => {
    if (openRequest === handledOpenRequest.current || inFlight) return;
    handledOpenRequest.current = openRequest;
    openDialog(followUpContext);
  }, [followUpContext, inFlight, openDialog, openRequest]);
  useEffect(() => {
    if (followUpCancelRequest === handledFollowUpCancel.current) return;
    if (!dialogParent) { handledFollowUpCancel.current = followUpCancelRequest; return; }
    if (inFlight) return;
    const timer = window.setTimeout(() => {
      handledFollowUpCancel.current = followUpCancelRequest;
      close(false, true);
    }, 0);
    return () => window.clearTimeout(timer);
  }, [close, dialogParent, followUpCancelRequest, inFlight]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && open) {
        const target = event.target;
        const insideInline = target instanceof Node && Boolean(dialogRef.current?.contains(target));
        if (!inline || insideInline) { event.preventDefault(); close(); }
        return;
      }
      if (isQuickDispatchKey(event, fallbackEnabled)) {
        event.preventDefault();
        blockedParent.current = true;
        if (onStandalone) onStandalone(); else openDialog();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [close, fallbackEnabled, inline, onStandalone, open, openDialog]);
  useEffect(() => {
    if (!open) return;
    if (questionRef.current) questionRef.current.focus();
    else dialogRef.current?.querySelector<HTMLElement>('button:not(:disabled)')?.focus();
    const current = useInvestigationStore.getState();
    if (!current.catalog && !current.catalogLoading && !catalogRequested.current) {
      catalogRequested.current = true;
      void current.loadCatalog();
    }
  }, [dialogParent, inlineReply, open, hasPendingStart]);
  useEffect(() => {
    const request = ++choicesGeneration.current;
    if (!open || !completionRole || !scopeValid) {
      return () => { if (choicesGeneration.current === request) choicesGeneration.current += 1; };
    }
    void invokeOr<InvestigationObligationChoices | null>('investigation_obligation_choices', null, { scope }).then((value) => {
      if (choicesGeneration.current !== request) return;
      if (!validObligationChoices(value)) throw new Error('Invalid completion choices');
      setChoices(value); setChoicesIdentity(choicesRequestIdentity); setSelectedInventoryIds([]);
      setChoicesError(null); setChoicesErrorIdentity(null);
    }).catch(() => {
      if (choicesGeneration.current === request) {
        setChoices(null); setChoicesIdentity(null);
        setChoicesError('Completion choices could not be resolved. Refresh the dialog before selecting inventory.');
        setChoicesErrorIdentity(choicesRequestIdentity);
      }
    });
    return () => { if (choicesGeneration.current === request) choicesGeneration.current += 1; };
  }, [choicesRequestIdentity, completionRole, open, scope, scopeValid]);

  const accepted = (id: string | null) => {
    if (id === null) return;
    onAcceptedTask(id);
    close(true);
    setQuestion('');
  };
  const refreshAfterStaleCompletion = (requestedSpecialist: SpecialistId | undefined) => {
    const current = useInvestigationStore.getState();
    if (!requestedSpecialist || !isCompletionSpecialist(requestedSpecialist) || current.pendingStart ||
        current.startRejection !== 'refresh_required') return;
    setSpecialistId(requestedSpecialist);
    ++choicesGeneration.current;
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    setChoicesRefresh((generation) => generation + 1);
  };
  const validParentPlacement = () => !dialogParent || (!blockedParent.current &&
    followUpContext?.parent_id === dialogParent.parent_id && followUpContext.conversation_id === dialogParent.conversation_id &&
    (!inline || (inlineReply?.taskId === dialogParent.parent_id && inlineReply.host.isConnected)));
  const start = async () => {
    if (!valid || locked || state.pendingStart || !validParentPlacement() ||
        (dialogParent && followUpCancelRequest !== handledFollowUpCancel.current)) return;
    // Construct the quick request only from overlay-owned inputs. Full-composer
    // obligations, parent links, and inspection requirements never leak here.
    const draft: InvestigationDraft = {
      specialist_id: specialistId,
      question,
      scope,
      provider_mode: providerMode,
      limit_profile: profile,
      ...(dialogParent ? { conversation_id: dialogParent.conversation_id, parent_id: dialogParent.parent_id } : {}),
      ...(completionRole ? { obligations: completionObligations } : {}),
    };
    setBusy(true);
    try {
      const id = await state.start(draft, { preserveComposer: true, deferOpen: true,
        ...(completionRole && currentChoices ? { completionChoices: currentChoices } : {}) });
      if (id) accepted(id);
      else refreshAfterStaleCompletion(specialistId);
    } finally { setBusy(false); }
  };
  const retry = async () => {
    if (busy || !state.pendingStart || !validParentPlacement() ||
        (dialogParent && followUpCancelRequest !== handledFollowUpCancel.current)) return;
    const requestedSpecialist = state.pendingStart.specialist_id;
    setBusy(true);
    try {
      const id = await state.retryStart({ deferOpen: true });
      if (id) accepted(id);
      else refreshAfterStaleCompletion(requestedSpecialist);
    } finally { setBusy(false); }
  };

  if (!open) return null;
  const pendingMatchesParent = !dialogParent || Boolean(state.pendingStart &&
    state.pendingStart.conversation_id === dialogParent.conversation_id && state.pendingStart.parent_id === dialogParent.parent_id);
  const pendingIsQuick = state.pendingStart && (specialists.some((item) => item.id === state.pendingStart?.specialist_id) ||
    (parentOffered && state.pendingStart.specialist_id === dialogParent?.specialist_id)) && pendingMatchesParent;
  const content = <div className={inline ? 'quick-dispatch-inline' : 'quick-dispatch-backdrop'} onMouseDown={(event) => { if (!inline && event.target === event.currentTarget && !inFlight) close(); }}>
    <section ref={dialogRef} className="quick-dispatch-dialog" role={inline ? 'region' : 'dialog'} aria-modal={inline ? undefined : true}
      aria-label={inline ? `Reply to ${dialogParent?.parent_id}` : 'Quick investigation dispatch'} onKeyDown={(event) => {
      if (inline && event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); void start(); return; }
      if (inline || event.key !== 'Tab') return;
      const items = [...event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')];
      if (!items.length) { event.preventDefault(); return; }
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    }}>
      <header><div><h2>{inline ? 'Reply task' : dialogParent ? 'Follow-up task' : 'Quick investigation'}</h2><p className="muted">Start a saved investigation with a supported specialist. Findings remain T3 / InferredWeak.</p></div>
        <button type="button" className="secondary-button" onClick={() => close()} disabled={inFlight} aria-label={inline ? 'Close inline reply' : 'Close quick investigation'}>Close</button></header>
      {state.catalogError && <p className="error-text" role="alert">Specialist catalog unavailable. Refresh in Investigations before starting.</p>}
      {state.startError && <p className="error-text" role="alert">{state.startError}</p>}
      {state.pendingStart ? <div>
        <p>{pendingIsQuick ? 'The start outcome is uncertain. Retry the same saved request to recover its identity.' : 'A different investigation request is pending. Continue in the full Investigations composer to recover it.'}</p>
        <p>Question: {state.pendingStart.question}</p>
        {pendingIsQuick && <button type="button" onClick={() => void retry()} disabled={busy || state.starting}>Retry same request</button>}
        {!pendingIsQuick && <button type="button" onClick={() => { onNavigate('investigations'); close(); }}>Open Investigations</button>}
      </div> : <form onSubmit={(event) => { event.preventDefault(); void start(); }}>
        {dialogParent && <p className="muted">Following <code>{dialogParent.parent_id}</code> in conversation <code>{dialogParent.conversation_id}</code>.
          Saved producer: {dialogParent.specialist_id} · {dialogParent.saved_provider_id} · {dialogParent.saved_model}.</p>}
        {inline && <p className="muted">Context is the selected saved parent result as bounded T3 history, not all earlier questions or answers. Sending creates a separate child task.</p>}
        {!supported && dialogParent && <p className="error-text" role="alert">This saved specialist version is not supported for Quick Dispatch. Continue in Investigations.</p>}
        <label>Specialist<select value={specialistId} disabled={locked || Boolean(dialogParent) || !specialists.length} onChange={(event) => setSpecialistId(event.target.value as SpecialistId)}>
          {dialogParent && !specialists.some((item) => item.id === dialogParent.specialist_id) &&
            <option value={dialogParent.specialist_id}>{parentRole ? `${ROLE_LABELS[parentRole]} · ` : ''}{dialogParent.specialist_id}{parentOffered ? ' · saved version' : ' · unsupported version'}</option>}
          {specialists.map((item) => <option key={item.id} value={item.id}>{ROLE_LABELS[roleOf(item.id)!]}</option>)}
        </select></label>
        <p className="muted">{specialist?.purpose} T3 · InferredWeak. Advanced evidence and completion work stays in the full composer.</p>
        <p className="muted">Scope: {investigationScopeLabel(scope)}{dialogParent ? ' · captured from the saved parent' : '. Change scope in the full Investigations composer.'}</p>
        <label>Provider<select value={providerMode} disabled={locked || Boolean(dialogParent) || !providers.length} onChange={(event) => setProviderMode(event.target.value as InvestigationProviderMode)}>
          {dialogParent && !providers.some((item) => item.mode === dialogParent.provider_mode) &&
            <option value={dialogParent.provider_mode}>{dialogParent.provider_mode} · unavailable</option>}
          {providers.map((item) => <option key={item.mode} value={item.mode} disabled={!item.available}>{item.mode} · {item.provider_id} · {item.model}{item.available ? '' : ` · unavailable: ${item.unavailable_reason ?? 'not available'}`}</option>)}
        </select></label>
        {dialogParent && provider && <p className="muted">Current configured provider for {providerMode}: {provider.provider_id} · {provider.model}.</p>}
        {!provider?.available && provider && <p className="muted">{provider.unavailable_reason ?? 'This configured provider is unavailable.'} Choose an available provider or use the full composer.</p>}
        <label>Question<textarea ref={questionRef} value={question} maxLength={2048} disabled={locked} onChange={(event) => setQuestion(event.target.value)} /></label>
        {questionBytes > 2048 && <p className="error-text">Question exceeds the existing 2,048-byte limit.</p>}
        {completionRole && <fieldset disabled={locked || choicesLoading} aria-label="Recorded inventory obligations"><legend>Required completion evidence</legend>
          <p>Select at least one recorded inventory from this scope. Only the selections below are included in the start request.</p>
          {choicesLoading && <p role="status">Resolving recorded inventories for this scope…</p>}
          {currentChoicesError && <p role="alert" className="error-text">{currentChoicesError}</p>}
          {!policy && <p role="status">This specialist has no compatible completion policy in the current catalog.</p>}
          {currentChoices && <p role="status">Selected {selectedObligations.length} / {maxCompletionObligations} inventory obligations.</p>}
          {currentChoices && <ul>{currentChoices.inventories.map((item) => {
            const obligation = item.status === 'recorded' ? item.obligation : undefined;
            const selected = Boolean(obligation && selectedInventoryIds.includes(obligation.obligation_id));
            return <li key={item.repo}><strong>{item.repo}</strong> · {item.status}{obligation && <button type="button"
              disabled={!selected && atCompletionObligationLimit}
              aria-pressed={selected} aria-label={`${selected ? 'Remove' : 'Require'} inventory ${item.repo}`}
              onClick={() => setSelectedInventoryIds((current) => selected ? current.filter((id) => id !== obligation.obligation_id) : [...current, obligation.obligation_id])}>
              {selected ? 'Remove selection' : 'Require inventory'}</button>}
            </li>;
          })}</ul>}
          {!choicesLoading && currentChoices && selectedObligations.length === 0 && <p role="status">Select at least one recorded inventory to continue.</p>}
          {completionRole && policy && requestBytes > policy.request_bytes && <p role="alert" className="error-text">The complete request exceeds the existing {policy.request_bytes.toLocaleString()}-byte limit. Remove an inventory or shorten the question.</p>}
        </fieldset>}
        {state.catalog && specialists.length === 0 && <p className="muted">No supported quick-dispatch specialists are available. Use the full Investigations composer.</p>}
        {scope.type === 'neighborhood' && !scopeValid && <p role="alert" className="error-text">Choose a valid fact anchor and one to three hops in the full Investigations composer.</p>}
        {scope.type === 'area' && !scopeValid && <p role="alert" className="error-text">Choose an area in the full Investigations composer.</p>}
        <footer><button type="button" className="secondary-button" onClick={() => close()} disabled={inFlight}>Cancel</button>
          <button type="submit" disabled={!valid || locked || state.catalogLoading}>{inline ? 'Send reply task' : dialogParent ? 'Start follow-up task' : 'Start investigation'}</button></footer>
      </form>}
    </section>
  </div>;
  if (!inline) return content;
  return inlineReply && dialogParent && inlineReply.taskId === dialogParent.parent_id && inlineReply.host.isConnected
    ? createPortal(content, inlineReply.host) : null;
}
