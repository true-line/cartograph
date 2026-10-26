import { describe, expect, it } from 'vitest';
import { create } from 'zustand';
import { createAgentChatDockStore } from './agentChatDockStore';
import { useInvestigationStore, type InvestigationState } from './investigationStore';
import { investigationDetail } from './investigationFixtures';
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
