import { afterEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { createSavedResultNoticeStore, SAVED_RESULT_NOTICE_LIMIT, SAVED_RESULT_TOAST_LIMIT } from './savedResultNoticeStore';
import type { InvestigationChat } from './investigationTypes';

function savedChat(taskId: string, resultId = `result-${taskId}`): InvestigationChat {
  return {
    chat_id: `chat-${taskId}`, task_id: taskId,
    producer: { specialist_id: 'domain-analyst@3', specialist_version: 3, prompt_fingerprint: 'a'.repeat(64),
      tier: 'Agentic', confidence_tier: 'InferredWeak', provider_mode: 'local', provider_id: 'ollama', model: 'test', protocol_version: null },
    messages: [
      { id: `question-${taskId}`, ordinal: 0, author: 'user', message: { kind: 'user_question', question: 'Question' } },
      { id: `result-${taskId}`, ordinal: 1, author: 'specialist', message: { kind: 'result_reference', task_id: taskId, result_id: resultId, result_hash: 'b'.repeat(64) } },
    ],
  };
}

function pendingChat(taskId: string): InvestigationChat {
  const chat = savedChat(taskId);
  return { ...chat, messages: [chat.messages[0]] };
}

function event(taskId: string, revision: number) {
  return { investigation_id: taskId, revision, last_event_sequence: revision };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
});

