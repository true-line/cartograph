// AC-0287: activated identities, explicit intent and complete bounded replay.
import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { admissionCatalog, admissionDetail, admissionResult, queryHistoryCatalog, queryHistoryDetail, queryHistoryResult, completionChoices, inventoryObligation } from './completionFixtures';
import { validCompletionDetail, validCompletionResult, validCompletionSummary } from './investigationCompletion';
import { investigationPolicy } from './investigationObligations';
import { useInvestigationStore } from './investigationStore';
import supportedWire from './fixtures/completion-v10-supported.json';
import insufficientWire from './fixtures/completion-v10-insufficient.json';
import terminalWire from './fixtures/completion-v10-terminal.json';
import type { InvestigationDetail, InvestigationResult, InvestigationEvent, SpecialistId } from './investigationTypes';

beforeEach(() => { vi.stubGlobal('window', {}); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); });

it('accepts only the exact @10 summary7/result7/disclosure6 mapping', () => {
  const detail = admissionDetail(); const result = admissionResult();
  expect(validCompletionSummary(detail)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
  expect(validCompletionResult(result.completion, result, 56)).toBe(true);
  expect(validCompletionDetail(detail.completion, detail, queryHistoryResult(detail.investigation_id))).toBe(false);
  expect(validCompletionDetail(queryHistoryDetail().completion, queryHistoryDetail(), result)).toBe(false);
  for (const changed of [
    { ...result, result_id: result.result_id.replace('-v7:', '-v6:') },
    { ...result, disclosure_hash: result.disclosure_hash!.replace('-v6:', '-v5:') },
    { ...result, result_id: `${result.result_id}\n` },
    { ...result, disclosure_hash: `${result.disclosure_hash}\n` },
  ]) expect(validCompletionResult(changed.completion, changed, 56)).toBe(false);
  expect(validCompletionSummary({ ...detail, specialist_id: 'domain-analyst@9' })).toBe(false);
  expect(validCompletionSummary({ ...detail, schema_version: 6 })).toBe(false);
  expect(validCompletionSummary({ ...detail, schema_version: '7' as unknown as 7 })).toBe(false);
  expect(validCompletionSummary({ ...detail, specialist_id: 'domain-analyst@11' as SpecialistId })).toBe(false);
});

it.each(['domain-analyst@10', 'evidence-auditor@10'] as const)('requires advertised request7/envelope2/v2 for %s', async (specialist) => {
  expect(investigationPolicy(admissionCatalog, 'investigation-v2', specialist)?.request_schema_version).toBe(7);
  expect(investigationPolicy(queryHistoryCatalog, 'investigation-v2', specialist)).toBeUndefined();
  expect(investigationPolicy(admissionCatalog, 'investigation-v1', specialist)).toBeUndefined();
  const wrong = structuredClone(admissionCatalog); wrong.policies![0].request_schema_version = 6;
  expect(investigationPolicy(wrong, 'investigation-v2', specialist)).toBeUndefined();
  useInvestigationStore.setState({ catalog: admissionCatalog, obligationChoices: completionChoices });
  const starts: unknown[] = [];
  mockIPC((command, args) => { if (command === 'start_investigation') starts.push((args as { request: unknown }).request); throw new Error('uncertain'); });
  await useInvestigationStore.getState().start({ specialist_id: specialist, limit_profile: 'investigation-v2', question: 'Inspect the exact selected evidence.',
    scope: { type: 'all' }, provider_mode: 'local', obligations: { schema_version: 2, contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ schema_version: 7, specialist_id: specialist, expected_context_id: completionChoices.context_id,
    limit_profile: 'investigation-v2', obligations: { schema_version: 2 } });
});

it('never upgrades a frozen @9 nonce when the catalog replaces new Expanded starts with @10', async () => {
  useInvestigationStore.setState({ catalog: queryHistoryCatalog, obligationChoices: completionChoices });
  const store = useInvestigationStore.getState();
  store.updateComposer({ specialistId: 'domain-analyst@9', limitProfile: 'investigation-v2' });
  const starts: unknown[] = [];
  mockIPC((command, args) => {
    if (command === 'investigation_specialists') return admissionCatalog;
    if (command === 'start_investigation') starts.push(structuredClone((args as { request: unknown }).request));
    throw new Error('uncertain');
  });
  await store.start({ specialist_id: 'domain-analyst@9', limit_profile: 'investigation-v2', question: 'Keep original intent.',
    scope: { type: 'all' }, provider_mode: 'local', obligations: { schema_version: 2, contract: 'investigation-completion-obligations@2', obligations: [inventoryObligation] } });
  await store.loadCatalog();
  store.updateComposer({ specialistId: 'domain-analyst@10' });
  await store.retryStart();
  expect(starts).toHaveLength(2); expect(starts[1]).toEqual(starts[0]);
  expect(starts[0]).toMatchObject({ schema_version: 6, specialist_id: 'domain-analyst@9' });
  expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@9');
});

it('reopens all512events and preserves saved data when a later page is missing or exceeds the sequence cap', async () => {
  const detail = admissionDetail(); detail.last_event_sequence = 512;
  const result = admissionResult();
  const events: InvestigationEvent[] = Array.from({ length: 512 }, (_, i) => ({ investigation_id: detail.investigation_id, sequence: i + 1,
    revision: detail.revision, kind: 'capacity_refused', step_id: 'step-1', tool: null, summary: 'Acquisition was not admitted.', created_at: detail.created_at }));
  const cursors: number[] = []; let omit = false;
  mockIPC((command, args) => {
    if (command === 'get_investigation') return detail;
    if (command === 'investigation_result') return result;
    if (command === 'investigation_events') {
      const after = (args as { afterSequence: number }).afterSequence; cursors.push(after);
      const page = events.slice(after, after + 50);
      return { investigation_id: detail.investigation_id, items: omit ? [] : page, next_sequence: omit ? after : page.at(-1)?.sequence ?? after, has_more: after + 50 < events.length };
    }
    return null;
  });
  await useInvestigationStore.getState().open(detail.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(cursors).toHaveLength(11); expect(useInvestigationStore.getState().events).toEqual(events);
  // Reopen starts from an empty event cursor, exercising a missing page instead
  // of only refreshing the already retained tail.
  const savedResult = useInvestigationStore.getState().result;
  detail.last_event_sequence = 513;
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).not.toBeNull();
  expect(useInvestigationStore.getState().events).toEqual(events);
  expect(useInvestigationStore.getState().result).toEqual(savedResult);
  detail.last_event_sequence = 512; omit = true;
  useInvestigationStore.setState({ events: [] });
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).not.toBeNull();
  expect(useInvestigationStore.getState().result).toEqual(savedResult);
});

it('keeps @9 saved history readable without changing its definition under the @10 catalog', async () => {
  const detail = queryHistoryDetail(); detail.last_event_sequence = 0; const result = queryHistoryResult();
  useInvestigationStore.setState({ catalog: admissionCatalog });
  mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result :
    command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: [], next_sequence: 0, has_more: false } : null);
  await useInvestigationStore.getState().open(detail.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().detail).toEqual(detail);
  expect(useInvestigationStore.getState().result).toEqual(result);
});


