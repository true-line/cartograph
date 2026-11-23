import { desktopAnalystCatalog, admissionCatalog, queryHistoryCatalog, queryHistoryDetail, queryHistoryResult, expandedCompletionCatalog, completionCatalog, completionChoices, inventoryObligation, sourceObligation, specWriterDetail, specWriterResult } from './completionFixtures';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC as mockTauriIPC } from '@tauri-apps/api/mocks';
import backendInspection from './fixtures/investigation-inspection-backend.json';
import queryHistoryWire from './fixtures/completion-v9-supported.json';
import backendQualified from './fixtures/investigation-coverage-backend.json';
import { desktopDefaultComposer, mergeInvestigationEvents, restoredComposer, useInvestigationStore } from './investigationStore';
import { qualifiedCoverageContext, qualifiedDetail, qualifiedResult, inspectionAssessment, inspectionDetail, inspectionTarget, investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult, legacyInvestigationDetail, legacyInvestigationResult } from './investigationFixtures';
import type { InvestigationCapturedLocation, InvestigationChat, InvestigationCitationRead, InvestigationDetail, InvestigationEvent, InvestigationInspectionTarget, InvestigationResult, StartInvestigationRequest } from './investigationTypes';

// These commands use named JSON arguments; Tauri also supports unrelated binary payloads.
function mockIPC(handler: (command: string, args?: Record<string, unknown>) => unknown) {
  mockTauriIPC((command, args) => handler(command, args as Record<string, unknown> | undefined));
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<T>((success, failure) => { resolve = success; reject = failure; });
  return { promise, resolve, reject };
}

function response(command: string, args?: Record<string, unknown>) {
  const id = args?.investigationId as string;
  switch (command) {
    case 'get_investigation': return investigationDetail(id);
    case 'investigation_result': return investigationResult(id);
    case 'get_investigation_chat': return null;
    case 'investigation_consent': return null;
    case 'investigation_events': {
      const events = investigationEvents(id);
      return { investigation_id: id, items: events.filter((event) => event.sequence > Number(args?.afterSequence ?? 0)),
        next_sequence: events.length, has_more: false };
    }
    default: throw new Error('Unexpected command in fixture.');
  }
}

function savedChat(detail: InvestigationDetail, result: InvestigationResult | null): InvestigationChat {
  return { chat_id: `chat:${detail.investigation_id}`, task_id: detail.investigation_id,
    producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
      prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
      confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
      provider_id: detail.provider.provider_id, model: detail.provider.model,
      protocol_version: detail.provider.protocol_version ?? null },
    messages: [{ id: `user:${detail.investigation_id}`, ordinal: 0, author: 'user',
      message: { kind: 'user_question', question: detail.question } },
    ...(result ? [{ id: `result:${detail.investigation_id}`, ordinal: 1, author: 'specialist' as const,
      message: { kind: 'result_reference' as const, task_id: detail.investigation_id,
        result_id: result.result_id, result_hash: 'c'.repeat(64) } }] : [])] };
}

beforeEach(() => {
  vi.stubGlobal('window', {});
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});

