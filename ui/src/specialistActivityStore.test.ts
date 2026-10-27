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

function activityItem(summary: ReturnType<typeof investigationDetail>) {
  return { summary, saved_provider: {
    mode: summary.provider.mode, provider_id: summary.provider.provider_id, model: summary.provider.model,
  } };
}

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});

describe('separate specialist task activity projection (AC-0379, AC-0381)', () => {
  it('reads a complete empty active page without changing history, search, or selection', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const selected = investigationDetail('selected-task');
    useInvestigationStore.setState({ history: [selected], historySearch: 'needle', historyActiveOnly: true,
      selectedId: selected.investigation_id, detail: selected });
    mockIPC((command, args) => {
      expect(command).toBe('list_specialist_task_activity');
      expect(args).toEqual({ cursor: null });
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
    mockIPC(() => ({ items: tasks.map(activityItem), next_cursor: 'more-active' }));
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState().status).toBe('incomplete'));

    expect(store.getState()).toMatchObject({ status: 'incomplete', tasks: tasks.map(activityItem) });
    expect(store.getState().tasks.map((task) => task.summary.status)).toEqual([
      'queued', 'preparing', 'running', 'awaiting_consent',
    ]);
    expect(store.getState().tasks.find((task) => task.summary.investigation_id === 'running')?.summary.cancel_requested).toBe(true);
  });

  it('publishes each completed observation and coalesces continuous events into sequential reads', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const first = deferred<{ items: ReturnType<typeof activityItem>[]; next_cursor: string | null }>();
    const second = deferred<{ items: ReturnType<typeof activityItem>[]; next_cursor: string | null }>();
    const intermediate = investigationDetail('intermediate-observation', { status: 'running', has_result: false });
    const active = investigationDetail('created-during-refresh', { status: 'awaiting_consent', has_result: false });
    let calls = 0;
    mockIPC((command, args) => {
      expect(command).toBe('list_specialist_task_activity');
      expect(args).toEqual({ cursor: null });
      calls += 1;
      if (calls === 1) return first.promise;
      if (calls === 2) return second.promise;
      return { items: [activityItem(active)], next_cursor: null };
    });
    const store = createSpecialistActivityStore();
    const observations: { status: string; taskIds: string[]; refreshing: boolean }[] = [];
    store.subscribe((state) => observations.push({ status: state.status,
      taskIds: state.tasks.map((task) => task.summary.investigation_id), refreshing: state.refreshing }));
    store.getState().setVisible(true);
    store.getState().invalidate();
    store.getState().invalidate();
    first.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(calls).toBe(2));
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [], refreshing: true });

    store.getState().invalidate();
    second.resolve({ items: [activityItem(intermediate)], next_cursor: null });
    await vi.waitFor(() => expect(calls).toBe(3));
    expect(observations).toContainEqual({ status: 'ready', taskIds: [], refreshing: true });
    expect(observations).toContainEqual({ status: 'ready', taskIds: ['intermediate-observation'], refreshing: true });

    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [activityItem(active)], refreshing: false }));

    expect(calls).toBe(3);
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [activityItem(active)], refreshing: false });
  });

  it('replaces active rows with a complete terminal observation and fails closed on malformed/error responses', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const active = investigationDetail('terminal-transition', { status: 'awaiting_consent', has_result: false });
    let respond: () => unknown = () => ({ items: [activityItem(active)], next_cursor: null });
    mockIPC(() => respond());
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [activityItem(active)] }));
    expect(store.getState().tasks).toEqual([activityItem(active)]);
    const terminal = deferred<{ items: ReturnType<typeof activityItem>[]; next_cursor: string | null }>();
    respond = () => terminal.promise;
    store.getState().invalidate();
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [activityItem(active)], refreshing: true });
    terminal.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(store.getState()).toMatchObject({ status: 'ready', tasks: [], refreshing: false }));

    respond = () => ({ items: [activityItem({ ...active, status: 'outcome_unknown' })], next_cursor: null });
    await store.getState().refresh();
    expect(store.getState().status).toBe('error');
    respond = () => ({ items: [{ ...activityItem(active), saved_provider: {
      ...activityItem(active).saved_provider, mode: 'cloud',
    } }], next_cursor: null });
    await store.getState().refresh();
    expect(store.getState()).toMatchObject({ status: 'error', tasks: [] });
    respond = () => ({ items: [{ ...activityItem(active), saved_provider: {
      ...activityItem(active).saved_provider, model: 'm'.repeat(2049),
    } }], next_cursor: null });
    await store.getState().refresh();
    expect(store.getState()).toMatchObject({ status: 'error', tasks: [] });
    respond = () => { throw new Error('offline'); };
    await store.getState().refresh();
    expect(store.getState()).toMatchObject({ status: 'error', tasks: [], error: expect.stringContaining('unknown') });
  });

  it('ignores a late response after the surface closes and reopens', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const older = deferred<{ items: ReturnType<typeof activityItem>[]; next_cursor: string | null }>();
    const fresh = investigationDetail('fresh-after-reopen', { status: 'preparing', has_result: false });
    let calls = 0;
    mockIPC(() => ++calls === 1 ? older.promise : { items: [activityItem(fresh)], next_cursor: null });
    const store = createSpecialistActivityStore();
    store.getState().setVisible(true);
    store.getState().setVisible(false);
    store.getState().setVisible(true);
    expect(calls).toBe(1);
    expect(store.getState()).toMatchObject({ status: 'loading', tasks: [], refreshing: true });
    older.resolve({ items: [], next_cursor: null });
    await vi.waitFor(() => expect(store.getState().tasks).toEqual([activityItem(fresh)]));
    expect(calls).toBe(2);
    expect(store.getState()).toMatchObject({ status: 'ready', tasks: [activityItem(fresh)] });
  });
  // AC-0417: exact strings are displayed unchanged; legacy absence differs from malformed presence.
  it('validates optional exact reported usage without number coercion', async () => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
    const item = activityItem(investigationDetail('usage', { status: 'running', has_result: false }));
    let row: unknown = item;
    mockIPC(() => ({ items: [row], next_cursor: null }));
    const store = createSpecialistActivityStore(); store.getState().setVisible(true);
    await vi.waitFor(() => expect(store.getState().status).toBe('ready'));
    for (const usage of [
      { input_tokens: '18446744073709551615', output_tokens: null },
      { input_tokens: null, output_tokens: '0' },
    ]) {
      // AC-0418: the existing consumer accepts additive outer backend telemetry.
      row = { ...item, reported_usage: usage, reported_cache_read_tokens: '18446744073709551615' }; await store.getState().refresh();
      expect(store.getState().status).toBe('ready');
      expect(store.getState().tasks[0].reported_usage).toEqual(usage);
      expect(store.getState().tasks[0].reported_cache_read_tokens).toBe('18446744073709551615');
    }
    for (const usage of [null, {}, { input_tokens: null, output_tokens: null, extra: true },
      ...['01', '-1', '1.5', '1e3', '18446744073709551616', '9'.repeat(21), 1].map((input_tokens) => ({ input_tokens, output_tokens: null }))]) {
      row = { ...item, reported_usage: usage }; await store.getState().refresh();
      expect(store.getState().status).toBe('error');
    }
    // AC-0419: cache availability is independent of absent/unknown input-output reports.
    for (const cache of [null, '0', '18446744073709551615']) {
      for (const usage of [{}, { reported_usage: { input_tokens: null, output_tokens: null } }]) {
        row = { ...item, ...usage, reported_cache_read_tokens: cache }; await store.getState().refresh();
        expect(store.getState().status).toBe('ready');
        expect(store.getState().tasks[0].reported_cache_read_tokens).toBe(cache);
      }
    }
    for (const cache of [undefined, 0, -1, {}, [], '', '01', '-1', '1.5', '1e3', '18446744073709551616', '9'.repeat(21)]) {
      row = { ...item, reported_usage: { input_tokens: '1', output_tokens: '2' }, reported_cache_read_tokens: cache };
      await store.getState().refresh(); expect(store.getState().status).toBe('error');
    }
    row = item; await store.getState().refresh(); expect(store.getState().status).toBe('ready');
    expect('reported_cache_read_tokens' in store.getState().tasks[0]).toBe(false);
  });

});
