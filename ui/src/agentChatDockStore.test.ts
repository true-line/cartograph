import { describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { create } from 'zustand';
import { createAgentChatDockStore } from './agentChatDockStore';
import { useInvestigationStore, type InvestigationState } from './investigationStore';
import { investigationCatalog, investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult } from './investigationFixtures';
import { queryHistoryDetail, queryHistoryResult } from './completionFixtures';
import type { InvestigationChat, InvestigationResult } from './investigationTypes';

function chat(id: string): InvestigationChat {
  const detail = investigationDetail(id);
  return { chat_id: `real-chat:${id}`, task_id: id,
    producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
      prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
      confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
      provider_id: detail.provider.provider_id, model: detail.provider.model, protocol_version: null },
    messages: [{ id: `question:${id}`, ordinal: 0, author: 'user',
      message: { kind: 'user_question', question: detail.question } }] };
}
function completedState(id: string): Partial<InvestigationState> {
  const result = investigationResult(id);
  const saved = chat(id);
  saved.messages.push({ id: `answer:${id}`, ordinal: 1, author: 'specialist',
    message: { kind: 'result_reference', task_id: id, result_id: result.result_id, result_hash: 'e'.repeat(64) } });
  return { detail: investigationDetail(id), result, chat: saved, chatStatus: 'saved' };
}
const tick = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };
function harness(selectedId: string | null = null, autoConnect = true) {
  const calls: string[] = [];
  let release: (() => void) | undefined;
  let hold = false;
  const coordinator = create<InvestigationState>(() => ({ ...useInvestigationStore.getInitialState(), selectedId,
    open: async (id) => {
      calls.push(id);
      coordinator.setState((state) => ({ selectedId: id, selectionGeneration: state.selectionGeneration + 1,
        chat: null, chatStatus: 'loading', detail: null }));
      if (hold) await new Promise<void>((done) => { release = done; });
    } }));
  const dock = createAgentChatDockStore(coordinator);
  let leave = () => {};
  const connect = () => { leave = dock.getState().connect(); };
  if (autoConnect) connect();
  const finish = (id: string) => coordinator.setState({ detail: investigationDetail(id), chat: chat(id), chatStatus: 'saved' });
  const open = async (id: string) => { dock.getState().open(id); await tick(); finish(id); await tick(); };
  return { coordinator, dock, calls, leave: () => leave(), connect, finish, open,
    hold: () => { hold = true; }, release: () => { hold = false; release?.(); } };
}

function delayedCloudChats() {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  const pending: { id: string; resolve: (value: InvestigationChat) => void }[] = [];
  mockIPC((command, payload) => {
    const { investigationId: id = '', afterSequence = 0 } = (payload ?? {}) as { investigationId?: string; afterSequence?: number };
    if (command === 'get_investigation') return investigationDetail(id, { has_result: false,
      status: 'awaiting_consent', revision: 4, last_event_sequence: 4, provider_mode: 'cloud', provider: investigationCatalog.providers[1] });
    if (command === 'investigation_consent') return investigationConsent(id);
    if (command === 'investigation_events') return { investigation_id: id,
      items: investigationEvents(id).slice(0, 4).filter((event) => event.sequence > afterSequence), next_sequence: 4, has_more: false };
    if (command === 'get_investigation_chat') return new Promise<InvestigationChat>((resolve) => pending.push({ id, resolve }));
    return null;
  });
  const resolve = (index: number) => {
    const saved = chat(pending[index].id);
    saved.producer = { ...saved.producer, provider_mode: 'cloud', provider_id: 'azure-openai', model: 'fixture-cloud-model' };
    pending[index].resolve(saved);
  };
  return { pending, resolve, cleanup: () => {
    pending.forEach((_, index) => resolve(index)); clearMocks(); vi.unstubAllGlobals();
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  } };
}

