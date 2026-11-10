import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useInvestigationStore } from './investigationStore';
import { queryHistoryCatalog, expandedCompletionCatalog, completionChoices, inventoryObligation, sourceObligation } from './completionFixtures';
import { investigationPolicy, obligationEnvelope, validObligationEnvelope, validObligationChoices } from './investigationObligations';
import type { StartInvestigationRequest } from './investigationTypes';

beforeEach(() => { vi.stubGlobal('window', {}); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); });
afterEach(() => { clearMocks(); vi.unstubAllGlobals(); useInvestigationStore.setState(useInvestigationStore.getInitialState(), true); });
const draft = () => ({ specialist_id: 'domain-analyst@7' as const, question: 'Inspect selected evidence.', scope: { type: 'all' as const },
  provider_mode: 'local' as const, limit_profile: 'investigation-v1', obligations: obligationEnvelope([inventoryObligation]) });

describe('explicit mixed obligations (AC-0266)', () => {
  it('freezes generation4 envelope, exact context and nonce across uncertain retry', async () => {
    const starts: StartInvestigationRequest[] = [];
    mockIPC((command, args) => { if (command === 'start_investigation') { starts.push(structuredClone((args as {request: StartInvestigationRequest}).request)); throw new Error('uncertain'); } return null; });
    useInvestigationStore.setState({ obligationChoices: completionChoices });
    const value = draft(); await useInvestigationStore.getState().start(value);
    value.question = 'Changed question'; value.obligations.obligations = [sourceObligation];
    await useInvestigationStore.getState().retryStart();
    expect(starts).toHaveLength(2); expect(starts[0]).toEqual(starts[1]);
    expect(starts[0].schema_version).toBe(4); expect(starts[0].expected_context_id).toBe(completionChoices.context_id);
    expect(starts[0]).not.toHaveProperty('inspection_requirements');
    expect(starts[0].question).toBe('Inspect selected evidence.');
  });
  it('admits at most12 combined selections, rejects conflicts and never changes earlier identities', () => {
    const state = useInvestigationStore.getState(); useInvestigationStore.setState({ obligationChoices: completionChoices });
    expect(state.addObligation(sourceObligation)).toBe(true); expect(state.addObligation(inventoryObligation)).toBe(true);
    if (inventoryObligation.kind !== 'inventory') throw new Error('fixture');
    expect(state.addObligation({ ...inventoryObligation, target: { ...inventoryObligation.target, repo: 'changed' } })).toBe(false);
    expect(state.addObligation({ ...inventoryObligation, obligation_id: '4'.repeat(64), target: { ...inventoryObligation.target, inventory_hash: 'f'.repeat(64) } })).toBe(false);
    expect(state.addObligation({ ...inventoryObligation, obligation_id: '4'.repeat(64), target: { ...inventoryObligation.target, context_id: `context-v2:${'f'.repeat(64)}` } })).toBe(false);
    for (let i = 0; i < 10; i++) expect(state.addObligation({ ...inventoryObligation, obligation_id: (i + 4).toString(16).repeat(64), target: { ...inventoryObligation.target, repo: `repo${i}` } })).toBe(true);
    expect(state.addObligation({ ...inventoryObligation, obligation_id: 'f'.repeat(64) })).toBe(false);
    expect(useInvestigationStore.getState().draftObligations).toHaveLength(12);
  });
  it('bounds the complete serialized request and requires refreshing after scope changes', async () => {
    let starts = 0; mockIPC(() => { starts++; return null; }); useInvestigationStore.setState({ obligationChoices: completionChoices });
    await useInvestigationStore.getState().start({ ...draft(), question: 'x'.repeat(8192) });
    expect(starts).toBe(0); expect(useInvestigationStore.getState().obligationDraftError).toContain('8,192');
    useInvestigationStore.getState().addObligation(inventoryObligation);
    useInvestigationStore.getState().addObligation(sourceObligation);
    useInvestigationStore.getState().updateComposer({ scopeType: 'neighborhood', anchor: 'fact:1' });
    expect(useInvestigationStore.getState().obligationChoices).toBeNull();
    expect(useInvestigationStore.getState().draftObligations).toEqual([]);
    expect(useInvestigationStore.getState().obligationDraftError).toContain('Scope changed');
    await useInvestigationStore.getState().start(draft()); expect(starts).toBe(0);
  });
  it('rejects stale/unrecorded selection claims and malformed choices without legacy fallback', () => {
    const malformed = structuredClone(completionChoices); malformed.inventories[1].obligation = inventoryObligation;
    expect(validObligationChoices(malformed)).toBe(false);
    expect(validObligationChoices(Object.assign(structuredClone(completionChoices), { completion_obligations: [] }))).toBe(false);
  });
  it('keeps both kinds after definitive drift rejection until explicit follow-up clearing', async () => {
    mockIPC(() => { throw { code: 'refresh_required', definitive: true }; }); useInvestigationStore.setState({ obligationChoices: completionChoices });
    useInvestigationStore.getState().addObligation(sourceObligation); useInvestigationStore.getState().addObligation(inventoryObligation);
    await useInvestigationStore.getState().start(draft());
    expect(useInvestigationStore.getState().pendingStart).toBeNull(); expect(useInvestigationStore.getState().draftObligations).toHaveLength(2);
    useInvestigationStore.getState().clearObligations(); expect(useInvestigationStore.getState().draftObligations).toEqual([]);
  });
});