describe('durable investigation observations (AC-0190)', () => {
  it('loads conversation pages without fetching sibling detail and rejects stale conversation responses (AC-0374)', async () => {
    const first = investigationDetail('turn-first', { conversation_id: 'conversation-a', parent_id: null });
    const second = investigationDetail('turn-second', { conversation_id: 'conversation-b', parent_id: 'turn-first' });
    const pendingA = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: string[] = [];
    mockIPC((command, args) => {
      calls.push(command);
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      if (args?.conversationId === 'conversation-a') return pendingA.promise;
      return { items: [second], next_cursor: null };
    });
    const loadA = useInvestigationStore.getState().openConversation('conversation-a');
    useInvestigationStore.setState({ conversationPinned: first });
    await useInvestigationStore.getState().openConversation('conversation-b');
    expect(useInvestigationStore.getState().conversationPinned).toBeNull();
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id)).toEqual(['turn-second']);
    pendingA.resolve({ items: [first], next_cursor: null });
    await loadA;
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id)).toEqual(['turn-second']);
    expect(calls).toEqual(['list_investigation_conversation', 'list_investigation_conversation']);
  });

  it('keeps the newer selected terminal revision when a delayed conversation page is stale (AC-0374)', async () => {
    const stale = investigationDetail('terminal-turn', { conversation_id: 'conversation-terminal', revision: 3,
      status: 'running', has_result: false });
    const terminal = investigationDetail('terminal-turn', { conversation_id: 'conversation-terminal', revision: 9,
      status: 'completed', has_result: true });
    const pending = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    mockIPC((command) => command === 'list_investigation_conversation' ? pending.promise : null);
    useInvestigationStore.setState({ selectedId: terminal.investigation_id, detail: terminal, history: [terminal],
      conversationId: terminal.conversation_id, conversationItems: [], conversationPinned: terminal });

    const loading = useInvestigationStore.getState().loadConversation();
    pending.resolve({ items: [stale], next_cursor: null });
    await loading;

    expect(useInvestigationStore.getState().conversationItems.map((item) => ({
      id: item.investigation_id, status: item.status, revision: item.revision,
    }))).toEqual([{ id: terminal.investigation_id, status: 'completed', revision: 9 }]);
  });

  it('coalesces a conversation event burst and publishes the in-flight page before one trailing refresh (AC-0374)', async () => {
    const initial = investigationDetail('busy-turn', { conversation_id: 'busy-conversation', revision: 1,
      status: 'running', has_result: false });
    const firstResponse = investigationDetail('busy-turn', { conversation_id: 'busy-conversation', revision: 2,
      status: 'running', has_result: false, last_event_sequence: 2 });
    const trailingResponse = investigationDetail('busy-turn', { conversation_id: 'busy-conversation', revision: 4,
      status: 'completed', has_result: true, last_event_sequence: 4 });
    const firstPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const trailingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      return calls.length === 1 ? firstPage.promise : trailingPage.promise;
    });
    useInvestigationStore.setState({ conversationId: initial.conversation_id, conversationItems: [initial], conversationPagePrefixCount: 1,
      history: [initial], historyLoading: true });

    const loading = useInvestigationStore.getState().loadConversation();
    for (const sequence of [2, 3, 4]) useInvestigationStore.getState().invalidate({
      investigation_id: initial.investigation_id, revision: sequence, last_event_sequence: sequence,
    });
    expect(calls).toHaveLength(1);

    firstPage.resolve({ items: [firstResponse], next_cursor: null });
    await loading;
    expect(useInvestigationStore.getState().conversationItems[0]).toMatchObject({ revision: 2, status: 'running' });
    expect(calls).toHaveLength(2);
    expect(useInvestigationStore.getState().conversationLoading).toBe(true);

    trailingPage.resolve({ items: [trailingResponse], next_cursor: null });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));
    expect(useInvestigationStore.getState().conversationItems[0]).toMatchObject({ revision: 4, status: 'completed' });
    expect(calls).toHaveLength(2);
  });

  it('runs one trailing refresh after a failed request and does not retry again without a new event (AC-0374)', async () => {
    const turn = investigationDetail('failure-turn', { conversation_id: 'failure-conversation', status: 'running', has_result: false });
    const firstPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const trailingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      return calls.length === 1 ? firstPage.promise : trailingPage.promise;
    });
    useInvestigationStore.setState({ conversationId: turn.conversation_id, conversationItems: [turn], conversationPagePrefixCount: 1,
      history: [turn], historyLoading: true });

    const loading = useInvestigationStore.getState().loadConversation();
    useInvestigationStore.getState().invalidate({ investigation_id: turn.investigation_id, revision: 1, last_event_sequence: 1 });
    firstPage.reject(new Error('temporary failure'));
    await loading;
    expect(calls).toHaveLength(2);
    expect(useInvestigationStore.getState().conversationLoading).toBe(true);

    trailingPage.reject(new Error('still unavailable'));
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));
    expect(useInvestigationStore.getState().conversationError).toContain('could not be loaded');
    expect(calls).toHaveLength(2);
  });

  it('clears a pending event refresh on same-conversation reopen and suppresses the older response (AC-0374)', async () => {
    const turn = investigationDetail('reopen-turn', { conversation_id: 'reopen-conversation', revision: 1,
      status: 'running', has_result: false });
    const obsoletePage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const reopenedPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const currentPage = investigationDetail('reopen-turn', { conversation_id: 'reopen-conversation', revision: 3,
      status: 'completed', has_result: true });
    const trailingPage = investigationDetail('reopen-turn', { conversation_id: 'reopen-conversation', revision: 4,
      status: 'completed', has_result: true });
    let calls = 0;
    mockIPC((command) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls += 1;
      if (calls === 1) return obsoletePage.promise;
      if (calls === 2) return reopenedPage.promise;
      return { items: [trailingPage], next_cursor: null };
    });
    useInvestigationStore.setState({ conversationId: turn.conversation_id, conversationItems: [turn], conversationPagePrefixCount: 1,
      history: [turn], historyLoading: true });

    const oldLoad = useInvestigationStore.getState().loadConversation();
    useInvestigationStore.getState().invalidate({ investigation_id: turn.investigation_id, revision: 2, last_event_sequence: 2 });
    const reopened = useInvestigationStore.getState().openConversation(turn.conversation_id);
    expect(useInvestigationStore.getState().conversationPagePrefixCount).toBe(1);
    useInvestigationStore.getState().invalidate({ investigation_id: turn.investigation_id, revision: 3, last_event_sequence: 3 });
    obsoletePage.resolve({ items: [turn], next_cursor: null });
    await oldLoad;
    expect(calls).toBe(2);

    reopenedPage.resolve({ items: [currentPage], next_cursor: null });
    await reopened;
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));

    expect(calls).toBe(3);
    expect(useInvestigationStore.getState().conversationItems[0]).toMatchObject({ revision: 4, status: 'completed' });
  });

  it('clears a pending event refresh on close without applying or restarting the stale request (AC-0374)', async () => {
    const turn = investigationDetail('closed-turn', { conversation_id: 'closed-conversation', status: 'running', has_result: false });
    const pending = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    let calls = 0;
    mockIPC((command) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls += 1;
      return pending.promise;
    });
    useInvestigationStore.setState({ conversationId: turn.conversation_id, conversationItems: [turn], conversationPagePrefixCount: 1,
      history: [turn], historyLoading: true });

    const loading = useInvestigationStore.getState().loadConversation();
    useInvestigationStore.getState().invalidate({ investigation_id: turn.investigation_id, revision: 1, last_event_sequence: 1 });
    useInvestigationStore.getState().closeConversation();
    pending.resolve({ items: [turn], next_cursor: null });
    await loading;

    expect(calls).toBe(1);
    expect(useInvestigationStore.getState().conversationId).toBeNull();
    expect(useInvestigationStore.getState().conversationItems).toEqual([]);
    expect(useInvestigationStore.getState().conversationPagePrefixCount).toBe(0);
  });

  it('preserves loaded older turns and adopts the refreshed cursor after a continuation (AC-0374)', async () => {
    const first = investigationDetail('page-first', { conversation_id: 'paged-conversation' });
    const second = investigationDetail('page-second', { conversation_id: 'paged-conversation', parent_id: first.investigation_id });
    const newest = investigationDetail('page-newest', { conversation_id: 'paged-conversation', parent_id: first.investigation_id });
    const third = investigationDetail('page-third', { conversation_id: 'paged-conversation', parent_id: first.investigation_id });
    const continuation = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const refreshedFirstPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: Record<string, unknown>[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args ?? {});
      if (calls.length === 1) return continuation.promise;
      if (calls.length === 2) return refreshedFirstPage.promise;
      return { items: [second, third], next_cursor: null };
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first], conversationPagePrefixCount: 1,
      conversationCursor: 'older-page-2', history: [first], historyLoading: true });

    const loadingOlder = useInvestigationStore.getState().loadConversation(true);
    useInvestigationStore.getState().invalidate({ investigation_id: first.investigation_id, revision: 1, last_event_sequence: 1 });
    continuation.resolve({ items: [second], next_cursor: 'older-page-3' });
    await loadingOlder;
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([first.investigation_id, second.investigation_id]);
    expect(useInvestigationStore.getState().conversationPageBookmarks)
      .toContainEqual({ cursor: 'older-page-2', ids: [second.investigation_id] });
    expect(calls).toHaveLength(2);
    expect(calls[0]).toEqual({ conversationId: first.conversation_id, cursor: 'older-page-2' });
    expect(calls[1]).toEqual({ conversationId: first.conversation_id });

    refreshedFirstPage.resolve({ items: [newest, first], next_cursor: 'fresh-first-page-cursor' });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([newest.investigation_id, first.investigation_id, second.investigation_id]);
    expect(useInvestigationStore.getState().conversationCursor).toBe('fresh-first-page-cursor');

    await useInvestigationStore.getState().loadConversation(true);
    expect(calls[2]).toEqual({ conversationId: first.conversation_id, cursor: 'fresh-first-page-cursor' });
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([newest.investigation_id, first.investigation_id, second.investigation_id, third.investigation_id]);
    expect(useInvestigationStore.getState().conversationPagePrefixCount).toBe(4);
    expect(useInvestigationStore.getState().conversationCursor).toBeNull();
  });

  it('keeps cached older turns while replacing an exhausted cursor with the refreshed first-page cursor (AC-0374)', async () => {
    const first = investigationDetail('exhausted-first', { conversation_id: 'exhausted-conversation' });
    const older = investigationDetail('exhausted-older', { conversation_id: 'exhausted-conversation' });
    const newest = investigationDetail('exhausted-newest', { conversation_id: 'exhausted-conversation' });
    const middle = investigationDetail('exhausted-middle', { conversation_id: 'exhausted-conversation' });
    const calls: Record<string, unknown>[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args ?? {});
      if (calls.length <= 2) return { items: [newest, first], next_cursor: 'refreshed-cursor' };
      return { items: [middle], next_cursor: null };
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, older],
      conversationPagePrefixCount: 2, conversationCursor: null, history: [first, older], historyLoading: true });

    const loading = useInvestigationStore.getState().loadConversation();
    useInvestigationStore.getState().invalidate({ investigation_id: first.investigation_id, revision: 2, last_event_sequence: 2 });
    await loading;
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));
    expect(calls).toHaveLength(2);
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([newest.investigation_id, first.investigation_id, older.investigation_id]);
    expect(useInvestigationStore.getState().conversationCursor).toBe('refreshed-cursor');

    await useInvestigationStore.getState().loadConversation(true);
    expect(calls[2]).toEqual({ conversationId: first.conversation_id, cursor: 'refreshed-cursor' });
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([newest.investigation_id, first.investigation_id, middle.investigation_id, older.investigation_id]);
    expect(useInvestigationStore.getState().conversationPagePrefixCount).toBe(3);
    expect(useInvestigationStore.getState().conversationCursor).toBeNull();
  });

  it('publishes a refreshed older page and coalesces later invalidations into one trailing page read (AC-0374)', async () => {
    const first = investigationDetail('dirty-first', { conversation_id: 'dirty-conversation' });
    const older = investigationDetail('dirty-older', { conversation_id: first.conversation_id, revision: 1, status: 'running', has_result: false });
    const refreshed = investigationDetail('dirty-older', { conversation_id: first.conversation_id, revision: 2, status: 'running', has_result: false });
    const trailing = investigationDetail('dirty-older', { conversation_id: first.conversation_id, revision: 3, status: 'completed', has_result: true });
    const firstPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const trailingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      return calls.length === 1 ? firstPage.promise : trailingPage.promise;
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, older],
      conversationPagePrefixCount: 1, conversationPageBookmarks: [{ cursor: 'older-page-cursor', ids: [older.investigation_id] }],
      conversationCursor: 'next-page-cursor', historyLoading: true });

    useInvestigationStore.getState().invalidate({ investigation_id: older.investigation_id, revision: 2, last_event_sequence: 2 });
    useInvestigationStore.getState().invalidate({ investigation_id: older.investigation_id, revision: 3, last_event_sequence: 3 });
    expect(calls).toHaveLength(1);
    firstPage.resolve({ items: [refreshed], next_cursor: null });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationItems[1]).toMatchObject({ revision: 2, status: 'running' }));
    await vi.waitFor(() => expect(calls).toHaveLength(2));
    expect(useInvestigationStore.getState().conversationItems[1]).toMatchObject({ revision: 2, status: 'running' });
    trailingPage.resolve({ items: [trailing], next_cursor: null });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationItems[1]).toMatchObject({ revision: 3, status: 'completed' }));

    expect(calls).toEqual([
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
    ]);
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([first.investigation_id, older.investigation_id]);
    expect(useInvestigationStore.getState().conversationPagePrefixCount).toBe(1);
    expect(useInvestigationStore.getState().conversationCursor).toBe('next-page-cursor');
    expect(useInvestigationStore.getState().conversationDirtyPages).toEqual([]);
  });

  it('keeps a failed older-page refresh dirty and retries it on manual refresh (AC-0374)', async () => {
    const first = investigationDetail('retry-first', { conversation_id: 'retry-conversation' });
    const older = investigationDetail('retry-older', { conversation_id: first.conversation_id, revision: 1, status: 'running', has_result: false });
    const refreshed = investigationDetail('retry-older', { conversation_id: first.conversation_id, revision: 2, status: 'completed', has_result: true });
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      if (calls.length === 1) throw new Error('temporary page failure');
      if (args?.cursor === 'older-page-cursor') return { items: [refreshed], next_cursor: null };
      return { items: [first], next_cursor: 'fresh-page-cursor' };
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, older],
      conversationPagePrefixCount: 1, conversationPageBookmarks: [{ cursor: 'older-page-cursor', ids: [older.investigation_id] }],
      conversationCursor: 'older-page-cursor', historyLoading: true });

    useInvestigationStore.getState().invalidate({ investigation_id: older.investigation_id, revision: 2, last_event_sequence: 2 });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));
    expect(useInvestigationStore.getState().conversationDirtyPages).toEqual(['older-page-cursor']);
    expect(calls).toHaveLength(1);

    await useInvestigationStore.getState().loadConversation();
    expect(useInvestigationStore.getState().conversationItems[1]).toMatchObject({ revision: 2, status: 'completed' });
    expect(useInvestigationStore.getState().conversationDirtyPages).toEqual([]);
    expect(calls).toEqual([
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
      { conversationId: first.conversation_id },
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
    ]);
  });

  it('stops a dirty-page pass at the first failure without hiding it behind later pages (AC-0374)', async () => {
    const first = investigationDetail('dirty-failure-first', { conversation_id: 'dirty-failure-conversation' });
    const olderA = investigationDetail('dirty-failure-a', { conversation_id: first.conversation_id });
    const olderB = investigationDetail('dirty-failure-b', { conversation_id: first.conversation_id });
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      throw new Error('older page unavailable');
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, olderA, olderB],
      conversationPagePrefixCount: 1, conversationPageBookmarks: [
        { cursor: 'older-a-cursor', ids: [olderA.investigation_id] },
        { cursor: 'older-b-cursor', ids: [olderB.investigation_id] },
      ], conversationDirtyPages: ['older-a-cursor', 'older-b-cursor'], historyLoading: true });

    useInvestigationStore.getState().invalidate({ investigation_id: olderA.investigation_id, revision: 1, last_event_sequence: 1 });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationLoading).toBe(false));

    expect(calls).toEqual([{ conversationId: first.conversation_id, cursor: 'older-a-cursor' }]);
    expect(useInvestigationStore.getState().conversationDirtyPages).toEqual(['older-a-cursor', 'older-b-cursor']);
    expect(useInvestigationStore.getState().conversationError).toContain('could not be refreshed');
  });

  it('preserves a failed first-page error instead of flushing dirty pages afterward (AC-0374)', async () => {
    const first = investigationDetail('first-page-failure', { conversation_id: 'first-page-failure-conversation' });
    const older = investigationDetail('first-page-failure-older', { conversation_id: first.conversation_id });
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      if (args?.cursor === undefined) throw new Error('first page unavailable');
      return { items: [older], next_cursor: null };
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, older],
      conversationPagePrefixCount: 1, conversationPageBookmarks: [{ cursor: 'older-page-cursor', ids: [older.investigation_id] }],
      conversationDirtyPages: ['older-page-cursor'], historyLoading: true });

    await useInvestigationStore.getState().loadConversation();

    expect(calls).toEqual([{ conversationId: first.conversation_id }]);
    expect(useInvestigationStore.getState().conversationDirtyPages).toEqual(['older-page-cursor']);
    expect(useInvestigationStore.getState().conversationError).toContain('could not be loaded');
  });

  it('requeues a bookmarked page when a same-conversation reopen invalidates its response (AC-0374)', async () => {
    const first = investigationDetail('reopen-page-first', { conversation_id: 'reopen-page-conversation' });
    const older = investigationDetail('reopen-page-older', { conversation_id: first.conversation_id, revision: 1, status: 'running', has_result: false });
    const stale = investigationDetail('reopen-page-older', { conversation_id: first.conversation_id, revision: 2, status: 'running', has_result: false });
    const fresh = investigationDetail('reopen-page-older', { conversation_id: first.conversation_id, revision: 3, status: 'completed', has_result: true });
    const obsoletePage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const reopenedFirstPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command !== 'list_investigation_conversation') throw new Error(`Unexpected ${command}`);
      calls.push(args);
      if (calls.length === 1) return obsoletePage.promise;
      if (calls.length === 2) return reopenedFirstPage.promise;
      return { items: [fresh], next_cursor: null };
    });
    useInvestigationStore.setState({ conversationId: first.conversation_id, conversationItems: [first, older],
      conversationPagePrefixCount: 1, conversationPageBookmarks: [{ cursor: 'older-page-cursor', ids: [older.investigation_id] }],
      conversationCursor: 'older-page-cursor', historyLoading: true });

    useInvestigationStore.getState().invalidate({ investigation_id: older.investigation_id, revision: 2, last_event_sequence: 2 });
    await vi.waitFor(() => expect(calls).toHaveLength(1));
    const reopening = useInvestigationStore.getState().openConversation(first.conversation_id);
    reopenedFirstPage.resolve({ items: [first], next_cursor: 'next-page-cursor' });
    await reopening;
    expect(calls).toHaveLength(2);
    obsoletePage.resolve({ items: [stale], next_cursor: null });
    await vi.waitFor(() => expect(calls).toHaveLength(3));
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationItems[1]).toMatchObject({ revision: 3, status: 'completed' }));

    expect(calls).toEqual([
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
      { conversationId: first.conversation_id },
      { conversationId: first.conversation_id, cursor: 'older-page-cursor' },
    ]);
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([first.investigation_id, older.investigation_id]);
  });

  it('opens a parent outside the loaded page, binds its citation, then clears on a foreign conversation (AC-0374)', async () => {
    const child = investigationDetail('selected-child', { conversation_id: 'conversation-pinned', parent_id: 'selected-parent' });
    const parent = investigationDetail('selected-parent', { conversation_id: 'conversation-pinned' });
    const loadedSibling = investigationDetail('loaded-sibling', { conversation_id: 'conversation-pinned', parent_id: 'selected-parent' });
    const foreign = investigationDetail('foreign-turn', { conversation_id: 'different-conversation' });
    const pendingParent = deferred<InvestigationDetail>();
    const citationReads: unknown[] = [];
    mockIPC((command, args) => {
      const id = args?.investigationId as string;
      if (command === 'get_investigation') return id === parent.investigation_id ? pendingParent.promise : foreign;
      if (command === 'investigation_result') return investigationResult(id);
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id, items: investigationEvents(id), next_sequence: 9, has_more: false };
      if (command === 'read_investigation_citation') { citationReads.push(args); return { investigation_id: id, citation_id: 'C1',
        citation: investigationCitations[0], status: 'available', text: 'Saved parent evidence' };
      }
      throw new Error(`Unexpected ${command}`);
    });
    useInvestigationStore.setState({ selectedId: child.investigation_id, detail: child,
      history: [child], conversationId: child.conversation_id, conversationItems: [loadedSibling], conversationPinned: child });
    const opening = useInvestigationStore.getState().open(parent.investigation_id);
    expect(useInvestigationStore.getState().detail).toBeNull();
    expect(useInvestigationStore.getState().conversationId).toBe(child.conversation_id);
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id)).toEqual(['loaded-sibling']);
    expect(useInvestigationStore.getState().conversationPinned).toBeNull();
    pendingParent.resolve(parent);
    await opening;
    expect(useInvestigationStore.getState().conversationPinned?.investigation_id).toBe(parent.investigation_id);
    expect(useInvestigationStore.getState().result?.investigation_id).toBe(parent.investigation_id);
    await useInvestigationStore.getState().readCitation('C1');
    expect(citationReads).toEqual([{ investigationId: 'selected-parent', citationId: 'C1' }]);
    expect(useInvestigationStore.getState().citationRead?.investigation_id).toBe(parent.investigation_id);
    expect(useInvestigationStore.getState().citationRead?.text).toBe('Saved parent evidence');
    await useInvestigationStore.getState().open(foreign.investigation_id);
    expect(useInvestigationStore.getState().conversationId).toBeNull();
    expect(useInvestigationStore.getState().conversationPinned).toBeNull();
    expect(useInvestigationStore.getState().result?.investigation_id).toBe(foreign.investigation_id);
    expect(useInvestigationStore.getState().citationRead).toBeNull();
    expect(useInvestigationStore.getState().citationId).toBeNull();
  });

  it('refreshes the open conversation after a successfully admitted follow-up (AC-0374)', async () => {
    const parent = investigationDetail('follow-up-parent', { conversation_id: 'follow-up-conversation' });
    const child = investigationDetail('follow-up-child', { conversation_id: parent.conversation_id,
      parent_id: parent.investigation_id, status: 'preparing', has_result: false, last_event_sequence: 1 });
    const pendingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const pageCalls: unknown[] = [];
    let started: StartInvestigationRequest | null = null;
    mockIPC((command, args) => {
      const id = args?.investigationId as string;
      if (command === 'start_investigation') { started = args?.request as StartInvestigationRequest; return child; }
      if (command === 'get_investigation') return child;
      if (command === 'investigation_result') return null;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id, items: investigationEvents(id).slice(0, 1), next_sequence: 1, has_more: false };
      if (command === 'list_investigation_conversation') {
        pageCalls.push(args);
        return pendingPage.promise;
      }
      throw new Error(`Unexpected ${command}`);
    });
    useInvestigationStore.setState({ selectedId: parent.investigation_id, detail: parent,
      history: [parent], conversationId: parent.conversation_id, conversationItems: [parent], conversationPinned: parent });

    const admittedId = await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3', question: 'Follow the saved result.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
      conversation_id: parent.conversation_id, parent_id: parent.investigation_id });

    expect(admittedId).toBe(child.investigation_id);
    expect(started).toMatchObject({ conversation_id: parent.conversation_id, parent_id: parent.investigation_id });
    expect(useInvestigationStore.getState().selectedId).toBe(child.investigation_id);
    expect(useInvestigationStore.getState().detail?.investigation_id).toBe(child.investigation_id);
    expect(useInvestigationStore.getState().conversationLoading).toBe(true);
    pendingPage.resolve({ items: [child, parent], next_cursor: null });
    await vi.waitFor(() => expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([child.investigation_id, parent.investigation_id]));
    expect(useInvestigationStore.getState().conversationItems.map((item) => item.investigation_id))
      .toEqual([child.investigation_id, parent.investigation_id]);
    expect(useInvestigationStore.getState().conversationPinned?.investigation_id).toBe(child.investigation_id);
    expect(pageCalls).toEqual([{ conversationId: parent.conversation_id }]);
  });

  it('rejects a conversation page containing a summary from another conversation (AC-0374)', async () => {
    const wrong = investigationDetail('wrong-turn', { conversation_id: 'other-conversation' });
    mockIPC((command) => {
      if (command === 'list_investigation_conversation') return { items: [wrong], next_cursor: null };
      throw new Error(`Unexpected ${command}`);
    });
    await useInvestigationStore.getState().openConversation('expected-conversation');
    expect(useInvestigationStore.getState().conversationItems).toEqual([]);
    expect(useInvestigationStore.getState().conversationError).toContain('could not be loaded');
  });

  it('rejects a server search page containing a nonmatching summary (AC-0368)', async () => {
    const nonmatch = investigationDetail('wrong-search-result', { question: 'Unrelated history' });
    mockIPC((command) => command === 'list_investigations'
      ? { items: [nonmatch], next_cursor: null } : null);

    await useInvestigationStore.getState().searchHistory('needle');
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().historyError).toBe('Investigation history search could not be completed.');
  });

  it('ignores stale search responses after a newer query or clear (AC-0368)', async () => {
    const alpha = investigationDetail('alpha-result', { question: 'Alpha saved question' });
    const beta = investigationDetail('beta-result', { question: 'Beta saved question' });
    const gamma = investigationDetail('gamma-result', { question: 'Gamma saved question' });
    const ordinary = investigationDetail('ordinary-result', { question: 'Ordinary history' });
    const pendingAlpha = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const pendingGamma = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    mockIPC((command, args) => {
      if (command !== 'list_investigations') return null;
      if (args?.search === 'alpha') return pendingAlpha.promise;
      if (args?.search === 'gamma') return pendingGamma.promise;
      if (args?.search === 'beta') return { items: [beta], next_cursor: 'beta-next' };
      return { items: [ordinary], next_cursor: 'ordinary-next' };
    });

    const alphaSearch = useInvestigationStore.getState().searchHistory('alpha');
    await useInvestigationStore.getState().searchHistory('beta');
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['beta-result']);
    pendingAlpha.resolve({ items: [alpha], next_cursor: null });
    await alphaSearch;
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['beta-result']);

    const gammaSearch = useInvestigationStore.getState().searchHistory('gamma');
    await useInvestigationStore.getState().searchHistory('');
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['ordinary-result']);
    pendingGamma.resolve({ items: [gamma], next_cursor: null });
    await gammaSearch;
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['ordinary-result']);
  });

  it('keeps persisted-history search complete and filters selected/background task updates (AC-0368)', async () => {
    const match = investigationDetail('older-match', { question: 'Needle in an older saved question' });
    const nonmatch = investigationDetail('selected-nonmatch', { question: 'A different saved question' });
    const listCalls: Array<Record<string, unknown>> = [];
    const clearingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    let holdClear = false;
    mockIPC((command, args) => {
      if (command === 'list_investigations') {
        listCalls.push(args ?? {});
        if (!args?.search && holdClear) return clearingPage.promise;
        return { items: args?.search ? [match] : [match, nonmatch],
          next_cursor: args?.search ? 'search-next' : 'ordinary-next' };
      }
      if (command === 'get_investigation') return nonmatch;
      if (command === 'investigation_result') return investigationResult(nonmatch.investigation_id);
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return {
        investigation_id: nonmatch.investigation_id, items: investigationEvents(nonmatch.investigation_id),
        next_sequence: nonmatch.last_event_sequence, has_more: false,
      };
      return null;
    });

    const store = useInvestigationStore.getState();
    await store.searchHistory('\u000bNeedle\u000b');
    expect(listCalls[0]).toEqual({ search: '\u000bNeedle\u000b' });
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['older-match']);
    expect(useInvestigationStore.getState().nextCursor).toBe('search-next');

    await useInvestigationStore.getState().open(nonmatch.investigation_id);
    expect(useInvestigationStore.getState().selectedId).toBe(nonmatch.investigation_id);
    expect(useInvestigationStore.getState().detail?.question).toBe(nonmatch.question);
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['older-match']);

    useInvestigationStore.getState().invalidate({ investigation_id: 'remote-nonmatch', revision: 1, last_event_sequence: 1 });
    await vi.waitFor(() => expect(listCalls).toHaveLength(2));
    expect(listCalls[1]).toEqual({ search: '\u000bNeedle\u000b' });
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['older-match']);

    holdClear = true;
    const clearing = useInvestigationStore.getState().searchHistory('');
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().nextCursor).toBeNull();
    expect(useInvestigationStore.getState().selectedId).toBe(nonmatch.investigation_id);
    expect(useInvestigationStore.getState().detail?.question).toBe(nonmatch.question);
    clearingPage.resolve({ items: [match, nonmatch], next_cursor: 'ordinary-next' });
    await clearing;
    expect(listCalls[2]).toEqual({});
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['older-match', 'selected-nonmatch']);
    expect(useInvestigationStore.getState().selectedId).toBe(nonmatch.investigation_id);
    expect(useInvestigationStore.getState().detail?.question).toBe(nonmatch.question);
    expect(useInvestigationStore.getState().nextCursor).toBe('ordinary-next');
  });

  it('preserves older explicit pages and their cursor when refreshing ordered recent history', async () => {
    let recent = investigationDetail('recent', { revision: 10 });
    mockIPC((command, args) => {
      expect(command).toBe('list_investigations');
      return args?.cursor ? { items: [investigationDetail('older')], next_cursor: 'after-older' }
        : { items: [recent], next_cursor: 'after-recent' };
    });
    await useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().loadHistory(true);
    recent = investigationDetail('recent', { revision: 9, question: 'Stale summary' });
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['recent', 'older']);
    expect(useInvestigationStore.getState().history[0].revision).toBe(10);
    expect(useInvestigationStore.getState().nextCursor).toBe('after-older');
  });

  it('does not let an older active page restore a task after a terminal summary arrives (AC-0372)', async () => {
    const active = investigationDetail('active-race', { status: 'running', has_result: false,
      actions: { can_cancel: true, can_follow_up: false }, revision: 2 });
    const terminal = investigationDetail('active-race', { status: 'cancelled', has_result: false,
      actions: { can_cancel: false, can_follow_up: false }, revision: 3 });
    const pendingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    mockIPC((command) => {
      if (command === 'list_investigations') return pendingPage.promise;
      if (command === 'cancel_investigation') return terminal;
      return response(command);
    });

    useInvestigationStore.setState({ historyActiveOnly: true }, false);
    const oldPage = useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().cancel(active.investigation_id);
    expect(useInvestigationStore.getState().history).toEqual([]);
    pendingPage.resolve({ items: [active], next_cursor: null });
    await oldPage;
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().historyLoading).toBe(false);
  });

  it('removes a terminal task from active history while updating its conversation pin and rejecting a stale page (AC-0372, AC-0374)', async () => {
    const active = investigationDetail('active-pinned-race', { conversation_id: 'conversation-active', status: 'running',
      has_result: false, actions: { can_cancel: true, can_follow_up: false }, revision: 2 });
    const terminal = investigationDetail('active-pinned-race', { conversation_id: 'conversation-active', status: 'cancelled',
      has_result: false, actions: { can_cancel: false, can_follow_up: false }, revision: 3 });
    const pendingPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    mockIPC((command) => {
      if (command === 'list_investigations') return pendingPage.promise;
      if (command === 'cancel_investigation') return terminal;
      throw new Error(`Unexpected ${command}`);
    });
    useInvestigationStore.setState({ historyActiveOnly: true, history: [active], selectedId: 'another-task',
      conversationId: active.conversation_id, conversationItems: [active], conversationPinned: active });

    const oldPage = useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().cancel(active.investigation_id);

    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().conversationPinned).toMatchObject({
      investigation_id: active.investigation_id, conversation_id: active.conversation_id,
      status: 'cancelled', revision: 3,
    });
    pendingPage.resolve({ items: [active], next_cursor: null });
    await oldPage;
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState().conversationPinned?.revision).toBe(3);
    expect(useInvestigationStore.getState().historyLoading).toBe(false);
  });

  it('replaces active pages on refresh and ignores a stale page across view switches (AC-0372)', async () => {
    const oldAllPage = deferred<{ items: InvestigationDetail[]; next_cursor: string | null }>();
    const staleAll = investigationDetail('stale-all-result');
    const activeFirst = investigationDetail('active-first', { status: 'running', has_result: false });
    const activeRefresh = investigationDetail('active-refresh', { status: 'awaiting_consent', has_result: false });
    const latestAll = investigationDetail('latest-all-result');
    let activeCalls = 0;
    let allCalls = 0;
    mockIPC((command, args) => {
      if (command !== 'list_investigations') return null;
      if (args?.activeOnly) {
        activeCalls += 1;
        return activeCalls === 1
          ? { items: [activeFirst], next_cursor: 'active-first-cursor' }
          : { items: [activeRefresh], next_cursor: 'active-refresh-cursor' };
      }
      allCalls += 1;
      return allCalls === 1 ? oldAllPage.promise : { items: [latestAll], next_cursor: 'latest-all-cursor' };
    });

    const oldAllRequest = useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().selectHistoryActive(true);
    useInvestigationStore.setState({ history: [activeFirst, investigationDetail('older-active', { status: 'preparing', has_result: false })],
      nextCursor: 'older-active-cursor' });
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['active-refresh']);
    expect(useInvestigationStore.getState().nextCursor).toBe('active-refresh-cursor');

    await useInvestigationStore.getState().selectHistoryActive(false);
    oldAllPage.resolve({ items: [staleAll], next_cursor: 'stale-all-cursor' });
    await oldAllRequest;
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['latest-all-result']);
    expect(useInvestigationStore.getState().nextCursor).toBe('latest-all-cursor');
  });

  it('orders exact duplicate pages and rejects gaps, foreign identities and conflicting events', () => {
    const events = investigationEvents('original');
    expect(mergeInvestigationEvents('original', events.slice(0, 4), events.slice(3))).toEqual(events);
    expect(() => mergeInvestigationEvents('original', [], [events[1]])).toThrow('Missing event');
    expect(() => mergeInvestigationEvents('other', [], [events[0]])).toThrow('Invalid event identity');
    expect(() => mergeInvestigationEvents('original', [events[0]], [{ ...events[0], summary: 'changed' }])).toThrow('Conflicting event');
  });

  it('recovers an uncertain start with exactly the original nonce and request', async () => {
    const starts: StartInvestigationRequest[] = [];
    mockIPC((command, args) => {
      if (command === 'start_investigation') {
        starts.push((args as { request: StartInvestigationRequest }).request);
        if (starts.length === 1) throw new Error('Transport lost after durable admission.');
        return investigationDetail('same-durable-task');
      }
      return response(command, args);
    });
    const uncertain = await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@2', question: 'Inspect this scope.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
    expect(uncertain).toBeNull();
    expect(useInvestigationStore.getState().pendingStart).toEqual(starts[0]);
    await useInvestigationStore.getState().start({ specialist_id: 'evidence-auditor@2', question: 'Cannot replace pending start.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
    expect(starts).toHaveLength(1);
    const recoveredId = await useInvestigationStore.getState().retryStart();
    expect(recoveredId).toBe('same-durable-task');
    expect(starts).toHaveLength(2);
    expect(starts[1]).toEqual(starts[0]);
    expect(starts[0].request_nonce.length).toBeGreaterThan(0);
    expect(useInvestigationStore.getState().selectedId).toBe('same-durable-task');
    expect(useInvestigationStore.getState().pendingStart).toBeNull();
  });

  it.each([
    { label: 'quick retry opened from the ordinary investigation surface', startDeferred: true, retryDeferred: false, opens: 1 },
    { label: 'ordinary retry opened from Quick Dispatch', startDeferred: false, retryDeferred: true, opens: 0 },
  ])('lets the actual retry caller own landing when $label', async ({ startDeferred, retryDeferred, opens }) => {
    const requests: StartInvestigationRequest[] = [];
    const open = vi.fn(async () => {});
    useInvestigationStore.setState({ open });
    mockIPC((command, args) => {
      if (command === 'start_investigation') {
        requests.push((args as { request: StartInvestigationRequest }).request);
        if (requests.length === 1) throw new Error('Transport lost after durable admission.');
        return investigationDetail('caller-owned-landing');
      }
      return response(command, args);
    });
    const draft = { specialist_id: 'domain-analyst@3' as const, question: 'Keep the request unchanged.',
      scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1' };

    expect(await useInvestigationStore.getState().start(draft, { deferOpen: startDeferred })).toBeNull();
    const acceptedId = await useInvestigationStore.getState().retryStart({ deferOpen: retryDeferred });

    expect(acceptedId).toBe('caller-owned-landing');
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
    expect(open).toHaveBeenCalledTimes(opens);
  });

  it('returns confirmed admission identity even when the subsequent detail read fails', async () => {
    mockIPC((command) => {
      if (command === 'start_investigation') return investigationDetail('accepted-but-unavailable');
      if (command === 'get_investigation') throw new Error('Detail read failed');
      return response(command, { investigationId: 'accepted-but-unavailable' });
    });
    const id = await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3', question: 'Inspect this scope.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
    expect(id).toBe('accepted-but-unavailable');
    expect(useInvestigationStore.getState().selectedId).toBe('accepted-but-unavailable');
    expect(useInvestigationStore.getState().pendingStart).toBeNull();
    expect(useInvestigationStore.getState().startError).toBeNull();
    expect(useInvestigationStore.getState().error).toMatch(/could not be refreshed consistently/i);
  });

  it('ignores an older task response after selecting another task', async () => {
    const first = deferred<InvestigationDetail>();
    mockIPC((command, args) => command === 'get_investigation' && args?.investigationId === 'first'
      ? first.promise : response(command, args));
    const earlier = useInvestigationStore.getState().open('first');
    await useInvestigationStore.getState().open('second');
    first.resolve(investigationDetail('first'));
    await earlier;
    expect(useInvestigationStore.getState().detail?.investigation_id).toBe('second');
    expect(useInvestigationStore.getState().result?.investigation_id).toBe('second');
    expect(useInvestigationStore.getState().events.every((event) => event.investigation_id === 'second')).toBe(true);
  });

  it('ignores a superseded same-task failure and an earlier same-ID reopen', async () => {
    const first = deferred<InvestigationDetail>();
    const reopened = deferred<InvestigationDetail>();
    let calls = 0;
    mockIPC((command, args) => {
      if (command === 'get_investigation') {
        calls += 1;
        if (calls === 1) return first.promise;
        if (calls === 3) return reopened.promise;
      }
      return response(command, args);
    });
    const earlier = useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().refreshSelected();
    first.reject(new Error('Earlier failure with private details.'));
    await earlier;
    expect(useInvestigationStore.getState().error).toBeNull();
    const earlierOpen = useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().open('same');
    reopened.resolve(investigationDetail('same', { question: 'Stale question' }));
    await earlierOpen;
    expect(useInvestigationStore.getState().detail?.question).not.toBe('Stale question');
  });

  it('retains saved activity when a page is missing and recovers by reading the durable cursor', async () => {
    const events = investigationEvents('same');
    let complete = false;
    const cursors: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'investigation_events') {
        cursors.push(args?.afterSequence);
        return { investigation_id: 'same', items: complete ? events : events.filter((event) => event.sequence !== 4),
          next_sequence: 9, has_more: false };
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().events).toEqual([]);
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
    complete = true;
    await useInvestigationStore.getState().refreshSelected();
    expect(cursors).toEqual([0, 0]);
    expect(useInvestigationStore.getState().events).toEqual(events);
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('retains an admitted late result while cancellation remains authoritative', async () => {
    // AC-0187/0190: task status is not rewritten as completed to display a saved answer.
    mockIPC((command, args) => command === 'get_investigation'
      ? investigationDetail('same', { status: 'cancelled', cancel_requested: true,
        actions: { can_cancel: false, can_follow_up: true } }) : response(command, args));
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().detail?.status).toBe('cancelled');
    expect(useInvestigationStore.getState().result).toEqual(investigationResult('same'));
  });

  it('rejects a result read that straddles the result commit and recovers on the next refresh', async () => {
    // AC-0204: `has_result: true` with no result must not be recorded as a completed task without findings.
    let committed = false;
    mockIPC((command, args) => command === 'investigation_result' && !committed ? null : response(command, args));
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().detail).toBeNull();
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
    committed = true;
    await useInvestigationStore.getState().refreshSelected();
    expect(useInvestigationStore.getState().detail?.status).toBe('completed');
    expect(useInvestigationStore.getState().result).toEqual(investigationResult('same'));
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('keeps saved findings and activity when a later refresh straddles the result commit', async () => {
    // AC-0204: an inconsistent later observation leaves the previously saved state in place.
    let straddle = false;
    mockIPC((command, args) => command === 'get_investigation' && straddle
      ? investigationDetail('same', { status: 'running', has_result: false, revision: 10,
        actions: { can_cancel: true, can_follow_up: false } }) : response(command, args));
    await useInvestigationStore.getState().open('same');
    const { detail, result, events } = useInvestigationStore.getState();
    expect(result).toEqual(investigationResult('same'));
    straddle = true;
    await useInvestigationStore.getState().refreshSelected();
    expect(useInvestigationStore.getState().detail).toEqual(detail);
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().events).toEqual(events);
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
  });

  it('refreshes again when the task changes while a refresh is loading (#2476)', async () => {
    // A failed run's terminal notification can land during an in-flight read
    // that still observes the running task. It must not be dropped.
    const running = investigationDetail('same', { status: 'running', has_result: false, revision: 5,
      last_event_sequence: 5, actions: { can_cancel: true, can_follow_up: false } });
    const held = deferred<InvestigationDetail>();
    let reads = 0;
    let terminal = false;
    mockIPC((command, args) => {
      if (command === 'get_investigation') {
        reads += 1;
        if (reads === 2) return held.promise;
        return terminal ? investigationDetail('same') : running;
      }
      if (command === 'investigation_result') return terminal ? investigationResult('same') : null;
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().detail?.status).toBe('running');
    const stale = useInvestigationStore.getState().refreshSelected();
    expect(useInvestigationStore.getState().loading).toBe(true);
    terminal = true;
    useInvestigationStore.getState().invalidate({ investigation_id: 'same', revision: 9, last_event_sequence: 9 });
    expect(reads).toBe(2);
    held.resolve(running);
    await stale;
    await vi.waitFor(() => expect(useInvestigationStore.getState().detail?.status).toBe('completed'));
    expect(reads).toBe(3);
    expect(useInvestigationStore.getState().loading).toBe(false);
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('rejects a result that is newer than the detail it was read with', async () => {
    // AC-0204: a result paired with a pre-commit detail is an inconsistent observation, not a running task with findings.
    mockIPC((command, args) => command === 'get_investigation'
      ? investigationDetail('same', { status: 'running', has_result: false, revision: 8,
        actions: { can_cancel: true, can_follow_up: false } }) : response(command, args));
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().detail).toBeNull();
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
  });

  it('fetches multiple journal pages sequentially without inventing or dropping activity', async () => {
    const events: InvestigationEvent[] = Array.from({ length: 70 }, (_, index) => ({
      ...investigationEvents('paged')[0], sequence: index + 1, revision: index + 1,
    }));
    const cursors: number[] = [];
    mockIPC((command, args) => {
      if (command === 'get_investigation') return investigationDetail('paged', { last_event_sequence: 70, revision: 70 });
      if (command === 'investigation_events') {
        const after = Number(args?.afterSequence);
        cursors.push(after);
        const items = events.slice(after, after + 50);
        return { investigation_id: 'paged', items, next_sequence: items.at(-1)!.sequence,
          has_more: after + 50 < events.length };
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('paged');
    expect(cursors).toEqual([0, 50]);
    expect(useInvestigationStore.getState().events).toEqual(events);
  });
});

describe('investigation consent and historical reads (AC-0186/0189/0190)', () => {
  it('submits exactly the displayed step revision and hash, and never approves a later step implicitly', async () => {
    let step = 1;
    const approvals: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'get_investigation') return investigationDetail('same', { status: 'awaiting_consent',
        provider_mode: 'cloud', revision: step + 3, actions: { can_cancel: true, can_follow_up: false } });
      if (command === 'investigation_consent') return investigationConsent('same', step + 3, `step-${step}`);
      if (command === 'approve_investigation_step') {
        approvals.push(args); step = 2;
        return investigationDetail('same', { status: 'awaiting_consent', revision: 5 });
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    useInvestigationStore.getState().showConsent();
    await useInvestigationStore.getState().approve();
    expect(approvals).toEqual([{ investigationId: 'same', stepId: 'step-1', revision: 4, payloadHash: 'payload:same:step-1' }]);
    expect(useInvestigationStore.getState().consent?.step_id).toBe('step-2');
    expect(useInvestigationStore.getState().consentOpen).toBe(false);
  });

  it('does not merge an approved completed summary ahead of its result', async () => {
    // AC-0204: the approval acknowledgment may report completion before the result read sees it.
    let completed = false;
    mockIPC((command, args) => {
      if (command === 'get_investigation') return completed ? investigationDetail('same', { revision: 10 })
        : investigationDetail('same', { status: 'awaiting_consent', provider_mode: 'cloud', revision: 4,
          has_result: false, actions: { can_cancel: true, can_follow_up: false } });
      if (command === 'investigation_consent') return completed ? null : investigationConsent('same', 4, 'step-1');
      if (command === 'investigation_result') return null;
      if (command === 'approve_investigation_step') {
        completed = true;
        return investigationDetail('same', { revision: 10 });
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    useInvestigationStore.getState().showConsent();
    await useInvestigationStore.getState().approve();
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().detail?.status).toBe('awaiting_consent');
    expect(useInvestigationStore.getState().detail?.has_result).toBe(false);
    expect(useInvestigationStore.getState().history[0].status).toBe('completed');
  });

  it('clears stale consent observations without inventing local fallback or a grant', async () => {
    const commands: string[] = [];
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'get_investigation') return investigationDetail('same', { status: 'cancelled', cancel_requested: true });
      if (command === 'investigation_consent') return investigationConsent('same');
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    useInvestigationStore.getState().showConsent();
    await useInvestigationStore.getState().approve();
    expect(useInvestigationStore.getState().consent).toBeNull();
    expect(commands.some((command) => command.includes('approve') || command.includes('start'))).toBe(false);
  });

  it('discards late historical source when the selected task changes', async () => {
    const first = deferred<InvestigationCitationRead>();
    const reads: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'read_investigation_citation') { reads.push(args); return first.promise; }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('first');
    const pending = useInvestigationStore.getState().readCitation('C1');
    await useInvestigationStore.getState().open('second');
    first.resolve({ investigation_id: 'first', citation_id: 'C1', citation: investigationCitations[0],
      status: 'available', text: 'ORIGINAL HISTORICAL SOURCE' });
    await pending;
    expect(reads).toEqual([{ investigationId: 'first', citationId: 'C1' }]);
    expect(useInvestigationStore.getState().citationRead).toBeNull();
    expect(useInvestigationStore.getState().citationId).toBeNull();
  });

  it('rejects source-bearing legacy reads and never calls a current checkout reader', async () => {
    const commands: string[] = [];
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'read_investigation_citation') return { investigation_id: 'same', citation_id: 'C3',
        citation: investigationCitations[2], status: 'working_tree_unverified', text: 'MUST NOT DISPLAY' };
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().readCitation('C3');
    expect(useInvestigationStore.getState().citationRead).toBeNull();
    expect(useInvestigationStore.getState().citationError).toContain('No current checkout source was substituted');
    expect(commands.filter((command) => command.startsWith('read_'))).toEqual(['read_investigation_citation']);
  });
});

describe('versioned investigation history (AC-0180/0190/0235)', () => {
  it.each([1, 2] as const)('retains saved specialist @%i and result v1 without disclosure fields', async (version) => {
    mockIPC((command, args) => {
      if (command === 'get_investigation') return legacyInvestigationDetail(version, 'legacy');
      if (command === 'investigation_result') return legacyInvestigationResult('legacy');
      return response(command, args);
    });
    await useInvestigationStore.getState().open('legacy');
    const { detail, result } = useInvestigationStore.getState();
    expect(detail?.specialist.id).toBe(`domain-analyst@${version}`);
    expect(detail?.specialist.version).toBe(version);
    expect(result?.schema_version).toBe(1);
    expect(result).not.toHaveProperty('invocation_step_id');
    expect(result).not.toHaveProperty('disclosure_hash');
  });

  it('rejects a narrative attached to a legacy result on the saved-result IPC path (AC-0404)', async () => {
    const detail = legacyInvestigationDetail(1, 'legacy-narrative');
    const result = { ...legacyInvestigationResult(detail.investigation_id), narrative: {
      text: 'This result predates the authored-message contract.', finding_ids: ['finding:legacy'],
    } };
    mockIPC((command, args) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result : response(command, args));
    await useInvestigationStore.getState().open(detail.investigation_id);
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().error).toContain('consistently');
  });

  it('keeps current specialist and result disclosure bindings on refresh', async () => {
    mockIPC(response);
    await useInvestigationStore.getState().open('current');
    await useInvestigationStore.getState().refreshSelected();
    const { detail, result } = useInvestigationStore.getState();
    expect(detail?.specialist_id).toBe('domain-analyst@3');
    expect(result?.schema_version).toBe(2);
    expect(result?.invocation_step_id).toBe('step-1');
    expect(result?.disclosure_hash).toBe(`investigation-disclosure-v1:${'a'.repeat(64)}`);
  });
});

describe('result version binding admission (AC-0235)', () => {
  it.each([
    { schema_version: 2, invocation_step_id: undefined },
    { schema_version: 2, disclosure_hash: undefined },
    { schema_version: 2, disclosure_hash: 'investigation-disclosure-v1:short' },
    { schema_version: 2, disclosure_hash: `investigation-disclosure-v1:${'A'.repeat(64)}` },
    { schema_version: 2, invocation_step_id: 'step/2' },
    { schema_version: 2, invocation_step_id: 'step-1\n' },
    { schema_version: 2, disclosure_hash: `investigation-disclosure-v1:${'a'.repeat(64)}\n` },
    { schema_version: 2, invocation_step_id: 'a'.repeat(129) },
    { schema_version: 1 },
    { schema_version: 99 },
  ])('rejects malformed/unsupported bindings without legacy fallback: %j', async (patch) => {
    mockIPC((command, args) => command === 'investigation_result'
      ? { ...investigationResult('tampered'), ...patch } : response(command, args));
    await useInvestigationStore.getState().open('tampered');
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().error).toContain('consistently');
  });
});

describe('explicit source inspection intent (AC-0261/0262)', () => {
  it('freezes schema-2 requirements and nonce across an uncertain admission retry', async () => {
    const starts: StartInvestigationRequest[] = [];
    mockIPC((command, args) => {
      if (command === 'start_investigation') {
        starts.push(structuredClone(args?.request as StartInvestigationRequest));
        throw new Error('Transport uncertain');
      }
      return response(command, args);
    });
    const target = structuredClone(inspectionTarget);
    const draft = { specialist_id: 'domain-analyst@5' as const, question: 'Inspect the exact source.',
      scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1',
      inspection_requirements: [target] };
    await useInvestigationStore.getState().start(draft);
    target.index = 999;
    draft.question = 'Changed after submission';
    await useInvestigationStore.getState().retryStart();
    expect(starts).toHaveLength(2);
    expect(starts[0]).toEqual(starts[1]);
    expect(starts[0].schema_version).toBe(2);
    expect(starts[0].inspection_requirements?.[0].index).toBe(2);
    expect(starts[0].question).toBe('Inspect the exact source.');
  });

  it('unlocks stale selections only after a definitive pre-admission rejection', async () => {
    mockIPC(() => { throw { code: 'refresh_required', definitive: true, message: 'Stale' }; });
    await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@5', question: 'Inspect.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1', inspection_requirements: [inspectionTarget] });
    expect(useInvestigationStore.getState().pendingStart).toBeNull();
    expect(useInvestigationStore.getState().startError).toContain('No investigation was started');
  });

  it('rejects mixed graph drafts without rebinding earlier targets', () => {
    expect(useInvestigationStore.getState().addInspectionRequirement(inspectionTarget)).toBe(true);
    expect(useInvestigationStore.getState().addInspectionRequirement({ ...inspectionTarget,
      target_id: 'c'.repeat(64), graph_snapshot_id: 'snapshot:new' })).toBe(false);
    expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([inspectionTarget]);
  });

  it('shows a valid partial schema-3 result independently of unmet requirements', async () => {
    const assessment = inspectionAssessment();
    mockIPC((command, args) => {
      if (command === 'get_investigation') return inspectionDetail('required', {
        inspection_requirements: [inspectionTarget], inspection_assessment: assessment });
      if (command === 'investigation_result') return { ...investigationResult('required'), schema_version: 3,
        inspection_assessment: assessment };
      return response(command, args);
    });
    await useInvestigationStore.getState().open('required');
    expect(useInvestigationStore.getState().error).toBeNull();
    expect(useInvestigationStore.getState().result?.findings).toHaveLength(2);
    expect(useInvestigationStore.getState().detail?.inspection_assessment?.requirements[0].status).toBe('unmet');
  });

  it('rejects an assessment from a different finishing disclosure', async () => {
    const assessment = { ...inspectionAssessment(), finishing_step_id: 'another-step' };
    mockIPC((command, args) => {
      if (command === 'get_investigation') return inspectionDetail('required', { inspection_assessment: assessment });
      if (command === 'investigation_result') return { ...investigationResult('required'), schema_version: 3,
        inspection_assessment: assessment };
      return response(command, args);
    });
    await useInvestigationStore.getState().open('required');
    expect(useInvestigationStore.getState().result).toBeNull();
    expect(useInvestigationStore.getState().error).not.toBeNull();
  });

  it('ignores exact-location responses after selecting another saved task', async () => {
    const location = deferred<InvestigationCapturedLocation>();
    mockIPC((command, args) => command === 'inspection_captured_location' ? location.promise : response(command, args));
    useInvestigationStore.setState({ selectedId: 'required', detail: inspectionDetail('required', {
      inspection_requirements: [inspectionTarget] }) });
    const reading = useInvestigationStore.getState().readInspectionLocation(inspectionTarget);
    await useInvestigationStore.getState().open('different');
    location.resolve({ target_id: inspectionTarget.target_id, captured: inspectionTarget.captured,
      byte_start: 12, byte_end: 54, start_line: 1, start_column: 13, end_line: 2, end_column: 23,
      column_unit: 'utf8_bytes', end_exclusive: true });
    await reading;
    expect(useInvestigationStore.getState().inspectionLocation).toBeNull();
  });
});

it('cannot edit draft requirements while the original nonce outcome is uncertain (AC-0261)', async () => {
  useInvestigationStore.getState().addInspectionRequirement(inspectionTarget);
  mockIPC(() => { throw new Error('Uncertain transport'); });
  await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@5', question: 'Inspect.',
    scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1', inspection_requirements: [inspectionTarget] });
  useInvestigationStore.getState().removeInspectionRequirement(inspectionTarget.target_id);
  useInvestigationStore.getState().clearInspectionRequirements();
  expect(useInvestigationStore.getState().addInspectionRequirement({ ...inspectionTarget, target_id: 'd'.repeat(64) })).toBe(false);
  expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([inspectionTarget]);
  expect(useInvestigationStore.getState().pendingStart?.inspection_requirements).toEqual([inspectionTarget]);
});


it('keeps the composer frozen with its exact pending nonce until admission is resolved (AC-0261)', async () => {
  const store = useInvestigationStore.getState();
  store.updateComposer({ specialistId: 'evidence-auditor@5', question: 'Keep this explicit follow-up.',
    providerMode: 'cloud', scopeType: 'neighborhood', anchor: 'rule:stock', hops: 2,
    parent: { parent_id: 'parent', conversation_id: 'conversation-parent' } });
  store.addInspectionRequirement(inspectionTarget);
  const composer = structuredClone(useInvestigationStore.getState().composer);
  mockIPC(() => { throw new Error('Uncertain transport'); });
  await store.start({ specialist_id: 'evidence-auditor@5', question: composer.question,
    provider_mode: 'cloud', scope: { type: 'neighborhood', anchor: 'rule:stock', hops: 2 },
    limit_profile: 'investigation-v1', ...composer.parent!, inspection_requirements: [inspectionTarget] });
  const pending = structuredClone(useInvestigationStore.getState().pendingStart);
  store.updateComposer({ question: 'Cannot change unresolved intent', parent: null });
  expect(useInvestigationStore.getState().composer).toEqual(composer);
  expect(useInvestigationStore.getState().pendingStart).toEqual(pending);
  const confirmed = inspectionDetail('confirmed', { specialist_id: 'evidence-auditor@5',
    specialist: { ...inspectionDetail().specialist, id: 'evidence-auditor@5' } });
  mockIPC((command, args) => {
    if (command === 'start_investigation' || command === 'get_investigation') return confirmed;
    if (command === 'investigation_result') return { ...investigationResult('confirmed'), schema_version: 3,
      inspection_assessment: inspectionAssessment() };
    return response(command, args);
  });
  await store.retryStart();
  expect(useInvestigationStore.getState().pendingStart).toBeNull();
  expect(useInvestigationStore.getState().composer.parent).toBeNull();
  expect(useInvestigationStore.getState().composer.question).toBe('');
  expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([]);
});

it('unlocks a definitive validation rejection but retains editable explicit intent (AC-0261)', async () => {
  const store = useInvestigationStore.getState();
  store.updateComposer({ question: 'Inspect this source.', parent: { parent_id: 'parent', conversation_id: 'conversation-parent' } });
  store.addInspectionRequirement(inspectionTarget);
  mockIPC(() => { throw { code: 'invalid_input', definitive: true, message: 'Invalid captured requirement.' }; });
  await store.start({ specialist_id: 'domain-analyst@5', question: 'Inspect this source.',
    provider_mode: 'local', scope: { type: 'all' }, limit_profile: 'investigation-v1', inspection_requirements: [inspectionTarget] });
  expect(useInvestigationStore.getState().pendingStart).toBeNull();
  expect(useInvestigationStore.getState().startError).toContain('No investigation was started. Invalid captured requirement.');
  expect(useInvestigationStore.getState().composer.parent?.parent_id).toBe('parent');
  store.updateComposer({ question: 'Corrected request.' });
  expect(useInvestigationStore.getState().composer.question).toBe('Corrected request.');
  expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([inspectionTarget]);
});


// Generated by the native TS-capture/production-host test. These are actual DTO
// bytes, not a hand-written schema-1 approximation of a V5 response.
it('accepts actual schema-2 V5 start/history/detail and schema-3 result/location DTOs (AC-0261/0262)', async () => {
  const fixture = backendInspection;
  const detail = fixture.detail;
  // This UI test supplies only the journal transport required for refresh; it
  // does not claim these placeholder events as native execution receipts.
  const events = Array.from({ length: detail.last_event_sequence }, (_, index) => ({
    investigation_id: detail.investigation_id, sequence: index + 1, revision: detail.revision,
    kind: 'created', step_id: null, tool: null, summary: 'Journal transport fixture.', created_at: detail.created_at,
  }));
  mockIPC((command, args) => {
    if (command === 'start_investigation' || command === 'get_investigation') return detail;
    if (command === 'list_investigations') return { items: [detail], next_cursor: null };
    if (command === 'investigation_result') return fixture.result;
    if (command === 'investigation_consent') return null;
    if (command === 'get_investigation_chat') return null;
    if (command === 'inspection_captured_location') return fixture.location;
    if (command === 'investigation_events') {
      const items = events.filter((event) => event.sequence > Number(args?.afterSequence ?? 0));
      return { investigation_id: detail.investigation_id, items, next_sequence: detail.last_event_sequence, has_more: false };
    }
    throw new Error('Unexpected fixture command.');
  });
  const store = useInvestigationStore.getState();
  await store.loadHistory();
  expect(useInvestigationStore.getState().history).toEqual([detail]);
  const target = fixture.target as InvestigationInspectionTarget;
  await store.start({ specialist_id: 'domain-analyst@5', question: detail.question,
    provider_mode: 'local', scope: { type: 'all' }, limit_profile: 'investigation-v1', inspection_requirements: [target] });
  expect(useInvestigationStore.getState().pendingStart).toBeNull();
  expect(useInvestigationStore.getState().startError).toBeNull();
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().result).toEqual(fixture.result);
  expect(useInvestigationStore.getState().detail?.inspection_assessment).toEqual(fixture.detail.inspection_assessment);
  expect(target.index).toBe(0);
  expect(target.receipt_inventory_index).toBe(12);
  await store.readInspectionLocation(target);
  expect(useInvestigationStore.getState().inspectionLocation).toEqual(fixture.location);
});

it.each([
  { schema_version: 1 },
  { schema_version: 2, specialist_id: 'domain-analyst@4' },
  { schema_version: 3 },
  { inspection_requirement_count: 0 },
  { inspection_requirement_count: 13 },
  { inspection_requirement_count: 1.5 },
  { inspection_requirement_count: undefined },
  { inspection_status: undefined },
  { inspection_status: 'pending' },
  { inspection_status: 'unknown' },
  { has_result: false, inspection_status: 'supplied' },
  { has_result: false, status: 'running', inspection_status: 'not_assessed' },
  { has_result: false, status: 'cancelled', invocation_pending: true, inspection_status: 'not_assessed' },
  { inspection_status: 'not_assessed' },
])('rejects inconsistent V5 summary schema/count/status: %j', async (override) => {
  mockIPC(() => ({ items: [{ ...backendInspection.detail, ...override }], next_cursor: null }));
  await useInvestigationStore.getState().loadHistory();
  expect(useInvestigationStore.getState().history).toEqual([]);
  expect(useInvestigationStore.getState().historyError).not.toBeNull();
});

it.each(['domain-analyst@1', 'evidence-auditor@1', 'domain-analyst@2', 'evidence-auditor@2',
  'domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4'] as const)(
  'retains schema-1 history for %s', async (specialist_id) => {
    const summary = investigationDetail('legacy', { specialist_id });
    mockIPC(() => ({ items: [summary], next_cursor: null }));
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history).toEqual([summary]);
    expect(useInvestigationStore.getState().historyError).toBeNull();
  });


it.each([
  { status: 'running', invocation_pending: false, inspection_status: 'pending' },
  { status: 'cancelled', invocation_pending: true, inspection_status: 'pending' },
  { status: 'failed', invocation_pending: false, inspection_status: 'not_assessed' },
  { status: 'cancelled', invocation_pending: false, inspection_status: 'not_assessed' },
  { status: 'interrupted', invocation_pending: false, inspection_status: 'not_assessed' },
  { status: 'outcome_unknown', invocation_pending: false, inspection_status: 'not_assessed' },
])('accepts V5 no-result lifecycle assessment state: %j', async (lifecycle) => {
  const summary = { ...backendInspection.detail, ...lifecycle, has_result: false };
  mockIPC(() => ({ items: [summary], next_cursor: null }));
  await useInvestigationStore.getState().loadHistory();
  expect(useInvestigationStore.getState().historyError).toBeNull();
  expect(useInvestigationStore.getState().history).toEqual([summary]);
});

/** Replay actual saved DTOs; activity is a bounded transport stub, not provider evidence. */
function qualifiedTransport(detail: InvestigationDetail, result: InvestigationResult | null) {
  mockIPC((command, args) => {
    if (command === 'get_investigation') return detail;
    if (command === 'list_investigations') return { items: [detail], next_cursor: null };
    if (command === 'investigation_result') return result;
    if (command === 'investigation_consent') return null;
    if (command === 'investigation_events') return { investigation_id: detail.investigation_id,
      items: Array.from({ length: detail.last_event_sequence }, (_, index) => ({
        investigation_id: detail.investigation_id, sequence: index + 1, revision: detail.revision,
        kind: 'created', step_id: null, tool: null, summary: 'Saved activity transport fixture.', created_at: detail.created_at,
      })).filter((event) => event.sequence > Number(args?.afterSequence ?? 0)),
      next_sequence: detail.last_event_sequence, has_more: false };
    if (command === 'get_investigation_chat') return null;
    throw new Error('Unexpected qualified command');
  });
}

describe('saved coverage-qualified investigations (AC-0239)', () => {
  it('accepts actual Rust worker/firewall/store V6 detail and result without source obligations', async () => {
    const fixture = backendQualified as { detail: InvestigationDetail; result: InvestigationResult };
    qualifiedTransport(fixture.detail, fixture.result);
    await useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().open(fixture.detail.investigation_id);
    const state = useInvestigationStore.getState();
    expect(state.historyError).toBeNull();
    expect(state.error).toBeNull();
    expect(state.detail).toEqual(fixture.detail);
    expect(state.result).toEqual(fixture.result);
    expect(state.detail?.inspection_requirements).toBeUndefined();
    expect(state.result?.inspection_assessment).toBeUndefined();
  });

  it('freezes schema-3 context expectation and nonce while leaving implicit targets absent', async () => {
    const starts: StartInvestigationRequest[] = [];
    mockIPC((command, args) => { expect(command).toBe('start_investigation');
      starts.push(structuredClone(args?.request as StartInvestigationRequest)); throw new Error('Uncertain'); });
    useInvestigationStore.getState().addInspectionRequirement(inspectionTarget);
    const draft = { specialist_id: 'domain-analyst@6' as const, question: 'Qualify this context.',
      scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1',
      expected_context_id: qualifiedCoverageContext.context_id };
    await useInvestigationStore.getState().start(draft);
    draft.expected_context_id = `context-v2:${'9'.repeat(64)}`;
    await useInvestigationStore.getState().retryStart();
    expect(starts).toHaveLength(2);
    expect(starts[1]).toEqual(starts[0]);
    expect(starts[0].schema_version).toBe(3);
    expect(starts[0].expected_context_id).toBe(qualifiedCoverageContext.context_id);
    expect('inspection_requirements' in starts[0]).toBe(false);
  });

  it('supports explicit V6 targets without conflating their assessment with coverage', async () => {
    const assessment = { ...inspectionAssessment(), disclosure_hash: qualifiedResult().disclosure_hash! };
    const detail = qualifiedDetail('targeted', { inspection_requirements: [inspectionTarget],
      inspection_assessment: assessment, inspection_status: 'unmet', inspection_requirement_count: 1 });
    const result = qualifiedResult('targeted', { inspection_assessment: assessment });
    qualifiedTransport(detail, result);
    await useInvestigationStore.getState().open('targeted');
    expect(useInvestigationStore.getState().error).toBeNull();
    expect(useInvestigationStore.getState().result?.coverage).toEqual(result.coverage);
    expect(useInvestigationStore.getState().result?.inspection_assessment?.requirements[0].status).toBe('unmet');
  });

  it('sends explicit V6 targets only when present and preserves their graph expectation', async () => {
    let sent: StartInvestigationRequest | null = null;
    mockIPC((command, args) => { expect(command).toBe('start_investigation'); sent = args?.request as StartInvestigationRequest;
      throw new Error('Uncertain'); });
    await useInvestigationStore.getState().start({ specialist_id: 'evidence-auditor@6', question: 'Inspect.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1', inspection_requirements: [inspectionTarget] });
    expect(sent).toMatchObject({ schema_version: 3, inspection_requirements: [inspectionTarget],
      expected_graph_revision: inspectionTarget.graph_snapshot_id });
  });

  it('accepts queued V6 before coverage preparation without inventing a basis', async () => {
    const detail = qualifiedDetail('queued', { status: 'queued', has_result: false, graph_snapshot_id: null,
      coverage_context: undefined, scope_snapshot_id: null });
    qualifiedTransport(detail, null);
    await useInvestigationStore.getState().open('queued');
    expect(useInvestigationStore.getState().error).toBeNull();
    expect(useInvestigationStore.getState().detail?.coverage_context).toBeUndefined();
  });

  it('retains original prepared qualification when a later response substitutes inventory over identical graph', async () => {
    const detail = qualifiedDetail('live', { status: 'running', has_result: false });
    qualifiedTransport(detail, null);
    await useInvestigationStore.getState().open('live');
    const replacement = structuredClone(detail);
    replacement.revision += 1;
    replacement.coverage_context!.context_id = `context-v2:${'9'.repeat(64)}`;
    replacement.coverage_context!.coverage.coverage_id = `coverage-context-v1:${'8'.repeat(64)}`;
    qualifiedTransport(replacement, null);
    await useInvestigationStore.getState().refreshSelected();
    expect(useInvestigationStore.getState().error).not.toBeNull();
    expect(useInvestigationStore.getState().detail?.coverage_context).toEqual(detail.coverage_context);
  });

  it.each([
    { schema_version: 1 }, { schema_version: 2 }, { specialist_id: 'domain-analyst@5' },
    { inspection_requirement_count: 0 }, { inspection_requirement_count: 1 },
    { inspection_status: 'supplied' },
  ])('rejects incompatible V6 summary wire: %j', async (override) => {
    qualifiedTransport({ ...qualifiedDetail(), ...override } as InvestigationDetail, qualifiedResult());
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().historyError).not.toBeNull();
  });

  it.each(['absent', 'exhaustive', 'stale_counts', 'unrecorded_inventory', 'wrong_context', 'wrong_coverage', 'old_disclosure', 'bad_hash', 'missing_attestation'])(
    'refuses inconsistent saved coverage: %s', async (mutation) => {
      const detail = structuredClone(qualifiedDetail());
      const result = structuredClone(qualifiedResult());
      if (mutation === 'absent') delete detail.coverage_context;
      if (mutation === 'exhaustive') Object.assign(detail.coverage_context!.coverage, { exhaustive: true });
      if (mutation === 'stale_counts') detail.coverage_context!.coverage.repos[1].counts = { missing_input: 1, not_selected: 0, not_applicable: 0, unsupported: 0 };
      if (mutation === 'unrecorded_inventory') detail.coverage_context!.coverage.repos[2].inventory_hash = '7'.repeat(64);
      if (mutation === 'wrong_context') result.coverage!.context_id = `context-v2:${'7'.repeat(64)}`;
      if (mutation === 'wrong_coverage') result.coverage!.coverage_id = `coverage-context-v1:${'7'.repeat(64)}`;
      if (mutation === 'old_disclosure') result.disclosure_hash = `investigation-disclosure-v1:${'7'.repeat(64)}`;
      if (mutation === 'bad_hash') result.coverage!.representation_hash += '\n';
      if (mutation === 'missing_attestation') delete result.coverage;
      qualifiedTransport(detail, result);
      await useInvestigationStore.getState().open(detail.investigation_id);
      expect(useInvestigationStore.getState().error).not.toBeNull();
      expect(useInvestigationStore.getState().result).toBeNull();
    });
});

