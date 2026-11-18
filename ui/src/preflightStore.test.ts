import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type PreflightReport, type SystemPreflight } from './store';

const report = (detector: string): PreflightReport => ({
  languages: [],
  frameworks: [],
  unsupported: [],
  potential_gaps: [],
  detector,
});

const reset = {
  ingestSource: 'local' as const,
  ingestTarget: '/repos/app',
  preflight: null,
  systemPreflight: null,
  preflightBusy: false,
  preflightError: null,
  preflightProgress: null,
};

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState(reset);
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState(reset);
});

describe('preflight progress and cancellation (AC-0197/AC-0198, #235)', () => {
  it('shows progress only while running and clears it when the scan settles', async () => {
    let finish: (value: PreflightReport) => void = () => {};
    let run = -1;
    mockIPC((_command, args) => {
      run = (args as { run: number }).run;
      return new Promise<PreflightReport>((resolve) => { finish = resolve; });
    });
    const running = useAppStore.getState().runPreflight();
    const progress = { run, path: 'src/a.ts', done: 0, total: 2 };
    useAppStore.getState().applyPreflightProgress(progress);
    expect(useAppStore.getState().preflightProgress).toEqual(progress);

    finish(report('preflight@1'));
    await running;
    const state = useAppStore.getState();
    expect(state.preflight?.detector).toBe('preflight@1');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflightProgress).toBeNull();

    useAppStore.getState().applyPreflightProgress(progress);
    expect(useAppStore.getState().preflightProgress).toBeNull();
  });

  it('reports a cancelled scan without a raw error and sends cancel_preflight', async () => {
    const commands: string[] = [];
    let reject: (reason: string) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      if (command === 'cancel_preflight') {
        reject('cancelled');
        return null;
      }
      return new Promise((_resolve, fail) => { reject = fail; });
    });
    const running = useAppStore.getState().runPreflight();
    await useAppStore.getState().cancelPreflight();
    await running;
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    const state = useAppStore.getState();
    expect(state.preflightError).toBe('Preflight cancelled. No findings were recorded.');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflight).toBeNull();
  });

  it('shows the result of a scan whose cancel lost to its commit (AC-0215, #493 review)', async () => {
    let resolve: (value: PreflightReport) => void = () => {};
    mockIPC((command) => {
      if (command === 'cancel_preflight') {
        // The run had already claimed its commit: the backend completes it.
        resolve(report('completed'));
        return null;
      }
      return new Promise<PreflightReport>((done) => { resolve = done; });
    });
    const running = useAppStore.getState().runPreflight();
    await useAppStore.getState().cancelPreflight();
    await running;
    const state = useAppStore.getState();
    expect(state.preflight?.detector).toBe('completed');
    expect(state.preflightError).toBeNull();
    expect(state.preflightBusy).toBe(false);
  });

  it('never lets a superseded scan overwrite the current one', async () => {
    const pending: Array<{ resolve: (value: PreflightReport) => void; reject: (reason: string) => void }> = [];
    mockIPC(() => new Promise<PreflightReport>((resolve, reject) => { pending.push({ resolve, reject }); }));
    const first = useAppStore.getState().runPreflight();
    const second = useAppStore.getState().runPreflight();

    pending[1].resolve(report('current'));
    await second;
    pending[0].reject('cancelled');
    await first;

    const state = useAppStore.getState();
    expect(state.preflight?.detector).toBe('current');
    expect(state.preflightError).toBeNull();
    expect(state.preflightBusy).toBe(false);
  });

  it('cancels a running local scan when the source switches to GitHub', async () => {
    const commands: string[] = [];
    let reject: (reason: string) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      if (command === 'cancel_preflight') {
        reject('cancelled');
        return null;
      }
      return new Promise((_resolve, fail) => { reject = fail; });
    });
    const local = useAppStore.getState().runPreflight();
    // The Connect screen's source picker — no second Preflight click.
    useAppStore.getState().setIngestSource('github');
    await local;

    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    const state = useAppStore.getState();
    expect(state.ingestSource).toBe('github');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflight).toBeNull();

    // Switching while idle sends nothing.
    useAppStore.getState().setIngestSource('manifest');
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
  });

  // AC-0198 (#454): editing the local path abandons the running scan.
  it('cancels and retires a running local scan when the local path changes', async () => {
    const commands: string[] = [];
    let resolve: (value: PreflightReport) => void = () => {};
    let run = -1;
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'cancel_preflight') return null;
      run = (args as { run: number }).run;
      return new Promise<PreflightReport>((done) => { resolve = done; });
    });
    const local = useAppStore.getState().runPreflight();
    // A whitespace-only edit doesn't change what is being scanned.
    useAppStore.getState().setIngestTarget(' /repos/app ');
    expect(commands).toEqual(['preflight']);
    expect(useAppStore.getState().preflightBusy).toBe(true);

    // Back to Connect during the scan, then a different path.
    useAppStore.getState().setIngestTarget('/repos/other');
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    let state = useAppStore.getState();
    expect(state.ingestTarget).toBe('/repos/other');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflightProgress).toBeNull();

    // The abandoned run's late ping and result (a cancel that lost the
    // persist race) never reach the store.
    useAppStore.getState().applyPreflightProgress({ run, path: 'old/a.ts', done: 1, total: 2 });
    resolve(report('old-path'));
    await local;
    state = useAppStore.getState();
    expect(state.preflight).toBeNull();
    expect(state.preflightProgress).toBeNull();
    expect(state.preflightBusy).toBe(false);

    // Editing while idle sends nothing.
    useAppStore.getState().setIngestTarget('/repos/third');
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
  });

  it("ignores a superseded scan's late progress ping (#434 review)", async () => {
    const runs: number[] = [];
    const pending: Array<(value: PreflightReport) => void> = [];
    mockIPC((_command, args) => {
      runs.push((args as { run: number }).run);
      return new Promise<PreflightReport>((resolve) => { pending.push(resolve); });
    });
    const first = useAppStore.getState().runPreflight();
    const second = useAppStore.getState().runPreflight();
    expect(runs[1]).toBeGreaterThan(runs[0]);

    const current = { run: runs[1], path: 'new/a.ts', done: 0, total: 9 };
    useAppStore.getState().applyPreflightProgress(current);
    useAppStore.getState().applyPreflightProgress({ run: runs[0], path: 'old/z.ts', done: 5, total: 6 });
    expect(useAppStore.getState().preflightProgress).toEqual(current);

    pending[0](report('old'));
    pending[1](report('new'));
    await Promise.all([first, second]);
  });

  it('shows the reconciled report once a local recovery completes (AC-0200, #243)', async () => {
    const reconciled = report('preflight@1');
    reconciled.potential_gaps = [
      {
        kind: 'inline-eval',
        path: 'app.ts',
        line: 4,
        message: 'const-shaped',
        detector: 'preflight@1',
        request_adapter: null,
      },
    ];
    mockIPC((command) =>
      command === 'ingest_path'
        ? { job_id: 1, files: 1, nodes: 0, edges: 0, layers: {}, preflight: reconciled }
        : null,
    );
    useAppStore.setState({ preflight: report('pending'), preflightError: 'cancelled earlier' });
    await useAppStore.getState().ingest('/repos/app', 'local');

    const state = useAppStore.getState();
    expect(state.preflight).toEqual(reconciled);
    expect(state.preflightError).toBeNull();
  });

  it("keeps a newer preflight's report when an older recovery settles late (#439 review)", async () => {
    let settleRecovery: (summary: unknown) => void = () => {};
    mockIPC((command) =>
      command === 'ingest_path'
        ? new Promise((resolve) => { settleRecovery = resolve; })
        : command === 'preflight'
          ? report('repo-b')
          : null,
    );
    const recovery = useAppStore.getState().ingest('/repos/a', 'local');
    useAppStore.setState({ ingestSource: 'local', ingestTarget: '/repos/b' });
    await useAppStore.getState().runPreflight();
    expect(useAppStore.getState().preflight?.detector).toBe('repo-b');

    settleRecovery({ job_id: 1, files: 1, nodes: 0, edges: 0, layers: {}, preflight: report('repo-a') });
    await recovery;
    expect(useAppStore.getState().preflight?.detector).toBe('repo-b');
  });
});