// AC-0389: snapshots are admitted only after the coordinator validates the chat.
describe('saved task chat docks', () => {
  // AC-0396: navigation metadata belongs to the validated snapshot, not global selection.
  it('captures the matching conversation and parent without following unrelated detail', async () => {
    const h = harness();
    h.dock.getState().open('child'); await tick();
    const detail = investigationDetail('child', { conversation_id: 'family-a', parent_id: 'parent-a' });
    h.coordinator.setState({ detail, chat: chat('child'), chatStatus: 'saved' }); await tick();
    const captured = h.dock.getState().docks[0].turn;
    expect(captured).toMatchObject({ investigation_id: 'child', conversation_id: 'family-a', parent_id: 'parent-a' });
    h.coordinator.setState({ selectedId: 'unrelated', detail: investigationDetail('unrelated') });
    expect(h.dock.getState().docks[0].turn).toEqual(captured);
    h.leave();
  });

  it('hands one replaceable notice intent to the next connection and cancels an abandoned handoff', async () => {
    const h = harness('already-selected', false);
    h.dock.getState().requestOpenFromNotice('first');
    h.dock.getState().requestOpenFromNotice('chosen');
    expect(h.calls).toEqual([]);
    h.connect(); await tick();
    expect(h.calls).toEqual(['chosen']);
    h.finish('chosen'); await tick();
    expect(h.dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['chosen']);
    h.leave();

    const abandoned = harness(null, false);
    abandoned.dock.getState().requestOpenFromNotice('never-open');
    abandoned.dock.getState().cancelNavigationIntent();
    abandoned.connect(); await tick();
    expect(abandoned.calls).toEqual([]);
    expect(abandoned.dock.getState().docks).toEqual([]);
    abandoned.leave();
  });

  it('focuses an existing connected dock from a notice without changing its age', async () => {
    const h = harness();
    await h.open('older');
    expect(h.dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['older']);
    await h.open('newer');
    expect(h.dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['older', 'newer']);
    h.dock.getState().requestOpenFromNotice('older'); await tick();
    expect(h.calls).toEqual(['older', 'newer', 'older']);
    h.finish('older'); await tick();
    expect(h.dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['older', 'newer']);
    expect(h.dock.getState().focusedTask).toBe('older');
    h.leave();
  });

  it('deduplicates without changing age, evicts the oldest valid dock, and reopens closed chats', async () => {
    const h = harness();
    for (const id of ['a', 'b', 'c', 'd', 'b', 'e']) await h.open(id);
    expect(h.dock.getState().docks.map((dock) => dock.chat.chat_id)).toEqual(['real-chat:b', 'real-chat:c', 'real-chat:d', 'real-chat:e']);
    h.dock.getState().close('c');
    await h.open('c');
    expect(h.dock.getState().docks.map((dock) => dock.chat.task_id)).toEqual(['b', 'd', 'e', 'c']);
    expect(h.dock.getState().docks[0].chat.producer).toEqual(chat('b').producer);
    h.leave();
  });

  it('does not capture detail-ready/chat-pending or a wrong task transcript', async () => {
    const h = harness();
    h.dock.getState().open('a'); await tick();
    h.coordinator.setState({ detail: investigationDetail('a') });
    expect(h.dock.getState().docks).toEqual([]);
    h.coordinator.setState({ chat: chat('wrong'), chatStatus: 'saved' });
    expect(h.dock.getState().docks).toEqual([]);
    expect(h.dock.getState().error).toBeTruthy();
    await h.open('a');
    expect(h.dock.getState().docks[0].chat.chat_id).toBe('real-chat:a');
    h.leave();
  });

  it.each(['legacy', 'error', 'preview'] as const)('keeps four docks on %s and allows retry', async (status) => {
    const h = harness();
    for (const id of ['a', 'b', 'c', 'd']) await h.open(id);
    h.dock.getState().open('e'); await tick();
    h.coordinator.setState({ chatStatus: status, detail: investigationDetail('e'), chat: null });
    expect(h.dock.getState().docks.map((dock) => dock.chat.task_id)).toEqual(['a', 'b', 'c', 'd']);
    expect(h.dock.getState().fallbackTask).toBe(status === 'legacy' ? 'e' : null);
    await h.open('e');
    expect(h.dock.getState().docks.at(-1)?.chat.task_id).toBe('e');
    h.leave();
  });

  it('serializes opens and coalesces superseded requests, ignoring late A', async () => {
    const h = harness(); h.hold();
    h.dock.getState().open('a'); await tick();
    for (let index = 0; index < 100; index += 1) h.dock.getState().open(`superseded-${index}`);
    h.dock.getState().open('c');
    h.finish('a');
    expect(h.dock.getState().docks).toEqual([]);
    expect(h.calls).toEqual(['a']);
    h.release(); await tick(); await tick();
    expect(h.calls).toEqual(['a', 'c']);
    h.finish('c');
    expect(h.dock.getState().docks.map((dock) => dock.chat.task_id)).toEqual(['c']);
    expect(h.coordinator.getState().selectedId).toBe('c');
    h.leave();
  });

  it.each(['close', 'leave', 'external-selection'] as const)('cancels capture on %s', async (action) => {
    const h = harness(); h.hold();
    h.dock.getState().open('a'); await tick();
    if (action === 'close') h.dock.getState().close('a');
    if (action === 'leave') { h.dock.getState().open('queued'); h.leave(); }
    if (action === 'external-selection') {
      h.dock.getState().open('queued');
      h.coordinator.setState({ selectedId: 'other', selectionGeneration: 2 });
    }
    h.finish('a'); h.release(); await tick(); await tick();
    expect(h.dock.getState().docks).toEqual([]);
    expect(h.calls).toEqual(['a']);
    expect(h.dock.getState().pendingTask).toBeNull();
    h.leave();
  });

  it('does not redock a closed coordinator selection on reconnect, but explicit open can restore it', async () => {
    const h = harness('a'); await tick(); h.finish('a'); await tick();
    expect(h.dock.getState().docks[0].chat.chat_id).toBe('real-chat:a');
    h.dock.getState().close('a');
    expect(h.coordinator.getState().selectedId).toBe('a');
    h.leave(); const leaveAgain = h.dock.getState().connect(); await tick();
    expect(h.calls).toEqual(['a']);
    expect(h.dock.getState().docks).toEqual([]);
    await h.open('a');
    expect(h.calls).toEqual(['a', 'a']);
    expect(h.dock.getState().docks[0].chat.chat_id).toBe('real-chat:a');
    leaveAgain();
  });

  it.each([false, true])('serializes actual chat IPCs through settlement, detail failure=%s', async (detailFailed) => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    const reads: string[] = [];
    const pending = new Map<string, (value: InvestigationChat) => void>();
    mockIPC((command, payload) => {
      const id = (payload as { investigationId?: string })?.investigationId ?? '';
      if (command === 'get_investigation') return id === 'a' && detailFailed ? null : investigationDetail(id, { has_result: false,
        status: 'running', revision: 4, last_event_sequence: 4, invocation_pending: true });
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).slice(0, 4), next_sequence: 4, has_more: false };
      if (command === 'get_investigation_chat') {
        reads.push(id);
        return new Promise<InvestigationChat>((resolve) => pending.set(id, resolve));
      }
      return null;
    });
    const dock = createAgentChatDockStore(); const leave = dock.getState().connect();
    try {
      dock.getState().open('a');
      await vi.waitFor(() => expect(detailFailed ? useInvestigationStore.getState().error
        : useInvestigationStore.getState().detail?.investigation_id).toBeTruthy());
      // App poll/focus and invalidation all use this loading guard. A failed
      // detail read must retain it too, until its physical transcript settles.
      expect(useInvestigationStore.getState().loading).toBe(true);
      useInvestigationStore.getState().invalidate({ investigation_id: 'a', revision: 5, last_event_sequence: 5 });
      dock.getState().open('b'); dock.getState().open('c');
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(reads).toEqual(['a']);
      pending.get('a')!(chat('a'));
      await vi.waitFor(() => expect(reads).toEqual(['a', 'c']));
      pending.get('c')!(chat('c'));
      await vi.waitFor(() => expect(dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['c']));
      await vi.waitFor(() => expect(useInvestigationStore.getState().loading).toBe(false));
    } finally {
      leave(); pending.forEach((resolve, id) => resolve(chat(id)));
      clearMocks(); vi.unstubAllGlobals();
      useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    }
  });

  it('captures a follow-up only from the settled physical coordinator read, not a stale saved selection', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    const id = 'follow-up-parent';
    const detail = investigationDetail(id);
    const result = investigationResult(id);
    const saved: InvestigationChat = { chat_id: `chat:${id}`, task_id: id,
      producer: { specialist_id: detail.specialist.id, specialist_version: detail.specialist.version,
        prompt_fingerprint: detail.specialist.prompt_fingerprint, tier: detail.specialist.tier,
        confidence_tier: detail.specialist.confidence_tier, provider_mode: detail.provider.mode,
        provider_id: detail.provider.provider_id, model: detail.provider.model, protocol_version: null },
      messages: [
        { id: `question:${id}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question: detail.question } },
        { id: `result:${id}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference',
          task_id: id, result_id: result.result_id, result_hash: 'c'.repeat(64) } },
      ] };
    let finishChat: ((value: InvestigationChat) => void) | undefined;
    let chatReads = 0;
    mockIPC((command, payload) => {
      const args = payload as { investigationId?: string; afterSequence?: number } | undefined;
      if (command === 'get_investigation') return detail;
      if (command === 'investigation_result') return result;
      if (command === 'investigation_consent') return null;
      if (command === 'investigation_events') return { investigation_id: id,
        items: investigationEvents(id).filter((event) => event.sequence > (args?.afterSequence ?? 0)),
        next_sequence: detail.last_event_sequence, has_more: false };
      if (command === 'get_investigation_chat') {
        chatReads += 1;
        return new Promise<InvestigationChat>((resolve) => { finishChat = resolve; });
      }
      return null;
    });
    const dock = createAgentChatDockStore();
    const leave = dock.getState().connect();
    // A previously validated saved selection must not satisfy the new intent.
    useInvestigationStore.setState({ selectedId: id, selectionGeneration: 8, detail, result,
      chat: saved, chatStatus: 'saved', loading: false });
    try {
      dock.getState().open(id);
      const capture = dock.getState().openForFollowUp(id);
      let resolved = false;
      void capture.then(() => { resolved = true; });
      await vi.waitFor(() => expect(chatReads).toBe(1));
      expect(useInvestigationStore.getState().loading).toBe(true);
      expect(resolved).toBe(false);
      finishChat?.(saved);
      const captured = await capture;
      expect(captured?.selectedId).toBe(id);
      expect(captured?.chatStatus).toBe('saved');
      expect(captured?.loading).toBe(false);
      expect(captured?.chat?.messages[1]).toMatchObject({ message: { kind: 'result_reference', result_id: result.result_id } });
      expect(chatReads).toBe(1);
    } finally {
      leave(); finishChat?.(saved); clearMocks(); vi.unstubAllGlobals();
      useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
    }
  });

  it('cancels parent capture when its read is closed or superseded', async () => {
    const closed = harness(); closed.hold();
    closed.dock.getState().open('parent'); await tick();
    const closedCapture = closed.dock.getState().openForFollowUp('parent');
    closed.dock.getState().close('parent');
    expect(await closedCapture).toBeNull();
    closed.finish('parent'); closed.release(); await tick(); await tick();
    expect(closed.dock.getState().docks).toEqual([]);
    closed.leave();

    const superseded = harness(); superseded.hold();
    superseded.dock.getState().open('parent'); await tick();
    const staleCapture = superseded.dock.getState().openForFollowUp('parent');
    superseded.dock.getState().open('other');
    expect(await staleCapture).toBeNull();
    superseded.finish('parent'); superseded.release(); await tick(); await tick();
    expect(superseded.calls).toEqual(['parent', 'other']);
    superseded.finish('other'); await tick();
    expect(superseded.dock.getState().docks.map((item) => item.chat.task_id)).toEqual(['other']);
    superseded.leave();
  });

  it.each(['close', 'leave'] as const)('releases UI loading on %s while retaining the physical docking read slot', async (action) => {
    const transport = delayedCloudChats();
    const dock = createAgentChatDockStore(); const leave = dock.getState().connect();
    try {
      dock.getState().open('a');
      await vi.waitFor(() => expect(useInvestigationStore.getState().consent?.investigation_id).toBe('a'));
      expect(useInvestigationStore.getState().loading).toBe(true);
      if (action === 'close') dock.getState().close('a'); else leave();
      expect(useInvestigationStore.getState().loading).toBe(false);
      useInvestigationStore.getState().showConsent();
      expect(useInvestigationStore.getState().consentOpen).toBe(true);
      useInvestigationStore.getState().hideConsent();
      // A queued/reconnected dock cannot replace the unresolved physical read.
      const reconnect = action === 'leave' ? dock.getState().connect() : () => {};
      dock.getState().open('queued'); await tick();
      expect(transport.pending.map((read) => read.id)).toEqual(['a']);
      dock.getState().close('queued'); reconnect();
      // Ordinary Investigations refresh remains usable while old chat I/O stalls.
      await useInvestigationStore.getState().refreshSelected();
      expect(transport.pending.map((read) => read.id)).toEqual(['a', 'a']);
      expect(useInvestigationStore.getState().loading).toBe(false);
      transport.resolve(0); await new Promise((resolve) => setTimeout(resolve, 0));
      expect(dock.getState().docks).toEqual([]);
      expect(useInvestigationStore.getState().chatStatus).toBe('loading');
      transport.resolve(1);
      await vi.waitFor(() => expect(useInvestigationStore.getState().chatStatus).toBe('saved'));
    } finally { leave(); transport.cleanup(); }
  });

  it('does not let aborted old ownership or late cleanup clear a newer read loading state', async () => {
    const transport = delayedCloudChats(); const owner = new AbortController();
    try {
      const oldRead = useInvestigationStore.getState().open('a', { awaitChat: true, loadingSignal: owner.signal });
      await vi.waitFor(() => expect(useInvestigationStore.getState().detail?.investigation_id).toBe('a'));
      const newRead = useInvestigationStore.getState().open('b', { awaitChat: true });
      await vi.waitFor(() => expect(useInvestigationStore.getState().detail?.investigation_id).toBe('b'));
      owner.abort(); expect(useInvestigationStore.getState().loading).toBe(true);
      transport.resolve(0); await oldRead;
      expect(useInvestigationStore.getState().loading).toBe(true);
      expect(useInvestigationStore.getState().selectedId).toBe('b');
      transport.resolve(1); await newRead;
      expect(useInvestigationStore.getState().loading).toBe(false);
      await useInvestigationStore.getState().open('ignored', { awaitChat: true, loadingSignal: owner.signal });
      expect(useInvestigationStore.getState().selectedId).toBe('b');
      expect(transport.pending).toHaveLength(2);
    } finally { transport.cleanup(); }
  });

  it('reconnects after effect cleanup and retains snapshots without implying live updates', async () => {
    const h = harness(); await h.open('a'); h.leave();
    const leaveAgain = h.dock.getState().connect(); await tick();
    expect(h.calls).toEqual(['a', 'a']);
    h.finish('a');
    h.coordinator.setState({ chat: null, chatStatus: 'loading' });
    expect(h.dock.getState().docks[0].chat.chat_id).toBe('real-chat:a');
    leaveAgain();
  });
});

