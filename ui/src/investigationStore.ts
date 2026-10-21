import { create } from 'zustand';
import { invokeOr } from './tauri';
import type {
  InvestigationCatalog, InvestigationChanged, InvestigationCitationRead,
  InvestigationConsent, InvestigationDetail, InvestigationEvent,
  InvestigationEventPage, InvestigationPage, InvestigationResult,
  InvestigationSummary, StartInvestigationRequest, InvestigationInspectionTarget, InvestigationCapturedLocation,
  SpecialistId, InvestigationProviderMode,
} from './investigationTypes';

export type InvestigationDraft = Omit<StartInvestigationRequest, 'schema_version' | 'request_nonce'>;
export interface InvestigationComposer {
  specialistId: SpecialistId;
  providerMode: InvestigationProviderMode;
  question: string;
  scopeType: 'all' | 'neighborhood';
  anchor: string;
  hops: number;
  parent: { conversation_id: string; parent_id: string } | null;
}
const initialComposer: InvestigationComposer = {
  specialistId: 'domain-analyst@3', providerMode: 'local', question: '',
  scopeType: 'all', anchor: '', hops: 1, parent: null,
};
const TERMINAL = new Set(['completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown']);
export function investigationIsActive(summary: InvestigationSummary): boolean {
  return !TERMINAL.has(summary.status) || summary.invocation_pending;
}

const LEGACY_SPECIALISTS = new Set<SpecialistId>([
  'domain-analyst@1', 'evidence-auditor@1', 'domain-analyst@2', 'evidence-auditor@2',
  'domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4',
]);
function validSummary(value: InvestigationSummary | null, id?: string): value is InvestigationSummary {
  if (value === null || typeof value.investigation_id !== 'string' || value.investigation_id.length === 0 ||
      (id && value.investigation_id !== id) || !Number.isSafeInteger(value.revision) || value.revision < 0 ||
      !Number.isSafeInteger(value.last_event_sequence) || value.last_event_sequence < 0 ||
      value.last_event_sequence > 96 || typeof value.actions?.can_cancel !== 'boolean' ||
      typeof value.actions?.can_follow_up !== 'boolean' || typeof value.has_result !== 'boolean') return false;
  if (value.schema_version === 1) {
    return LEGACY_SPECIALISTS.has(value.specialist_id) && value.inspection_status === undefined &&
      value.inspection_requirement_count === undefined && value.inspection_requirements === undefined &&
      value.inspection_assessment === undefined;
  }
  const requirementCount = value.inspection_requirement_count;
  return value.schema_version === 2 &&
    (value.specialist_id === 'domain-analyst@5' || value.specialist_id === 'evidence-auditor@5') &&
    typeof requirementCount === 'number' && Number.isSafeInteger(requirementCount) && requirementCount >= 1 &&
    requirementCount <= 12 && (value.has_result
      ? value.inspection_status === 'supplied' || value.inspection_status === 'unmet'
      : value.inspection_status === (investigationIsActive(value) ? 'pending' : 'not_assessed'));
}

/** Host validates result content; the UI also rejects malformed version bindings. */
function validResultBinding(result: InvestigationResult): boolean {
  if (result.schema_version === 1) {
    return !('invocation_step_id' in result) && !('disclosure_hash' in result) && !('inspection_assessment' in result);
  }
  const disclosureBound = (result.schema_version === 2 || result.schema_version === 3) && typeof result.invocation_step_id === 'string' &&
    /^[A-Za-z0-9_:-]{1,128}(?![\s\S])/.test(result.invocation_step_id) &&
    typeof result.disclosure_hash === 'string' &&
    /^investigation-disclosure-v1:[0-9a-f]{64}(?![\s\S])/.test(result.disclosure_hash);
  if (!disclosureBound) return false;
  if (result.schema_version === 2) return !('inspection_assessment' in result);
  const assessment = result.inspection_assessment;
  return assessment?.schema_version === 1 && assessment.input_ledger_hash === result.input_ledger_hash &&
    assessment.finishing_step_id === result.invocation_step_id && assessment.disclosure_hash === result.disclosure_hash &&
    Array.isArray(assessment.requirements) && assessment.requirements.length > 0 && assessment.requirements.length <= 12 &&
    new Set(assessment.requirements.map((item) => item.target_id)).size === assessment.requirements.length &&
    assessment.requirements.every((item) => item.target_id === item.target.target_id &&
      item.target.graph_snapshot_id === result.graph_snapshot_id && typeof item.acquired === 'boolean' &&
      typeof item.supplied === 'boolean' && typeof item.cited === 'boolean' &&
      (item.status === 'supplied' ? item.supplied : item.status === 'unmet' && !item.supplied));
}