it('loads only coherent generation4 detail/result pairs and rejects altered saved bindings (AC-0266)', async () => {
  const { completionDetail, completionResult } = await import('./completionFixtures');
  let detail = completionDetail('saved', { last_event_sequence: 0 });
  let result = completionResult('saved');
  mockIPC((command) => {
    if (command === 'get_investigation') return detail;
    if (command === 'investigation_result') return result;
    if (command === 'investigation_consent') return null;
    if (command === 'investigation_events') return { investigation_id: 'saved', items: [], next_sequence: 0, has_more: false };
    return null;
  });
  await useInvestigationStore.getState().open('saved');
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().result?.schema_version).toBe(5);
  result = structuredClone(result); result.completion!.obligations_hash = 'f'.repeat(64);
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).toContain('consistently');
  expect(useInvestigationStore.getState().result?.completion?.obligations_hash).toBe('b'.repeat(64));
  result = completionResult('saved'); detail = completionDetail('saved', { last_event_sequence: 0 });
  Object.assign(detail.completion!, { obligations_alias: detail.completion!.obligations });
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).toContain('consistently');
});

it('reopens a saved @17 summary/result from its exact family without a catalog promotion (AC-0461)', async () => {
  const { admissionDetail, admissionResult } = await import('./completionFixtures');
  const { completionFamily, isCompletionSpecialist, isExpandedSpecialist } = await import('./investigationFamilies');
  const detail = admissionDetail('saved-analyst-17');
  detail.last_event_sequence = 0;
  detail.specialist_id = 'domain-analyst@17';
  detail.specialist = { ...detail.specialist, id: 'domain-analyst@17', version: 17,
    prompt_fingerprint: 'fixture-domain-analyst@17' };
  const result = admissionResult('saved-analyst-17');
  mockIPC((command) => {
    if (command === 'get_investigation') return detail;
    if (command === 'investigation_result') return result;
    if (command === 'investigation_consent') return null;
    if (command === 'investigation_events') return { investigation_id: 'saved-analyst-17', items: [], next_sequence: 0, has_more: false };
    return null;
  });

  const family = completionFamily(7);
  expect(family?.specialistIds).toContain('domain-analyst@17');
  expect(family).toMatchObject({ roleVersions: expect.arrayContaining([17]), requestVersion: 7, resultVersion: 7 });
  expect(isCompletionSpecialist('domain-analyst@17')).toBe(true);
  expect(isExpandedSpecialist('domain-analyst@17')).toBe(true);
  await useInvestigationStore.getState().open('saved-analyst-17');
  expect(useInvestigationStore.getState().catalog).toBeNull();
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().detail?.specialist).toMatchObject({ id: 'domain-analyst@17', version: 17 });
  expect(useInvestigationStore.getState().result).toMatchObject({ schema_version: 7, result_id: `investigation-result-v7:${'7'.repeat(64)}` });
});

it('does not resurrect a source selection after the user clears or changes its context (AC-0266)', async () => {
  let release!: (value: typeof sourceObligation) => void;
  mockIPC((command) => command === 'resolve_investigation_source_obligation'
    ? new Promise((resolve) => { release = resolve; }) : null);
  useInvestigationStore.setState({ obligationChoices: completionChoices });
  const pending = useInvestigationStore.getState().addSourceObligation(sourceObligation.target);
  useInvestigationStore.getState().clearObligations();
  useInvestigationStore.setState({ obligationChoices: completionChoices });
  release(sourceObligation);
  expect(await pending).toBe(false);
  expect(useInvestigationStore.getState().draftObligations).toEqual([]);
});