it('rejects a premature V6 inspection assessment before a finishing result (AC-0239)', async () => {
  const detail = qualifiedDetail('pending', { status: 'running', has_result: false,
    inspection_requirement_count: 1, inspection_status: 'pending', inspection_requirements: [inspectionTarget],
    inspection_assessment: inspectionAssessment() });
  qualifiedTransport(detail, null);
  await useInvestigationStore.getState().open('pending');
  expect(useInvestigationStore.getState().error).not.toBeNull();
  expect(useInvestigationStore.getState().detail).toBeNull();
});


it('freezes expanded policy, envelope, generation and nonce across uncertain retry (AC-0281)', async () => {
  useInvestigationStore.setState({ catalog: expandedCompletionCatalog, obligationChoices: completionChoices });
  const store = useInvestigationStore.getState();
  store.updateComposer({ specialistId: 'domain-analyst@8', limitProfile: 'investigation-v2' });
  const requests: unknown[] = [];
  mockIPC((command, args) => { if (command === 'start_investigation') requests.push(structuredClone(args)); throw new Error('Uncertain transport'); });
  await store.start({ specialist_id: 'domain-analyst@8', limit_profile: 'investigation-v2', question: 'Inspect evidence.',
    scope: { type: 'all' }, provider_mode: 'local', obligations: { schema_version: 2,
      contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  expect(useInvestigationStore.getState().pendingStart).toMatchObject({ schema_version: 5, limit_profile: 'investigation-v2' });
  store.updateComposer({ specialistId: 'domain-analyst@7', limitProfile: 'investigation-v1' });
  useInvestigationStore.setState({ catalog: null });
  await store.retryStart();
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
  expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@8', limitProfile: 'investigation-v2' });
});

it('uses overlay-local completion choices and preserves the full composer through exact retry (AC-0385)', async () => {
  const accepted = specWriterDetail('quick-completion');
  const requests: StartInvestigationRequest[] = [];
  mockIPC((command, args) => {
    if (command === 'start_investigation') {
      requests.push(structuredClone(args?.request as StartInvestigationRequest));
      if (requests.length === 1) throw new Error('Uncertain start acknowledgement');
      return accepted;
    }
    if (command === 'get_investigation') return accepted;
    if (command === 'investigation_result') return specWriterResult(accepted.investigation_id);
    if (command === 'get_investigation_chat' || command === 'investigation_consent') return null;
    if (command === 'investigation_events') { const items = investigationEvents(accepted.investigation_id);
      return { investigation_id: accepted.investigation_id, items, next_sequence: items.length, has_more: false }; }
    throw new Error(`Unexpected command ${command}`);
  });
  const initial = useInvestigationStore.getState();
  const composer = { ...initial.composer, specialistId: 'evidence-auditor@6' as const, limitProfile: 'investigation-v1',
    providerMode: 'cloud' as const, question: 'Keep this separate composer question.', scopeType: 'neighborhood' as const,
    anchor: 'business-rule:global', hops: 2, parent: { conversation_id: 'composer-conversation', parent_id: 'composer-parent' } };
  useInvestigationStore.setState({ catalog: completionCatalog, obligationChoices: null, composer,
    draftObligations: [sourceObligation], draftInspectionRequirements: [inspectionTarget] });
  const quickDraft = { specialist_id: 'spec-writer@1' as const, question: 'Draft a bounded proposal.',
    scope: { type: 'all' as const }, provider_mode: 'local' as const, limit_profile: 'investigation-v1',
    obligations: { schema_version: 1 as const, contract: 'investigation-completion-obligations@1' as const,
      obligations: [inventoryObligation] } };
  const options = { preserveComposer: true, completionChoices };
  await useInvestigationStore.getState().start(quickDraft, options);
  const pending = useInvestigationStore.getState().pendingStart;
  expect(pending).toMatchObject({ schema_version: 4, specialist_id: 'spec-writer@1',
    expected_graph_revision: completionChoices.graph_snapshot_id, expected_context_id: completionChoices.context_id,
    conversation_id: null, parent_id: null, obligations: quickDraft.obligations });
  expect(useInvestigationStore.getState().composer).toEqual(composer);
  await useInvestigationStore.getState().retryStart();
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
  expect(requests[0]).not.toHaveProperty('preserveComposer');
  expect(requests[0]).not.toHaveProperty('completionChoices');
  const final = useInvestigationStore.getState();
  expect(final.selectedId).toBe('quick-completion');
  expect(final.composer).toEqual(composer);
  expect(final.draftObligations).toEqual([sourceObligation]);
  expect(final.draftInspectionRequirements).toEqual([inspectionTarget]);
  expect(final.obligationChoices).toBeNull();
  const ordinary = investigationDetail('ordinary-after-quick', { status: 'queued', has_result: false,
    last_event_sequence: 0, actions: { can_cancel: true, can_follow_up: false } });
  mockIPC((command, args) => {
    if (command === 'start_investigation') return ordinary;
    if (command === 'get_investigation') return args?.investigationId === ordinary.investigation_id ? ordinary : accepted;
    if (command === 'investigation_result' || command === 'get_investigation_chat' || command === 'investigation_consent') return null;
    if (command === 'investigation_events') return { investigation_id: String(args?.investigationId), items: [], next_sequence: 0, has_more: false };
    throw new Error(`Unexpected command ${command}`);
  });
  await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3', question: 'A normal composer start.',
    scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
  expect(useInvestigationStore.getState().composer.question).toBe('');
  expect(useInvestigationStore.getState().composer.parent).toBeNull();
  expect(useInvestigationStore.getState().draftObligations).toEqual([]);
  expect(useInvestigationStore.getState().draftInspectionRequirements).toEqual([]);
});

it('clears quick preservation context after definitive refusal before a normal composer start (AC-0385)', async () => {
  const initial = useInvestigationStore.getState();
  const composer = { ...initial.composer, question: 'Preserve on quick refusal.',
    parent: { conversation_id: 'saved-conversation', parent_id: 'saved-parent' } };
  useInvestigationStore.setState({ composer, catalog: completionCatalog });
  mockIPC((command) => {
    if (command === 'start_investigation') throw { code: 'invalid_input', definitive: true, message: 'Fixture refusal' };
    throw new Error(`Unexpected command ${command}`);
  });
  await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3', question: 'Quick request.',
    scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' }, { preserveComposer: true });
  expect(useInvestigationStore.getState().pendingStart).toBeNull();
  expect(useInvestigationStore.getState().startRejection).toBe('invalid_input');
  expect(useInvestigationStore.getState().composer).toEqual(composer);
  const ordinary = investigationDetail('ordinary-after-refusal', { status: 'queued', has_result: false,
    last_event_sequence: 0, actions: { can_cancel: true, can_follow_up: false } });
  mockIPC((command) => {
    if (command === 'start_investigation' || command === 'get_investigation') return ordinary;
    if (command === 'investigation_result' || command === 'get_investigation_chat' || command === 'investigation_consent') return null;
    if (command === 'investigation_events') return { investigation_id: ordinary.investigation_id, items: [], next_sequence: 0, has_more: false };
    throw new Error(`Unexpected command ${command}`);
  });
  await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3', question: 'Normal start.',
    scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
  expect(useInvestigationStore.getState().startRejection).toBeNull();
  expect(useInvestigationStore.getState().composer.question).toBe('');
  expect(useInvestigationStore.getState().composer.parent).toBeNull();
});

it('keeps quick completion size errors out of the full composer (AC-0385)', async () => {
  let ipcCalls = 0;
  mockIPC(() => { ipcCalls += 1; throw new Error('Oversized quick request must not reach IPC'); });
  const initial = useInvestigationStore.getState();
  const composer = { ...initial.composer, question: 'Full composer draft stays intact.',
    parent: { conversation_id: 'saved-conversation', parent_id: 'saved-parent' } };
  const obligations = [inventoryObligation];
  useInvestigationStore.setState({ catalog: completionCatalog, composer, draftObligations: obligations,
    obligationChoices: null, obligationDraftError: 'Existing full-composer validation message.', startRejection: 'refresh_required' });
  await useInvestigationStore.getState().start({ specialist_id: 'spec-writer@1', question: 'q'.repeat(9000),
    scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1',
    obligations: { schema_version: 1, contract: 'investigation-completion-obligations@1', obligations } },
  { preserveComposer: true, completionChoices });
  const final = useInvestigationStore.getState();
  expect(ipcCalls).toBe(0);
  expect(final.pendingStart).toBeNull();
  expect(final.startRejection).toBeNull();
  expect(final.startError).toContain('complete request exceeds');
  expect(final.obligationDraftError).toBe('Existing full-composer validation message.');
  expect(final.composer).toEqual(composer);
  expect(final.draftObligations).toEqual(obligations);
});

it('refuses expanded roles without an eligible policy (AC-0281)', async () => {
  const start = vi.fn(); mockIPC(start);
  useInvestigationStore.setState({ catalog: { ...expandedCompletionCatalog, policies: [] }, obligationChoices: completionChoices });
  await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@8', limit_profile: 'investigation-v2',
    question: 'Inspect evidence.', scope: { type: 'all' }, provider_mode: 'local', obligations: {
      schema_version: 2, contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  expect(start).not.toHaveBeenCalled();
});


it.each([['domain-analyst@9'], ['evidence-auditor@9']] as const)('serializes the explicitly advertised query-history role %s without changing V2 limits (AC-0286)', async (specialist) => {
  useInvestigationStore.setState({ catalog: queryHistoryCatalog, obligationChoices: completionChoices });
  const starts: unknown[] = [];
  mockIPC((command, args) => { if (command === 'start_investigation') starts.push(args?.request); throw new Error('Uncertain transport'); });
  await useInvestigationStore.getState().start({ specialist_id: specialist, limit_profile: 'investigation-v2', question: 'Inspect evidence.',
    scope: { type: 'all' }, provider_mode: 'local', obligations: { schema_version: 2,
      contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ schema_version: 6, specialist_id: specialist, limit_profile: 'investigation-v2',
    obligations: { schema_version: 2 }, expected_context_id: completionChoices.context_id });
  useInvestigationStore.setState({ catalog: expandedCompletionCatalog });
  await useInvestigationStore.getState().retryStart();
  expect(starts[1]).toEqual(starts[0]);
});

it('does not upgrade an uncertain @8 request when the host catalog moves to @9 (AC-0286)', async () => {
  useInvestigationStore.setState({ catalog: expandedCompletionCatalog, obligationChoices: completionChoices });
  const store = useInvestigationStore.getState();
  store.updateComposer({ specialistId: 'domain-analyst@8', limitProfile: 'investigation-v2' });
  const requests: unknown[] = [];
  mockIPC((command, args) => { if (command === 'investigation_specialists') return queryHistoryCatalog;
    if (command === 'start_investigation') requests.push(structuredClone(args?.request)); throw new Error('Uncertain transport'); });
  await store.start({ specialist_id: 'domain-analyst@8', limit_profile: 'investigation-v2', question: 'Retain original intent.',
    scope: { type: 'all' }, provider_mode: 'local', obligations: { schema_version: 2,
      contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  await store.loadCatalog();
  store.updateComposer({ specialistId: 'domain-analyst@9' });
  await store.retryStart();
  expect(useInvestigationStore.getState().catalog).toEqual(queryHistoryCatalog);
  expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@8');
  expect(requests).toHaveLength(2);
  expect(requests[0]).toMatchObject({ schema_version: 5, specialist_id: 'domain-analyst@8' });
  expect(requests[1]).toEqual(requests[0]);
});

it('opens query-history saved results without a catalog and rejects a self-consistent older family result (AC-0286)', async () => {
  const detail = queryHistoryDetail(); const result = queryHistoryResult();
  mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result :
    command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: [], next_sequence: 0, has_more: false } : null);
  detail.last_event_sequence = 0;
  await useInvestigationStore.getState().open(detail.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().result?.schema_version).toBe(6);
  result.schema_version = 5; result.result_id = result.result_id.replace('-v6:', '-v5:');
  result.disclosure_hash = result.disclosure_hash!.replace('-v5:', '-v4:');
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).not.toBeNull();
});


it('reopens actual durable query-history worker history with its original journal (AC-0286)', async () => {
  const detail = queryHistoryWire.detail as unknown as InvestigationDetail;
  const result = queryHistoryWire.result as unknown as InvestigationResult;
  const events = queryHistoryWire.events as unknown as InvestigationEvent[];
  useInvestigationStore.setState({ catalog: queryHistoryCatalog });
  mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result :
    command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: events,
      next_sequence: events.at(-1)!.sequence, has_more: false } : null);
  await useInvestigationStore.getState().open(detail.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().detail).toEqual(detail);
  expect(useInvestigationStore.getState().result).toEqual(result);
  expect(useInvestigationStore.getState().events).toEqual(events);
});

describe('persisted investigation chat UI binding (AC-0378)', () => {
  function taskTransport(detail: InvestigationDetail, result: InvestigationResult | null,
    chat: InvestigationChat | null | (() => unknown)) {
    mockIPC((command, payload) => {
      const args = payload as { investigationId?: string; afterSequence?: number } | undefined;
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'get_investigation_chat') return typeof chat === 'function' ? chat() : chat;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: detail.investigation_id,
        items: investigationEvents(detail.investigation_id).filter((event) => event.sequence > Number(args?.afterSequence ?? 0)),
        next_sequence: detail.last_event_sequence, has_more: false };
      return null;
    });
  }

  // AC-0420: exact counts are validated before publication, never reconstructed from numeric usage.
  it('validates independent exact saved detail reports without coercion', async () => {
    for (const value of [undefined, null, {}, { input_tokens: '1', output_tokens: null },
      ...['01', '-1', '18446744073709551616', 1].map((input_tokens) => ({ input_tokens, output_tokens: null, cache_read_tokens: '0' }))]) {
      for (const completion of [false, true]) {
        const detail = completion ? specWriterDetail('exact-usage') : investigationDetail('exact-usage');
        detail.reported_usage_exact = value as never;
        const result = completion ? specWriterResult(detail.investigation_id) : investigationResult(detail.investigation_id);
        taskTransport(detail, result, savedChat(detail, result));
        await useInvestigationStore.getState().open(detail.investigation_id);
        expect(useInvestigationStore.getState().error).not.toBeNull();
        expect(useInvestigationStore.getState().detail).toBeNull();
      }
    }
    for (const usage of [undefined, { input_tokens: '18446744073709551615', output_tokens: '0', cache_read_tokens: null }]) {
      const detail = investigationDetail('exact-usage', usage ? { reported_usage_exact: usage } : {});
      const result = investigationResult(detail.investigation_id);
      taskTransport(detail, result, savedChat(detail, result));
      await useInvestigationStore.getState().open(detail.investigation_id);
      expect(useInvestigationStore.getState().error).toBeNull();
      expect(useInvestigationStore.getState().detail?.reported_usage_exact).toEqual(usage);
    }
    const current = specWriterDetail('exact-current');
    current.reported_usage_exact = { input_tokens: '18446744073709551615', output_tokens: null, cache_read_tokens: '0' };
    const currentResult = specWriterResult(current.investigation_id);
    taskTransport(current, currentResult, savedChat(current, currentResult));
    await useInvestigationStore.getState().open(current.investigation_id);
    expect(useInvestigationStore.getState().error).toBeNull();
    expect(useInvestigationStore.getState().detail?.reported_usage_exact).toEqual(current.reported_usage_exact);
  });

  // AC-1423: the runtime a tool-calling run executed on is shown only when well formed.
  it('accepts a well-formed runtime identity and rejects any other', async () => {
    for (const value of [null, {}, { lane: 'codex', runtime: 'cartograph-agent', version: '1', runner: 'pi-agent-core@0.73.1' },
      { lane: 'pi', runtime: 'cartograph-agent', version: '1', runner: '' }, { lane: 'pi', runtime: 'cartograph agent', version: '1', runner: 'pi-agent-core@0.73.1' },
      { lane: 'pi', runtime: 'cartograph-agent', version: '1', runner: 'pi-agent-core@0.73.1', extra: 1 }, { lane: 'pi', runtime: 'cartograph-agent', version: '1' }]) {
      for (const completion of [false, true]) {
        const detail = completion ? specWriterDetail('runtime') : investigationDetail('runtime');
        detail.runtime = value as never;
        const result = completion ? specWriterResult(detail.investigation_id) : investigationResult(detail.investigation_id);
        taskTransport(detail, result, savedChat(detail, result));
        await useInvestigationStore.getState().open(detail.investigation_id);
        expect(useInvestigationStore.getState().error).not.toBeNull();
        expect(useInvestigationStore.getState().detail).toBeNull();
      }
    }
    const runtime = { lane: 'pi' as const, runtime: 'cartograph-agent', version: '1', runner: 'pi-agent-core@0.73.1' };
    for (const completion of [false, true]) {
      const detail = completion ? specWriterDetail('runtime-ok') : investigationDetail('runtime-ok');
      detail.runtime = runtime;
      const result = completion ? specWriterResult(detail.investigation_id) : investigationResult(detail.investigation_id);
      taskTransport(detail, result, savedChat(detail, result));
      await useInvestigationStore.getState().open(detail.investigation_id);
      expect(useInvestigationStore.getState().error).toBeNull();
      expect(useInvestigationStore.getState().detail?.runtime).toEqual(runtime);
    }
  });

  it('loads pending user-authored question and completed saved specialist reference', async () => {
    const pending = investigationDetail('pending-chat-ui', { status: 'running', has_result: false,
      actions: { can_cancel: true, can_follow_up: false } });
    const pendingChat = savedChat(pending, null);
    taskTransport(pending, null, pendingChat);
    await useInvestigationStore.getState().open(pending.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().chatStatus).toBe('saved');
    expect(useInvestigationStore.getState().chat?.messages).toHaveLength(1);
    expect(useInvestigationStore.getState().chat?.messages[0]).toMatchObject({ author: 'user', ordinal: 0,
      message: { kind: 'user_question', question: pending.question } });
    expect(useInvestigationStore.getState().chat?.producer).toMatchObject({
      specialist_id: pending.specialist.id, specialist_version: pending.specialist.version,
      prompt_fingerprint: pending.specialist.prompt_fingerprint, provider_id: pending.provider.provider_id,
      model: pending.provider.model,
    });

    const completed = investigationDetail('completed-chat-ui');
    const result = investigationResult(completed.investigation_id);
    const completedChat = savedChat(completed, result);
    taskTransport(completed, result, completedChat);
    await useInvestigationStore.getState().open(completed.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().chatStatus).toBe('saved');
    expect(useInvestigationStore.getState().chat?.messages[1]).toMatchObject({ author: 'specialist', ordinal: 1,
      message: { kind: 'result_reference', task_id: completed.investigation_id, result_id: result.result_id } });
    expect(useInvestigationStore.getState().result).toEqual(result);
  });

  it('distinguishes legacy absence from read failure and rejects mismatches without hiding findings', async () => {
    const detail = investigationDetail('chat-mismatch');
    const result = investigationResult(detail.investigation_id);
    const mismatched = savedChat(detail, result);
    const reference = mismatched.messages[1];
    if (reference.message.kind === 'result_reference') reference.message.result_id = 'result:another-task';
    taskTransport(detail, result, mismatched);
    await useInvestigationStore.getState().open(detail.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('error'));
    expect(useInvestigationStore.getState().chatStatus).toBe('error');
    expect(useInvestigationStore.getState().chat).toBeNull();
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().error).toBeNull();

    taskTransport(detail, result, null);
    await useInvestigationStore.getState().refreshSelected();
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('legacy'));
    expect(useInvestigationStore.getState().chatStatus).toBe('legacy');
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().error).toBeNull();

    mockIPC((command) => {
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'get_investigation_chat') throw new Error('Host unavailable.');
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: detail.investigation_id,
        items: investigationEvents(detail.investigation_id), next_sequence: detail.last_event_sequence, has_more: false };
      return null;
    });
    await useInvestigationStore.getState().refreshSelected();
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('error'));
    expect(useInvestigationStore.getState().chatStatus).toBe('error');
    expect(useInvestigationStore.getState().chatError).toMatch(/could not be read/);
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('keeps findings visible when chat read straddles result admission, then reconnects on refresh', async () => {
    const detail = investigationDetail('chat-straddled');
    const result = investigationResult(detail.investigation_id);
    const admittedChat = savedChat(detail, result);
    let chat: InvestigationChat = { ...admittedChat, messages: [admittedChat.messages[0]] };
    taskTransport(detail, result, () => chat);
    await useInvestigationStore.getState().open(detail.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('error'));
    expect(useInvestigationStore.getState().chatStatus).toBe('error');
    expect(useInvestigationStore.getState().chatError).toMatch(/did not match this task and result/);
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().error).toBeNull();

    chat = admittedChat;
    await useInvestigationStore.getState().refreshSelected();
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().chatStatus).toBe('saved');
    expect(useInvestigationStore.getState().chat?.messages).toHaveLength(2);
    expect(useInvestigationStore.getState().result).toEqual(result);
  });

  it('publishes valid findings while the independent transcript request is still pending', async () => {
    const detail = investigationDetail('chat-pending-read');
    const result = investigationResult(detail.investigation_id);
    const chat = savedChat(detail, result);
    const delayed = deferred<InvestigationChat>();
    taskTransport(detail, result, () => delayed.promise);

    const opening = useInvestigationStore.getState().open(detail.investigation_id);
    for (let attempt = 0; attempt < 20 && !useInvestigationStore.getState().detail; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    expect(useInvestigationStore.getState().detail).toEqual(detail);
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().chatStatus).toBe('loading');
    expect(useInvestigationStore.getState().error).toBeNull();

    delayed.resolve(chat);
    await opening;
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().result).toEqual(result);
  });

  it('returns confirmed start identity before its optional transcript read completes', async () => {
    const detail = investigationDetail('start-chat-pending', { status: 'running', has_result: false,
      actions: { can_cancel: true, can_follow_up: false } });
    const delayed = deferred<InvestigationChat | null>();
    mockIPC((command, args) => {
      if (command === 'start_investigation' || command === 'get_investigation') return detail;
      if (command === 'investigation_result') return null;
      if (command === 'get_investigation_chat') return delayed.promise;
      return response(command, args);
    });

    const id = await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@3',
      question: 'Inspect the admitted investigation.', scope: { type: 'all' }, provider_mode: 'local',
      limit_profile: 'investigation-v1' });
    expect(id).toBe(detail.investigation_id);
    expect(useInvestigationStore.getState().detail).toEqual(detail);
    expect(useInvestigationStore.getState().chatStatus).toBe('loading');
    expect(useInvestigationStore.getState().error).toBeNull();

    delayed.resolve(savedChat(detail, null));
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().chat?.task_id).toBe(id);
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('rejects saved producer and ordinal mismatches', async () => {
    const detail = investigationDetail('chat-binding-mismatch');
    const result = investigationResult(detail.investigation_id);
    for (const corrupt of [
      (chat: InvestigationChat) => { chat.producer.specialist_version += 1; },
      (chat: InvestigationChat) => { chat.messages[1].ordinal = 2; },
    ]) {
      const chat = savedChat(detail, result);
      corrupt(chat);
      taskTransport(detail, result, chat);
      await useInvestigationStore.getState().open(detail.investigation_id);
      await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('error'));
      expect(useInvestigationStore.getState().chatStatus).toBe('error');
      expect(useInvestigationStore.getState().chat).toBeNull();
      expect(useInvestigationStore.getState().result).toEqual(result);
    }
  });

  it('refreshes the result reference after admission without losing the user turn', async () => {
    let detail = investigationDetail('chat-admitted-later', { status: 'running', has_result: false,
      revision: 10, actions: { can_cancel: true, can_follow_up: false } });
    let result: InvestigationResult | null = null;
    let chat: InvestigationChat = savedChat(detail, null);
    taskTransport(detail, result, () => chat);
    await useInvestigationStore.getState().open(detail.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chat?.messages).toHaveLength(1));
    expect(useInvestigationStore.getState().chat?.messages).toHaveLength(1);

    detail = { ...detail, status: 'completed', has_result: true, revision: 11,
      actions: { can_cancel: false, can_follow_up: true } };
    result = investigationResult(detail.investigation_id);
    chat = savedChat(detail, result);
    taskTransport(detail, result, () => chat);
    await useInvestigationStore.getState().refreshSelected();
    await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    expect(useInvestigationStore.getState().chatStatus).toBe('saved');
    expect(useInvestigationStore.getState().chat?.messages).toHaveLength(2);
    expect(useInvestigationStore.getState().chat?.messages[0].message).toEqual({ kind: 'user_question', question: detail.question });
    expect(useInvestigationStore.getState().chat?.messages[1].message).toMatchObject({ kind: 'result_reference', result_id: result.result_id });
  });

  it('ignores delayed responses after A to B and same-task reopen selections', async () => {
    const a = investigationDetail('chat-a'); const aResult = investigationResult(a.investigation_id);
    const aChat = savedChat(a, aResult);
    const b = investigationDetail('chat-b'); const bResult = investigationResult(b.investigation_id);
    const bChat = savedChat(b, bResult);
    const delayedA = deferred<InvestigationChat>();
    const delayedReopenA = deferred<InvestigationChat>();
    let aReads = 0;
    mockIPC((command, payload) => {
      const args = payload as { investigationId?: string; afterSequence?: number } | undefined;
      const id = args?.investigationId;
      const detail = id === a.investigation_id ? a : b;
      const result = id === a.investigation_id ? aResult : bResult;
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'get_investigation_chat') {
        if (id === a.investigation_id) {
          aReads += 1;
          if (aReads === 1) return delayedA.promise;
          if (aReads === 2) return delayedReopenA.promise;
          return aChat;
        }
        return bChat;
      }
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: detail.investigation_id,
        items: investigationEvents(detail.investigation_id).filter((event) => event.sequence > Number(args?.afterSequence ?? 0)),
        next_sequence: detail.last_event_sequence, has_more: false };
      return null;
    });

    const oldA = useInvestigationStore.getState().open(a.investigation_id);
    await useInvestigationStore.getState().open(b.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chat?.task_id).toBe(b.investigation_id));
    delayedA.resolve(aChat);
    await oldA;
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(useInvestigationStore.getState().selectedId).toBe(b.investigation_id);
    expect(useInvestigationStore.getState().chat?.task_id).toBe(b.investigation_id);

    const oldSameTask = useInvestigationStore.getState().open(a.investigation_id);
    await useInvestigationStore.getState().open(a.investigation_id);
    await vi.waitFor(() => expect(useInvestigationStore.getState().chat?.chat_id).toBe(aChat.chat_id));
    delayedReopenA.resolve({ ...aChat, chat_id: 'chat:reopened' });
    await oldSameTask;
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(useInvestigationStore.getState().selectedId).toBe(a.investigation_id);
    expect(useInvestigationStore.getState().chat?.chat_id).toBe(aChat.chat_id);
  });
});