function mergeHistory(existing: InvestigationSummary[], incoming: InvestigationSummary[]) {
  const byId = new Map(existing.map((item) => [item.investigation_id, item]));
  for (const item of incoming) {
    const previous = byId.get(item.investigation_id);
    if (!previous || item.revision >= previous.revision) byId.set(item.investigation_id, item);
  }
  return [...byId.values()];
}

/** A duplicate must agree exactly; a hole is not silently compressed into a log. */
export function mergeInvestigationEvents(
  id: string, current: InvestigationEvent[], incoming: InvestigationEvent[],
): InvestigationEvent[] {
  const bySequence = new Map(current.map((item) => [item.sequence, item]));
  for (const item of incoming) {
    if (item.investigation_id !== id || !Number.isSafeInteger(item.sequence) ||
        item.sequence < 1 || item.sequence > 96) throw new Error('Invalid event identity.');
    const previous = bySequence.get(item.sequence);
    if (previous && JSON.stringify(previous) !== JSON.stringify(item)) throw new Error('Conflicting event.');
    bySequence.set(item.sequence, item);
  }
  const result = [...bySequence.values()].sort((a, b) => a.sequence - b.sequence);
  if (result.some((item, index) => item.sequence !== index + 1)) throw new Error('Missing event.');
  return result;
}

export interface InvestigationState {
  catalog: InvestigationCatalog | null;
  catalogLoading: boolean;
  catalogError: string | null;
  history: InvestigationSummary[];
  nextCursor: string | null;
  historyLoading: boolean;
  historyError: string | null;
  selectedId: string | null;
  selectionGeneration: number;
  detail: InvestigationDetail | null;
  result: InvestigationResult | null;
  events: InvestigationEvent[];
  consent: InvestigationConsent | null;
  consentOpen: boolean;
  loading: boolean;
  error: string | null;
  actionBusy: boolean;
  actionError: string | null;
  pendingStart: StartInvestigationRequest | null;
  starting: boolean;
  startError: string | null;
  composer: InvestigationComposer;
  updateComposer: (patch: Partial<InvestigationComposer>) => void;
  draftInspectionRequirements: InvestigationInspectionTarget[];
  inspectionDraftError: string | null;
  addInspectionRequirement: (target: InvestigationInspectionTarget) => boolean;
  removeInspectionRequirement: (targetId: string) => void;
  clearInspectionRequirements: () => void;
  inspectionLocation: InvestigationCapturedLocation | null;
  inspectionLocationTargetId: string | null;
  inspectionLocationLoading: boolean;
  inspectionLocationError: string | null;
  readInspectionLocation: (target: InvestigationInspectionTarget) => Promise<void>;
  citationId: string | null;
  citationRead: InvestigationCitationRead | null;
  citationLoading: boolean;
  citationError: string | null;
  loadCatalog: () => Promise<void>;
  loadHistory: (more?: boolean) => Promise<void>;
  start: (draft: InvestigationDraft) => Promise<void>;
  retryStart: () => Promise<void>;
  open: (id: string) => Promise<void>;
  refreshSelected: () => Promise<void>;
  invalidate: (message: InvestigationChanged) => void;
  showConsent: () => void;
  hideConsent: () => void;
  approve: () => Promise<void>;
  decline: () => Promise<void>;
  cancel: (id?: string) => Promise<void>;
  readCitation: (citationId: string) => Promise<void>;
  clearCitation: () => void;
}

let catalogRequest = 0;
let historyRequest = 0;
let detailRequest = 0;
let citationRequest = 0;
let actionRequest = 0;
let locationRequest = 0;

