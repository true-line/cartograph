import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { useAgentChatDockStore, type ChatTurn } from '../agentChatDockStore';
import { useInvestigationStore, type InvestigationState } from '../investigationStore';
import { InvestigationFindings } from './InvestigationFindings';
import { InvestigationChatTranscript } from './InvestigationChatTranscript';

export interface AgentChatsSurfaceProps {
  state: InvestigationState;
  onOpenResult: (investigationId: string) => void;
  onFollowUp: (taskId: string) => void;
  onCancelFollowUp: (taskId: string) => void;
  preparingFollowUpTask: string | null;
  followUpError: string | null;
}

function statusLabel(status: string) {
  return status.split('_').map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(' ');
}

/** A saved task-chat destination; all history and transcript reads use the
 * existing investigation coordinator and its task/result binding checks. */
export function AgentChatsSurface({ state, onOpenResult, onFollowUp, onCancelFollowUp, preparingFollowUpTask, followUpError }: AgentChatsSurfaceProps) {
  const [searchText, setSearchText] = useState(state.historySearch);
  const dock = useAgentChatDockStore();
  const [conversation, setConversation] = useState<ChatTurn | null>(null);
  const closeConversation = state.closeConversation;
  const ownedConversation = useRef<string | null>(null);
  const releaseConversation = useCallback(() => {
    const owned = ownedConversation.current;
    ownedConversation.current = null;
    if (owned && useInvestigationStore.getState().conversationId === owned) closeConversation();
  }, [closeConversation]);
  useEffect(() => releaseConversation, [releaseConversation]);
  const showConversation = (turn: ChatTurn) => {
    setConversation(turn);
    ownedConversation.current = turn.conversation_id;
    void state.openConversation(turn.conversation_id);
  };
  const closeTurns = () => { setConversation(null); releaseConversation(); };
  const matchingConversation = conversation?.conversation_id === state.conversationId;
  const turns = matchingConversation ? state.conversationItems : [];
  const focusedTurn = dock.docks.find((item) => item.chat.task_id === dock.focusedTask &&
    item.turn.conversation_id === conversation?.conversation_id)?.turn;
  const selectedTurn = focusedTurn ?? conversation;
  const coordinatorPin = state.conversationPinned;
  const currentTurn = selectedTurn && coordinatorPin?.investigation_id === selectedTurn.investigation_id &&
    coordinatorPin.conversation_id === selectedTurn.conversation_id && coordinatorPin.revision >= selectedTurn.revision
    ? coordinatorPin : selectedTurn;
  const pinnedTurn = currentTurn && !turns.some((item) => item.investigation_id === currentTurn.investigation_id) ? currentTurn : null;
  const turnButton = (turn: ChatTurn) => <>
    <button type="button" className="agent-chat-history-item" aria-current={turn.investigation_id === dock.focusedTask ? 'true' : undefined}
      onClick={() => dock.open(turn.investigation_id)}>
      <strong>{turn.question}</strong><span>{turn.specialist_id} · {statusLabel(turn.status)}</span><time>{turn.updated_at}</time>
      {turn.parent_id && <span>Follow-up to {turn.parent_id}</span>}
    </button>
    {turn.parent_id && <button type="button" className="secondary-button"
      onClick={() => dock.open(turn.parent_id!)}>Open parent {turn.parent_id}</button>}
  </>;
  const connect = dock.connect;
  useEffect(() => connect(), [connect]);
  const submitSearch = (event: FormEvent) => {
    event.preventDefault();
    void state.searchHistory(searchText);
  };

  return <section className="agent-chats-surface" aria-label="Agent Chats">
    <header className="agent-chats-heading">
      <div><h2>Agent Chats</h2>
        <p className="muted">Dock up to four saved task chats and open their findings.</p></div>
      <button type="button" className="secondary-button" disabled={state.historyLoading}
        onClick={() => { void state.loadHistory(); if (dock.focusedTask) dock.open(dock.focusedTask); }}>Refresh saved chats</button>
    </header>
    {preparingFollowUpTask && <p role="status">Refreshing parent task {preparingFollowUpTask}…
      <button type="button" className="secondary-button" onClick={() => onCancelFollowUp(preparingFollowUpTask)}>Cancel follow-up</button></p>}
    {followUpError && <p role="alert" className="error-text">{followUpError}</p>}
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
          <button type="button" className={`agent-chat-history-item${item.investigation_id === dock.focusedTask ? ' selected' : ''}`}
            aria-pressed={item.investigation_id === dock.focusedTask} onClick={() => dock.open(item.investigation_id)}>
            <strong>{item.question}</strong>
            <span>{item.specialist_id} · {statusLabel(item.status)}</span>
            <time>{item.updated_at}</time>
          </button>
        </li>)}</ul>
        {state.nextCursor && <button type="button" className="secondary-button" disabled={state.historyLoading}
          onClick={() => void state.loadHistory(true)}>Load older saved tasks</button>}
      </aside>
      <div className="agent-chat-workspace">
        {dock.pendingTask && <p role="status">Loading saved chat {dock.pendingTask}…
          <button type="button" className="secondary-button" onClick={() => dock.close(dock.pendingTask!)}>Cancel opening chat</button></p>}
        {dock.error && <div role="alert" className="error-text">{dock.error}
          {dock.fallbackTask && <button type="button" className="secondary-button"
            onClick={() => onOpenResult(dock.fallbackTask!)}>Open this investigation to inspect its findings</button>}
        </div>}
        {!dock.docks.length && !dock.pendingTask && <p className="muted">Select a saved task to dock its persisted conversation.</p>}
        {conversation && <section aria-label="Conversation turns">
          <h3>Conversation turns</h3><code>{conversation.conversation_id}</code>
          <p className="muted">Saved task chats, newest first. Parent links show forks; every task keeps separate findings and citations.</p>
          <button type="button" className="secondary-button" onClick={closeTurns}>Close conversation turns</button>
          <button type="button" className="secondary-button" disabled={matchingConversation && state.conversationLoading}
            onClick={() => matchingConversation ? void state.loadConversation() : showConversation(conversation)}>Refresh conversation turns</button>
          {!matchingConversation && <p role="status">Conversation detail is no longer active. Refresh to reload this captured conversation.</p>}
          {matchingConversation && state.conversationLoading && <p role="status">Loading conversation turns…</p>}
          {matchingConversation && state.conversationError && <p role="alert" className="error-text">{state.conversationError}</p>}
          {pinnedTurn && <div aria-label="Selected conversation turn"><p>Selected turn (pinned; last observed)</p>{turnButton(pinnedTurn)}</div>}
          {matchingConversation && !state.conversationLoading && !turns.length && <p className="muted">No saved turns in this conversation page.</p>}
          <ol>{turns.map((turn) => <li key={turn.investigation_id}>{turnButton(turn)}</li>)}</ol>
          {matchingConversation && state.conversationCursor && <><p className="muted">More saved turns are available.</p>
            <button type="button" className="secondary-button" disabled={state.conversationLoading}
              onClick={() => void state.loadConversation(true)}>Load older turns</button></>}
        </section>}
        <div className={`agent-chat-docks agent-chat-docks-${dock.docks.length}`}>
          {dock.docks.map(({ chat, question, turn, answer }) => <section className="agent-chat-pane" key={chat.chat_id}
            aria-label={`Saved chat ${chat.task_id}`}>
            <header><h3>{question}</h3><code>{chat.task_id}</code>
              <p className="muted">Last observed snapshot · {chat.chat_id}</p>
              <button type="button" className="secondary-button" aria-pressed={dock.focusedTask === chat.task_id}
                onClick={() => dock.open(chat.task_id)}>Focus / refresh {chat.task_id}</button>
              <button type="button" className="secondary-button" onClick={() => showConversation(turn)}>Conversation turns</button>
              <button type="button" className="secondary-button" disabled={preparingFollowUpTask !== null}
                onClick={() => onFollowUp(chat.task_id)}>Follow-up task</button>
              <button type="button" className="secondary-button" onClick={() => { onCancelFollowUp(chat.task_id); dock.close(chat.task_id); if (conversation?.investigation_id === chat.task_id) closeTurns(); }}>Close {chat.task_id}</button>
            </header>
            <InvestigationChatTranscript chat={chat} status="saved" error={null}
              onRefresh={() => dock.open(chat.task_id)} onOpenResult={onOpenResult} />
            {answer && <InvestigationFindings readOnly sectionId={`saved-chat-findings-${chat.chat_id}`}
              result={answer.result} citations={answer.citations}
              citationId={dock.citation?.taskId === chat.task_id ? dock.citation.citationId : null}
              read={dock.citation?.taskId === chat.task_id ? dock.citation.read : null}
              loading={dock.citation?.taskId === chat.task_id && dock.citation.loading}
              error={dock.citation?.taskId === chat.task_id ? dock.citation.error : null}
              onRead={(citationId) => void dock.readCitation(chat.task_id, citationId)} onClose={dock.clearCitation} />}
          </section>)}
        </div>
      </div>
    </div>
  </section>;
}