describe('saved result notice candidate reads (AC-0386)', () => {
  it('coalesces event bursts by task and serializes candidate reads', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const first = deferred<InvestigationChat>();
    let active = 0;
    let maximumActive = 0;
    const calls: string[] = [];
    mockIPC(async (command, args) => {
      expect(command).toBe('get_investigation_chat');
      const taskId = (args as { investigationId: string }).investigationId;
      calls.push(taskId);
      active += 1; maximumActive = Math.max(active, maximumActive);
      try {
        if (calls.length === 1) return await first.promise;
        return savedChat(taskId);
      } finally { active -= 1; }
    });
    const store = createSavedResultNoticeStore();
    store.getState().observe(event('task-a', 1));
    store.getState().observe(event('task-a', 2));
    store.getState().observe(event('task-a', 3));
    store.getState().observe(event('task-a', 3));
    store.getState().observe(event('task-a', 2));
    store.getState().observe(event('task-b', 1));
    await vi.waitFor(() => expect(calls).toEqual(['task-a']));
    expect(calls).toEqual(['task-a']);
    first.resolve(pendingChat('task-a'));
    await vi.waitFor(() => expect(store.getState().unread).toHaveLength(1));
    expect(store.getState().unread[0].task_id).toBe('task-a');
    expect(calls).toEqual(['task-a', 'task-a', 'task-b']);
    expect(maximumActive).toBe(1);
  });

  it('requires a valid pending baseline and ignores an already-saved result on a later no-op event', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const attempts = new Map<string, number>();
    mockIPC((_command, args) => {
      const taskId = (args as { investigationId: string }).investigationId;
      const attempt = (attempts.get(taskId) ?? 0) + 1;
      attempts.set(taskId, attempt);
      if (attempt === 1 && taskId === 'legacy') return null;
      if (attempt === 1 && taskId === 'malformed') return { ...savedChat(taskId), messages: [] };
      if (attempt === 1 && taskId === 'unavailable') throw new Error('Unavailable');
      if (attempt === 1 && taskId === 'missing') return undefined;
      return attempt === 1 ? pendingChat(taskId) : savedChat(taskId);
    });
    const store = createSavedResultNoticeStore();
    for (const taskId of ['legacy', 'malformed', 'unavailable', 'missing']) {
      store.getState().observe(event(taskId, 1));
      await vi.waitFor(() => expect(attempts.get(taskId)).toBe(1));
    }
    for (const taskId of ['legacy', 'malformed', 'unavailable', 'missing']) store.getState().observe(event(taskId, 2));
    await vi.waitFor(() => expect([...attempts.values()].reduce((sum, count) => sum + count, 0)).toBe(8));
    expect(store.getState().unread).toEqual([]);
    store.getState().observe(event('pending', 1));
    await vi.waitFor(() => expect(attempts.get('pending')).toBe(1));
    expect(store.getState().unread).toEqual([]);
    store.getState().observe(event('pending', 2));
    await vi.waitFor(() => expect(store.getState().unread).toMatchObject([{ task_id: 'pending', result_id: 'result-pending' }]));
  });

  it('caps toast cards, retains dismissed unread entries, and consumes only a visibly opened result', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const reads = new Map<string, number>();
    let completedReads = 0;
    mockIPC(async (_command, args) => {
      const taskId = (args as { investigationId: string }).investigationId;
      const count = (reads.get(taskId) ?? 0) + 1;
      reads.set(taskId, count);
      const value = count === 1 ? pendingChat(taskId) : savedChat(taskId);
      await Promise.resolve();
      completedReads += 1;
      return value;
    });
    const store = createSavedResultNoticeStore();
    for (let i = 0; i < SAVED_RESULT_NOTICE_LIMIT + 1; i += 1) store.getState().observe(event(`task-${i}`, 1));
    await vi.waitFor(() => expect(completedReads).toBe(SAVED_RESULT_NOTICE_LIMIT));
    expect(store.getState().unread).toEqual([]);
    for (let i = 0; i < SAVED_RESULT_NOTICE_LIMIT; i += 1) store.getState().observe(event(`task-${i}`, 2));
    await vi.waitFor(() => expect(store.getState().unread).toHaveLength(SAVED_RESULT_NOTICE_LIMIT));
    expect(store.getState().toasts).toHaveLength(SAVED_RESULT_TOAST_LIMIT);
    const latest = store.getState().toasts.at(-1)!;
    store.getState().dismissToast(latest);
    expect(store.getState().unread).toHaveLength(SAVED_RESULT_NOTICE_LIMIT);
    store.getState().markVisible('task-127', 'result-task-127');
    expect(store.getState().unread).toHaveLength(SAVED_RESULT_NOTICE_LIMIT - 1);
    expect(store.getState().incomplete).toBe(true);
  });

  it('does not re-add a result when it becomes visible before its candidate read returns', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const pending = deferred<InvestigationChat>();
    let calls = 0;
    mockIPC((_command, args) => {
      calls += 1;
      const taskId = (args as { investigationId: string }).investigationId;
      if (calls === 1) return pendingChat(taskId);
      return calls === 2 ? pending.promise : savedChat(taskId);
    });
    const store = createSavedResultNoticeStore();
    store.getState().observe(event('task-a', 1));
    await vi.waitFor(() => expect(calls).toBe(1));
    store.getState().observe(event('task-a', 2));
    await vi.waitFor(() => expect(calls).toBe(2));
    store.getState().markVisible('task-a', 'result-task-a');
    pending.resolve(savedChat('task-a'));
    await vi.waitFor(() => expect(calls).toBe(2));
    store.getState().observe(event('task-a', 3));
    await vi.waitFor(() => expect(calls).toBe(3));
    expect(store.getState().unread).toEqual([]);
    expect(store.getState().toasts).toEqual([]);
    expect(store.getState().incomplete).toBe(false);
  });

  it('does not announce an old result after a no-op terminal invalidation', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    let calls = 0;
    const first = deferred<InvestigationChat>();
    mockIPC(() => {
      calls += 1;
      return calls === 1 ? first.promise : savedChat('already-completed');
    });
    const store = createSavedResultNoticeStore();
    store.getState().observe(event('already-completed', 7));
    await vi.waitFor(() => expect(calls).toBe(1));
    store.getState().observe(event('already-completed', 8));
    first.resolve(savedChat('already-completed'));
    await vi.waitFor(() => expect(calls).toBe(2));
    expect(store.getState().unread).toEqual([]);
    expect(store.getState().toasts).toEqual([]);
  });
});