describe('register-order precedence for preflight reports (AC-0216, #458)', () => {
  const stamped = (detector: string, repo: string, epoch: number): PreflightReport => ({
    ...report(detector),
    register: { repo, epoch },
  });
  const summary = (preflight: PreflightReport, preflight_register: unknown) => ({
    job_id: 1, files: 1, nodes: 0, edges: 0, layers: {}, preflight, preflight_register,
  });

  it("shows a same-repo recovery's report even when a preflight began while it ran", async () => {
    let settleRecovery: (value: unknown) => void = () => {};
    mockIPC((command) =>
      command === 'ingest_path'
        ? new Promise((resolve) => { settleRecovery = resolve; })
        : command === 'preflight'
          ? stamped('scan', 'local/same', 2)
          : null,
    );
    const recovery = useAppStore.getState().ingest('/repos/same', 'local');
    await useAppStore.getState().runPreflight();
    expect(useAppStore.getState().preflight?.detector).toBe('scan');

    // The recovery reconciled after that scan persisted: the register holds
    // the recovery's classification, so the surface must show it.
    settleRecovery(summary(report('reconciled'), { repo: 'local/same', epoch: 3 }));
    await recovery;
    expect(useAppStore.getState().preflight?.detector).toBe('reconciled');
  });

  it('never lets an older scan that settles last replace the newer reconciled report', async () => {
    let settleScan: (value: unknown) => void = () => {};
    let settleRecovery: (value: unknown) => void = () => {};
    mockIPC((command) =>
      command === 'ingest_path'
        ? new Promise((resolve) => { settleRecovery = resolve; })
        : command === 'preflight'
          ? new Promise((resolve) => { settleScan = resolve; })
          : null,
    );
    const recovery = useAppStore.getState().ingest('/repos/late', 'local');
    const scan = useAppStore.getState().runPreflight();
    settleRecovery(summary(report('reconciled'), { repo: 'local/late', epoch: 7 }));
    await recovery;
    settleScan(stamped('pending', 'local/late', 6));
    await scan;
    expect(useAppStore.getState().preflight?.detector).toBe('reconciled');
  });

  it('keeps the shown report when the recovery wrote nothing over a newer scan', async () => {
    mockIPC((command) =>
      command === 'ingest_path' ? summary(report('reconciled'), null) : null,
    );
    useAppStore.setState({ preflight: report('newer scan') });
    await useAppStore.getState().ingest('/repos/app', 'local');
    expect(useAppStore.getState().preflight?.detector).toBe('newer scan');
  });

  it("shows a retried recovery's reconciled report and keeps the job row clean", async () => {
    const job = {
      id: 41, kind: 'ingest-source-v1:src_fixture', status: 'done', execution_tracking: 'recorded',
      created_at: 'c', updated_at: 'u', stage: null, progress: 100,
    };
    mockIPC((command) =>
      command === 'retry_job'
        ? { ...job, recovery: { preflight: report('retried'), preflight_register: { repo: 'local/retry', epoch: 9 } } }
        : null,
    );
    const applyJobEvent = useAppStore.getState().applyJobEvent;
    const applied: unknown[] = [];
    useAppStore.setState({ preflight: report('pending'), applyJobEvent: (row) => applied.push(row) });
    try {
      await useAppStore.getState().retryJob(41);
    } finally {
      useAppStore.setState({ applyJobEvent });
    }

    expect(useAppStore.getState().preflight?.detector).toBe('retried');
    expect(applied).toEqual([job]);
  });
});

