import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useInvestigationStore, type InvestigationDraft } from '../investigationStore';
import { investigationPolicy, obligationEnvelope, validObligationChoices, validObligationEnvelope } from '../investigationObligations';
import { invokeOr } from '../tauri';
import type { InvestigationObligation, InvestigationObligationChoices, InvestigationProviderMode, InvestigationScope, SpecialistId } from '../investigationTypes';

const QUICK_IDS: SpecialistId[] = ['domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4',
  'domain-analyst@7', 'evidence-auditor@7', 'spec-writer@1', 'gap-triager@1'];
const QUICK_COMPLETION_IDS = new Set<SpecialistId>(['domain-analyst@7', 'evidence-auditor@7', 'spec-writer@1', 'gap-triager@1']);

export interface InvestigationQuickDispatchProps {
  fallbackEnabled: boolean;
  openRequest: number;
  onNavigate: (view: 'investigations') => void;
}

export function InvestigationQuickDispatch({ fallbackEnabled, openRequest, onNavigate }: InvestigationQuickDispatchProps) {
  const state = useInvestigationStore();
  const [open, setOpen] = useState(false);
  const [question, setQuestion] = useState('');
  const [specialistId, setSpecialistId] = useState<SpecialistId>('domain-analyst@3');
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
  const catalogRequested = useRef(false);
  const dialogRef = useRef<HTMLElement>(null);
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const choicesGeneration = useRef(0);
  const specialists = useMemo(() => state.catalog?.specialists.filter((item) => QUICK_IDS.includes(item.id) &&
    (!QUICK_COMPLETION_IDS.has(item.id) || Boolean(investigationPolicy(state.catalog, 'investigation-v1', item.id)))) ?? [], [state.catalog]);
  const specialist = specialists.find((item) => item.id === specialistId);
  const providers = state.catalog?.providers ?? [];
  const provider = providers.find((item) => item.mode === providerMode);
  const supported = specialists.some((item) => item.id === specialistId);
  const composer = state.composer;
  const scope: InvestigationScope = useMemo(() => composer.scopeType === 'all' ? { type: 'all' } :
    { type: 'neighborhood', anchor: composer.anchor, hops: composer.hops }, [composer.anchor, composer.hops, composer.scopeType]);
  const scopeKey = JSON.stringify(scope);
  const choicesRequestIdentity = useMemo(() => ({ choicesRefresh, open, scopeKey, specialistId }),
    [choicesRefresh, open, scopeKey, specialistId]);
  const scopeValid = composer.scopeType === 'all' || (composer.anchor.trim().length > 0 && Number.isInteger(composer.hops) && composer.hops >= 1 && composer.hops <= 3);
  const questionBytes = new TextEncoder().encode(question).length;
  const hasPendingStart = state.pendingStart !== null;
  const inFlight = busy || state.starting;
  const locked = inFlight || state.pendingStart !== null;
  const completionRole = QUICK_COMPLETION_IDS.has(specialistId);
  const policy = completionRole ? investigationPolicy(state.catalog, 'investigation-v1', specialistId) : undefined;
  const currentChoices = choicesIdentity === choicesRequestIdentity ? choices : null;
  const choicesLoading = open && completionRole && scopeValid && choicesIdentity !== choicesRequestIdentity && choicesErrorIdentity !== choicesRequestIdentity;
  const currentChoicesError = choicesErrorIdentity === choicesRequestIdentity ? choicesError : null;
  const selectedObligations: InvestigationObligation[] = currentChoices?.inventories
    .filter((item) => item.status === 'recorded' && item.obligation && selectedInventoryIds.includes(item.obligation.obligation_id))
    .map((item) => item.obligation!) ?? [];
  const completionObligations = obligationEnvelope(selectedObligations, policy);
  const validCompletionSelection = Boolean(currentChoices && validObligationChoices(currentChoices) && policy &&
    validObligationEnvelope(completionObligations, policy) && completionObligations.obligations.length <= policy.max_completion_obligations);
  const requestBytes = completionRole && policy ? new TextEncoder().encode(JSON.stringify({ schema_version: policy.request_schema_version,
    request_nonce: '0'.repeat(36), specialist_id: specialistId, obligations: completionObligations,
    question, scope, provider_mode: providerMode, limit_profile: 'investigation-v1',
    expected_graph_revision: currentChoices?.graph_snapshot_id ?? null, expected_context_id: currentChoices?.context_id ?? '',
    conversation_id: null, parent_id: null })).length : 0;
  const valid = supported && Boolean(provider?.available) && question.trim().length > 0 && questionBytes <= 2048 && scopeValid &&
    (!completionRole || (validCompletionSelection && requestBytes <= (policy?.request_bytes ?? 0)));

  const openDialog = useCallback(() => {
    if (open) return;
    previousFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    setOpen(true);
  }, [open]);
  const close = useCallback((accepted = false) => {
    if (inFlight && !accepted) return;
    ++choicesGeneration.current;
    setOpen(false);
    catalogRequested.current = false;
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    if (previousFocus.current?.isConnected) previousFocus.current.focus();
  }, [inFlight]);
  useEffect(() => {
    if (openRequest === handledOpenRequest.current) return;
    handledOpenRequest.current = openRequest;
    openDialog();
  }, [openDialog, openRequest]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && open) { event.preventDefault(); close(); return; }
      if (event.altKey || event.ctrlKey || event.shiftKey || !event.metaKey) return;
      if (event.code === 'Space' || (event.key.toLowerCase() === 'j' && fallbackEnabled)) {
        event.preventDefault();
        openDialog();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [close, fallbackEnabled, open, openDialog]);
  useEffect(() => {
    if (!open) return;
    if (questionRef.current) questionRef.current.focus();
    else dialogRef.current?.querySelector<HTMLElement>('button:not(:disabled)')?.focus();
    const current = useInvestigationStore.getState();
    if (!current.catalog && !current.catalogLoading && !catalogRequested.current) {
      catalogRequested.current = true;
      void current.loadCatalog();
    }
  }, [open, hasPendingStart]);
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
    onNavigate('investigations');
    close(true);
    setQuestion('');
  };
  const refreshAfterStaleCompletion = (requestedSpecialist: SpecialistId | undefined) => {
    const current = useInvestigationStore.getState();
    if (!requestedSpecialist || !QUICK_COMPLETION_IDS.has(requestedSpecialist) || current.pendingStart || !current.startError) return;
    setSpecialistId(requestedSpecialist);
    ++choicesGeneration.current;
    setChoices(null); setChoicesIdentity(null); setSelectedInventoryIds([]); setChoicesError(null); setChoicesErrorIdentity(null);
    setChoicesRefresh((generation) => generation + 1);
  };
  const start = async () => {
    if (!valid || locked || state.pendingStart) return;
    // Construct the quick request only from overlay-owned inputs. Full-composer
    // obligations, parent links, and inspection requirements never leak here.
    const draft: InvestigationDraft = {
      specialist_id: specialistId,
      question,
      scope,
      provider_mode: providerMode,
      limit_profile: 'investigation-v1',
      ...(completionRole ? { obligations: completionObligations } : {}),
    };
    setBusy(true);
    try {
      const id = await state.start(draft, { preserveComposer: true,
        ...(completionRole && currentChoices ? { completionChoices: currentChoices } : {}) });
      if (id) accepted(id);
      else refreshAfterStaleCompletion(specialistId);
    } finally { setBusy(false); }
  };
  const retry = async () => {
    if (busy || !state.pendingStart) return;
    const requestedSpecialist = state.pendingStart.specialist_id;
    setBusy(true);
    try {
      const id = await state.retryStart();
      if (id) accepted(id);
      else refreshAfterStaleCompletion(requestedSpecialist);
    } finally { setBusy(false); }
  };

  if (!open) return null;
  const pendingIsQuick = state.pendingStart && QUICK_IDS.includes(state.pendingStart.specialist_id);
  return <div className="quick-dispatch-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !inFlight) close(); }}>
    <section ref={dialogRef} className="quick-dispatch-dialog" role="dialog" aria-modal="true" aria-label="Quick investigation dispatch" onKeyDown={(event) => {
      if (event.key !== 'Tab') return;
      const items = [...event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')];
      if (!items.length) { event.preventDefault(); return; }
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    }}>
      <header><div><h2>Quick investigation</h2><p className="muted">Start a saved investigation with a supported specialist. Findings remain T3 / InferredWeak.</p></div>
        <button type="button" className="secondary-button" onClick={() => close()} disabled={inFlight} aria-label="Close quick investigation">Close</button></header>
      {state.catalogError && <p className="error-text" role="alert">Specialist catalog unavailable. Refresh in Investigations before starting.</p>}
      {state.startError && <p className="error-text" role="alert">{state.startError}</p>}
      {state.pendingStart ? <div>
        <p>{pendingIsQuick ? 'The start outcome is uncertain. Retry the same saved request to recover its identity.' : 'A different investigation request is pending. Continue in the full Investigations composer to recover it.'}</p>
        <p>Question: {state.pendingStart.question}</p>
        {pendingIsQuick && <button type="button" onClick={() => void retry()} disabled={busy || state.starting}>Retry same request</button>}
        {!pendingIsQuick && <button type="button" onClick={() => { onNavigate('investigations'); close(); }}>Open Investigations</button>}
      </div> : <form onSubmit={(event) => { event.preventDefault(); void start(); }}>
        <label>Specialist<select value={specialistId} disabled={locked || !specialists.length} onChange={(event) => setSpecialistId(event.target.value as SpecialistId)}>
          {specialists.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.id}</option>)}
        </select></label>
        <p className="muted">{specialist?.purpose} T3 · InferredWeak. Advanced evidence and completion work stays in the full composer.</p>
        <p className="muted">Scope: {composer.scopeType === 'all' ? 'Recovered system' : `Fact neighborhood · ${composer.anchor || 'anchor required'} · ${composer.hops} hop${composer.hops === 1 ? '' : 's'}`}. Change scope in the full Investigations composer.</p>
        <label>Provider<select value={providerMode} disabled={locked || !providers.length} onChange={(event) => setProviderMode(event.target.value as InvestigationProviderMode)}>
          {providers.map((item) => <option key={item.mode} value={item.mode} disabled={!item.available}>{item.mode} · {item.provider_id} · {item.model}{item.available ? '' : ` · unavailable: ${item.unavailable_reason ?? 'not available'}`}</option>)}
        </select></label>
        {!provider?.available && provider && <p className="muted">{provider.unavailable_reason ?? 'This configured provider is unavailable.'} Choose an available provider or use the full composer.</p>}
        <label>Question<textarea ref={questionRef} value={question} maxLength={2048} disabled={locked} onChange={(event) => setQuestion(event.target.value)} /></label>
        {questionBytes > 2048 && <p className="error-text">Question exceeds the existing 2,048-byte limit.</p>}
        {completionRole && <fieldset disabled={locked || choicesLoading} aria-label="Recorded inventory obligations"><legend>Required completion evidence</legend>
          <p>Select at least one recorded inventory from this scope. Only the selections below are included in the start request.</p>
          {choicesLoading && <p role="status">Resolving recorded inventories for this scope…</p>}
          {currentChoicesError && <p role="alert" className="error-text">{currentChoicesError}</p>}
          {!policy && <p role="status">This specialist has no compatible Standard completion policy in the current catalog.</p>}
          {currentChoices && <ul>{currentChoices.inventories.map((item) => {
            const obligation = item.status === 'recorded' ? item.obligation : undefined;
            const selected = Boolean(obligation && selectedInventoryIds.includes(obligation.obligation_id));
            return <li key={item.repo}><strong>{item.repo}</strong> · {item.status}{obligation && <button type="button"
              aria-pressed={selected} aria-label={`${selected ? 'Remove' : 'Require'} inventory ${item.repo}`}
              onClick={() => setSelectedInventoryIds((current) => selected ? current.filter((id) => id !== obligation.obligation_id) : [...current, obligation.obligation_id])}>
              {selected ? 'Remove selection' : 'Require inventory'}</button>}
            </li>;
          })}</ul>}
          {!choicesLoading && currentChoices && selectedObligations.length === 0 && <p role="status">Select at least one recorded inventory to continue.</p>}
          {completionRole && policy && requestBytes > policy.request_bytes && <p role="alert" className="error-text">The complete request exceeds the existing {policy.request_bytes.toLocaleString()}-byte limit. Remove an inventory or shorten the question.</p>}
        </fieldset>}
        {state.catalog && specialists.length === 0 && <p className="muted">No supported quick-dispatch specialists are available. Use the full Investigations composer.</p>}
        {composer.scopeType === 'neighborhood' && !scopeValid && <p role="alert" className="error-text">Choose a valid fact anchor and one to three hops in the full Investigations composer.</p>}
        <footer><button type="button" className="secondary-button" onClick={() => close()} disabled={inFlight}>Cancel</button>
          <button type="submit" disabled={!valid || locked || state.catalogLoading}>Start investigation</button></footer>
      </form>}
    </section>
  </div>;
}
