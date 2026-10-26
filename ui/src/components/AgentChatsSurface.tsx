import { useState, type FormEvent } from 'react';
import type { InvestigationState } from '../investigationStore';
import { InvestigationChatTranscript } from './InvestigationChatTranscript';

export interface AgentChatsSurfaceProps {
  state: InvestigationState;
  onOpenResult: (investigationId: string) => void;
}

function statusLabel(status: string) {
  return status.split('_').map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(' ');
}

/** A saved task-chat destination; all history and transcript reads use the
 * existing investigation coordinator and its task/result binding checks. */
export function AgentChatsSurface({ state, onOpenResult }: AgentChatsSurfaceProps) {
  const [searchText, setSearchText] = useState(state.historySearch);
  const selectedSummary = state.detail?.investigation_id === state.selectedId
    ? state.detail : state.history.find((item) => item.investigation_id === state.selectedId);
  const submitSearch = (event: FormEvent) => {
    event.preventDefault();
    void state.searchHistory(searchText);
  };

  return <section className="agent-chats-surface" aria-label="Agent Chats">
    <header className="agent-chats-heading">
      <div><h2>Agent Chats</h2>
        <p className="muted">Review saved specialist conversations and open their findings.</p></div>
      <button type="button" className="secondary-button" disabled={state.historyLoading}
        onClick={() => { void state.loadHistory(); if (state.selectedId) void state.refreshSelected(); }}>Refresh saved chats</button>
    </header>
    <div className="agent-chats-layout">
      <aside className="agent-chats-history" aria-label="Saved task history">
        <h3>Saved tasks</h3>
        <div role="group" aria-label="History filter">
          <button type="button" className="secondary-button" aria-pressed={!state.historyActiveOnly}
            onClick={() => void state.selectHistoryActive(false)}>All saved tasks</button>
          <button type="button" className="secondary-button" aria-pressed={state.historyActiveOnly}
            onClick={() => void state.selectHistoryActive(true)}>Active tasks</button>
        </div>
        <form aria-label="Search saved chats" onSubmit={submitSearch}>
          <label>Search saved questions or specialist IDs
            <input type="search" value={searchText} onChange={(event) => setSearchText(event.target.value)} />
          </label>
          <button type="submit" className="secondary-button">Search history</button>
          {(searchText || state.historySearch) && <button type="button" className="secondary-button" onClick={() => {
            setSearchText('');
            void state.searchHistory('');
          }}>Clear search</button>}
        </form>
        {state.historyLoading && <p role="status">Loading saved task history…</p>}
        {state.historyError && <p role="alert" className="error-text">{state.historyError}</p>}
        {!state.historyLoading && state.history.length === 0 && <p className="muted">
          {state.historyError ? 'Saved task history is unavailable. Refresh to try again.' : state.historySearch
            ? 'No saved tasks match this search.' : state.historyActiveOnly ? 'No active saved tasks.' : 'No saved tasks loaded.'}
        </p>}
        <ul>{state.history.map((item) => <li key={item.investigation_id}>
          <button type="button" className={`agent-chat-history-item${item.investigation_id === state.selectedId ? ' selected' : ''}`}
            aria-pressed={item.investigation_id === state.selectedId} onClick={() => void state.open(item.investigation_id)}>
            <strong>{item.question}</strong>
            <span>{item.specialist_id} · {statusLabel(item.status)}</span>
            <time>{item.updated_at}</time>
          </button>
        </li>)}</ul>
        {state.nextCursor && <button type="button" className="secondary-button" disabled={state.historyLoading}
          onClick={() => void state.loadHistory(true)}>Load older saved tasks</button>}
      </aside>
      <section className="agent-chat-pane" aria-label="Selected saved conversation">
        {!state.selectedId && <p className="muted">Select a saved task to view its persisted question and result reference.</p>}
        {state.selectedId && <>
          <header><div><h3>{selectedSummary?.question ?? 'Loading saved task'}</h3>
            <code>{state.selectedId}</code>{selectedSummary && <p role="status">{statusLabel(selectedSummary.status)}</p>}</div></header>
          {state.loading && !state.detail && <p role="status">Loading saved task…</p>}
          {state.error && <p role="alert" className="error-text">{state.error}</p>}
          {state.detail && <InvestigationChatTranscript chat={state.chat} status={state.chatStatus}
            error={state.chatError} onRefresh={() => void state.refreshSelected()} onOpenResult={onOpenResult}
            onOpenInvestigation={() => state.selectedId && onOpenResult(state.selectedId)} />}
        </>}
      </section>
    </div>
  </section>;
}