// AC-0397: the adapter trusts existing schema validation, never imposes a new finding cap.
describe('saved task chat answers and citation ownership', () => {
  it('retains only the referenced citations and all already-admitted expanded findings', async () => {
    const h = harness();
    h.dock.getState().open('expanded'); await tick();
    const result = queryHistoryResult('expanded');
    result.findings = Array.from({ length: 56 }, (_, index) => ({ ...investigationResult().findings[0],
      finding_id: `F${index}`, citation_ids: ['C1'] }));
    const state = completedState('expanded');
    state.result = result; state.detail = queryHistoryDetail('expanded');
    state.chat!.messages[1].message = { kind: 'result_reference', task_id: 'expanded', result_id: result.result_id, result_hash: 'e'.repeat(64) };
    h.coordinator.setState(state); await tick();
    const answer = h.dock.getState().docks[0].answer!;
    expect(Object.keys(answer)).toEqual(['result', 'citations']);
    expect(answer.result).toBe(result);
    expect(answer.result.findings).toHaveLength(56);
    expect(answer.citations.map((item) => item.citation_id)).toEqual(['C1']);
    h.leave();
  });

  it.each(['wrong-task', 'wrong-result', 'pending'] as const)('does not admit an inconsistent transcript for %s', async (failure) => {
    const h = harness(); h.dock.getState().open('a'); await tick();
    const state = completedState('a');
    if (failure === 'pending') state.chat!.messages.pop();
    else state.chat!.messages[1].message = { kind: 'result_reference', task_id: failure === 'wrong-task' ? 'b' : 'a',
      result_id: failure === 'wrong-result' ? 'other-result' : state.result!.result_id, result_hash: 'e'.repeat(64) };
    h.coordinator.setState(state); await tick();
    expect(h.dock.getState().docks).toEqual([]);
    expect(h.dock.getState().error).toBeTruthy(); h.leave();
  });

  it('does not resurrect a dismissed citation while its task refresh is pending', async () => {
    const h = harness(); h.dock.getState().open('a'); await tick();
    h.coordinator.setState(completedState('a')); await tick();
    const read = vi.fn(); h.coordinator.setState({ readCitation: read }); h.hold();
    const pending = h.dock.getState().readCitation('a', 'C1'); await tick();
    h.dock.getState().clearCitation();
    h.coordinator.setState(completedState('a')); h.release(); await pending;
    expect(h.dock.getState().citation).toBeNull(); expect(read).not.toHaveBeenCalled(); h.leave();
  });

  it('reports an unavailable inspection for a nonnull foreign citation response', async () => {
    const h = harness(); h.dock.getState().open('a'); await tick();
    h.coordinator.setState(completedState('a')); await tick();
    h.coordinator.setState({ readCitation: async () => { h.coordinator.setState({ citationRead: {
      investigation_id: 'b', citation_id: 'C1', citation: investigationCitations[0], status: 'available', text: 'Foreign source' } }); } });
    const pending = h.dock.getState().readCitation('a', 'C1'); await tick();
    h.coordinator.setState(completedState('a')); await pending;
    expect(h.dock.getState().citation).toMatchObject({ read: null, loading: false, error: 'Historical evidence could not be inspected.' });
    h.leave();
  });

  it.each(['close', 'leave', 'selection'] as const)('clears a pending inspector on %s and ignores late evidence', async (action) => {
    const h = harness(); h.dock.getState().open('a'); await tick();
    h.coordinator.setState(completedState('a')); await tick();
    let release!: () => void;
    h.coordinator.setState({ readCitation: async () => { await new Promise<void>((resolve) => { release = resolve; });
      h.coordinator.setState({ citationRead: { investigation_id: 'a', citation_id: 'C1', citation: investigationCitations[0],
        status: 'available', text: 'Old retained source' } }); } });
    const pending = h.dock.getState().readCitation('a', 'C1'); await tick();
    h.coordinator.setState(completedState('a')); await tick();
    expect(h.dock.getState().citation?.loading).toBe(true);
    if (action === 'close') h.dock.getState().close('a');
    else if (action === 'leave') h.leave();
    else h.coordinator.setState((state) => ({ selectedId: 'b', selectionGeneration: state.selectionGeneration + 1 }));
    expect(h.dock.getState().citation).toBeNull();
    release(); await pending; expect(h.dock.getState().citation).toBeNull(); h.leave();
  });
});

