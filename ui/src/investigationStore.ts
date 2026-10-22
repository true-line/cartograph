import { sameReviewSubject, mergeReviews, validReview, validReviews, type FindingReview, type FindingReviews, type FindingReviewDecision } from './findingReviews';
import { isCompletionDisclosure, isCompletionSpecialist, isExpandedSpecialist } from './investigationFamilies';
import { validCompletionDetail, validCompletionResult, validCompletionSummary } from './investigationCompletion';
import { investigationPolicy, obligationEnvelope, validObligation, validObligationChoices, validObligationEnvelope } from './investigationObligations';
import { create } from 'zustand';
import { invokeOr } from './tauri';
import type {
  InvestigationCatalog, InvestigationChanged, InvestigationCitationRead,
  InvestigationConsent, InvestigationDetail, InvestigationEvent,
  InvestigationEventPage, InvestigationPage, InvestigationResult,
  InvestigationSummary, StartInvestigationRequest, InvestigationInspectionTarget, InvestigationCapturedLocation,
  SpecialistId, InvestigationProviderMode, InvestigationObligation, InvestigationObligationChoices, InvestigationScope,
} from './investigationTypes';

export interface InvestigationDraft {
  specialist_id: SpecialistId; question: string; scope: InvestigationScope;
  provider_mode: InvestigationProviderMode; limit_profile: string;
  inspection_requirements?: InvestigationInspectionTarget[];
  obligations?: import('./investigationTypes').InvestigationObligationEnvelope;
  expected_graph_revision?: string; expected_context_id?: string;
  conversation_id?: string; parent_id?: string;
}
export interface InvestigationComposer {
  specialistId: SpecialistId;
  limitProfile?: string;
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
      value.last_event_sequence > (isExpandedSpecialist(value.specialist_id) ? 512 : 96) || typeof value.actions?.can_cancel !== 'boolean' ||
      typeof value.actions?.can_follow_up !== 'boolean' || typeof value.has_result !== 'boolean') return false;
  if ([4, 5, 6, 7].includes(value.schema_version)) return validCompletionSummary(value);
  if (['completion_outcome', 'obligation_status', 'completion', 'completion_obligations'].some((key) => key in value)) return false;
  if (value.schema_version === 1) {
    return LEGACY_SPECIALISTS.has(value.specialist_id) && value.inspection_status === undefined &&
      value.inspection_requirement_count === undefined && value.inspection_requirements === undefined &&
      value.inspection_assessment === undefined;
  }
  const v5 = value.schema_version === 2 &&
    (value.specialist_id === 'domain-analyst@5' || value.specialist_id === 'evidence-auditor@5');
  const v6 = value.schema_version === 3 &&
    (value.specialist_id === 'domain-analyst@6' || value.specialist_id === 'evidence-auditor@6');
  if (!v5 && !v6) return false;
  const requirementCount = value.inspection_requirement_count;
  if (v6 && requirementCount === undefined) return value.inspection_status === undefined &&
    value.inspection_requirements === undefined && value.inspection_assessment === undefined;
  return typeof requirementCount === 'number' && Number.isSafeInteger(requirementCount) && requirementCount >= 1 &&
    requirementCount <= 12 && (value.has_result
      ? value.inspection_status === 'supplied' || value.inspection_status === 'unmet'
      : value.inspection_status === (investigationIsActive(value) ? 'pending' : 'not_assessed'));
}

const HEX_HASH = /^[0-9a-f]{64}(?![\s\S])/;
const CONTEXT_ID = /^context-v2:[0-9a-f]{64}(?![\s\S])/;
const COVERAGE_ID = /^coverage-context-v1:[0-9a-f]{64}(?![\s\S])/;
const identity = (value: unknown): value is string => typeof value === 'string' && value.length > 0 &&
  new TextEncoder().encode(value).length <= 256 && !value.includes('\0');
const hash = (value: unknown): value is string => typeof value === 'string' && HEX_HASH.test(value);
const closedFields = (value: object, fields: string[]) => Object.keys(value).length === fields.length &&
  Object.keys(value).every((key) => fields.includes(key));

