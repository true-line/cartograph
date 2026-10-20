import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { BUSY_REASON, dropSetStatus, summaryRepos, toDropItems, type DropItem } from './dropIntake';
import { useAppStore, type IngestSummary } from './store';

const item = (status: DropItem['status']): DropItem => ({
  path: `/drop/${status}`,
  source: status === 'rejected' ? null : 'local',
  status,
  reason: null,
  repos: [],
});

describe('drop set status (AC-0237, T-0237)', () => {
  it('is complete only when every dropped path ingested', () => {
    expect(dropSetStatus([item('ingested')])).toEqual({
      complete: true,
      unresolved: 0,
      label: 'Dropped folder ingested',
    });
    expect(dropSetStatus([item('ingested'), item('ingested')]).label).toBe(
      'All 2 dropped folders ingested',
    );
    for (const other of ['queued', 'ingesting', 'rejected', 'failed'] as const) {
      expect(dropSetStatus([item('ingested'), item(other)]).complete).toBe(false);
    }
    expect(dropSetStatus([]).complete).toBe(false);
  });

  it('names every unresolved, rejected and failed path in the label', () => {
    const status = dropSetStatus([
      item('ingested'),
      item('queued'),
      item('ingesting'),
      item('rejected'),
      item('failed'),
    ]);
    expect(status).toEqual({
      complete: false,
      unresolved: 2,
      label: '1 of 5 ingested · 2 in progress · 1 rejected · 1 failed',
    });
  });

  it('queues accepted candidates and keeps each rejection reason', () => {
    expect(
      toDropItems([
        { path: '/a', source: 'local', reason: null },
        { path: '/b.txt', source: null, reason: 'not a folder' },
      ]).map(({ status, reason }) => [status, reason]),
    ).toEqual([
      ['queued', null],
      ['rejected', 'not a folder'],
    ]);
  });

  it('lists each repo with its commit identity', () => {
    const base = { job_id: 1 } as IngestSummary;
    expect(summaryRepos({ ...base, repo: 'local/app' })).toEqual(['local/app@workdir']);
    expect(summaryRepos({ ...base, repo: 'acme/api', commit_sha: '0123456789abcdef' })).toEqual([
      'acme/api@0123456789ab',
    ]);
    expect(summaryRepos({ ...base, repos: ['local/a@workdir', 'acme/b@abc'] })).toEqual([
      'local/a@workdir',
      'acme/b@abc',
    ]);
    expect(summaryRepos(null)).toEqual([]);
  });
});

