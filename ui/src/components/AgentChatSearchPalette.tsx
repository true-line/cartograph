import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import type { ChatDock } from '../agentChatDockStore';
import { historyTextMatches, normalizedHistorySearch, type InvestigationState } from '../investigationStore';
import { readQuickDispatchFallback } from '../quickDispatchPreference';
import { isQuickDispatchKey } from '../shortcuts';

export interface AgentChatSearchPaletteProps {
  state: InvestigationState; docks: readonly ChatDock[]; query: string;
  onQuery: (query: string) => void; onClose: () => void; onOpen: (taskId: string) => void;
}

/** One view over existing bounded snapshots and the shared submitted history query. */
export function AgentChatSearchPalette({ state, docks, query, onQuery, onClose, onOpen }: AgentChatSearchPaletteProps) {
  const [submitted, setSubmitted] = useState<string | null>(null);
  const [invoker] = useState(() => document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const dialog = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    return () => { if (invoker?.isConnected) invoker.focus(); };
  }, [invoker]);
  const sameQuery = submitted !== null && !state.historyActiveOnly &&
    normalizedHistorySearch(state.historySearch) === normalizedHistorySearch(submitted);
  const currentHistory = sameQuery && !state.historyLoading && !state.historyError;
  const rows = new Map<string, { id: string; title: string; specialist: string; open: boolean; match: string | null }>();
  // The coordinator retains validated rows on refresh/continuation failure, but clears them for a new query.
  if (sameQuery) for (const item of state.history) {
    const cached = docks.find((dock) => dock.chat.task_id === item.investigation_id);
    rows.set(item.investigation_id, { id: item.investigation_id, title: item.question,
      specialist: cached?.specialistName ?? item.specialist_id, open: Boolean(cached),
      match: state.historyNameMatchIds.includes(item.investigation_id) ? 'Name match'
        : state.historyAnswerMatchIds.includes(item.investigation_id) ? 'Answer match' : null });
  }
  for (const dock of docks) {
    const result = dock.answer?.result;
    const texts = [dock.question, dock.turn.specialist_id, dock.specialistName,
      ...(result?.findings.flatMap((finding) => [finding.title, finding.statement]) ?? []), result?.narrative?.text ?? ''];
    if (texts.some((text) => historyTextMatches(text, submitted ?? ''))) {
      rows.set(dock.chat.task_id, { id: dock.chat.task_id, title: dock.question,
        specialist: dock.specialistName, open: true, match: 'Last observed open snapshot' });
    }
  }
  const choose = (id: string) => { onClose(); onOpen(id); };
  const onKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); return; }
    const mod = event.metaKey || event.ctrlKey;
    if (mod && event.key.toLowerCase() === 'f') {
      event.preventDefault(); event.stopPropagation(); input.current?.focus(); return;
    }
    // Let existing shell/summon handlers run, after dismissing this competing modal.
    if ((mod && (event.key.toLowerCase() === 'k' || /^[1-8]$/.test(event.key))) ||
        isQuickDispatchKey(event, readQuickDispatchFallback())) onClose();
    const controls = Array.from(dialog.current?.querySelectorAll<HTMLElement>('input, button:not(:disabled)') ?? []);
    if (event.key === 'Tab' && controls.length) {
      const edge = event.shiftKey ? controls[0] : controls.at(-1);
      if (document.activeElement === edge) { event.preventDefault(); (event.shiftKey ? controls.at(-1) : controls[0])?.focus(); }
    }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      const results = Array.from(dialog.current?.querySelectorAll<HTMLButtonElement>('.chat-search-result') ?? []);
      if (results.length) {
        event.preventDefault();
        const index = results.findIndex((button) => button === document.activeElement);
        const next = index < 0 ? (event.key === 'ArrowDown' ? 0 : results.length - 1)
          : (index + (event.key === 'ArrowDown' ? 1 : results.length - 1)) % results.length;
        results[next].focus();
      }
    }
  };
  return <div className="cmdk-backdrop" role="presentation" onClick={onClose}>
    <div className="cmdk chat-search-palette" role="dialog" aria-modal="true" aria-label="Search open and saved chats"
      ref={dialog} onClick={(event) => event.stopPropagation()} onKeyDown={onKey}>
      <h2 className="cmdk-title">Search open + saved chats</h2>
      <form onSubmit={(event) => { event.preventDefault(); setSubmitted(query); void state.searchHistory(query, { activeOnly: false }); }}>
        <label>Specialist name or ID, question title, or saved answer
          <input ref={input} type="search" value={query} onChange={(event) => onQuery(event.target.value)} />
        </label>
        <button type="submit" className="secondary-button">Search all saved chats</button>
        <button type="button" className="secondary-button" onClick={onClose}>Close chat search</button>
      </form>
      <p className="muted">Open snapshots are last observed. Submit to search all saved tasks; editing makes no request.</p>
      {sameQuery && state.historyLoading && <p role="status">Searching saved chats…</p>}
      {sameQuery && state.historyError && <p role="alert">{state.historyError}</p>}
      {submitted !== null && !sameQuery && <p role="status">Shared history selection changed. Submit again to search all saved tasks.</p>}
      <ul className="chat-search-results">{[...rows.values()].map((row) => <li key={row.id}>
        <button type="button" className="agent-chat-history-item chat-search-result" onClick={() => choose(row.id)}>
          <strong>{row.title}</strong><span>{row.open ? 'OPEN' : 'HISTORY'} · {row.specialist}</span>
          <code>{row.id}</code>{row.match && <span>{row.match}</span>}
        </button>
      </li>)}</ul>
      {currentHistory && !rows.size && <p>No open or saved chats match this search.</p>}
      {sameQuery && state.nextCursor && <button type="button" className="secondary-button" disabled={state.historyLoading}
        onClick={() => void state.loadHistory(true)}>Load older saved matches</button>}
    </div>
  </div>;
}