/** Real coordinator transport: only the task-chat and historical citation replies are deferred. */
function ownedCitationTransport() {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  const pending: { resolve: (value: unknown) => void }[] = [];
  const chatPending: (() => void)[] = [];
  let holdChat = false; let chatReads = 0;
  mockIPC((command, payload) => {
    const args = (payload ?? {}) as { investigationId?: string; afterSequence?: number };
    const state = completedState(args.investigationId ?? 'a');
    if (command === 'get_investigation') return state.detail;
    if (command === 'investigation_result') return state.result;
    if (command === 'get_investigation_chat') {
      ++chatReads;
      if (holdChat) return new Promise((resolve) => chatPending.push(() => resolve(state.chat)));
      return state.chat;
    }
    if (command === 'investigation_events') return { investigation_id: args.investigationId,
      items: investigationEvents(args.investigationId).filter((event) => event.sequence > (args.afterSequence ?? 0)), next_sequence: 9, has_more: false };
    if (command === 'read_investigation_citation') return new Promise((resolve) => pending.push({ resolve }));
    return null;
  });
  const dock = createAgentChatDockStore(); const leave = dock.getState().connect();
  const reply = (index: number) => pending[index].resolve({ investigation_id: 'a', citation_id: 'C1',
    citation: investigationCitations[0], status: 'available', text: `Retained source ${index}` });
  return { dock, pending, reply, leave, chatReads: () => chatReads,
    holdChat: () => { holdChat = true; }, releaseChat: () => { holdChat = false; chatPending.splice(0).forEach((resolve) => resolve()); },
    cleanup: () => { leave(); chatPending.splice(0).forEach((resolve) => resolve()); pending.forEach((_, index) => reply(index));
      clearMocks(); vi.unstubAllGlobals(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); } };
}

