import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useInvestigationStore, type InvestigationDraft } from '../investigationStore';
import type { InvestigationProviderMode, SpecialistId } from '../investigationTypes';

const QUICK_IDS: SpecialistId[] = ['domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4'];

export interface InvestigationQuickDispatchProps {
  fallbackEnabled: boolean;
  onNavigate: (view: 'investigations') => void;
}

export function InvestigationQuickDispatch({ fallbackEnabled, onNavigate }: InvestigationQuickDispatchProps) {
  const state = useInvestigationStore();
  const [open, setOpen] = useState(false);
  const [question, setQuestion] = useState('');
  const [specialistId, setSpecialistId] = useState<SpecialistId>('domain-analyst@3');
  const [providerMode, setProviderMode] = useState<InvestigationProviderMode>('local');
  const [busy, setBusy] = useState(false);
  const previousFocus = useRef<HTMLElement | null>(null);
  const catalogRequested = useRef(false);
  const dialogRef = useRef<HTMLElement>(null);
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const specialists = useMemo(() => state.catalog?.specialists.filter((item) => QUICK_IDS.includes(item.id)) ?? [], [state.catalog]);
  const specialist = specialists.find((item) => item.id === specialistId);
  const providers = state.catalog?.providers ?? [];
  const provider = providers.find((item) => item.mode === providerMode);
  const supported = specialists.some((item) => item.id === specialistId);
  const composer = state.composer;
  const scopeValid = composer.scopeType === 'all' || (composer.anchor.trim().length > 0 && Number.isInteger(composer.hops) && composer.hops >= 1 && composer.hops <= 3);
  const questionBytes = new TextEncoder().encode(question).length;
  const hasPendingStart = state.pendingStart !== null;
  const locked = busy || state.starting || state.pendingStart !== null;
  const valid = supported && Boolean(provider?.available) && question.trim().length > 0 && questionBytes <= 2048 && scopeValid;

  const close = useCallback(() => {
    setOpen(false);
    catalogRequested.current = false;
    if (previousFocus.current?.isConnected) previousFocus.current.focus();
  }, []);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && open) { event.preventDefault(); close(); return; }
      if (event.altKey || event.ctrlKey || event.shiftKey || !event.metaKey) return;
      if (event.code === 'Space' || (event.key.toLowerCase() === 'j' && fallbackEnabled)) {
        event.preventDefault();
        if (!open) {
          previousFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
          setOpen(true);
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [close, fallbackEnabled, open]);
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

  const accepted = (id: string | null) => {
    if (id === null) return;
    onNavigate('investigations');
    close();
    setQuestion('');
  };
  const start = async () => {
    if (!valid || locked || state.pendingStart) return;
    // Deliberately construct only the simple-start contract. Advanced
    // composer obligations, parent links, and inspection requirements never
    // leak into this quick path.
    const draft: InvestigationDraft = {
      specialist_id: specialistId,
      question,
      scope: composer.scopeType === 'all' ? { type: 'all' } : { type: 'neighborhood', anchor: composer.anchor, hops: composer.hops },
      provider_mode: providerMode,
      limit_profile: 'investigation-v1',
    };
    setBusy(true);
    try { accepted(await state.start(draft)); } finally { setBusy(false); }
  };
  const retry = async () => {
    if (busy || !state.pendingStart) return;
    setBusy(true);
    try { accepted(await state.retryStart()); } finally { setBusy(false); }
  };

  if (!open) return null;
  const pendingIsQuick = state.pendingStart && QUICK_IDS.includes(state.pendingStart.specialist_id);
  return <div className="quick-dispatch-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !locked) close(); }}>
    <section ref={dialogRef} className="quick-dispatch-dialog" role="dialog" aria-modal="true" aria-label="Quick investigation dispatch" onKeyDown={(event) => {
      if (event.key !== 'Tab') return;
      const items = [...event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')];
      if (!items.length) { event.preventDefault(); return; }
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    }}>
      <header><div><h2>Quick investigation</h2><p className="muted">Start a saved investigation with a supported specialist. Findings remain T3 / InferredWeak.</p></div>
        <button type="button" className="secondary-button" onClick={close} aria-label="Close quick investigation">Close</button></header>
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
        {state.catalog && specialists.length === 0 && <p className="muted">No supported quick-dispatch specialists are available. Use the full Investigations composer.</p>}
        {composer.scopeType === 'neighborhood' && !scopeValid && <p role="alert" className="error-text">Choose a valid fact anchor and one to three hops in the full Investigations composer.</p>}
        <footer><button type="button" className="secondary-button" onClick={close} disabled={locked}>Cancel</button>
          <button type="submit" disabled={!valid || locked || state.catalogLoading}>Start investigation</button></footer>
      </form>}
    </section>
  </div>;
}
