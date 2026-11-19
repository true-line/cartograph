import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { mockIPC, clearMocks } from '@tauri-apps/api/mocks';
import { useInvestigationStore as store } from './investigationStore';
import { investigationResult } from './investigationFixtures';
import { mergeCurations, validCurations, type FindingCuration, type FindingCurations } from './findingCurations';
import type { FindingReview } from './findingReviews';

const result = investigationResult();
const subject = (index: number) => `investigation-finding-review-v1:${String(index).repeat(64)}`;
const reviews: FindingReview[] = result.findings.map((finding, index) => ({
  subject: { investigation_id: result.investigation_id, result_id: result.result_id, finding_id: finding.finding_id, subject_id: subject(index) },
  revision: 0, decision: null, reviewed_at: null,
}));
const curation = (index: number, revision: number, decision: FindingCuration['review_decision'] = 'accepted'): FindingCuration => ({
  subject_id: subject(index), finding_id: result.findings[index].finding_id, review_revision: revision,
  review_decision: decision, reviewed_at: '2026-11-18T09:00:00Z',
});
const snapshot = (rows: FindingCuration[] = []): FindingCurations =>
  ({ schema_version: 1, investigation_id: result.investigation_id, result_id: result.result_id, reviews: rows });
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { resolve, promise }; }

beforeEach(() => {
  vi.stubGlobal('window', {});
  store.setState(store.getInitialState(), true);
  store.setState({ selectedId: result.investigation_id, result, findingReviews: reviews });
});
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); });

it('AC-0846 reads a result with no curation review as no rows, and rejects rows of another result or finding', async () => {
  expect(validCurations(snapshot(), result)).toBe(true);
  expect(validCurations(snapshot([curation(0, 1)]), result)).toBe(true);
  for (const wire of [
    snapshot([curation(0, 1), curation(0, 2)]),
    snapshot([{ ...curation(0, 1), finding_id: 'not-in-result' }]),
    snapshot([{ ...curation(0, 1), review_revision: 0 }]),
    snapshot([{ ...curation(0, 1), review_decision: 'accepted_for_follow_up' as never }]),
    { ...snapshot(), result_id: 'different' },
  ]) {
    expect(validCurations(wire, result)).toBe(false);
    mockIPC(() => wire);
    await store.getState().refreshCurations();
    expect(store.getState().curationError).toMatch(/unavailable/);
    expect(store.getState().result).toEqual(result);
  }
  mockIPC(() => snapshot([curation(1, 2, 'rejected')]));
  await store.getState().refreshCurations();
  expect(store.getState().curationError).toBeNull();
  expect(store.getState().findingCurations).toEqual([curation(1, 2, 'rejected')]);
});

it('AC-0846 sends the review subject with the expected revision and shows only the confirmed write', async () => {
  const requests: unknown[] = [];
  const pending = deferred<FindingCuration>();
  mockIPC((command, args) => {
    expect(command).toBe('curate_investigation_finding');
    requests.push(JSON.parse((args as { requestJson: string }).requestJson));
    return pending.promise;
  });
  const write = store.getState().curateFinding('F1', 'accepted');
  // No optimistic state while the write is out.
  expect(store.getState().findingCurations).toEqual([]);
  expect(store.getState().curationBusy).toBe(true);
  pending.resolve(curation(0, 1));
  await write;
  expect(requests).toEqual([{ schema_version: 1, subject: reviews[0].subject, expected_revision: 0, decision: 'accepted' }]);
  expect(store.getState().findingCurations).toEqual([curation(0, 1)]);
  expect(store.getState().curationBusy).toBe(false);

  // Withdrawing names the revision just saved.
  mockIPC((_command, args) => {
    requests.push(JSON.parse((args as { requestJson: string }).requestJson));
    return curation(0, 2, 'rejected');
  });
  await store.getState().curateFinding('F1', 'rejected');
  expect(requests[1]).toMatchObject({ expected_revision: 1, decision: 'rejected' });
  expect(store.getState().findingCurations).toEqual([curation(0, 2, 'rejected')]);
  // The follow-up review is another store and is untouched.
  expect(store.getState().findingReviews).toEqual(reviews);
});