// AC-0402: match metadata is bounded and owned by the exact selected query and winning row.
// AC-0408: both evidence kinds follow the exact query and winning row.
describe.each(['answer', 'name'] as const)('saved %s history search', (kind) => {
  const wireKey = `${kind}_match_ids`;
  const stateKey = kind === 'answer' ? 'historyAnswerMatchIds' : 'historyNameMatchIds';
  it.each([null, ['a', 'a'], ['foreign'], 42, ...(kind === 'answer' ? [['a']] : [])])('rejects invalid markers %j', async (markers) => {
    const detail = investigationDetail('a', { has_result: false, question: 'Needle metadata' });
    mockIPC(() => ({ items: [detail], next_cursor: null, [wireKey]: markers }));
    await useInvestigationStore.getState().searchHistory('needle');
    expect(useInvestigationStore.getState().historyError).toBeTruthy();
    expect(useInvestigationStore.getState()[stateKey]).toEqual([]);
  });
  it('rejects nonquery markers including ASCII-whitespace-only queries', async () => {
    mockIPC(() => ({ items: [investigationDetail('a')], next_cursor: null, [wireKey]: ['a'] }));
    for (const query of ['', ' \t']) {
      await useInvestigationStore.getState().searchHistory(query);
      expect(useInvestigationStore.getState().historyError).toBeTruthy();
    }
  });
  it('keeps established markers with newer retained rows but never imports evidence into unrelated newer detail', async () => {
    const saved = investigationDetail('a', { question: 'Unrelated saved question', revision: 9 });
    let page = { items: [saved], next_cursor: 'next', [wireKey]: ['a'] };
    mockIPC(() => page);
    await useInvestigationStore.getState().searchHistory('needle');
    page = { items: [investigationDetail('a', { question: 'Needle stale metadata', revision: 8, has_result: false })], next_cursor: 'next', [wireKey]: [] };
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history[0].revision).toBe(9);
    expect(useInvestigationStore.getState()[stateKey]).toEqual(['a']);
    await useInvestigationStore.getState().loadHistory(true);
    expect(useInvestigationStore.getState()[stateKey]).toEqual(['a']);
    useInvestigationStore.setState({ history: [], [stateKey]: [], detail: saved });
    page = { items: [investigationDetail('a', { revision: 8 })], next_cursor: 'next', [wireKey]: ['a'] };
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history).toEqual([]);
    expect(useInvestigationStore.getState()[stateKey]).toEqual([]);
  });
  it('ignores stale query markers and clears them on search/active selection', async () => {
    const old = deferred<unknown>();
    mockIPC((_command, args) => args?.search === 'old' ? old.promise
      : { items: [investigationDetail('a')], next_cursor: null, [wireKey]: ['a'] });
    const pending = useInvestigationStore.getState().searchHistory('old');
    await useInvestigationStore.getState().searchHistory('new');
    old.resolve({ items: [investigationDetail('old')], next_cursor: null, [wireKey]: ['old'] }); await pending;
    expect(useInvestigationStore.getState()[stateKey]).toEqual(['a']);
    await useInvestigationStore.getState().selectHistoryActive(true);
    expect(useInvestigationStore.getState()[stateKey]).toEqual([]);
    await useInvestigationStore.getState().searchHistory('');
    expect(useInvestigationStore.getState()[stateKey]).toEqual([]);
  });
});

