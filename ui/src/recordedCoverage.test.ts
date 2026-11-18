import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore } from './store';
import type { RestoredCoverage } from './coverage';

// AC-0700 (#1656): Workspace's recorded coverage is read back at startup, and
// a failed or superseded read never restores older cards or hides its error.
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: string) => void;
  const promise = new Promise<T>((success, failure) => { resolve = success; reject = failure; });
  return { promise, resolve, reject };
}
const restored = (repo: string): RestoredCoverage => ({
  repo,
  status: 'recorded',
  inventory_hash: `blake3:${repo}`,
  compared: ['registration', 'plugins'],
  inventory: {
    inventory: 'coverage@1', repo, commit: 'a'.repeat(40), layer_hints: [],
    producers: [], missing_inputs: [], unsupported: [], limited: false,
  },
});
beforeEach(() => { vi.stubGlobal('window', {}); useAppStore.setState(useAppStore.getInitialState(), true); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); useAppStore.setState(useAppStore.getInitialState(), true); });

describe('recorded coverage after relaunch (AC-0700)', () => {
  it('restores the recorded inventories on startup refresh without an ingest', async () => {
    const commands: string[] = [];
    mockIPC((command) => {
      commands.push(command);
      if (command === 'ping') return { app: 'cartograph', version: 'test' };
      if (command === 'recorded_coverage') return [restored('acme/api')];
      if (command === 'list_staged_proposals') return { items: [], next_cursor: null };
      return null;
    });
    await useAppStore.getState().refresh();
    expect(useAppStore.getState()).toMatchObject({ recordedCoverage: [restored('acme/api')], recordedCoverageError: null, ingestSummary: null });
    expect(commands.filter((command) => /ingest_path|add_repo|add_system/.test(command))).toEqual([]);
  });
  it('drops a superseded read, and a failure replaces earlier cards with its error', async () => {
    const older = deferred<RestoredCoverage[]>(), latest = deferred<RestoredCoverage[]>();
    let reads = 0;
    mockIPC((command) => command === 'recorded_coverage' ? (++reads === 1 ? older.promise : latest.promise) : null);
    const first = useAppStore.getState().loadRecordedCoverage();
    const second = useAppStore.getState().loadRecordedCoverage();
    latest.reject('incompatible coverage schema'); await second;
    older.resolve([restored('acme/old')]); await first;
    expect(useAppStore.getState()).toMatchObject({ recordedCoverage: [], recordedCoverageError: 'incompatible coverage schema' });
  });
  it('never lets a read in flight across a clear restore the cleared cards', async () => {
    const inFlight = deferred<RestoredCoverage[]>();
    let reads = 0;
    mockIPC((command) => {
      if (command === 'recorded_coverage') return ++reads === 1 ? inFlight.promise : [];
      if (command === 'clear_graph') return { nodes: 0, edges: 0 };
      if (command === 'ping') return { app: 'cartograph', version: 'test' };
      if (command === 'list_staged_proposals') return { items: [], next_cursor: null };
      return null;
    });
    useAppStore.setState({ recordedCoverage: [restored('acme/api')] });
    const read = useAppStore.getState().loadRecordedCoverage();
    const clear = useAppStore.getState().clearGraph();
    expect(useAppStore.getState().recordedCoverage).toEqual([]);
    inFlight.resolve([restored('acme/api')]); await read; await clear;
    expect(useAppStore.getState()).toMatchObject({ recordedCoverage: [], recordedCoverageError: null });
  });
});