/** Check the public saved wire. The host verifies its private identity proof. */
function validCoverageContext(value: InvestigationDetail['coverage_context']): boolean {
  if (!value || !closedFields(value, ['context_id', 'coverage']) || !CONTEXT_ID.test(value.context_id) || !value.coverage ||
      !closedFields(value.coverage, ['contract', 'exhaustive', 'coverage_id', 'repos']) ||
      value.coverage.contract !== 'coverage-qualification@1' || value.coverage.exhaustive !== false ||
      !COVERAGE_ID.test(value.coverage.coverage_id) || !Array.isArray(value.coverage.repos) ||
      value.coverage.repos.length > 4096) return false;
  const repos = value.coverage.repos;
  return new Set(repos.map((repo) => repo.repo)).size === repos.length && repos.every((repo) => {
    if (!closedFields(repo, ['repo', 'status', 'commit', 'inventory_hash', 'limited', 'counts', 'layer_hints', 'compared']) ||
        !identity(repo.repo) || !Array.isArray(repo.compared)) return false;
    const derivedAbsent = repo.commit === null && repo.limited === null && repo.counts === null && repo.layer_hints === null;
    if (repo.status === 'unrecorded') return derivedAbsent && repo.inventory_hash === null && repo.compared.length === 0;
    if (!hash(repo.inventory_hash) || (JSON.stringify(repo.compared) !== '["registration","plugins"]' &&
        JSON.stringify(repo.compared) !== '["registration","plugins","layer_hints"]')) return false;
    if (repo.status === 'stale') return derivedAbsent;
    if (repo.status !== 'recorded' || !identity(repo.commit) || !repo.counts || !Array.isArray(repo.layer_hints) ||
        !repo.layer_hints.every(identity)) return false;
    if (!closedFields(repo.counts, ['not_selected', 'missing_input', 'not_applicable', 'unsupported'])) return false;
    const counts = [repo.counts.not_selected, repo.counts.missing_input, repo.counts.not_applicable, repo.counts.unsupported];
    return counts.every((count) => Number.isSafeInteger(count) && count >= 0) &&
      repo.limited === (repo.counts.not_selected > 0 || repo.counts.missing_input > 0 || repo.counts.unsupported > 0);
  });
}

/** Host validates result content; the UI rejects incompatible version bindings. */
function validResultBinding(result: InvestigationResult, maximum = 12): boolean {
  if (result.schema_version === 5 || result.schema_version === 6 || result.schema_version === 7) return validCompletionResult(result.completion, result, maximum) &&
    typeof result.invocation_step_id === 'string' && /^[A-Za-z0-9_:-]{1,128}$/.test(result.invocation_step_id) &&
    isCompletionDisclosure(result.disclosure_hash, result.schema_version) &&
    Boolean(result.coverage && CONTEXT_ID.test(result.coverage.context_id) && COVERAGE_ID.test(result.coverage.coverage_id) && hash(result.coverage.representation_hash));
  if ('completion' in result || 'completion_outcome' in result) return false;
  if (result.schema_version === 1) {
    return !('invocation_step_id' in result) && !('disclosure_hash' in result) &&
      !('inspection_assessment' in result) && !('coverage' in result);
  }
  if (![2, 3, 4].includes(result.schema_version) || typeof result.invocation_step_id !== 'string' ||
      !/^[A-Za-z0-9_:-]{1,128}(?![\s\S])/.test(result.invocation_step_id) || typeof result.disclosure_hash !== 'string' ||
      !(result.schema_version === 4 ? /^investigation-disclosure-v3:[0-9a-f]{64}(?![\s\S])/ :
        /^investigation-disclosure-v1:[0-9a-f]{64}(?![\s\S])/).test(result.disclosure_hash)) return false;
  if (result.schema_version === 4) {
    if (!result.coverage || !closedFields(result.coverage, ['context_id', 'coverage_id', 'representation_hash']) || !CONTEXT_ID.test(result.coverage.context_id) ||
        !COVERAGE_ID.test(result.coverage.coverage_id) || !hash(result.coverage.representation_hash)) return false;
    if (!('inspection_assessment' in result)) return true;
  } else if ('coverage' in result) return false;
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
  id: string, current: InvestigationEvent[], incoming: InvestigationEvent[], maximumSequence = 96,
): InvestigationEvent[] {
  const bySequence = new Map(current.map((item) => [item.sequence, item]));
  for (const item of incoming) {
    if (item.investigation_id !== id || !Number.isSafeInteger(item.sequence) ||
        item.sequence < 1 || item.sequence > maximumSequence) throw new Error('Invalid event identity.');
    const previous = bySequence.get(item.sequence);
    if (previous && JSON.stringify(previous) !== JSON.stringify(item)) throw new Error('Conflicting event.');
    bySequence.set(item.sequence, item);
  }
  const result = [...bySequence.values()].sort((a, b) => a.sequence - b.sequence);
  if (result.some((item, index) => item.sequence !== index + 1)) throw new Error('Missing event.');
  return result;
}