// AC-0408: name evidence is not an answer and survives a pending status refresh.
it('retains pending name hits across status and active-filter refresh, rejecting overlap', async () => {
  let pending = investigationDetail('pending-name', { question: 'Unrelated question', has_result: false, status: 'running' });
  mockIPC((command) => command === 'cancel_investigation' ? pending
    : { items: [pending], next_cursor: null, name_match_ids: [pending.investigation_id] });
  await useInvestigationStore.getState().searchHistory('Domain analyst');
  pending = { ...pending, revision: 10 };
  await useInvestigationStore.getState().cancel(pending.investigation_id);
  expect(useInvestigationStore.getState().historyNameMatchIds).toEqual([pending.investigation_id]);
  expect(useInvestigationStore.getState().history[0].has_result).toBe(false);
  await useInvestigationStore.getState().selectHistoryActive(true);
  expect(useInvestigationStore.getState().historyNameMatchIds).toEqual([pending.investigation_id]);
  mockIPC(() => ({ items: [investigationDetail('a')], next_cursor: null, name_match_ids: ['a'], answer_match_ids: ['a'] }));
  await useInvestigationStore.getState().selectHistoryActive(false);
  await useInvestigationStore.getState().searchHistory('overlap');
  expect(useInvestigationStore.getState().historyError).toBeTruthy();
  expect(useInvestigationStore.getState().historyNameMatchIds).toEqual([]);
});

