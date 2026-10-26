import { afterEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC as mockTauriIPC } from '@tauri-apps/api/mocks';
import { investigationDetail } from './investigationFixtures';
import { useInvestigationStore } from './investigationStore';
import { createSpecialistActivityStore } from './specialistActivityStore';

function mockIPC(handler: (command: string, args?: Record<string, unknown>) => unknown) {
  mockTauriIPC((command, args) => handler(command, args as Record<string, unknown> | undefined));
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});

describe('separate specialist task activity projection (AC-0379)', () => {
  it('reads a complete empty active page without changing history, search, or selection', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const selected = investigationDetail('selected-task');
    useInvestigationStore.setState({ history: [selected], historySearch: 'needle', historyActiveOnly: true,
      selectedId: selected.investigation_id, detail: selected });
    mockIPC((command, args) => {
      expect(command).toBe('list_investigations');
      expect(args).toEqual({ activeOnly: true });
      return { items: [], next_cursor: null };
    });
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState().status).toBe('ready'));

    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [], error: null });
    expect(useInvestigationStore.getState()).toMatchObject({ history: [selected], historySearch: 'needle',
      historyActiveOnly: true, selectedId: selected.investigation_id, detail: selected });
  });

  it('keeps real active statuses and cancellation-requested state, while a cursor stays incomplete', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const tasks = [
      investigationDetail('queued', { status: 'queued', has_result: false }),
      investigationDetail('preparing', { status: 'preparing', has_result: false }),
      investigationDetail('running', { status: 'running', has_result: false, cancel_requested: true }),
      investigationDetail('consent', { status: 'awaiting_consent', has_result: false }),
    ];
    mockIPC(() => ({ items: tasks, next_cursor: 'more-active' }));
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState().status).toBe('incomplete'));

    expect(store.getState()).toMatchObject({ status: 'incomplete', tasks });
    expect(store.getState().tasks.map((task) => task.status)).toEqual([
      'queued', 'preparing', 'running', 'awaiting_consent',
    ]);
    expect(store.getState().tasks.find((task) => task.investigation_id === 'running')?.cancel_requested).toBe(true);
  });

  it('publishes each completed observation and coalesces continuous events into sequential reads', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const first = deferred<{ items: ReturnType<typeof investigationDetail>[]; next_cursor: string | null }>();
    const second = deferred<{ items: ReturnType<typeof investigationDetail>[]; next_cursor: string | null }>();
    const intermediate = investigationDetail('intermediate-observation', { status: 'running', has_result: false });
    const active = investigationDetail('created-during-refresh', { status: 'awaiting_consent', has_result: false });
    let calls = 0;
    mockIPC((command, args) => {
      expect(command).toBe('list_investigations');
      expect(args).toEqual({ activeOnly: true });
      calls += 1;
      if (calls === 1) return first.promise;
      if (calls === 2) return second.promise;
      return { items: [active], next_cursor: null };
    });
    const store = createSpecialistActivityStore();
    const observations: { status: string; taskIds: string[]; refreshing: boolean }[] = [];
    store.subscribe((state) => observations.push({ status: state.status,
      taskIds: state.tasks.map((task) => task.investigation_id), refreshing: state.refreshing }));
    store.getState().setVisible(true);
    store.getState().invalidate();
    store.getState().invalidate();
    first.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(calls).toBe(2));
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [], refreshing: true });

    store.getState().invalidate();
    second.resolve({ items: [intermediate], next_cursor: null });
    await vi.waitFor(() => expect(calls).toBe(3));
    expect(observations).toContainEqual({ status: 'ready', taskIds: [], refreshing: true });
    expect(observations).toContainEqual({ status: 'ready', taskIds: ['intermediate-observation'], refreshing: true });

    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [active], refreshing: false }));

    expect(calls).toBe(3);
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [active], refreshing: false });
  });

  it('replaces active rows with a complete terminal observation and fails closed on malformed/error responses', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const active = investigationDetail('terminal-transition', { status: 'awaiting_consent', has_result: false });
    let respond: () => unknown = () => ({ items: [active], next_cursor: null });
    mockIPC(() => respond());
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [active] }));
    expect(store.getState().tasks).toEqual([active]);
    const terminal = deferred<{ items: ReturnType<typeof investigationDetail>[]; next_cursor: string | null }>();
    respond = () => terminal.promise;
    store.getState().invalidate();
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [active], refreshing: true });
    terminal.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [], refreshing: false }));

    respond = () => ({ items: [{ ...active, status: 'outcome_unknown' }], next_cursor: null });
    await store.getState().refresh();
    expect(store.getState().status).toBe('error');
    respond = () => { throw new Error('offline'); };
    await store.getState().refresh();
    expect(store.getState()).toMatchObject({ status: 'error', tasks: [], error: expect.stringContaining('unknown') });
  });

  it('ignores a late response after the surface closes and reopens', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const older = deferred<{ items: ReturnType<typeof investigationDetail>[]; next_cursor: string | null }>();
    const fresh = investigationDetail('fresh-after-reopen', { status: 'preparing', has_result: false });
    let calls = 0;
    mockIPC(() => ++calls === 1 ? older.promise : { items: [fresh], next_cursor: null });
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    store.getState().setVisible(false);
    store.getState().setVisible(true);
    expect(calls).toBe(1);
    expect(store.getState()).toMatchObject({ status: 'loading', tasks: [], refreshing: true });
    older.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(store.getState().tasks).toEqual([fresh]));
    expect(calls).toBe(2);
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [fresh] });
  });
});