describe('dropped folders ingest through the typed-path commands (AC-0237, T-0237)', () => {
  beforeEach(() => {
    vi.stubGlobal('window', {});
    useAppStore.setState({ dropIntake: null, ingestBusy: false, ingestError: null });
  });

  afterEach(() => {
    clearMocks();
    vi.unstubAllGlobals();
    useAppStore.setState({ dropIntake: null, ingestBusy: false, ingestError: null });
  });

  it('gives each folder an explicit outcome and never reports a partial set complete', async () => {
    const calls: [string, unknown][] = [];
    let observedMidway: string | null = null;
    mockIPC((command, args) => {
      if (command === 'classify_drop') {
        return [
          { path: '/repos/app', source: 'local', reason: null },
          { path: '/repos/notes.txt', source: null, reason: 'not a folder' },
          { path: '/repos/system', source: 'manifest', reason: null },
          { path: '/repos/broken', source: 'local', reason: null },
        ];
      }
      if (command === 'ingest_path' || command === 'add_system') {
        calls.push([command, args]);
        const { path } = args as { path: string };
        observedMidway ??= dropSetStatus(useAppStore.getState().dropIntake ?? []).label;
        if (path === '/repos/broken') return Promise.reject('permission denied');
        return command === 'add_system'
          ? { job_id: 2, repos: ['local/api@workdir', 'local/web@workdir'] }
          : { job_id: 1, repo: 'local/app' };
      }
      return null;
    });

    await useAppStore.getState().ingestDropped(['/repos/app', '/repos/notes.txt', '/repos/system', '/repos/broken']);

    // The same commands a typed local path or manifest uses, in drop order.
    expect(calls).toEqual([
      ['ingest_path', { path: '/repos/app' }],
      ['add_system', { path: '/repos/system' }],
      ['ingest_path', { path: '/repos/broken' }],
    ]);
    expect(observedMidway).toBe('0 of 4 ingested · 3 in progress · 1 rejected');
    const items = useAppStore.getState().dropIntake ?? [];
    expect(items.map(({ status, reason, repos }) => [status, reason, repos])).toEqual([
      ['ingested', null, ['local/app@workdir']],
      ['rejected', 'not a folder', []],
      ['ingested', null, ['local/api@workdir', 'local/web@workdir']],
      ['failed', 'permission denied', []],
    ]);
    expect(dropSetStatus(items)).toEqual({
      complete: false,
      unresolved: 0,
      label: '2 of 4 ingested · 1 rejected · 1 failed',
    });
  });

  it('rejects a drop that arrives while intake is still running, visibly', async () => {
    useAppStore.setState({ dropIntake: [item('ingesting')] });
    mockIPC(() => null);
    await useAppStore.getState().ingestDropped(['/repos/late']);
    const items = useAppStore.getState().dropIntake ?? [];
    expect(items.map(({ path, status, reason }) => [path, status, reason])).toEqual([
      ['/drop/ingesting', 'ingesting', null],
      ['/repos/late', 'rejected', BUSY_REASON],
    ]);
    // An unresolved drop cannot be dismissed out of sight.
    useAppStore.getState().dismissDropIntake();
    expect(useAppStore.getState().dropIntake).toHaveLength(2);
  });

  it('reserves the intake before classification so a concurrent drop cannot race it', async () => {
    let release: (value: unknown) => void = () => {};
    mockIPC((command, args) => {
      if (command === 'classify_drop') {
        const { paths } = args as { paths: string[] };
        if (paths[0] === '/a') {
          return new Promise((resolve) => {
            release = resolve;
          }).then(() => [{ path: '/a', source: 'local', reason: null }]);
        }
        return [{ path: '/b', source: 'local', reason: null }];
      }
      if (command === 'ingest_path') {
        const { path } = args as { path: string };
        return { job_id: 1, repo: path === '/a' ? 'local/a' : 'local/b' };
      }
      return null;
    });
    const first = useAppStore.getState().ingestDropped(['/a']);
    await useAppStore.getState().ingestDropped(['/b']);
    release(null);
    await first;
    const items = useAppStore.getState().dropIntake ?? [];
    expect(items.map(({ path, status, reason, repos }) => [path, status, reason, repos])).toEqual([
      ['/a', 'ingested', null, ['local/a@workdir']],
      ['/b', 'rejected', BUSY_REASON, []],
    ]);
    expect(dropSetStatus(items).complete).toBe(false);
  });

  it('settles every item when the graph refresh after an ingest rejects', async () => {
    mockIPC((command) => {
      if (command === 'classify_drop') {
        return [
          { path: '/a', source: 'local', reason: null },
          { path: '/b', source: 'local', reason: null },
        ];
      }
      if (command === 'ingest_path') return { job_id: 1, repo: 'local/a' };
      return null;
    });
    const original = useAppStore.getState().refresh;
    useAppStore.setState({ refresh: vi.fn().mockRejectedValue('store locked') });
    try {
      await useAppStore.getState().ingestDropped(['/a', '/b']);
    } finally {
      useAppStore.setState({ refresh: original });
    }
    const items = useAppStore.getState().dropIntake ?? [];
    expect(items.map(({ status, reason }) => [status, reason])).toEqual([
      ['ingested', 'ingested; graph refresh failed: store locked'],
      ['ingested', 'ingested; graph refresh failed: store locked'],
    ]);
    expect(dropSetStatus(items).unresolved).toBe(0);
  });

  it('ingests a dropped path exactly as the OS gave it', async () => {
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'classify_drop') {
        return [{ path: '/repos/project ', source: 'local', reason: null }];
      }
      if (command === 'ingest_path') {
        calls.push(args);
        return { job_id: 1, repo: 'local/project ' };
      }
      return null;
    });
    await useAppStore.getState().ingestDropped(['/repos/project ']);
    expect(calls).toEqual([{ path: '/repos/project ' }]);
  });
});