// AC-0409: the palette changes shared scope and query atomically, without a second read.
it('searches all saved history in one request while ordinary searches preserve active scope', async () => {
  const calls: unknown[] = [];
  mockIPC((command, args) => { expect(command).toBe('list_investigations'); calls.push(args); return { items: [], next_cursor: null }; });
  useInvestigationStore.setState({ historyActiveOnly: true, historyNameMatchIds: ['old'], historyAnswerMatchIds: ['old'], nextCursor: 'old' });
  await useInvestigationStore.getState().searchHistory('saved name', { activeOnly: false });
  expect(calls).toEqual([{ search: 'saved name' }]);
  expect(useInvestigationStore.getState()).toMatchObject({ historyActiveOnly: false, historyNameMatchIds: [], historyAnswerMatchIds: [], nextCursor: null });
  useInvestigationStore.setState({ historyActiveOnly: true });
  await useInvestigationStore.getState().searchHistory('ordinary');
  expect(calls[1]).toEqual({ search: 'ordinary', activeOnly: true });
});

describe('remembered budget and specialist (AC-0563, #1636)', () => {
  it('restores the stored explicit choice after a restart when the catalog still offers it', async () => {
    const saved: unknown[] = [];
    let stored: unknown = null;
    mockIPC((command, args) => {
      if (command === 'investigation_specialists') return desktopAnalystCatalog;
      if (command === 'get_investigation_choice') return stored;
      if (command === 'set_investigation_choice') {
        saved.push(args);
        const { specialistId, limitProfile } = args as { specialistId: string; limitProfile: string };
        stored = { specialist_id: specialistId, limit_profile: limitProfile };
        return stored;
      }
      throw new Error('Unexpected command.');
    });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@27', limitProfile: 'investigation-v2' });
    expect(saved).toEqual([]);
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@3', limitProfile: 'investigation-v1' });
    await vi.waitFor(() => expect(saved).toEqual([{ specialistId: 'domain-analyst@3', limitProfile: 'investigation-v1' }]));
    // Restart: fresh store state, same profile.
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@3', limitProfile: 'investigation-v1' });
  });

  it('falls back to the first-run default when the stored pair is no longer offered', () => {
    const composer = useInvestigationStore.getState().composer;
    expect(restoredComposer(composer, desktopAnalystCatalog, { specialist_id: 'domain-analyst@99', limit_profile: 'investigation-v1' }))
      .toMatchObject({ specialistId: 'domain-analyst@27', limitProfile: 'investigation-v2' });
    expect(restoredComposer(composer, desktopAnalystCatalog, null))
      .toMatchObject({ specialistId: 'domain-analyst@27', limitProfile: 'investigation-v2' });
  });

  it('never overrides a choice made in this session', () => {
    const chosen = { ...useInvestigationStore.getState().composer, specialistId: 'domain-analyst@7' as const, limitProfile: 'investigation-v1' };
    expect(restoredComposer(chosen, desktopAnalystCatalog, { specialist_id: 'domain-analyst@3', limit_profile: 'investigation-v1' })).toBeNull();
  });
});

