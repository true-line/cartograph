import { describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { create } from 'zustand';
import { createAgentChatDockStore } from './agentChatDockStore';
import { useInvestigationStore, type InvestigationState } from './investigationStore';
import { investigationCatalog, investigationConsent, investigationDetail, investigationEvents } from './investigationFixtures';
import type { InvestigationChat } from './investigationTypes';

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
const tick = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };
function harness(selectedId: string | null = null) {
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
  const leave = dock.getState().connect();
  const finish = (id: string) => coordinator.setState({ detail: investigationDetail(id), chat: chat(id), chatStatus: 'saved' });
  const open = async (id: string) => { dock.getState().open(id); await tick(); finish(id); await tick(); };
  return { coordinator, dock, calls, leave, finish, open,
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