it('never decodes a generation5 result as legacy history (AC-0266)', async () => {
  const { investigationDetail } = await import('./investigationFixtures');
  const { completionResult } = await import('./completionFixtures');
  const detail = investigationDetail('legacy', { last_event_sequence: 0, graph_snapshot_id: sourceObligation.target.graph_snapshot_id });
  mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? completionResult('legacy') :
    command === 'investigation_events' ? { investigation_id: 'legacy', items: [], next_sequence: 0, has_more: false } : null);
  await useInvestigationStore.getState().open('legacy');
  expect(useInvestigationStore.getState().result).toBeNull();
  expect(useInvestigationStore.getState().error).toContain('consistently');
});

// AC-0266: changing scope while resolving choices cannot admit an old source
// into the newly selected neighborhood, even if fresh choices arrive first.
it('abandons source selection when scope changes during choice resolution', async () => {
  let release!: (value: typeof completionChoices) => void;
  let sourceResolutions = 0;
  mockIPC((command) => {
    if (command === 'investigation_obligation_choices') return new Promise((resolve) => { release = resolve; });
    if (command === 'resolve_investigation_source_obligation') { sourceResolutions++; return sourceObligation; }
    return null;
  });
  const pending = useInvestigationStore.getState().addSourceObligation(sourceObligation.target);
  useInvestigationStore.getState().updateComposer({ scopeType: 'neighborhood', anchor: 'other-fact' });
  useInvestigationStore.setState({ obligationChoices: structuredClone(completionChoices) });
  release(completionChoices);
  expect(await pending).toBe(false);
  expect(sourceResolutions).toBe(0);
  expect(useInvestigationStore.getState().draftObligations).toEqual([]);
});

it('keeps expanded envelopes separate from legacy admission bounds (AC-0281)', () => {
  const policy = expandedCompletionCatalog.policies![0];
  const selections = Array.from({ length: 13 }, (_, index) => ({ ...inventoryObligation,
    obligation_id: (index + 1).toString(16).padStart(64, '0'), target: { ...inventoryObligation.target, repo: `repo-${index}` } }));
  const expanded = obligationEnvelope(selections, policy);
  expect(expanded.schema_version).toBe(2);
  expect(validObligationEnvelope(expanded, policy)).toBe(true);
  expect(validObligationEnvelope(expanded)).toBe(false);
  expect(validObligationEnvelope(obligationEnvelope(selections))).toBe(false);
  expect(validObligationEnvelope({ ...expanded, schema_version: 1 }, policy)).toBe(false);
});


it('resolves request family from exact advertised role without reinterpreting the numeric profile (AC-0286)', () => {
  expect(investigationPolicy(expandedCompletionCatalog, 'investigation-v2', 'domain-analyst@8')?.request_schema_version).toBe(5);
  expect(investigationPolicy(queryHistoryCatalog, 'investigation-v2', 'domain-analyst@9')?.request_schema_version).toBe(6);
  expect(queryHistoryCatalog.policies![0].limits).toEqual(expandedCompletionCatalog.policies![0].limits);
  expect(investigationPolicy(queryHistoryCatalog, 'investigation-v2', 'domain-analyst@8')).toBeUndefined();
  expect(investigationPolicy(expandedCompletionCatalog, 'investigation-v2', 'domain-analyst@9')).toBeUndefined();
  expect(investigationPolicy(queryHistoryCatalog, 'investigation-v1', 'domain-analyst@9')).toBeUndefined();
  const changed = structuredClone(queryHistoryCatalog); changed.policies![0].request_schema_version = 5;
  expect(investigationPolicy(changed, 'investigation-v2', 'domain-analyst@9')).toBeUndefined();
  const oldChanged = structuredClone(expandedCompletionCatalog); oldChanged.policies![0].request_schema_version = 6;
  expect(investigationPolicy(oldChanged, 'investigation-v2', 'domain-analyst@8')).toBeUndefined();
});


