import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { mockIPC, clearMocks } from '@tauri-apps/api/mocks';
import { useInvestigationStore as store } from './investigationStore';
import { investigationResult } from './investigationFixtures';
import { validReviews, type FindingReview, type FindingReviews } from './findingReviews';
const result = investigationResult();
const rows = (revision = 0): FindingReview[] => result.findings.map((finding, index) => ({
  subject: { investigation_id: result.investigation_id, result_id: result.result_id, finding_id: finding.finding_id,
    subject_id: `investigation-finding-review-v1:${String(index).repeat(64)}` }, revision,
  decision: revision ? 'accepted_for_follow_up' : null, reviewed_at: revision ? '2026-10-21T00:00:00Z' : null,
}));
const snapshot = (reviews = rows()): FindingReviews => ({ schema_version: 1, investigation_id: result.investigation_id, result_id: result.result_id, reviews });
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { resolve, promise }; }
beforeEach(() => { vi.stubGlobal('window', {}); store.setState(store.getInitialState(), true); store.setState({ selectedId: result.investigation_id, result }); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); });
it('AC-0295 rejects incomplete, duplicate or mixed result joins without hiding the historical result', async () => {
  for (const wire of [snapshot([]), snapshot([rows()[0], rows()[0]]), { ...snapshot(), result_id: 'different' }]) {
    expect(validReviews(wire, result)).toBe(false);
    mockIPC(() => wire);
    await store.getState().refreshReviews();
    expect(store.getState().result).toEqual(result);
    expect(store.getState().reviewError).toMatch(/unavailable/);
  }
});
it('AC-0295 ignores a delayed pending snapshot after a committed mutation, without optimistic state', async () => {
  store.setState({ findingReviews: rows() });
  const pending = deferred<FindingReviews>(); const mutation = deferred<FindingReview>();
  mockIPC((command) => command === 'review_investigation_finding' ? mutation.promise : pending.promise);
  const write = store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  expect(store.getState().findingReviews[0].revision).toBe(0);
  const read = store.getState().refreshReviews();
  mutation.resolve(rows(1)[0]); await write;
  pending.resolve(snapshot()); await read;
  expect(store.getState().findingReviews[0].revision).toBe(1);
});
it('AC-0295 ignores late same-ID reopen and navigation metadata', async () => {
  const pending = deferred<FindingReviews>(); mockIPC(() => pending.promise);
  const read = store.getState().refreshReviews();
  store.setState({ selectionGeneration: 1, findingReviews: rows(2) });
  pending.resolve(snapshot()); await read;
  expect(store.getState().findingReviews[0].revision).toBe(2);
});
it('AC-0295 recovers lost acknowledgement by reading, with exactly one write', async () => {
  store.setState({ findingReviews: rows() }); let writes = 0;
  mockIPC((command) => { if (command === 'review_investigation_finding') { writes++; throw new Error('lost ack'); } return snapshot(rows(1)); });
  await store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  expect(store.getState().reviewError).toMatch(/could not be confirmed/);
  expect(store.getState().findingReviews[0].revision).toBe(0);
  await store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  await store.getState().refreshReviews();
  expect(writes).toBe(1); expect(store.getState().findingReviews[0].revision).toBe(1);
  expect(store.getState().reviewError).toBeNull(); expect(store.getState().result).toEqual(result);
});
it('AC-0295 shows conflict winner and requires refresh and a deliberate action', async () => {
  store.setState({ findingReviews: rows() }); let writes = 0;
  const winner = { ...rows(1)[0], decision: 'rejected_for_follow_up' as const };
  mockIPC((command, args) => {
    if (command !== 'review_investigation_finding') return snapshot([winner, rows()[1]]);
    writes++;
    if (writes === 1) throw { code: 'conflict', current: winner };
    const request = JSON.parse((args as Record<string, string>).requestJson);
    expect(request.expected_revision).toBe(1);
    return rows(2)[0];
  });
  await store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  expect(store.getState().findingReviews[0]).toEqual(winner);
  expect(store.getState().reviewError).toMatch(/Another decision won/);
  await store.getState().refreshReviews();
  await store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  expect(writes).toBe(2); expect(store.getState().findingReviews[0].revision).toBe(2);
});
it('AC-0295 preserves last decision on read failure and prevents an older in-flight read replacing a newer read', async () => {
  store.setState({ findingReviews: rows(1) });
  const older = deferred<FindingReviews>(); let reads = 0;
  mockIPC(() => ++reads === 1 ? older.promise : snapshot(rows(2)));
  const oldRead = store.getState().refreshReviews();
  await store.getState().refreshReviews();
  older.resolve(snapshot()); await oldRead;
  expect(store.getState().findingReviews[0].revision).toBe(2);
  mockIPC(() => { throw new Error('unavailable'); });
  await store.getState().refreshReviews();
  expect(store.getState().findingReviews[0].revision).toBe(2);
  expect(store.getState().reviewError).toMatch(/unavailable/);
  expect(store.getState().result).toEqual(result);
});
it('AC-0295 ignores a late failed write from a previous selection generation', async () => {
  store.setState({ findingReviews: rows() });
  const mutation = deferred<FindingReview>(); mockIPC(() => mutation.promise);
  const write = store.getState().reviewFinding(result.findings[0].finding_id, 'accepted_for_follow_up');
  store.setState({ selectionGeneration: 1, findingReviews: rows(2), reviewBusy: false });
  mutation.resolve(rows(1)[0]); await write;
  expect(store.getState().findingReviews[0].revision).toBe(2);
  expect(store.getState().reviewBusy).toBe(false);
});
it('AC-0295 review events recover through an exact-result metadata read only', async () => {
  const commands: string[] = [];
  mockIPC((command, args) => { commands.push(command); expect(args).toEqual({ investigationId: result.investigation_id, expectedResultId: result.result_id }); return snapshot(rows(1)); });
  store.getState().invalidateReview(rows(1)[0]);
  await vi.waitFor(() => expect(store.getState().findingReviews[0]?.revision).toBe(1));
  expect(commands).toEqual(['get_investigation_finding_reviews']);
});
it('AC-0295 accepts reordered metadata keys without inventing a conflict', async () => {
  store.setState({ findingReviews: rows(1) });
  const reordered = rows(1).map((r) => ({ reviewed_at: r.reviewed_at, decision: r.decision, revision: r.revision,
    subject: { subject_id: r.subject.subject_id, finding_id: r.subject.finding_id, result_id: r.subject.result_id, investigation_id: r.subject.investigation_id } }));
  mockIPC(() => snapshot(reordered));
  await store.getState().refreshReviews();
  expect(store.getState().reviewError).toBeNull();
});
it('AC-0295 rejects malformed canonical timestamps and equal-revision contradictions', async () => {
  for (const reviewed_at of ['2026-02-30T00:00:00Z', '2026-10-21T24:00:00Z', 'yesterday', '2026-10-21T00:00:00+00:00']) {
    expect(validReviews(snapshot([{ ...rows(1)[0], reviewed_at }, rows(1)[1]]), result)).toBe(false);
  }
  store.setState({ findingReviews: rows(1) });
  mockIPC(() => snapshot([{ ...rows(1)[0], decision: 'rejected_for_follow_up' }, rows(1)[1]]));
  await store.getState().refreshReviews();
  expect(store.getState().findingReviews).toEqual(rows(1));
  expect(store.getState().reviewError).toMatch(/unavailable/);
});