it('AC-0846 does not write without the review subject, and never retries a conflict, a busy store or a lost reply', async () => {
  let writes = 0;
  mockIPC(() => { writes++; return curation(0, 1); });
  store.setState({ findingReviews: [] });
  await store.getState().curateFinding('F1', 'accepted');
  expect(writes).toBe(0);
  store.setState({ findingReviews: reviews });

  for (const [thrown, message] of [
    [{ code: 'conflict', current_revision: 4 }, /saved first/],
    [{ code: 'busy' }, /busy/],
    [new Error('lost reply'), /could not be confirmed/],
    // A reply that is not the write that was asked for is not shown.
    [null, /could not be confirmed/],
  ] as const) {
    store.setState({ curationError: null, findingCurations: [] });
    writes = 0;
    mockIPC((command) => {
      if (command !== 'curate_investigation_finding') return snapshot([curation(0, 4, 'rejected')]);
      writes++;
      if (thrown === null) return curation(0, 7);
      throw thrown;
    });
    await store.getState().curateFinding('F1', 'accepted');
    expect(store.getState().curationError).toMatch(message);
    expect(store.getState().findingCurations).toEqual([]);
    // Blocked until a read: a second press writes nothing.
    await store.getState().curateFinding('F1', 'accepted');
    expect(writes).toBe(1);
    await store.getState().refreshCurations();
    expect(store.getState().curationError).toBeNull();
    expect(store.getState().findingCurations).toEqual([curation(0, 4, 'rejected')]);
  }
});

it('AC-0846 keeps a committed write over a delayed read, and drops a read for another selection', async () => {
  const stale = deferred<FindingCurations>(); const saved = deferred<FindingCuration>();
  mockIPC((command) => command === 'curate_investigation_finding' ? saved.promise : stale.promise);
  const write = store.getState().curateFinding('F1', 'accepted');
  const read = store.getState().refreshCurations();
  saved.resolve(curation(0, 1)); await write;
  expect(store.getState().curationBusy).toBe(false);
  // The read began before the write committed and saw no row.
  stale.resolve(snapshot()); await read;
  expect(store.getState().findingCurations).toEqual([curation(0, 1)]);

  const late = deferred<FindingCurations>();
  mockIPC(() => late.promise);
  const dropped = store.getState().refreshCurations();
  store.setState({ selectionGeneration: 1 });
  late.resolve(snapshot([curation(0, 9, 'rejected')]));
  await dropped;
  expect(store.getState().findingCurations).toEqual([curation(0, 1)]);

  expect(() => mergeCurations([curation(0, 1)], [curation(0, 1, 'rejected')])).toThrow(/Conflicting/);
  expect(() => mergeCurations([curation(0, 1)], [{ ...curation(0, 2), subject_id: subject(5) }])).toThrow(/subject/);
});

it('AC-0846 reads again on a change event only for a newer revision of the selected result', async () => {
  let reads = 0;
  mockIPC(() => { reads++; return snapshot([curation(0, 2)]); });
  store.setState({ findingCurations: [curation(0, 2)] });
  const event = { investigation_id: result.investigation_id, result_id: result.result_id, subject_id: subject(0), review_revision: 2 };
  store.getState().invalidateCuration(event);
  store.getState().invalidateCuration({ ...event, result_id: 'another' });
  expect(reads).toBe(0);
  store.getState().invalidateCuration({ ...event, review_revision: 3 });
  store.getState().invalidateCuration({ ...event, subject_id: subject(1), review_revision: 1 });
  await vi.waitFor(() => expect(store.getState().curationLoading).toBe(false));
  expect(reads).toBe(2);
});