export interface InvestigationState {
  findingReviews: FindingReview[];
  reviewLoading: boolean;
  reviewBusy: boolean;
  reviewError: string | null;
  refreshReviews: () => Promise<void>;
  reviewFinding: (findingId: string, decision: FindingReviewDecision) => Promise<void>;
  invalidateReview: (observation: FindingReview) => void;
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
  draftObligations: InvestigationObligation[];
  obligationChoices: InvestigationObligationChoices | null;
  obligationChoicesLoading: boolean;
  obligationDraftError: string | null;
  loadObligationChoices: (scope: InvestigationScope) => Promise<void>;
  addObligation: (value: InvestigationObligation) => boolean;
  addSourceObligation: (target: InvestigationInspectionTarget) => Promise<boolean>;
  removeObligation: (id: string) => void;
  clearObligations: () => void;
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

let reviewRequest = 0;
let catalogRequest = 0;
let historyRequest = 0;
let detailRequest = 0;
let citationRequest = 0;
let actionRequest = 0;
let locationRequest = 0;
let choicesRequest = 0;

export const useInvestigationStore = create<InvestigationState>((set, get) => {
  const updateSummary = (summary: InvestigationSummary) => set((state) => ({
    history: state.history.some((item) => item.investigation_id === summary.investigation_id)
      ? mergeHistory(state.history, [summary]) : [summary, ...state.history],
    // A summary that reports a result is merged into the detail only once that
    // result is held; otherwise the next consistent refresh supplies both.
    detail: ![4, 5, 6, 7].includes(summary.schema_version) && state.detail?.investigation_id === summary.investigation_id && summary.revision >= state.detail.revision &&
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
        draftInspectionRequirements: [], inspectionDraftError: null,
    draftObligations: [], obligationChoices: null, obligationChoicesLoading: false, obligationDraftError: null }));
      await get().open(summary.investigation_id);
    } catch (error) {
      if (typeof error === 'object' && error !== null && 'code' in error && (error.code === 'refresh_required' || error.code === 'invalid_input') &&
          'definitive' in error && error.definitive === true) {
        set({ starting: false, pendingStart: null,
          startError: error.code === 'refresh_required'
            ? [4, 5, 6, 7].includes(request.schema_version) ? 'The selected evidence or completion context changed before admission. No investigation was started. Refresh choices and remove stale obligations.'
              : 'The selected captured source changed before admission. No investigation was started. Remove the stale requirement and select its source again.'
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
    findingReviews: [], reviewLoading: false, reviewBusy: false, reviewError: null,
    refreshReviews: async () => {
      const { selectedId, selectionGeneration, result } = get();
      if (!selectedId || !result) return;
      const request = ++reviewRequest;
      const current = () => get().selectionGeneration === selectionGeneration && get().selectedId === selectedId && get().result?.result_id === result.result_id && request === reviewRequest;
      set({ reviewLoading: true });
      try {
        const value = await invokeOr<FindingReviews | null>('get_investigation_finding_reviews', null, { investigationId: selectedId, expectedResultId: result.result_id });
        if (!current()) return;
        if (!validReviews(value, result)) throw new Error('Invalid review join');
        set({ findingReviews: mergeReviews(get().findingReviews, value.reviews), reviewLoading: false, reviewError: null });
      } catch {
        if (current()) set({ reviewLoading: false, reviewError: 'Review state unavailable. Refresh reviews before making a decision. Historical findings remain readable.' });
      }
    },
    reviewFinding: async (findingId, decision) => {
      const { result, selectionGeneration, findingReviews, reviewBusy, reviewLoading, reviewError } = get();
      const row = findingReviews.find((r) => r.subject.finding_id === findingId);
      if (!result || !row || reviewBusy || reviewLoading || reviewError) return;
      const current = () => get().selectionGeneration === selectionGeneration && get().result?.result_id === result.result_id;
      set({ reviewBusy: true, reviewError: null });
      try {
        const value = await invokeOr<FindingReview | null>('review_investigation_finding', null, { requestJson: JSON.stringify({ schema_version: 1, subject: row.subject, expected_revision: row.revision, decision }) });
        if (!current()) return;
        if (!value || !validReview(value, result) || !sameReviewSubject(value.subject, row.subject) || value.revision !== row.revision + 1 || value.decision !== decision) throw new Error('Unconfirmed write');
        set({ findingReviews: mergeReviews(get().findingReviews, get().findingReviews.map((r) => r.subject.finding_id === findingId ? value : r)), reviewBusy: false });
      } catch (error) {
        if (!current()) return;
        if (typeof error === 'object' && error !== null && 'code' in error && error.code === 'conflict' && 'current' in error) {
          const winner = error.current as FindingReview;
          if (validReview(winner, result) && sameReviewSubject(winner.subject, row.subject)) {
            try {
              set({ findingReviews: mergeReviews(get().findingReviews, get().findingReviews.map((r) => r.subject.finding_id === findingId ? winner : r)), reviewBusy: false, reviewError: 'Another decision won. The saved winner is shown. Refresh reviews, then deliberately choose again.' });
              return;
            } catch { /* Inconsistent observations require a read. */ }
          }
        }
        set({ reviewBusy: false, reviewError: 'The review outcome could not be confirmed. Refresh reviews to recover the saved decision before choosing again. No automatic retry was made.' });
      }
    },
    invalidateReview: (observation) => {
      const { result } = get();
      if (result && validReview(observation, result)) void get().refreshReviews();
    },
    catalog: null, catalogLoading: false, catalogError: null,
    history: [], nextCursor: null, historyLoading: false, historyError: null,
    selectedId: null, selectionGeneration: 0, detail: null, result: null, events: [],
    consent: null, consentOpen: false, loading: false, error: null,
    actionBusy: false, actionError: null, pendingStart: null, starting: false, startError: null,
    composer: initialComposer,
    draftInspectionRequirements: [], inspectionDraftError: null,
    draftObligations: [], obligationChoices: null, obligationChoicesLoading: false, obligationDraftError: null,
    inspectionLocation: null, inspectionLocationTargetId: null, inspectionLocationLoading: false, inspectionLocationError: null,
    citationId: null, citationRead: null, citationLoading: false, citationError: null,

    updateComposer: (patch) => {
      if (!get().starting && !get().pendingStart && ['scopeType', 'anchor', 'hops'].some((key) => key in patch && patch[key as keyof InvestigationComposer] !== get().composer[key as keyof InvestigationComposer])) {
        ++choicesRequest;
        set({ draftObligations: [], obligationChoices: null, obligationChoicesLoading: false,
          obligationDraftError: get().draftObligations.length ? 'Scope changed. Select the evidence to inspect in this scope.' : null });
      }
      if (!get().starting && !get().pendingStart) set((state) => ({
        composer: { ...state.composer, ...structuredClone(patch) },
      }));
    },
    loadObligationChoices: async (scope) => {
      if (get().starting || get().pendingStart) return;
      const request = ++choicesRequest;
      set({ obligationChoicesLoading: true, obligationDraftError: null });
      try {
        const choices = await invokeOr<InvestigationObligationChoices | null>('investigation_obligation_choices', null, { scope });
        if (request !== choicesRequest) return;
        if (!validObligationChoices(choices)) throw new Error('Invalid choices');
        set({ obligationChoices: choices, obligationChoicesLoading: false });
      } catch {
        if (request === choicesRequest) set({ obligationChoices: null, obligationChoicesLoading: false,
          obligationDraftError: 'Completion choices could not be resolved. Refresh before selecting an obligation.' });
      }
    },
    addObligation: (value) => {
      const state = get();
      if (state.starting || state.pendingStart || !validObligation(value) || !state.obligationChoices) return false;
      const duplicate = state.draftObligations.find((item) => item.obligation_id === value.obligation_id);
      if (duplicate) return JSON.stringify(duplicate) === JSON.stringify(value);
      const policy = investigationPolicy(state.catalog, state.composer.limitProfile ?? 'investigation-v1', state.composer.specialistId);
      if (state.draftObligations.length >= (policy?.max_completion_obligations ?? 12) || state.draftObligations.some((item) => item.kind === 'source'
        ? item.target.graph_snapshot_id !== state.obligationChoices!.graph_snapshot_id : item.target.context_id !== state.obligationChoices!.context_id) ||
        (value.kind === 'inventory' && state.draftObligations.some((item) => item.kind === 'inventory' && item.target.context_id === value.target.context_id &&
          item.target.repo === value.target.repo && item.target.inventory_hash !== value.target.inventory_hash)) ||
        (value.kind === 'source' && state.draftObligations.some((item) => item.kind === 'source' && JSON.stringify(item.target.fact) === JSON.stringify(value.target.fact) &&
          item.target.role === value.target.role && item.target.index === value.target.index && item.target.target_id !== value.target.target_id)) || (value.kind === 'source'
        ? value.target.graph_snapshot_id !== state.obligationChoices.graph_snapshot_id
        : value.target.context_id !== state.obligationChoices.context_id)) {
        set({ obligationDraftError: `Choose at most ${policy?.max_completion_obligations ?? 12} obligations from one exact context. Remove stale selections before changing context.` });
        return false;
      }
      set({ draftObligations: obligationEnvelope([...state.draftObligations, structuredClone(value)]).obligations, obligationDraftError: null });
      return true;
    },
    addSourceObligation: async (target) => {
      const state = get();
      if (state.starting || state.pendingStart) return false;
      const scope: InvestigationScope = state.composer.scopeType === 'all' ? { type: 'all' } :
        { type: 'neighborhood', anchor: state.composer.anchor, hops: state.composer.hops };
      if (!state.obligationChoices) {
        const loading = get().loadObligationChoices(scope);
        const loadingGeneration = choicesRequest;
        await loading;
        if (loadingGeneration !== choicesRequest) return false;
      }
      if (get().starting || get().pendingStart) return false;
      const generation = choicesRequest;
      try {
        const obligation = await invokeOr<InvestigationObligation | null>('resolve_investigation_source_obligation', null, {
          fact: target.fact, expectedGraphSnapshotId: target.graph_snapshot_id,
          expectedReceiptId: target.receipt_id, inventoryIndex: target.receipt_inventory_index,
        });
        if (generation !== choicesRequest) return false;
        if (!obligation || obligation.kind !== 'source' || JSON.stringify(obligation.target) !== JSON.stringify(target)) throw new Error('Source changed');
        return get().addObligation(obligation);
      } catch {
        if (generation !== choicesRequest) return false;
        set({ obligationDraftError: 'The captured source could not be resolved for this context. Select it again.' });
        return false;
      }
    },
    removeObligation: (id) => {
      if (!get().starting && !get().pendingStart) set((state) => ({ draftObligations: state.draftObligations.filter((item) => item.obligation_id !== id), obligationDraftError: null }));
    },
    clearObligations: () => {
      if (!get().starting && !get().pendingStart) {
        ++choicesRequest;
        set({ draftObligations: [], obligationChoices: null, obligationChoicesLoading: false, obligationDraftError: null });
      }
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
      const { obligations: completionObligations, ...legacyBase } = draft;
      if (isCompletionSpecialist(draft.specialist_id)) {
        const policy = investigationPolicy(get().catalog, draft.limit_profile, draft.specialist_id);
        if (!policy) return;
        const choices = get().obligationChoices;
        if (!choices || !draft.obligations || !validObligationEnvelope(draft.obligations, policy) || draft.inspection_requirements !== undefined ||
            draft.obligations.obligations.some((item) => item.kind === 'source' ? item.target.graph_snapshot_id !== choices.graph_snapshot_id : item.target.context_id !== choices.context_id)) return;
        const request: StartInvestigationRequest = { schema_version: policy.request_schema_version, request_nonce, specialist_id: draft.specialist_id,
          obligations: structuredClone(draft.obligations), question: draft.question, scope: draft.scope,
          provider_mode: draft.provider_mode, limit_profile: draft.limit_profile,
          expected_graph_revision: choices.graph_snapshot_id, expected_context_id: choices.context_id,
          conversation_id: draft.conversation_id ?? null, parent_id: draft.parent_id ?? null };
        if (new TextEncoder().encode(JSON.stringify(request)).length > policy.request_bytes) {
          set({ obligationDraftError: `This complete request exceeds ${policy.request_bytes.toLocaleString()} bytes. Shorten it or remove an obligation.` });
          return;
        }
        await startRequest(request);
      } else if (completionObligations !== undefined) return;
      else if (draft.specialist_id.endsWith('@6')) {
        const { inspection_requirements: requirements, ...qualified } = legacyBase;
        if (requirements !== undefined && requirements.length === 0) return;
        await startRequest(structuredClone({ ...qualified, schema_version: 3, request_nonce,
          ...(requirements ? { expected_graph_revision: requirements[0].graph_snapshot_id,
            inspection_requirements: requirements } : {}) }));
      } else if (draft.specialist_id.endsWith('@5')) {
        const { expected_context_id, ...inspectionDraft } = legacyBase;
        if (!draft.inspection_requirements?.length || expected_context_id !== undefined) return;
        await startRequest(structuredClone({ ...inspectionDraft, schema_version: 2, request_nonce,
          expected_graph_revision: draft.inspection_requirements[0].graph_snapshot_id,
          inspection_requirements: draft.inspection_requirements }));
      } else {
        // Preserve legacy schema bytes: requirements are absent, never an empty list.
        const { inspection_requirements: requirements, expected_context_id, ...legacy } = legacyBase;
        if (requirements?.length || expected_context_id !== undefined) return;
        await startRequest(structuredClone({ ...legacy, schema_version: 1, request_nonce }));
      }
    },
    retryStart: async () => {
      const request = get().pendingStart;
      if (request) await startRequest(request);
    },
    open: async (id) => {
      ++detailRequest;
      ++reviewRequest;
      set({ findingReviews: [], reviewLoading: false, reviewBusy: false, reviewError: null });
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
        if (result && (!validResultBinding(result, [5, 6, 7].includes(detail.schema_version) ? 56 : 12) || result.investigation_id !== id ||
            result.graph_snapshot_id !== detail.graph_snapshot_id)) throw new Error('Wrong result.');
        if ([4, 5, 6, 7].includes(detail.schema_version)) {
          if (!validCompletionDetail(detail.completion, detail, result) || (detail.coverage_context !== undefined && !validCoverageContext(detail.coverage_context)) ||
              (detail.graph_snapshot_id !== null && !detail.coverage_context) || (result && (!detail.coverage_context ||
                result.coverage?.context_id !== detail.coverage_context.context_id || result.coverage.coverage_id !== detail.coverage_context.coverage.coverage_id))) throw new Error('Completion binding changed.');
        } else if (detail.schema_version === 3) {
          if (detail.inspection_requirement_count !== undefined &&
              (!Array.isArray(detail.inspection_requirements) || detail.inspection_requirements.length !== detail.inspection_requirement_count ||
               new Set(detail.inspection_requirements.map((target) => target.target_id)).size !== detail.inspection_requirements.length ||
               (detail.has_result ? !detail.inspection_assessment : detail.inspection_assessment !== undefined) ||
               detail.inspection_requirements.some((target) => target.schema_version !== 1 || !hash(target.target_id)) ||
               (result && (!result.inspection_assessment || result.inspection_assessment.requirements.length !== detail.inspection_requirements.length ||
                 detail.inspection_requirements.some((target) => !result.inspection_assessment?.requirements.some((row) =>
                   JSON.stringify(row.target) === JSON.stringify(target))))))) throw new Error('Inspection targets changed.');
          if (detail.coverage_context !== undefined && !validCoverageContext(detail.coverage_context)) throw new Error('Invalid saved coverage.');
          if (detail.graph_snapshot_id !== null && !detail.coverage_context) throw new Error('Missing prepared coverage.');
          if (result && (result.schema_version !== 4 || !detail.coverage_context ||
              result.coverage?.context_id !== detail.coverage_context.context_id ||
              result.coverage.coverage_id !== detail.coverage_context.coverage.coverage_id)) throw new Error('Coverage binding changed.');
        } else if (detail.coverage_context !== undefined || result?.schema_version === 4 || result?.schema_version === 5 || result?.schema_version === 6 || result?.schema_version === 7) throw new Error('Unexpected coverage version.');
        if ((result?.schema_version === 3 || result?.schema_version === 4) &&
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
        for (let pageIndex = 0; pageIndex < ([5, 6, 7].includes(detail.schema_version) ? Math.ceil(512 / 50) : 3) && hasMore; pageIndex += 1) {
          const page: InvestigationEventPage | null = await invokeOr<InvestigationEventPage | null>('investigation_events', null,
            { investigationId: id, afterSequence });
          if (!current()) return;
          if (!page || page.investigation_id !== id || !Array.isArray(page.items) || page.items.length > 50 ||
              !Number.isSafeInteger(page.next_sequence) || page.next_sequence < afterSequence) throw new Error('Invalid event page.');
          events = mergeInvestigationEvents(id, events, page.items, [5, 6, 7].includes(detail.schema_version) ? 512 : 96);
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
        // Family, immutable specialist definition and selected limits belong to
        // the saved task, even before a result exists or an assessment is made.
        if (saved && (saved.schema_version !== detail.schema_version || saved.specialist_id !== detail.specialist_id ||
            JSON.stringify(saved.specialist) !== JSON.stringify(detail.specialist) ||
            JSON.stringify(saved.limits) !== JSON.stringify(detail.limits))) throw new Error('Saved investigation family changed.');
        // A known immutable result cannot be replaced or disappear on a stale read.
        if (saved?.completion && (saved.completion.request_hash !== detail.completion?.request_hash || saved.completion.obligations_hash !== detail.completion?.obligations_hash ||
            JSON.stringify(saved.completion.obligations) !== JSON.stringify(detail.completion?.obligations))) throw new Error('Completion intent changed.');
        if (saved?.coverage_context && JSON.stringify(saved.coverage_context) !== JSON.stringify(detail.coverage_context)) {
          throw new Error('Saved coverage changed.');
        }
        if (saved?.completion && detail.completion && saved.completion.refusal_count > detail.completion.refusal_count) throw new Error('Refusal count regressed.');
        const savedResult = get().result;
        if (saved && [4, 5, 6, 7].includes(saved.schema_version) && savedResult && result === null) throw new Error('Completion result disappeared.');
        if (savedResult && result && JSON.stringify(savedResult) !== JSON.stringify(result)) throw new Error('Result changed.');
        updateSummary(detail);
        set((state) => ({ detail, result: result ?? savedResult, events, consent,
          consentOpen: state.consentOpen && state.consent?.step_id === consent?.step_id &&
            state.consent?.preview.payload_hash === consent?.preview.payload_hash,
          loading: false, error: null }));
        await get().refreshReviews();
      } catch {
        if (current()) set({ loading: false, consent: null, consentOpen: false,
          reviewError: 'Saved investigation binding could not be refreshed. Refresh before reviewing; historical findings remain visible.',
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