describe('desktop default specialist (AC-0574)', () => {
  it('adopts schema-backed @27 under Expanded when the host advertises it', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return desktopAnalystCatalog; throw new Error('Unexpected command.'); });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@27', limitProfile: 'investigation-v2' });
  });

  it('keeps @3 and Standard when @27 or its policy is not advertised', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return admissionCatalog; throw new Error('Unexpected command.'); });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@3');
    expect(useInvestigationStore.getState().composer.limitProfile).toBeUndefined();
    const noPolicy = { ...desktopAnalystCatalog, policies: desktopAnalystCatalog.policies!.map((policy) =>
      ({ ...policy, specialist_ids: policy.specialist_ids.filter((id) => id !== 'domain-analyst@27') })) };
    expect(desktopDefaultComposer(useInvestigationStore.getState().composer, noPolicy)).toBeNull();
  });

  it('never overrides an explicit choice or a captured parent on a later catalog refresh', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return desktopAnalystCatalog; throw new Error('Unexpected command.'); });
    useInvestigationStore.getState().updateComposer({ limitProfile: 'investigation-v1' });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@3', limitProfile: 'investigation-v1' });
    const base = { ...useInvestigationStore.getState().composer, limitProfile: undefined };
    expect(desktopDefaultComposer({ ...base, specialistId: 'domain-analyst@7' }, desktopAnalystCatalog)).toBeNull();
    expect(desktopDefaultComposer({ ...base, parent: { conversation_id: 'c', parent_id: 'p' } }, desktopAnalystCatalog)).toBeNull();
  });

  it('keeps an explicitly reselected @3 across a refresh that advertises @27', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return desktopAnalystCatalog; throw new Error('Unexpected command.'); });
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@7' });
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@3' });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@3', limitProfile: 'investigation-v1' });
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    useInvestigationStore.getState().updateComposer({ specialistId: 'domain-analyst@3' });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@3');
  });
});