// AC-0705..0707 (#1655): a system manifest gets a per-member preview.
describe('system manifest preflight preview', () => {
  const manifestState = { ...reset, ingestSource: 'manifest' as const, ingestTarget: '/sys/cartograph.system.toml' };
  const system: SystemPreflight = {
    members: [
      { kind: 'local', entry: 'api', path: '/sys/api', report: report('preflight@1') },
      { kind: 'deferred', entry: 'https://github.com/example/web' },
    ],
  };

  it('previews the manifest through preflight_system and shows every member', async () => {
    useAppStore.setState(manifestState);
    const calls: Array<{ command: string; args: unknown }> = [];
    mockIPC((command, args) => {
      calls.push({ command, args });
      return system;
    });
    await useAppStore.getState().runPreflight();
    expect(calls.map((call) => call.command)).toEqual(['preflight_system']);
    expect(calls[0].args).toMatchObject({ path: '/sys/cartograph.system.toml' });
    const state = useAppStore.getState();
    expect(state.systemPreflight).toEqual(system);
    expect(state.preflight).toBeNull();
    expect(state.preflightBusy).toBe(false);
  });

  it('shows no partial members when the preview fails or is cancelled', async () => {
    useAppStore.setState({ ...manifestState, systemPreflight: system });
    mockIPC(() => Promise.reject('missing: No such file or directory'));
    await useAppStore.getState().runPreflight();
    let state = useAppStore.getState();
    expect(state.systemPreflight).toBeNull();
    expect(state.preflightError).toBe('missing: No such file or directory');

    mockIPC(() => Promise.reject('cancelled'));
    await useAppStore.getState().runPreflight();
    state = useAppStore.getState();
    expect(state.systemPreflight).toBeNull();
    expect(state.preflightError).toBe('Preflight cancelled. Nothing was recorded.');
  });

  it("ignores a superseded preview's late result", async () => {
    useAppStore.setState(manifestState);
    const pending: Array<(value: SystemPreflight) => void> = [];
    mockIPC(() => new Promise<SystemPreflight>((resolve) => { pending.push(resolve); }));
    const first = useAppStore.getState().runPreflight();
    const second = useAppStore.getState().runPreflight();
    pending[1](system);
    pending[0]({ members: [{ kind: 'deferred', entry: 'stale' }] });
    await Promise.all([first, second]);
    expect(useAppStore.getState().systemPreflight).toEqual(system);
  });

  it('cancels a running preview when the source switches away from the manifest', async () => {
    useAppStore.setState(manifestState);
    const commands: string[] = [];
    let reject: (reason: string) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      if (command === 'cancel_preflight') {
        reject('cancelled');
        return null;
      }
      return new Promise((_resolve, fail) => { reject = fail; });
    });
    const running = useAppStore.getState().runPreflight();
    useAppStore.getState().setIngestSource('local');
    await running;
    expect(commands).toEqual(['preflight_system', 'cancel_preflight']);
    expect(useAppStore.getState().systemPreflight).toBeNull();
    expect(useAppStore.getState().preflightBusy).toBe(false);
  });

  it("ignores a preview that settles after a source switch it already won (Codex P2)", async () => {
    useAppStore.setState(manifestState);
    const commands: string[] = [];
    let resolve: (value: SystemPreflight) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      // The worker already claimed its commit: the cancel loses the race.
      if (command === 'cancel_preflight') return null;
      return new Promise<SystemPreflight>((done) => { resolve = done; });
    });
    const running = useAppStore.getState().runPreflight();
    useAppStore.getState().setIngestSource('local');
    expect(commands).toEqual(['preflight_system', 'cancel_preflight']);
    expect(useAppStore.getState().preflightBusy).toBe(false);

    resolve(system);
    await running;
    const state = useAppStore.getState();
    expect(state.systemPreflight).toBeNull();
    expect(state.preflightError).toBeNull();
    expect(state.preflightBusy).toBe(false);
    expect(state.ingestSource).toBe('local');
  });

  it('runs no preview for a GitHub target', async () => {
    useAppStore.setState({ ...reset, ingestSource: 'github', ingestTarget: 'https://github.com/o/r' });
    const commands: string[] = [];
    mockIPC((command) => {
      commands.push(command);
      return null;
    });
    await useAppStore.getState().runPreflight();
    expect(commands).toEqual([]);
    expect(useAppStore.getState().systemPreflight).toBeNull();
  });
});