it('constructs request7 only after explicit advertised @18 selection without changing defaults (AC-0553)', async () => {
  const { admissionCatalog, acquisitionCatalog } = await import('./completionFixtures');
  expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@3');
  expect(acquisitionCatalog.specialists.slice(0, -1)).toEqual(admissionCatalog.specialists);
  expect(acquisitionCatalog.policies![0].limits).toEqual(admissionCatalog.policies![0].limits);
  expect(acquisitionCatalog.policies![0].specialist_ids.slice(0, -1)).toEqual(admissionCatalog.policies![0].specialist_ids);
  expect(investigationPolicy(admissionCatalog, 'investigation-v2', 'domain-analyst@18')).toBeUndefined();
  expect(investigationPolicy(acquisitionCatalog, 'investigation-v1', 'domain-analyst@18')).toBeUndefined();
  const wrong = structuredClone(acquisitionCatalog); wrong.policies![0].request_schema_version = 6;
  expect(investigationPolicy(wrong, 'investigation-v2', 'domain-analyst@18')).toBeUndefined();
  const starts: StartInvestigationRequest[] = [];
  mockIPC((command, args) => { if (command === 'start_investigation') { starts.push((args as {request: StartInvestigationRequest}).request); throw new Error('uncertain'); } return null; });
  const selected = { ...draft(), specialist_id: 'domain-analyst@18' as const, limit_profile: 'investigation-v2',
    obligations: obligationEnvelope([inventoryObligation], acquisitionCatalog.policies![0]) };
  useInvestigationStore.setState({ catalog: admissionCatalog, obligationChoices: completionChoices });
  await useInvestigationStore.getState().start(selected);
  expect(starts).toHaveLength(0);
  useInvestigationStore.setState({ catalog: acquisitionCatalog });
  await useInvestigationStore.getState().start(selected);
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ schema_version: 7, specialist_id: 'domain-analyst@18', limit_profile: 'investigation-v2',
    obligations: { schema_version: 2, contract: 'investigation-completion-obligations@2' }, expected_context_id: completionChoices.context_id });
  expect(useInvestigationStore.getState().composer.specialistId).toBe('domain-analyst@3');
});

it('recognizes saved @18 and refuses a mismatched summary schema family (AC-0553)', async () => {
  const { admissionDetail, admissionResult } = await import('./completionFixtures');
  const { completionFamily, isCompletionSpecialist, isExpandedSpecialist } = await import('./investigationFamilies');
  let detail = admissionDetail('saved-analyst-18');
  detail.last_event_sequence = 0; detail.specialist_id = 'domain-analyst@18';
  detail.specialist = { ...detail.specialist, id: 'domain-analyst@18', version: 18, prompt_fingerprint: 'fixture-domain-analyst@18' };
  const result = admissionResult('saved-analyst-18');
  mockIPC((command) => command === 'get_investigation' ? detail : command === 'investigation_result' ? result :
    command === 'investigation_events' ? { investigation_id: detail.investigation_id, items: [], next_sequence: 0, has_more: false } : null);
  expect(completionFamily(7)?.roleVersions).toContain(18);
  expect(isCompletionSpecialist('domain-analyst@18')).toBe(true);
  expect(isExpandedSpecialist('domain-analyst@18')).toBe(true);
  await useInvestigationStore.getState().open(detail.investigation_id);
  expect(useInvestigationStore.getState().error).toBeNull();
  expect(useInvestigationStore.getState().detail?.specialist).toMatchObject({ id: 'domain-analyst@18', version: 18 });
  expect(useInvestigationStore.getState().result).toMatchObject({ schema_version: 7 });
  detail = structuredClone(detail); detail.schema_version = 6;
  await useInvestigationStore.getState().refreshSelected();
  expect(useInvestigationStore.getState().error).toContain('consistently');
  expect(useInvestigationStore.getState().detail?.specialist.version).toBe(18);
});

// AC-0585: the new auditor is explicitly advertised; saved @11 remains distinct.
it('recognizes auditor12 only in the advertised generation7 family', async () => {
  const { completionFamily, isCompletionSpecialist, isExpandedSpecialist } = await import('./investigationFamilies');
  expect(isCompletionSpecialist('evidence-auditor@12')).toBe(true);
  expect(isExpandedSpecialist('evidence-auditor@12')).toBe(true);
  expect(completionFamily(7)?.specialistIds).toContain('evidence-auditor@12');
  expect(completionFamily(7)?.specialistIds).toContain('evidence-auditor@11');
  expect(completionFamily(6)?.specialistIds).not.toContain('evidence-auditor@12');
  expect(investigationPolicy(null, 'investigation-v1', 'evidence-auditor@12')).toBeUndefined();
  const catalog = structuredClone(queryHistoryCatalog);
  const policy = catalog.policies![0];
  policy.specialist_ids = ['evidence-auditor@12'];
  policy.request_schema_version = 7;
  expect(investigationPolicy(catalog, 'investigation-v2', 'evidence-auditor@12')).toBe(policy);
});
