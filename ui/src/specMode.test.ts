import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type SpecBundle } from './store';
import { packetBundleFixture } from './workPacketFixtures';

const bundle = (mode: SpecBundle['mode']): SpecBundle => ({ ...packetBundleFixture, mode });
// `export_spec_with_views` pairs the bundle with its views; these tests observe the bundle.
const withViews = (pending: Promise<SpecBundle>) => pending.then((specBundle) => ({ bundle: specBundle, views: null }));
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: string) => void;
  const promise = new Promise<T>((success, failure) => { resolve = success; reject = failure; });
  return { promise, resolve, reject };
}
beforeEach(() => { vi.stubGlobal('window', {}); useAppStore.setState(useAppStore.getInitialState(), true); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); useAppStore.setState(useAppStore.getInitialState(), true); });

describe('visible spec mode request ownership (AC-0248)', () => {
  it('keeps the latest mode busy when a superseded request fails first', async () => {
    const older = deferred<SpecBundle>(), latest = deferred<SpecBundle>();
    mockIPC((_command, args) => (args as { mode: string }).mode === 'best-effort' ? withViews(older.promise) : withViews(latest.promise));
    const first = useAppStore.getState().setSpecMode('best-effort');
    const second = useAppStore.getState().setSpecMode('verified-only');
    older.reject('obsolete export failure'); await first;
    expect(useAppStore.getState()).toMatchObject({ specMode: 'verified-only', specBusy: true, specError: null, specBundle: null });
    latest.resolve(bundle('verified-only')); await second;
    expect(useAppStore.getState()).toMatchObject({ specBusy: false, specError: null, specBundle: bundle('verified-only') });
  });
  it('ignores an older successful response that arrives after the selected mode', async () => {
    const older = deferred<SpecBundle>(), latest = deferred<SpecBundle>();
    mockIPC((_command, args) => (args as { mode: string }).mode === 'best-effort' ? withViews(older.promise) : withViews(latest.promise));
    const first = useAppStore.getState().setSpecMode('best-effort');
    const second = useAppStore.getState().setSpecMode('verified-only');
    latest.resolve(bundle('verified-only')); await second;
    older.resolve(bundle('best-effort')); await first;
    expect(useAppStore.getState()).toMatchObject({ specMode: 'verified-only', specBusy: false, specBundle: bundle('verified-only') });
  });
  it('recompiles the selected mode after a pending curation is stored without letting the older mode request clear busy', async () => {
    const recorded = deferred<null>(), modeResult = deferred<SpecBundle>(), curatedResult = deferred<SpecBundle>();
    const compiledAfterCuration = deferred<void>();
    let exports = 0;
    mockIPC((command, args) => {
      if (command === 'record_assertion_decision') return recorded.promise;
      if (command === 'list_assertion_decisions') return [];
      if (command === 'export_spec_with_views') {
        expect((args as { mode: string }).mode).toBe('verified-only');
        if (++exports === 1) return withViews(modeResult.promise);
        compiledAfterCuration.resolve(); return withViews(curatedResult.promise);
      }
      throw new Error('Unexpected spec request');
    });
    const curation = useAppStore.getState().curateAssertion(packetBundleFixture.artifacts[0].assertions[0], 'rejected');
    const change = useAppStore.getState().setSpecMode('verified-only');
    recorded.resolve(null); await compiledAfterCuration.promise;
    modeResult.resolve(bundle('verified-only')); await change;
    expect(useAppStore.getState().specBusy).toBe(true);
    const compiled = { ...bundle('verified-only'), assertion_count: 0 };
    curatedResult.resolve(compiled); await curation;
    expect(useAppStore.getState()).toMatchObject({ specBusy: false, specBundle: compiled });
  });
  it('does not let a refresh started in the old mode replace a newer mode response', async () => {
    const refreshed = deferred<SpecBundle>();
    const requested = deferred<SpecBundle>();
    const observed = deferred<void>();
    useAppStore.setState({ specMode: 'best-effort' });
    mockIPC((command, args) => {
      if (command === 'ping') return { app: 'cartograph', version: 'test' };
      if (command === 'export_spec_with_views') {
        if ((args as { mode: string }).mode === 'best-effort') { observed.resolve(); return withViews(refreshed.promise); }
        return withViews(requested.promise);
      }
      if (command === 'list_staged_proposals') return { items: [], next_cursor: null };
      return null;
    });
    const refresh = useAppStore.getState().refresh(); await observed.promise;
    const change = useAppStore.getState().setSpecMode('verified-only');
    requested.resolve(bundle('verified-only')); await change;
    refreshed.resolve(bundle('best-effort')); await refresh;
    expect(useAppStore.getState()).toMatchObject({ specMode: 'verified-only', specBundle: bundle('verified-only') });
  });
});