// AC-0397: independent consumers share one physical read; dismissal owns its private historical read only.
describe('real coordinator citation and follow-up ownership', () => {
  it.each(['follow-up-first', 'citation-first'] as const)('joins both consumers without cancellation: %s', async (order) => {
    const h = ownedCitationTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      h.holdChat();
      let followUp: Promise<InvestigationState | null>; let citation: Promise<void>;
      if (order === 'follow-up-first') { followUp = h.dock.getState().openForFollowUp('a'); citation = h.dock.getState().readCitation('a', 'C1'); }
      else { citation = h.dock.getState().readCitation('a', 'C1'); followUp = h.dock.getState().openForFollowUp('a'); }
      await vi.waitFor(() => expect(h.chatReads()).toBe(2));
      h.releaseChat();
      expect((await followUp)?.detail?.investigation_id).toBe('a');
      await vi.waitFor(() => expect(h.pending).toHaveLength(1));
      h.reply(0); await citation;
      expect(h.chatReads()).toBe(2);
      expect(h.dock.getState().citation?.read?.investigation_id).toBe('a');
    } finally { h.cleanup(); }
  });

  it('releases both waiting consumers when their pending task closes', async () => {
    const h = ownedCitationTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      h.holdChat(); const followUp = h.dock.getState().openForFollowUp('a');
      const citation = h.dock.getState().readCitation('a', 'C1');
      h.dock.getState().close('a');
      expect(await followUp).toBeNull(); await citation;
      expect(h.pending).toHaveLength(0); expect(h.dock.getState().citation).toBeNull(); h.releaseChat();
    } finally { h.cleanup(); }
  });

  it.each(['dismiss', 'close', 'leave', 'completed'] as const)('clears its shared coordinator inspection on %s', async (action) => {
    const h = ownedCitationTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      const reading = h.dock.getState().readCitation('a', 'C1');
      await vi.waitFor(() => expect(h.pending).toHaveLength(1));
      if (action === 'completed') { h.reply(0); await reading; expect(useInvestigationStore.getState().citationRead).not.toBeNull(); }
      if (action === 'close') h.dock.getState().close('a');
      else if (action === 'leave') h.leave();
      else h.dock.getState().clearCitation();
      expect(useInvestigationStore.getState()).toMatchObject({ citationId: null, citationRead: null, citationLoading: false });
      if (action !== 'completed') { h.reply(0); await reading; }
      expect(useInvestigationStore.getState()).toMatchObject({ citationId: null, citationRead: null, citationLoading: false });
    } finally { h.cleanup(); }
  });

  it('preserves a newer shared read of the same task and citation when the old dock owner aborts', async () => {
    const h = ownedCitationTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      const oldRead = h.dock.getState().readCitation('a', 'C1');
      await vi.waitFor(() => expect(h.pending).toHaveLength(1));
      const newRead = useInvestigationStore.getState().readCitation('C1');
      await vi.waitFor(() => expect(h.pending).toHaveLength(2));
      h.dock.getState().clearCitation();
      expect(useInvestigationStore.getState()).toMatchObject({ citationId: 'C1', citationLoading: true });
      h.reply(0); await oldRead;
      expect(useInvestigationStore.getState()).toMatchObject({ citationRead: null, citationLoading: true });
      h.reply(1); await newRead;
      expect(useInvestigationStore.getState().citationRead?.text).toBe('Retained source 1');
      const aborted = new AbortController(); aborted.abort();
      await useInvestigationStore.getState().readCitation('C1', { signal: aborted.signal });
      expect(h.pending).toHaveLength(2);
      expect(useInvestigationStore.getState().citationRead?.text).toBe('Retained source 1');
    } finally { h.cleanup(); }
  });
});