export const useInvestigationStore = create<InvestigationState>((set, get) => {
  const updateSummary = (summary: InvestigationSummary) => set((state) => ({
    history: state.history.some((item) => item.investigation_id === summary.investigation_id)
      ? mergeHistory(state.history, [summary]) : [summary, ...state.history],
    // A summary that reports a result is merged into the detail only once that
    // result is held; otherwise the next consistent refresh supplies both.
    detail: state.detail?.investigation_id === summary.investigation_id && summary.revision >= state.detail.revision &&
      (!summary.has_result || state.result !== null)
      ? { ...state.detail, ...summary } : state.detail,
  }));

  const startRequest = async (request: StartInvestigationRequest) => {
    if (get().starting) return;
    set({ starting: true, pendingStart: request, startError: null });
    try {
      const summary = await invokeOr<InvestigationSummary | null>('start_investigation', null, { request });
      if (!validSummary(summary)) throw new Error('No durable identity returned.');
      updateSummary(summary);
      set((state) => ({ starting: false, pendingStart: null,
        composer: { ...state.composer, question: '', parent: null },
        draftInspectionRequirements: [], inspectionDraftError: null }));
      await get().open(summary.investigation_id);
    } catch (error) {
      if (typeof error === 'object' && error !== null && 'code' in error && (error.code === 'refresh_required' || error.code === 'invalid_input') &&
          'definitive' in error && error.definitive === true) {
        set({ starting: false, pendingStart: null,
          startError: error.code === 'refresh_required'
            ? 'The selected captured source changed before admission. No investigation was started. Remove the stale requirement and select its source again.'
            : `The request was rejected before admission. No investigation was started.${'message' in error && typeof error.message === 'string' ? ` ${error.message}` : ''}` });
        return;
      }
      set({ starting: false, startError: 'The start outcome could not be confirmed. Retry this same request to recover its durable identity; this does not start another investigation.' });
    }
  };

  const consentAction = async (command: string) => {
    const { consent, selectedId, selectionGeneration, actionBusy } = get();
    if (!consent || consent.investigation_id !== selectedId || actionBusy) return;
    const action = ++actionRequest;
    set({ actionBusy: true, actionError: null });
    try {
      const summary = await invokeOr<InvestigationSummary | null>(command, null, {
        investigationId: consent.investigation_id, stepId: consent.step_id,
        revision: consent.revision, payloadHash: consent.preview.payload_hash,
      });
      if (!validSummary(summary, consent.investigation_id)) throw new Error('Unconfirmed action.');
      updateSummary(summary);
      if (get().selectionGeneration === selectionGeneration && action === actionRequest) {
        set({ actionBusy: false, consentOpen: false, consent: null });
        await get().refreshSelected();
      }
    } catch {
      if (get().selectionGeneration === selectionGeneration && action === actionRequest) {
        set({ actionBusy: false, consentOpen: false, consent: null,
          actionError: 'The step action was not confirmed. Refresh this investigation before approving again.' });
      }
    }
  };

  return {
    catalog: null, catalogLoading: false, catalogError: null,
    history: [], nextCursor: null, historyLoading: false, historyError: null,
    selectedId: null, selectionGeneration: 0, detail: null, result: null, events: [],
    consent: null, consentOpen: false, loading: false, error: null,
    actionBusy: false, actionError: null, pendingStart: null, starting: false, startError: null,
    composer: initialComposer,
    draftInspectionRequirements: [], inspectionDraftError: null,
    inspectionLocation: null, inspectionLocationTargetId: null, inspectionLocationLoading: false, inspectionLocationError: null,
    citationId: null, citationRead: null, citationLoading: false, citationError: null,

    updateComposer: (patch) => {
      if (!get().starting && !get().pendingStart) set((state) => ({
        composer: { ...state.composer, ...structuredClone(patch) },
      }));
    },
    addInspectionRequirement: (target) => {
      const state = get();
      if (state.pendingStart || state.starting) {
        set({ inspectionDraftError: 'Resolve the pending start before adding source requirements.' });
        return false;
      }
      if (state.draftInspectionRequirements.some((item) => item.target_id === target.target_id)) return true;
      if (state.draftInspectionRequirements.length >= 12 || state.draftInspectionRequirements.some((item) =>
        item.graph_snapshot_id !== target.graph_snapshot_id)) {
        set({ inspectionDraftError: 'Choose at most 12 sources from the same recovered graph. Remove earlier requirements before changing the graph.' });
        return false;
      }
      set({ draftInspectionRequirements: [...state.draftInspectionRequirements, structuredClone(target)], inspectionDraftError: null });
      return true;
    },
    removeInspectionRequirement: (targetId) => {
      if (!get().starting && !get().pendingStart) set((state) => ({
        draftInspectionRequirements: state.draftInspectionRequirements.filter((target) => target.target_id !== targetId), inspectionDraftError: null }));
    },
    clearInspectionRequirements: () => {
      if (!get().starting && !get().pendingStart) set({ draftInspectionRequirements: [], inspectionDraftError: null });
    },
    readInspectionLocation: async (target) => {
      const { selectedId, selectionGeneration, detail } = get();
      const saved = detail?.inspection_requirements?.find((item) => item.target_id === target.target_id) ??
        detail?.inspection_assessment?.requirements.find((item) => item.target_id === target.target_id)?.target;
      if (!selectedId || !saved || JSON.stringify(saved) !== JSON.stringify(target)) return;
      const request = ++locationRequest;
      const current = () => request === locationRequest && get().selectedId === selectedId && get().selectionGeneration === selectionGeneration;
      set({ inspectionLocation: null, inspectionLocationTargetId: target.target_id,
        inspectionLocationLoading: true, inspectionLocationError: null });
      try {
        const location = await invokeOr<InvestigationCapturedLocation | null>('inspection_captured_location', null, { target });
        if (!current()) return;
        if (!location || location.target_id !== target.target_id || JSON.stringify(location.captured) !== JSON.stringify(target.captured) ||
            location.byte_start !== target.captured.byte_start || location.byte_end !== target.captured.byte_end ||
            location.column_unit !== 'utf8_bytes' || location.end_exclusive !== true ||
            ![location.start_line, location.start_column, location.end_line, location.end_column].every((value) => Number.isSafeInteger(value) && value >= 1)) {
          throw new Error('Invalid retained location');
        }
        set({ inspectionLocation: location, inspectionLocationLoading: false });
      } catch {
        if (current()) set({ inspectionLocationLoading: false,
          inspectionLocationError: 'Exact retained source coordinates are unavailable. Current checkout lines were not substituted.' });
      }
    },

    loadCatalog: async () => {
      const request = ++catalogRequest;
      set({ catalogLoading: true, catalogError: null });
      try {
        const catalog = await invokeOr<InvestigationCatalog | null>('investigation_specialists', null);
        if (!catalog || catalog.schema_version !== 1 || !Array.isArray(catalog.specialists) ||
            !Array.isArray(catalog.providers)) throw new Error('Catalog unavailable.');
        if (request === catalogRequest) set({ catalog, catalogLoading: false });
      } catch {
        if (request === catalogRequest) set({ catalogLoading: false,
          catalogError: 'Specialists are unavailable. Connect to the app core and refresh.' });
      }
    },

    loadHistory: async (more = false) => {
      if (more && (!get().nextCursor || get().historyLoading)) return;
      const request = ++historyRequest;
      const cursor = more ? get().nextCursor : null;
      set({ historyLoading: true, historyError: null });
      try {
        const page = await invokeOr<InvestigationPage | null>('list_investigations', null,
          cursor ? { cursor } : {});
        if (!page || !Array.isArray(page.items) || page.items.length > 50 ||
            page.items.some((item) => !validSummary(item))) throw new Error('Invalid history.');
        if (request !== historyRequest) return;
        set((state) => ({ history: more ? mergeHistory(state.history, page.items) : [
          ...page.items.map((item) => {
            const saved = state.history.find((previous) => previous.investigation_id === item.investigation_id);
            return saved && saved.revision > item.revision ? saved : item;
          }),
          ...state.history.filter((item) => !page.items.some((incoming) => incoming.investigation_id === item.investigation_id)),
        ],
        // Background first-page refresh must not discard older explicit pages
        // or move their continuation backwards while another task is active.
        nextCursor: !more && state.history.length > page.items.length ? state.nextCursor : page.next_cursor,
        historyLoading: false }));
      } catch {
        if (request === historyRequest) set({ historyLoading: false,
          historyError: 'Investigation history could not be refreshed. Previously loaded records are retained.' });
      }
    },

    start: async (draft) => {
      if (get().pendingStart || get().starting) return;
      const request_nonce = crypto.randomUUID();
      if (draft.specialist_id.endsWith('@5')) {
        if (!draft.inspection_requirements?.length) return;
        await startRequest(structuredClone({ ...draft, schema_version: 2, request_nonce,
          expected_graph_revision: draft.inspection_requirements[0].graph_snapshot_id,
          inspection_requirements: draft.inspection_requirements }));
      } else {
        // Preserve legacy schema bytes: requirements are absent, never an empty list.
        const { inspection_requirements: requirements, ...legacy } = draft;
        if (requirements?.length) return;
        await startRequest(structuredClone({ ...legacy, schema_version: 1, request_nonce }));
      }
    },
    retryStart: async () => {
      const request = get().pendingStart;
      if (request) await startRequest(request);
    },
    open: async (id) => {
      ++detailRequest;
      ++citationRequest;
      ++actionRequest;
      ++locationRequest;
      set((state) => ({ selectedId: id, selectionGeneration: state.selectionGeneration + 1,
        detail: null, result: null, events: [], consent: null, consentOpen: false,
        loading: false, error: null, actionBusy: false, actionError: null,
        citationId: null, citationRead: null, citationLoading: false, citationError: null }));
      set({ inspectionLocation: null, inspectionLocationTargetId: null, inspectionLocationLoading: false, inspectionLocationError: null });
      await get().refreshSelected();
    },

    refreshSelected: async () => {
      const { selectedId: id, selectionGeneration, events: previousEvents } = get();
      if (!id) return;
      const request = ++detailRequest;
      const current = () => get().selectedId === id && get().selectionGeneration === selectionGeneration && request === detailRequest;
      set({ loading: true, error: null });
      try {
        const [detail, result, consent] = await Promise.all([
          invokeOr<InvestigationDetail | null>('get_investigation', null, { investigationId: id }),
          invokeOr<InvestigationResult | null>('investigation_result', null, { investigationId: id }),
          invokeOr<InvestigationConsent | null>('investigation_consent', null, { investigationId: id }),
        ]);
        if (!current()) return;
        if (!validSummary(detail, id)) throw new Error('Wrong detail.');
        // Independent reads can straddle the result commit; refresh instead of
        // recording a task whose detail and findings disagree.
        if ((result !== null) !== detail.has_result) throw new Error('Result observation changed.');
        if (result && (!validResultBinding(result) || result.investigation_id !== id ||
            result.graph_snapshot_id !== detail.graph_snapshot_id)) throw new Error('Wrong result.');
        if (result?.schema_version === 3 &&
            JSON.stringify(detail.inspection_assessment) !== JSON.stringify(result.inspection_assessment)) {
          throw new Error('Inspection assessment observation changed.');
        }
        if (consent && (consent.investigation_id !== id || consent.revision !== detail.revision ||
            detail.status !== 'awaiting_consent' || detail.cancel_requested ||
            consent.preview.locality !== 'Cloud' || consent.preview.tier !== 'Agentic' ||
            consent.provider_profile?.locality !== 'Cloud' ||
            consent.provider_profile?.provider_id !== consent.preview.provider_id || !consent.completion_limits)) {
          // Independent reads can straddle a real transition; refresh instead of
          // showing an approval for a state the detail no longer describes.
          throw new Error('Consent observation changed.');
        }
        let events = previousEvents;
        let afterSequence = events.at(-1)?.sequence ?? 0;
        let hasMore = true;
        for (let pageIndex = 0; pageIndex < 3 && hasMore; pageIndex += 1) {
          const page: InvestigationEventPage | null = await invokeOr<InvestigationEventPage | null>('investigation_events', null,
            { investigationId: id, afterSequence });
          if (!current()) return;
          if (!page || page.investigation_id !== id || !Array.isArray(page.items) || page.items.length > 50 ||
              !Number.isSafeInteger(page.next_sequence) || page.next_sequence < afterSequence) throw new Error('Invalid event page.');
          events = mergeInvestigationEvents(id, events, page.items);
          const last = events.at(-1)?.sequence ?? 0;
          if (page.next_sequence !== last || (page.has_more && page.next_sequence === afterSequence)) throw new Error('Invalid continuation.');
          afterSequence = page.next_sequence;
          hasMore = page.has_more;
        }
        if (hasMore || afterSequence < detail.last_event_sequence) throw new Error('Incomplete journal.');
        if (!current()) return;
        const saved = get().detail;
        if (saved && saved.revision > detail.revision) {
          set({ loading: false });
          return;
        }
        // A known immutable result cannot be replaced or disappear on a stale read.
        const savedResult = get().result;
        if (savedResult && result && JSON.stringify(savedResult) !== JSON.stringify(result)) throw new Error('Result changed.');
        updateSummary(detail);
        set((state) => ({ detail, result: result ?? savedResult, events, consent,
          consentOpen: state.consentOpen && state.consent?.step_id === consent?.step_id &&
            state.consent?.preview.payload_hash === consent?.preview.payload_hash,
          loading: false, error: null }));
      } catch {
        if (current()) set({ loading: false, consent: null, consentOpen: false,
          error: 'The investigation could not be refreshed consistently. Saved findings and activity remain visible; refresh to reconnect.' });
      }
    },

    invalidate: (message) => {
      const state = get();
      if (message.investigation_id === state.selectedId && !state.loading &&
          (message.revision > (state.detail?.revision ?? -1) ||
            message.last_event_sequence > (state.events.at(-1)?.sequence ?? 0))) void state.refreshSelected();
      if (message.investigation_id !== state.selectedId && !state.historyLoading &&
          message.revision > (state.history.find((item) => item.investigation_id === message.investigation_id)?.revision ?? -1)) {
        void state.loadHistory();
      }
    },
    showConsent: () => {
      if (get().consent && !get().loading && !get().actionBusy) set({ consentOpen: true });
    },
    hideConsent: () => set({ consentOpen: false }),
    approve: () => consentAction('approve_investigation_step'),
    decline: () => consentAction('decline_investigation_step'),
    cancel: async (target) => {
      const id = target ?? get().selectedId;
      if (!id || get().actionBusy) return;
      const selection = get().selectionGeneration;
      const action = ++actionRequest;
      set({ actionBusy: true, actionError: null });
      try {
        const summary = await invokeOr<InvestigationSummary | null>('cancel_investigation', null, { investigationId: id });
        if (!validSummary(summary, id)) throw new Error('Cancellation not confirmed.');
        updateSummary(summary);
        if (selection === get().selectionGeneration && action === actionRequest) {
          set({ actionBusy: false, consent: null, consentOpen: false });
          if (id === get().selectedId) await get().refreshSelected();
        }
      } catch {
        if (selection === get().selectionGeneration && action === actionRequest) set({ actionBusy: false,
          actionError: 'Cancellation was not confirmed. Refresh the investigation; an outstanding model request may still finish.' });
      }
    },

    readCitation: async (citationId) => {
      const { selectedId: id, selectionGeneration, detail } = get();
      const citation = detail?.citations.find((item) => item.citation_id === citationId);
      if (!id || !citation) return;
      const request = ++citationRequest;
      const current = () => get().selectedId === id && get().selectionGeneration === selectionGeneration && request === citationRequest;
      set({ citationId, citationRead: null, citationLoading: true, citationError: null });
      try {
        const read = await invokeOr<InvestigationCitationRead | null>('read_investigation_citation', null,
          { investigationId: id, citationId });
        if (!current()) return;
        if (!read || read.investigation_id !== id || read.citation_id !== citationId ||
            JSON.stringify(read.citation) !== JSON.stringify(citation) ||
            (read.status === 'available' && (citation.origin.kind !== 'captured_primary_source' || typeof read.text !== 'string')) ||
            (read.status !== 'available' && read.text !== null)) throw new Error('Wrong historical citation.');
        set({ citationRead: read, citationLoading: false });
      } catch {
        if (current()) set({ citationLoading: false, citationError: 'Historical evidence could not be inspected. No current checkout source was substituted.' });
      }
    },
    clearCitation: () => {
      ++citationRequest;
      set({ citationId: null, citationRead: null, citationLoading: false, citationError: null });
    },
  };
});