it.each([['supported', supportedWire], ['insufficient', insufficientWire], ['terminal', terminalWire]] as const)(
  'reopens actual production-worker @10 %s wire without manufacturing identities', async (_name, wire) => {
    const detail = wire.detail as unknown as InvestigationDetail;
    const result = wire.result as unknown as InvestigationResult | null;
    expect(validCompletionSummary(detail)).toBe(true);
    expect(validCompletionDetail(detail.completion, detail, result)).toBe(true);
    if (result) expect(validCompletionResult(result.completion, result, 56)).toBe(true);
    mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result :
      command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: wire.events,
        next_sequence: wire.events.at(-1)!.sequence, has_more: false } : null);
    await useInvestigationStore.getState().open(detail.investigation_id);
    expect(useInvestigationStore.getState().error).toBeNull();
    expect(useInvestigationStore.getState().detail).toEqual(detail);
    expect(useInvestigationStore.getState().result).toEqual(result);
    expect(useInvestigationStore.getState().events).toEqual(wire.events);
  });


it('rejects a higher-revision no-result family, definition or limits rebind while preserving prior history and activity', async () => {
  const saved = queryHistoryDetail('same-unassessed-task');
  saved.status = 'running'; saved.has_result = false; saved.completion_outcome = undefined;
  saved.obligation_status = 'not_assessed'; saved.last_event_sequence = 1;
  saved.completion!.assessment_basis = { kind: 'not_assessed' }; saved.completion!.assessment = [];
  const event: InvestigationEvent = { investigation_id: saved.investigation_id, sequence: 1, revision: saved.revision,
    kind: 'created', step_id: null, tool: null, summary: 'Original saved task.', created_at: saved.created_at };
  let observed = structuredClone(saved);
  mockIPC((command, args) => command === 'get_investigation' ? observed : command === 'investigation_events' ? {
    investigation_id: saved.investigation_id, items: (args as { afterSequence: number }).afterSequence ? [] : [event],
    next_sequence: 1, has_more: false } : null);
  await useInvestigationStore.getState().open(saved.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  for (const field of ['family', 'definition', 'limits']) {
    observed = structuredClone(saved); observed.revision += 1;
    if (field === 'family') {
      observed.schema_version = 7; observed.specialist_id = 'domain-analyst@10';
      observed.specialist = admissionCatalog.specialists.find((item) => item.id === 'domain-analyst@10')!;
    } else if (field === 'definition') observed.specialist.prompt_fingerprint = 'different-saved-definition';
    else observed.limits.active_seconds += 1;
    expect(validCompletionSummary(observed)).toBe(true);
    expect(validCompletionDetail(observed.completion, observed, null)).toBe(true);
    await useInvestigationStore.getState().refreshSelected();
    expect(useInvestigationStore.getState().error).not.toBeNull();
    expect(useInvestigationStore.getState().detail).toEqual(saved);
    expect(useInvestigationStore.getState().history.find((item) => item.investigation_id === saved.investigation_id)).toMatchObject({
      schema_version: 6, specialist_id: 'domain-analyst@9', revision: saved.revision });
    expect(useInvestigationStore.getState().events).toEqual([event]);
    expect(useInvestigationStore.getState().result).toBeNull();
  }
});