/** AC-0401: real coordinator refreshes; docking observes but never initiates these reads. */
function progressTransport() {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
  let value = { detail: investigationDetail('a', { has_result: false, status: 'running', revision: 4, last_event_sequence: 4 }),
    result: null as InvestigationResult | null, chat: chat('a') as InvestigationChat | null };
  const calls: string[] = []; const pending: (() => void)[] = []; let hold = false;
  mockIPC((command, payload) => {
    const { investigationId: id = '', afterSequence = 0 } = (payload ?? {}) as { investigationId?: string; afterSequence?: number };
    calls.push(`${command}:${id}`);
    const saved = id === 'a' ? value : completedState(id);
    if (command === 'get_investigation') return saved.detail;
    if (command === 'investigation_result') return saved.result;
    if (command === 'get_investigation_chat') return hold
      ? new Promise((resolve) => pending.push(() => resolve(saved.chat))) : saved.chat;
    if (command === 'investigation_events') return { investigation_id: id,
      items: investigationEvents(id).slice(0, saved.detail!.last_event_sequence).filter((event) => event.sequence > afterSequence),
      next_sequence: saved.detail!.last_event_sequence, has_more: false };
    return null;
  });
  const dock = createAgentChatDockStore(); const leave = dock.getState().connect();
  const release = () => { hold = false; pending.splice(0).forEach((resolve) => resolve()); };
  return { dock, leave, calls, hold: () => { hold = true; }, release,
    update: (next: Partial<typeof value>) => { value = { ...value, ...next }; },
    complete: () => { const saved = completedState('a'); value = { detail: saved.detail!, result: saved.result!, chat: saved.chat! }; },
    cleanup: () => { leave(); release(); clearMocks(); vi.unstubAllGlobals(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); } };
}