describe('correction specialist compatibility (AC-0565)', () => {
  it('preserves an explicitly restored historical analyst when @27 is available', () => {
    const composer = useInvestigationStore.getState().composer;
    expect(restoredComposer(composer, desktopAnalystCatalog, {
      specialist_id: 'domain-analyst@18', limit_profile: 'investigation-v2',
    })).toMatchObject({ specialistId: 'domain-analyst@18', limitProfile: 'investigation-v2' });
  });
});

// AC-0566: changing the first-run default never rewrites a saved historical role.
it('preserves explicitly restored analyst19 with typed correction available', () => {
  expect(restoredComposer(useInvestigationStore.getState().composer, desktopAnalystCatalog, {
    specialist_id: 'domain-analyst@19', limit_profile: 'investigation-v2',
  })).toMatchObject({ specialistId: 'domain-analyst@19', limitProfile: 'investigation-v2' });
});

// AC-1402 (#1751, ADR-0071): the user picks a role and it runs the role's current version.
describe('role-based specialist choice (AC-1402)', () => {
  const withCurrent = { ...desktopAnalystCatalog,
    specialists: [...desktopAnalystCatalog.specialists, { ...desktopAnalystCatalog.specialists.find((item) => item.id === 'domain-analyst@27')!,
      id: 'domain-analyst@28' as const, version: 28, prompt_fingerprint: 'fixture-domain-analyst@28' }],
    policies: desktopAnalystCatalog.policies!.map((policy) => ({ ...policy, specialist_ids: [...policy.specialist_ids, 'domain-analyst@28' as const] })) };

  it('gives a programmatic role selection the profile its version runs under', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return withCurrent; throw new Error('Unexpected command.'); });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer.limitProfile).toBe('investigation-v2');
    // The Workbench's Draft this section path names only the specialist.
    useInvestigationStore.getState().updateComposer({ specialistId: 'section-writer@1', question: 'Draft it.' });
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'section-writer@1', limitProfile: 'investigation-v1' });
  });

  it('defaults to the current analyst when the host advertises it', async () => {
    mockIPC((command) => { if (command === 'investigation_specialists') return withCurrent; throw new Error('Unexpected command.'); });
    await useInvestigationStore.getState().loadCatalog();
    expect(useInvestigationStore.getState().composer).toMatchObject({ specialistId: 'domain-analyst@28', limitProfile: 'investigation-v2' });
  });

  it('offers each role once, at its current or newest offered version, with the profile that version runs under', async () => {
    const { currentVersion, offeredRoles, offeredVersions, profileFor } = await import('./specialistRoles');
    expect(offeredRoles(withCurrent)).toContain('domain-analyst');
    expect(currentVersion(withCurrent, 'domain-analyst')?.id).toBe('domain-analyst@28');
    expect(currentVersion(desktopAnalystCatalog, 'domain-analyst')?.id).toBe('domain-analyst@27');
    expect(offeredVersions(withCurrent, 'domain-analyst').map((item) => item.version)).toEqual(
      [...offeredVersions(withCurrent, 'domain-analyst').map((item) => item.version)].sort((left, right) => right - left));
    expect(profileFor(withCurrent, 'domain-analyst@28')).toBe('investigation-v2');
    expect(profileFor(withCurrent, 'domain-analyst@3')).toBe('investigation-v1');
    expect(profileFor(withCurrent, 'spec-writer@2')).toBe('investigation-v1');
  });
});