describe('focused durable progress', () => {
  it('reconciles committed findings only after the same refresh transcript settles, without sibling reads', async () => {
    const h = progressTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks).toHaveLength(1));
      const initial = h.dock.getState().docks[0];
      h.complete(); h.hold(); await useInvestigationStore.getState().refreshSelected();
      expect(useInvestigationStore.getState()).toMatchObject({ loading: false, chatStatus: 'loading', detail: { revision: 9 } });
      expect(h.dock.getState().docks[0]).toBe(initial);
      h.release(); await vi.waitFor(() => expect(h.dock.getState().docks[0].answer).not.toBeNull());
      expect(h.dock.getState().docks[0]).toMatchObject({ turn: { revision: 9, status: 'completed' },
        answer: { result: investigationResult('a'), citations: investigationCitations } });
      expect(useInvestigationStore.getState().events.map((event) => event.sequence)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
      expect(h.calls.filter((call) => call.startsWith('get_investigation_chat:'))).toEqual(['get_investigation_chat:a', 'get_investigation_chat:a']);
      expect(h.calls.filter((call) => call.startsWith('get_investigation:'))).toEqual(['get_investigation:a', 'get_investigation:a']);
    } finally { h.cleanup(); }
  });

  it.each(['switch', 'close', 'disconnect'] as const)('does not resurrect delayed A after %s', async (action) => {
    const h = progressTransport();
    try {
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks).toHaveLength(1));
      const initial = h.dock.getState().docks[0];
      h.complete(); h.hold(); await useInvestigationStore.getState().refreshSelected();
      if (action === 'switch') h.dock.getState().open('b');
      else if (action === 'close') h.dock.getState().close('a');
      else h.leave();
      h.release();
      if (action === 'switch') await vi.waitFor(() => expect(h.dock.getState().docks).toHaveLength(2));
      await tick();
      expect(h.dock.getState().docks.find((item) => item.chat.task_id === 'a')).toBe(action === 'close' ? undefined : initial);
      expect(h.calls.filter((call) => call.startsWith('get_investigation_chat:')))
        .toEqual(['get_investigation_chat:a', 'get_investigation_chat:a', ...(action === 'switch' ? ['get_investigation_chat:b'] : [])]);
    } finally { h.cleanup(); }
  });

  it.each(['revision', 'parent', 'conversation', 'scope-anchor', 'scope-hops', 'producer', 'chat', 'result', 'reference', 'legacy'] as const)('refuses changed cached %s on explicit reopen and supplies no fresh follow-up parent', async (change) => {
    const h = progressTransport();
    try {
      h.complete();
      if (change === 'scope-anchor' || change === 'scope-hops') {
        h.update({ detail: investigationDetail('a', { scope: { type: 'neighborhood', anchor: 'fact:stable', hops: 2 } }) });
      }
      h.dock.getState().open('a'); await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      const previous = h.dock.getState().docks[0];
      if (change === 'revision') h.update({ detail: investigationDetail('a', { revision: 8, last_event_sequence: 8 }) });
      if (change === 'parent') h.update({ detail: investigationDetail('a', { parent_id: 'other-parent' }) });
      if (change === 'conversation') h.update({ detail: investigationDetail('a', { conversation_id: 'other-conversation' }) });
      if (change === 'scope-anchor') h.update({ detail: investigationDetail('a', { scope: { type: 'neighborhood', anchor: 'fact:other', hops: 2 } }) });
      if (change === 'scope-hops') h.update({ detail: investigationDetail('a', { scope: { type: 'neighborhood', anchor: 'fact:stable', hops: 3 } }) });
      if (change === 'producer') h.update({ detail: investigationDetail('a', { provider: { ...investigationCatalog.providers[0], model: 'other-model' } }),
        chat: { ...previous.chat, producer: { ...previous.chat.producer, model: 'other-model' } } });
      if (change === 'chat') h.update({ chat: { ...previous.chat, chat_id: 'changed-chat' } });
      if (change === 'result') { const result = investigationResult('a'); result.findings[0].statement = 'Changed immutable saved finding'; h.update({ result }); }
      if (change === 'reference') h.update({ chat: { ...previous.chat, messages: previous.chat.messages.map((entry) => entry.message.kind === 'result_reference'
        ? { ...entry, message: { ...entry.message, result_hash: 'f'.repeat(64) } } : entry) } });
      if (change === 'legacy') h.update({ chat: null });
      expect(await h.dock.getState().openForFollowUp('a')).toBeNull();
      expect(useInvestigationStore.getState()).toMatchObject({ chatStatus: change === 'legacy' ? 'legacy' : 'saved', error: null, chatError: null });
      expect(h.dock.getState().docks[0]).toBe(previous);
      expect(h.dock.getState().focusedGeneration).toBeNull();
      expect(h.dock.getState().error).toBeTruthy();
    } finally { h.cleanup(); }
  });

  it('accepts the same canonical scope when the host object has a different key order', async () => {
    const h = progressTransport();
    try {
      h.complete();
      h.update({ detail: investigationDetail('a', { scope: { type: 'neighborhood', anchor: 'fact:stable', hops: 2 } }) });
      h.dock.getState().open('a');
      await vi.waitFor(() => expect(h.dock.getState().docks[0]?.answer).toBeTruthy());
      const previous = h.dock.getState().docks[0];
      h.update({ detail: investigationDetail('a', { revision: previous.turn.revision + 1,
        scope: { hops: 2, anchor: 'fact:stable', type: 'neighborhood' } }) });

      const captured = await h.dock.getState().openForFollowUp('a');
      expect(captured?.detail?.scope).toEqual({ type: 'neighborhood', anchor: 'fact:stable', hops: 2 });
      expect(h.dock.getState().docks[0]).not.toBe(previous);
    } finally { h.cleanup(); }
  });
});
