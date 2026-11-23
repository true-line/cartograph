import { mergeCurations, validCuration, validCurationChanged, validCurations, type CurationDecision, type FindingCuration, type FindingCurationChanged, type FindingCurations } from './findingCurations';
import { validReviewChanged, type FindingReviewChanged, sameReviewSubject, mergeReviews, validReview, validReviews, type FindingReview, type FindingReviews, type FindingReviewDecision } from './findingReviews';
import { isCompletionDisclosure, isCompletionSpecialist, isExpandedSpecialist } from './investigationFamilies';
import { validCompletionDetail, validCompletionResult, validCompletionSummary } from './investigationCompletion';
import { investigationPolicy, obligationEnvelope, validObligation, validObligationChoices, validObligationEnvelope } from './investigationObligations';
import { create } from 'zustand';
import { SPECIALIST_TABLE } from './generated/specialistTable';
import { profileFor } from './specialistRoles';
import { invokeOr } from './tauri';
import type {
  InvestigationCatalog, InvestigationChanged, InvestigationChat, InvestigationCitationRead,
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
/** UI-only start context. These values are validated/projected before IPC. */
export interface InvestigationStartOptions {
  preserveComposer?: boolean;
  /** Let the calling surface own navigation after a confirmed task start. */
  deferOpen?: boolean;
  completionChoices?: InvestigationObligationChoices;
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
/** Desktop default when the host advertises it (#1618): the domain analyst's current version (ADR-0071,
 * AC-1402), or schema-backed @27 (AC-0574) from a host that does not advertise the current one yet. */
const CURRENT_ANALYST = (Object.keys(SPECIALIST_TABLE) as SpecialistId[]).find((id) =>
  SPECIALIST_TABLE[id].role === 'domain-analyst' && SPECIALIST_TABLE[id].availability === 'current')!;
const DESKTOP_DEFAULT_CANDIDATES: readonly SpecialistId[] = [CURRENT_ANALYST, 'domain-analyst@27'];
export const DESKTOP_DEFAULT_SPECIALIST: SpecialistId = CURRENT_ANALYST;
const DESKTOP_DEFAULT_PROFILE = 'investigation-v2';
/** An untouched composer adopts the schema-backed default; explicit choices are kept. */
export function desktopDefaultComposer(composer: InvestigationComposer, catalog: InvestigationCatalog): InvestigationComposer | null {
  if (composer.limitProfile !== undefined || composer.specialistId !== initialComposer.specialistId || composer.parent !== null) return null;
  const specialistId = DESKTOP_DEFAULT_CANDIDATES.find((id) => catalog.specialists.some((item) => item.id === id) &&
    investigationPolicy(catalog, DESKTOP_DEFAULT_PROFILE, id));
  return specialistId ? { ...composer, specialistId, limitProfile: DESKTOP_DEFAULT_PROFILE } : null;
}
/** The user's last explicit budget and specialist, persisted in the app profile (#1636). */
export interface StoredInvestigationChoice { specialist_id: string; limit_profile: string }
/** An untouched composer restores a stored choice the catalog still offers, else the desktop default. */
export function restoredComposer(composer: InvestigationComposer, catalog: InvestigationCatalog,
  stored: StoredInvestigationChoice | null): InvestigationComposer | null {
  const untouched = composer.limitProfile === undefined && composer.specialistId === initialComposer.specialistId &&
    composer.parent === null;
  if (untouched && stored && catalog.specialists.some((item) => item.id === stored.specialist_id) &&
      investigationPolicy(catalog, stored.limit_profile, stored.specialist_id as SpecialistId))
    return { ...composer, specialistId: stored.specialist_id as SpecialistId, limitProfile: stored.limit_profile };
  return desktopDefaultComposer(composer, catalog);
}
async function storedChoice(): Promise<StoredInvestigationChoice | null> {
  try {
    const value = await invokeOr<StoredInvestigationChoice | null>('get_investigation_choice', null);
    return value && typeof value.specialist_id === 'string' && typeof value.limit_profile === 'string' ? value : null;
  } catch { return null; }
}
const TERMINAL = new Set(['completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown']);
const ACTIVE_HISTORY_STATUSES = new Set(['queued', 'preparing', 'running', 'awaiting_consent']);
export const INVESTIGATION_PAGE_LIMIT = 50;
export function investigationIsActive(summary: InvestigationSummary): boolean {
  return !TERMINAL.has(summary.status) || summary.invocation_pending;
}
export function investigationIsInActiveHistory(summary: InvestigationSummary): boolean {
  return ACTIVE_HISTORY_STATUSES.has(summary.status);
}
export type InvestigationChatStatus = 'idle' | 'loading' | 'saved' | 'legacy' | 'preview' | 'error';

const LEGACY_SPECIALISTS = new Set<SpecialistId>([
  'domain-analyst@1', 'evidence-auditor@1', 'domain-analyst@2', 'evidence-auditor@2',
  'domain-analyst@3', 'evidence-auditor@3', 'domain-analyst@4', 'evidence-auditor@4',
]);
export function validSummary(value: InvestigationSummary | null, id?: string): value is InvestigationSummary {
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

export function validReportedTaskUsage(value: unknown): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).sort().join(',') !== 'cache_read_tokens,input_tokens,output_tokens') return false;
  return Object.values(value).every((count) => count === null ||
    (typeof count === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(count) &&
      (count.length < 20 || count <= '18446744073709551615')));
}

/** ADR-0085: an app-only runtime identity; a malformed one rejects the detail. */
export function validRunRuntime(value: unknown): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).sort().join(',') !== 'lane,runner,runtime,version') return false;
  const { lane, runtime, version, runner } = value as Record<string, unknown>;
  const field = (text: unknown) => typeof text === 'string' && /^[A-Za-z0-9.\-_+@]{1,64}$/.test(text);
  return (lane === 'pi' || lane === 'claude') && field(runtime) && field(version) && field(runner);
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
  if (result.schema_version !== 8 && 'narrative' in result) return false;
  if (result.schema_version === 5 || result.schema_version === 6 || result.schema_version === 7 || result.schema_version === 8) return validCompletionResult(result.completion, result, maximum) &&
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

/** Validate the app-facing chat join against this selected saved task/result. */
function validInvestigationChat(value: unknown, id: string, detail: InvestigationDetail,
  result: InvestigationResult | null): value is InvestigationChat {
  const record = (item: unknown): item is Record<string, unknown> => typeof item === 'object' && item !== null && !Array.isArray(item);
  if (!record(value) || !closedFields(value, ['chat_id', 'task_id', 'producer', 'messages'])) return false;
  const chat = value as unknown as InvestigationChat;
  if (!identity(chat.chat_id) || chat.task_id !== id || !Array.isArray(chat.messages) ||
      chat.messages.length !== (detail.has_result ? 2 : 1)) return false;
  const producer = chat.producer as unknown;
  if (!record(producer) || !closedFields(producer, ['specialist_id', 'specialist_version', 'prompt_fingerprint', 'tier',
      'confidence_tier', 'provider_mode', 'provider_id', 'model', 'protocol_version']) ||
      producer.specialist_id !== detail.specialist.id || producer.specialist_version !== detail.specialist.version ||
      producer.prompt_fingerprint !== detail.specialist.prompt_fingerprint || !identity(producer.prompt_fingerprint) ||
      producer.tier !== detail.specialist.tier || producer.confidence_tier !== detail.specialist.confidence_tier ||
      producer.provider_mode !== detail.provider.mode || producer.provider_id !== detail.provider.provider_id ||
      producer.model !== detail.provider.model ||
      (producer.protocol_version !== null && !identity(producer.protocol_version)) ||
      (producer.protocol_version ?? null) !== (detail.provider.protocol_version ?? null)) return false;

  const ids = new Set<string>();
  const first = chat.messages[0] as unknown;
  if (!record(first) || !closedFields(first, ['id', 'ordinal', 'author', 'message']) ||
      !identity(first.id) || first.ordinal !== 0 || first.author !== 'user' || ids.has(first.id)) return false;
  ids.add(first.id);
  const firstMessage = first.message as unknown;
  if (!record(firstMessage) || !closedFields(firstMessage, ['kind', 'question']) || firstMessage.kind !== 'user_question' ||
      typeof firstMessage.question !== 'string' || new TextEncoder().encode(firstMessage.question).length > 16 * 1024 ||
      firstMessage.question !== detail.question) return false;
  if (!detail.has_result) return result === null;

  const second = chat.messages[1] as unknown;
  if (!result || !record(second) || !closedFields(second, ['id', 'ordinal', 'author', 'message']) ||
      !identity(second.id) || ids.has(second.id) || second.ordinal !== 1 || second.author !== 'specialist') return false;
  const secondMessage = second.message as unknown;
  return record(secondMessage) && closedFields(secondMessage, ['kind', 'task_id', 'result_id', 'result_hash']) &&
    secondMessage.kind === 'result_reference' && secondMessage.task_id === id &&
    secondMessage.result_id === result.result_id && hash(secondMessage.result_hash);
}

function mergeHistory(existing: InvestigationSummary[], incoming: InvestigationSummary[]) {
  const byId = new Map(existing.map((item) => [item.investigation_id, item]));
  for (const item of incoming) {
    const previous = byId.get(item.investigation_id);
    if (!previous || item.revision >= previous.revision) byId.set(item.investigation_id, item);
  }
  return [...byId.values()];
}

function mergeConversationPage(existing: InvestigationSummary[], incoming: InvestigationSummary[], prefixCount: number) {
  // The verified prefix follows the current server cursor; cached older rows stay behind it.
  const prefix = existing.slice(0, Math.min(prefixCount, existing.length));
  const tail = existing.slice(prefix.length);
  const prefixIds = new Set(prefix.map((item) => item.investigation_id));
  const incomingIds = new Set(incoming.map((item) => item.investigation_id));
  const seen = new Set(prefixIds);
  const pageRows = incoming.filter((item) => {
    if (seen.has(item.investigation_id)) return false;
    seen.add(item.investigation_id);
    return true;
  });
  const updatedPrefix = prefix.map((item) => {
    const fresh = incoming.find((candidate) => candidate.investigation_id === item.investigation_id);
    return fresh && fresh.revision >= item.revision ? fresh : item;
  });
  const retainedTail = tail.filter((item) => !incomingIds.has(item.investigation_id));
  return [...updatedPrefix, ...pageRows, ...retainedTail];
}

type ConversationPageBookmark = { cursor: string; ids: string[] };

export function normalizedHistorySearch(query: string) {
  return query.replace(/^[\t\n\v\f\r ]+|[\t\n\v\f\r ]+$/g, '')
    .replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

export function historyTextMatches(text: string, query: string) {
  return text.replace(/[A-Z]/g, (letter) => letter.toLowerCase()).includes(normalizedHistorySearch(query));
}

function summaryMatchesHistorySearch(summary: InvestigationSummary, query: string, answerMatchIds: readonly string[] = [], nameMatchIds: readonly string[] = []) {
  const needle = normalizedHistorySearch(query);
  if (!needle) return true;
  return historyTextMatches(summary.question, needle) || historyTextMatches(summary.specialist_id, needle) ||
    (summary.has_result && answerMatchIds.includes(summary.investigation_id)) || nameMatchIds.includes(summary.investigation_id);
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
  invalidateReview: (message: FindingReviewChanged) => void;
  /** Curation reviews of the selected result's findings (ADR-0074). A
   *  finding with no row has none. Separate from the follow-up reviews. */
  findingCurations: FindingCuration[];
  curationLoading: boolean;
  curationBusy: boolean;
  curationError: string | null;
  refreshCurations: () => Promise<void>;
  curateFinding: (findingId: string, decision: CurationDecision) => Promise<void>;
  invalidateCuration: (message: FindingCurationChanged) => void;
  catalog: InvestigationCatalog | null;
  catalogLoading: boolean;
  catalogError: string | null;
  history: InvestigationSummary[];
  historySearch: string;
  historyAnswerMatchIds: string[];
  historyNameMatchIds: string[];
  historyActiveOnly: boolean;
  nextCursor: string | null;
  historyLoading: boolean;
  historyError: string | null;
  conversationId: string | null;
  conversationItems: InvestigationSummary[];
  conversationPagePrefixCount: number;
  conversationPageBookmarks: ConversationPageBookmark[];
  conversationDirtyPages: string[];
  conversationPinned: InvestigationSummary | null;
  conversationCursor: string | null;
  conversationLoading: boolean;
  conversationError: string | null;
  selectedId: string | null;
  selectionGeneration: number;
  detail: InvestigationDetail | null;
  result: InvestigationResult | null;
  chat: InvestigationChat | null;
  chatStatus: InvestigationChatStatus;
  chatError: string | null;
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
  startRejection: 'refresh_required' | 'invalid_input' | null;
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
  selectHistoryActive: (activeOnly: boolean) => Promise<void>;
  searchHistory: (query: string, options?: { activeOnly: boolean }) => Promise<void>;
  start: (draft: InvestigationDraft, options?: InvestigationStartOptions) => Promise<string | null>;
  retryStart: (options?: { deferOpen?: boolean }) => Promise<string | null>;
  openConversation: (conversationId: string) => Promise<void>;
  loadConversation: (more?: boolean) => Promise<void>;
  closeConversation: () => void;
  open: (id: string, options?: { awaitChat?: boolean; loadingSignal?: AbortSignal }) => Promise<void>;
  refreshSelected: (options?: { awaitChat?: boolean; loadingSignal?: AbortSignal }) => Promise<void>;
  invalidate: (message: InvestigationChanged) => void;
  showConsent: () => void;
  hideConsent: () => void;
  approve: () => Promise<void>;
  decline: () => Promise<void>;
  cancel: (id?: string) => Promise<void>;
  readCitation: (citationId: string, options?: { signal?: AbortSignal }) => Promise<void>;
  clearCitation: () => void;
}

let reviewRequest = 0;
let curationRequest = 0;
let catalogRequest = 0;
let historyRequest = 0;
let conversationRequest = 0;
let conversationRefreshPending: number | null = null;
let conversationRefreshFirstPagePending = false;
let conversationDirtyInFlight: { conversationId: string; cursor: string; request: number } | null = null;
let detailRequest = 0;
// A newer task notification seen while a detail read is loading. That read may
// predate it (#2476), so one trailing refresh runs when the read settles.
let detailRefreshPending: { id: string; generation: number } | null = null;
let citationRequest = 0;
let citationOwnerCleanup: (() => void) | undefined;
let actionRequest = 0;
let locationRequest = 0;
let choicesRequest = 0;

export const useInvestigationStore = create<InvestigationState>((set, get) => {
  let pendingStartPreservesComposer = false;
  const refreshDirtyConversationPages = async (conversationId: string) => {
    if (conversationDirtyInFlight?.conversationId === conversationId) return;
    const dirtyCursors = [...get().conversationDirtyPages];
    for (const cursor of dirtyCursors) {
      if (get().conversationId !== conversationId || get().conversationLoading ||
          conversationDirtyInFlight?.conversationId === conversationId) return;
      const bookmark = get().conversationPageBookmarks.find((page) => page.cursor === cursor);
      set((state) => ({ conversationDirtyPages: state.conversationDirtyPages.filter((page) => page !== cursor) }));
      if (!bookmark) continue;
      const current = get();
      if (current.conversationId !== conversationId || current.conversationLoading) {
        set((state) => ({ conversationDirtyPages: state.conversationDirtyPages.includes(cursor)
          ? state.conversationDirtyPages : [...state.conversationDirtyPages, cursor] }));
        return;
      }
      const request = ++conversationRequest;
      const isCurrent = () => request === conversationRequest && get().conversationId === conversationId;
      conversationDirtyInFlight = { conversationId, cursor, request };
      let failed = false;
      set({ conversationLoading: true, conversationError: null });
      try {
        const page = await invokeOr<InvestigationPage | null>('list_investigation_conversation', null,
          { conversationId, cursor });
        if (!page || !Array.isArray(page.items) || page.items.length > 50 ||
            page.items.some((item) => !validSummary(item) || item.conversation_id !== conversationId) ||
            (page.next_cursor !== null && typeof page.next_cursor !== 'string') ||
            page.items.length !== bookmark.ids.length ||
            page.items.some((item, index) => item.investigation_id !== bookmark.ids[index])) {
          throw new Error('Invalid refreshed conversation page.');
        }
        if (!isCurrent()) return;
        set((state) => ({ conversationItems: state.conversationItems.map((item) => {
          const fresh = page.items.find((candidate) => candidate.investigation_id === item.investigation_id);
          if (!fresh) return item;
          const known = [...state.history, ...state.conversationItems,
            ...(state.detail ? [state.detail] : []), ...(state.conversationPinned ? [state.conversationPinned] : [])]
            .filter((candidate) => candidate.investigation_id === item.investigation_id)
            .reduce((newer, candidate) => candidate.revision > newer.revision ? candidate : newer, fresh);
          return known.revision >= item.revision ? known : item;
        }), conversationError: null }));
      } catch {
        failed = true;
        if (isCurrent()) {
          set((state) => ({ conversationDirtyPages: state.conversationDirtyPages.includes(cursor)
            ? state.conversationDirtyPages : [cursor, ...state.conversationDirtyPages],
          conversationError: 'Conversation turns could not be refreshed. Previously loaded turns remain available.' }));
        }
      } finally {
        if (conversationDirtyInFlight?.request === request) conversationDirtyInFlight = null;
        if (isCurrent()) set({ conversationLoading: false });
        else if (get().conversationId === conversationId) {
          // A same-conversation reopen invalidates the response generation but
          // must not lose the dirty page removed when this request began.
          set((state) => ({ conversationDirtyPages: state.conversationDirtyPages.includes(cursor)
            ? state.conversationDirtyPages : [cursor, ...state.conversationDirtyPages] }));
          void refreshDirtyConversationPages(conversationId);
        }
        if (isCurrent() && conversationRefreshPending === request) {
          conversationRefreshPending = null;
          const refreshFirstPage = conversationRefreshFirstPagePending;
          conversationRefreshFirstPagePending = false;
          if (refreshFirstPage) void get().loadConversation();
          else void refreshDirtyConversationPages(conversationId);
        }
      }
      if (failed) return;
    }
  };

  const refreshConversation = async (conversationId: string, investigationId?: string) => {
    const state = get();
    if (state.conversationId !== conversationId) return;
    const continuation = investigationId && state.conversationPageBookmarks.find((page) => page.ids.includes(investigationId));
    if (continuation) set((current) => ({ conversationDirtyPages: current.conversationDirtyPages.includes(continuation.cursor)
      ? current.conversationDirtyPages : [...current.conversationDirtyPages, continuation.cursor] }));
    if (state.conversationLoading) {
      conversationRefreshPending = conversationRequest;
      if (!continuation) conversationRefreshFirstPagePending = true;
      return;
    }
    if (continuation) await refreshDirtyConversationPages(conversationId);
    else await get().loadConversation();
  };

  const updateSummary = (summary: InvestigationSummary) => {
    if (get().historyActiveOnly) ++historyRequest;
    set((state) => {
      const saved = state.history.find((item) => item.investigation_id === summary.investigation_id);
      const row = saved && saved.revision > summary.revision ? saved : summary;
      const nameMatches = state.historyNameMatchIds.filter((id) => id !== row.investigation_id ||
        saved?.specialist_id === row.specialist_id);
      const matches = (!state.historyActiveOnly || investigationIsInActiveHistory(row)) && summaryMatchesHistorySearch(row, state.historySearch, state.historyAnswerMatchIds, nameMatches);
      const history = !matches
        ? state.history.filter((item) => item.investigation_id !== row.investigation_id)
        : saved ? mergeHistory(state.history, [row]) : [row, ...state.history];
      return {
        history,
        historyAnswerMatchIds: normalizedHistorySearch(state.historySearch)
          ? state.historyAnswerMatchIds.filter((id) => history.some((item) => item.investigation_id === id && item.has_result)) : [],
        historyNameMatchIds: normalizedHistorySearch(state.historySearch)
          ? nameMatches.filter((id) => history.some((item) => item.investigation_id === id)) : [],
        conversationPinned: state.conversationPinned?.investigation_id === summary.investigation_id &&
          state.conversationPinned.conversation_id === summary.conversation_id && summary.revision >= state.conversationPinned.revision
          ? summary : state.conversationPinned,
        // A summary that reports a result is merged into the detail only once that
        // result is held; otherwise the next consistent refresh supplies both.
        detail: ![4, 5, 6, 7].includes(summary.schema_version) && state.detail?.investigation_id === summary.investigation_id && summary.revision >= state.detail.revision &&
          (!summary.has_result || state.result !== null)
          ? { ...state.detail, ...summary } : state.detail,
        historyLoading: state.historyActiveOnly ? false : state.historyLoading,
      };
    });
  };

  const startRequest = async (request: StartInvestigationRequest, preserveComposer = pendingStartPreservesComposer,
    deferOpen = false): Promise<string | null> => {
    if (get().starting) return null;
    pendingStartPreservesComposer = preserveComposer;
    set({ starting: true, pendingStart: request, startError: null, startRejection: null });
    let summary: InvestigationSummary;
    try {
      const response = await invokeOr<InvestigationSummary | null>('start_investigation', null, { request });
      if (!validSummary(response)) throw new Error('No durable identity returned.');
      summary = response;
    } catch (error) {
      if (typeof error === 'object' && error !== null && 'code' in error && (error.code === 'refresh_required' || error.code === 'invalid_input') &&
          'definitive' in error && error.definitive === true) {
        pendingStartPreservesComposer = false;
        set({ starting: false, pendingStart: null,
          startRejection: error.code,
          startError: error.code === 'refresh_required'
            ? [4, 5, 6, 7].includes(request.schema_version) ? 'The selected evidence or completion context changed before admission. No investigation was started. Refresh choices and remove stale obligations.'
              : 'The selected captured source changed before admission. No investigation was started. Remove the stale requirement and select its source again.'
            : `The request was rejected before admission. No investigation was started.${'message' in error && typeof error.message === 'string' ? ` ${error.message}` : ''}` });
        return null;
      }
      set({ starting: false, startRejection: null,
        startError: 'The start outcome could not be confirmed. Retry this same request to recover its durable identity; this does not start another investigation.' });
      return null;
    }
    updateSummary(summary);
    const preserveComposerOnAccept = pendingStartPreservesComposer;
    pendingStartPreservesComposer = false;
    set((state) => ({ starting: false, pendingStart: null, startRejection: null,
      ...(preserveComposerOnAccept ? {} : { composer: { ...state.composer, question: '', parent: null },
        draftInspectionRequirements: [], inspectionDraftError: null,
        draftObligations: [], obligationChoices: null, obligationChoicesLoading: false, obligationDraftError: null }) }));
    // Admission is already confirmed. A later detail-load failure must not
    // turn this into an uncertain start or invite a fresh nonce.
    if (!deferOpen) {
      try {
        await get().open(summary.investigation_id);
      } catch {
        set({ error: 'Investigation started, but its detail could not be loaded. Refresh the saved investigation.' });
      }
    }
    if (get().conversationId === summary.conversation_id) {
      ++conversationRequest;
      set({ conversationLoading: false });
      // Admission is already confirmed; an optional conversation refresh must
      // not keep the start action busy while its page is loading.
      void get().loadConversation();
    }
    return summary.investigation_id;
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
      const { selectedId, result, selectionGeneration, findingReviews, reviewBusy, reviewLoading, reviewError } = get();
      const row = findingReviews.find((r) => r.subject.finding_id === findingId);
      if (!result || !row || reviewBusy || reviewLoading || reviewError) return;
      const current = () => get().selectedId === selectedId && get().selectionGeneration === selectionGeneration && get().result?.result_id === result.result_id;
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
        if (typeof error === 'object' && error !== null && 'code' in error && error.code === 'busy') {
          set({ reviewBusy: false, reviewError: 'Review storage was busy and this decision was not saved. Refresh reviews, then choose again. No automatic retry was made.' });
          return;
        }
        set({ reviewBusy: false, reviewError: 'The review outcome could not be confirmed. Refresh reviews to recover the saved decision before choosing again. No automatic retry was made.' });
      }
    },
    invalidateReview: (message) => {
      const { result, findingReviews } = get();
      if (!result || !validReviewChanged(message, result)) return;
      const known = findingReviews.find((row) => row.subject.subject_id === message.subject_id);
      if (!known || message.review_revision > known.revision) void get().refreshReviews();
    },
    findingCurations: [], curationLoading: false, curationBusy: false, curationError: null,
    refreshCurations: async () => {
      const { selectedId, selectionGeneration, result } = get();
      if (!selectedId || !result) return;
      const request = ++curationRequest;
      const current = () => get().selectionGeneration === selectionGeneration && get().selectedId === selectedId && get().result?.result_id === result.result_id && request === curationRequest;
      set({ curationLoading: true });
      try {
        const value = await invokeOr<FindingCurations | null>('get_investigation_finding_curations', null, { investigationId: selectedId, expectedResultId: result.result_id });
        if (!current()) return;
        if (!validCurations(value, result)) throw new Error('Invalid curation read');
        set({ findingCurations: mergeCurations(get().findingCurations, value.reviews), curationLoading: false, curationError: null });
      } catch {
        if (current()) set({ curationLoading: false, curationError: 'Curation reviews are unavailable. Refresh curation reviews before deciding. Historical findings remain readable.' });
      }
    },
    curateFinding: async (findingId, decision) => {
      const { selectedId, result, selectionGeneration, findingReviews, findingCurations, curationBusy, curationLoading, curationError } = get();
      // The host names a finding by its review subject, so that row must be loaded.
      const subject = findingReviews.find((row) => row.subject.finding_id === findingId)?.subject;
      const known = findingCurations.find((row) => row.finding_id === findingId);
      if (!result || !subject || curationBusy || curationLoading || curationError) return;
      if (known && known.subject_id !== subject.subject_id) {
        set({ curationError: 'The saved curation review names another finding identity. Refresh curation reviews before deciding.' });
        return;
      }
      const expected = known?.review_revision ?? 0;
      const current = () => get().selectedId === selectedId && get().selectionGeneration === selectionGeneration && get().result?.result_id === result.result_id;
      set({ curationBusy: true, curationError: null });
      try {
        const value = await invokeOr<FindingCuration | null>('curate_investigation_finding', null, { requestJson: JSON.stringify({ schema_version: 1, subject, expected_revision: expected, decision }) });
        if (!current()) return;
        if (!validCuration(value, result) || value.subject_id !== subject.subject_id || value.finding_id !== findingId || value.review_revision !== expected + 1 || value.review_decision !== decision) throw new Error('Unconfirmed write');
        set({ findingCurations: mergeCurations(get().findingCurations, [value]), curationBusy: false });
      } catch (error) {
        if (!current()) return;
        const code = typeof error === 'object' && error !== null && 'code' in error ? error.code : null;
        set({ curationBusy: false, curationError: code === 'conflict'
          ? 'Another curation review was saved first and this one was not. Refresh curation reviews, then deliberately choose again.'
          : code === 'busy'
            ? 'Curation storage was busy and this review was not saved. Refresh curation reviews, then choose again. No automatic retry was made.'
            : 'The curation review could not be confirmed. Refresh curation reviews to recover the saved decision before choosing again. No automatic retry was made.' });
      }
    },
    invalidateCuration: (message) => {
      const { result, findingCurations } = get();
      if (!result || !validCurationChanged(message, result)) return;
      const known = findingCurations.find((row) => row.subject_id === message.subject_id);
      if (!known || message.review_revision > known.review_revision) void get().refreshCurations();
    },
    catalog: null, catalogLoading: false, catalogError: null,
    history: [], historySearch: '', historyAnswerMatchIds: [], historyNameMatchIds: [], historyActiveOnly: false, nextCursor: null, historyLoading: false, historyError: null,
    conversationId: null, conversationItems: [], conversationPagePrefixCount: 0, conversationPageBookmarks: [], conversationDirtyPages: [], conversationPinned: null, conversationCursor: null, conversationLoading: false, conversationError: null,
    selectedId: null, selectionGeneration: 0, detail: null, result: null, chat: null,
    chatStatus: 'idle', chatError: null, events: [],
    consent: null, consentOpen: false, loading: false, error: null,
    actionBusy: false, actionError: null, pendingStart: null, starting: false, startError: null, startRejection: null,
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
      // An explicit specialist choice also fixes the budget, so a later catalog
      // refresh treats it as chosen (AC-0559). ADR-0071: the version decides its
      // profile, so every selection path (the composer, Draft this section)
      // gets the profile that version runs under (AC-1402).
      const pinned = patch.specialistId !== undefined && !('limitProfile' in patch)
        ? { limitProfile: profileFor(get().catalog, patch.specialistId) } : {};
      if (!get().starting && !get().pendingStart) {
        set((state) => ({ composer: { ...state.composer, ...structuredClone(patch), ...pinned } }));
        // Remember an explicit budget or specialist choice across restarts (#1636).
        const { specialistId, limitProfile } = get().composer;
        if (('specialistId' in patch || 'limitProfile' in patch) && limitProfile !== undefined)
          invokeOr('set_investigation_choice', null, { specialistId, limitProfile }).catch(() => undefined);
      }
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
        const [catalog, stored] = await Promise.all([
          invokeOr<InvestigationCatalog | null>('investigation_specialists', null), storedChoice()]);
        if (!catalog || catalog.schema_version !== 1 || !Array.isArray(catalog.specialists) ||
            !Array.isArray(catalog.providers)) throw new Error('Catalog unavailable.');
        if (request === catalogRequest) set((state) => {
          const composer = state.starting || state.pendingStart ? null : restoredComposer(state.composer, catalog, stored);
          return composer ? { catalog, catalogLoading: false, composer } : { catalog, catalogLoading: false };
        });
      } catch {
        if (request === catalogRequest) set({ catalogLoading: false,
          catalogError: 'Specialists are unavailable. Connect to the app core and refresh.' });
      }
    },

    loadHistory: async (more = false) => {
      if (more && (!get().nextCursor || get().historyLoading)) return;
      const request = ++historyRequest;
      const current = get();
      const cursor = more ? current.nextCursor : null;
      const search = current.historySearch;
      const activeOnly = current.historyActiveOnly;
      const normalizedSearch = normalizedHistorySearch(search);
      set({ historyLoading: true, historyError: null });
      try {
        const page = await invokeOr<InvestigationPage | null>('list_investigations', null,
          { ...(cursor ? { cursor } : {}), ...(search ? { search } : {}), ...(activeOnly ? { activeOnly: true } : {}) });
        const answerMatches = page?.answer_match_ids === undefined ? [] : page.answer_match_ids;
        const nameMatches = page?.name_match_ids === undefined ? [] : page.name_match_ids;
        if (!page || !Array.isArray(page.items) || page.items.length > INVESTIGATION_PAGE_LIMIT ||
            page.items.some((item) => !validSummary(item)) ||
            new Set(page.items.map((item) => item.investigation_id)).size !== page.items.length ||
            !Array.isArray(answerMatches) || answerMatches.length > page.items.length ||
            new Set(answerMatches).size !== answerMatches.length ||
            (!normalizedSearch && answerMatches.length !== 0) ||
            answerMatches.some((id) => typeof id !== 'string' || !page.items.some((item) => item.investigation_id === id && item.has_result)) ||
            !Array.isArray(nameMatches) || nameMatches.length > page.items.length ||
            new Set(nameMatches).size !== nameMatches.length || (!normalizedSearch && nameMatches.length !== 0) ||
            nameMatches.some((id) => typeof id !== 'string' || answerMatches.includes(id) ||
              !page.items.some((item) => item.investigation_id === id)) ||
            (activeOnly && page.items.some((item) => !investigationIsInActiveHistory(item))) ||
            (normalizedSearch && page.items.some((item) => !summaryMatchesHistorySearch(item, normalizedSearch, answerMatches, nameMatches)))) {
          throw new Error('Invalid history.');
        }
        const incoming = page.items;
        if (request !== historyRequest || normalizedHistorySearch(get().historySearch) !== normalizedSearch || get().historyActiveOnly !== activeOnly) return;
        set((state) => {
          const reconciled = incoming.map((item) => {
            const saved = state.history.find((previous) => previous.investigation_id === item.investigation_id);
            const selectedDetail = state.detail?.investigation_id === item.investigation_id ? state.detail : null;
            const known = [saved, selectedDetail].filter((value): value is InvestigationSummary => value != null)
              .reduce<InvestigationSummary | null>((latest, value) => !latest || value.revision > latest.revision ? value : latest, null);
            return known && known.revision > item.revision
              ? { item: known, answerMatch: known === saved && state.historyAnswerMatchIds.includes(item.investigation_id),
                nameMatch: known === saved && state.historyNameMatchIds.includes(item.investigation_id) }
              : { item, answerMatch: answerMatches.includes(item.investigation_id), nameMatch: nameMatches.includes(item.investigation_id) };
          });
          // Match evidence follows the row that won revision reconciliation.
          const matches = normalizedSearch ? [...state.historyAnswerMatchIds.filter((id) =>
            !incoming.some((item) => item.investigation_id === id)),
            ...reconciled.filter((entry) => entry.answerMatch).map((entry) => entry.item.investigation_id)] : [];
          const names = normalizedSearch ? [...state.historyNameMatchIds.filter((id) =>
            !incoming.some((item) => item.investigation_id === id)),
            ...reconciled.filter((entry) => entry.nameMatch).map((entry) => entry.item.investigation_id)] : [];
          const refreshed = reconciled.map((entry) => entry.item).filter((item) =>
            (!activeOnly || investigationIsInActiveHistory(item)) && summaryMatchesHistorySearch(item, state.historySearch, matches, names));
          const history = more
            ? mergeHistory(state.history, refreshed).filter((item) =>
              (!activeOnly || investigationIsInActiveHistory(item)) && summaryMatchesHistorySearch(item, state.historySearch, matches, names))
            : activeOnly ? refreshed : [
              ...refreshed,
              ...state.history.filter((item) => !incoming.some((fresh) => fresh.investigation_id === item.investigation_id)),
            ];
          return ({ history,
        historyAnswerMatchIds: matches.filter((id) => history.some((item) => item.investigation_id === id && item.has_result)),
        historyNameMatchIds: names.filter((id) => history.some((item) => item.investigation_id === id)),
        // Background first-page refresh must not discard older explicit pages
        // or move their continuation backwards while another task is active.
        nextCursor: activeOnly || more || state.history.length <= page.items.length ? page.next_cursor : state.nextCursor,
        historyLoading: false });
        });
      } catch {
        if (request === historyRequest) set({ historyLoading: false,
          historyError: normalizedSearch ? 'Investigation history search could not be completed.'
            : 'Investigation history could not be refreshed. Previously loaded records are retained.' });
      }
    },

    selectHistoryActive: async (activeOnly) => {
      if (get().historyActiveOnly === activeOnly) return;
      ++historyRequest;
      set({ historyActiveOnly: activeOnly, history: [], historyAnswerMatchIds: [], historyNameMatchIds: [], nextCursor: null, historyError: null });
      await get().loadHistory();
    },

    searchHistory: async (query, options) => {
      ++historyRequest;
      set({ historySearch: query, historyActiveOnly: options?.activeOnly ?? get().historyActiveOnly,
        history: [], historyAnswerMatchIds: [], historyNameMatchIds: [], nextCursor: null, historyError: null });
      await get().loadHistory();
    },

    openConversation: async (conversationId) => {
      const state = get();
      const sameConversation = state.conversationId === conversationId;
      const selected = state.detail ?? state.history.find((item) => item.investigation_id === state.selectedId) ??
        state.conversationItems.find((item) => item.investigation_id === state.selectedId);
      ++conversationRequest;
      conversationRefreshPending = null;
      conversationRefreshFirstPagePending = false;
      const reopenedDirty = sameConversation && conversationDirtyInFlight?.conversationId === conversationId
        ? [...state.conversationDirtyPages, conversationDirtyInFlight.cursor] : state.conversationDirtyPages;
      set({ conversationId, conversationItems: sameConversation ? state.conversationItems : [],
        conversationPagePrefixCount: sameConversation ? state.conversationPagePrefixCount : 0,
        conversationPageBookmarks: sameConversation ? state.conversationPageBookmarks : [],
        conversationDirtyPages: sameConversation ? [...new Set(reopenedDirty)] : [],
        conversationPinned: selected?.conversation_id === conversationId ? selected : sameConversation ? state.conversationPinned : null,
        conversationCursor: sameConversation ? state.conversationCursor : null,
        conversationLoading: false, conversationError: null });
      await get().loadConversation();
    },
    loadConversation: async (more = false) => {
      const current = get();
      if (!current.conversationId || current.conversationLoading || (more && !current.conversationCursor)) return;
      const conversationId = current.conversationId;
      const cursor = more ? current.conversationCursor : null;
      const request = ++conversationRequest;
      const isCurrent = () => request === conversationRequest && get().conversationId === conversationId;
      let loaded = false;
      set({ conversationLoading: true, conversationError: null });
      try {
        const page = await invokeOr<InvestigationPage | null>('list_investigation_conversation', null,
          { conversationId, ...(cursor ? { cursor } : {}) });
        if (!page || !Array.isArray(page.items) || page.items.length > 50 ||
            page.items.some((item) => !validSummary(item) || item.conversation_id !== conversationId) ||
            (page.next_cursor !== null && typeof page.next_cursor !== 'string')) throw new Error('Invalid conversation page.');
        if (!isCurrent()) return;
        set((state) => {
          const known = [...state.history, ...state.conversationItems,
            ...(state.detail ? [state.detail] : []), ...(state.conversationPinned ? [state.conversationPinned] : [])]
            .filter((item) => item.conversation_id === conversationId);
          const currentPage = page.items.map((item) => known.reduce((newer, candidate) =>
            candidate.investigation_id === item.investigation_id && candidate.revision > newer.revision ? candidate : newer, item));
          const pageIds = new Set(currentPage.map((item) => item.investigation_id));
          const retainedOlder = more ? [] : state.conversationItems.filter((item) => !pageIds.has(item.investigation_id))
            .map((item) => known.reduce((newer, candidate) =>
              candidate.investigation_id === item.investigation_id && candidate.revision > newer.revision ? candidate : newer, item));
          const prefixCount = more ? Math.min(state.conversationPagePrefixCount, state.conversationItems.length) : 0;
          const prefixIds = new Set(state.conversationItems.slice(0, prefixCount).map((item) => item.investigation_id));
          const newPrefixCount = more ? new Set(currentPage.filter((item) => !prefixIds.has(item.investigation_id))
            .map((item) => item.investigation_id)).size : 0;
          return { conversationItems: more
              ? mergeConversationPage(state.conversationItems, currentPage, prefixCount)
              : [...currentPage, ...retainedOlder],
            conversationPagePrefixCount: more ? prefixCount + newPrefixCount : currentPage.length,
            conversationPageBookmarks: more && cursor
              ? [...state.conversationPageBookmarks.filter((bookmark) => bookmark.cursor !== cursor),
                { cursor, ids: currentPage.map((item) => item.investigation_id) }]
              : state.conversationPageBookmarks,
            conversationCursor: page.next_cursor,
            conversationLoading: false, conversationError: null };
        });
        loaded = true;
      } catch {
        if (isCurrent()) set({ conversationLoading: false,
          conversationError: 'Conversation turns could not be loaded. Previously loaded turns remain available.' });
      } finally {
        if (isCurrent() && conversationRefreshPending === request) {
          conversationRefreshPending = null;
          const refreshFirstPage = conversationRefreshFirstPagePending;
          conversationRefreshFirstPagePending = false;
          if (refreshFirstPage) void get().loadConversation();
          else void refreshDirtyConversationPages(conversationId);
        }
      }
      if (loaded) await refreshDirtyConversationPages(conversationId);
    },
    closeConversation: () => {
      ++conversationRequest;
      conversationRefreshPending = null;
      conversationRefreshFirstPagePending = false;
      set({ conversationId: null, conversationItems: [], conversationPagePrefixCount: 0, conversationPageBookmarks: [], conversationDirtyPages: [], conversationPinned: null, conversationCursor: null,
        conversationLoading: false, conversationError: null });
    },

    start: async (draft, options = {}) => {
      if (get().pendingStart || get().starting) return null;
      // A definitive refusal belongs to the prior attempt. Never let it
      // influence local validation or the next request's recovery behavior.
      set({ startRejection: null });
      const request_nonce = crypto.randomUUID();
      const { obligations: completionObligations, ...legacyBase } = draft;
      if (isCompletionSpecialist(draft.specialist_id)) {
        const policy = investigationPolicy(get().catalog, draft.limit_profile, draft.specialist_id);
        if (!policy) return null;
        const choices = options.completionChoices ?? get().obligationChoices;
        if (!choices || !validObligationChoices(choices) || !draft.obligations || !validObligationEnvelope(draft.obligations, policy) || draft.inspection_requirements !== undefined ||
            draft.obligations.obligations.some((item) => item.kind === 'source' ? item.target.graph_snapshot_id !== choices.graph_snapshot_id : item.target.context_id !== choices.context_id)) return null;
        const request: StartInvestigationRequest = { schema_version: policy.request_schema_version, request_nonce, specialist_id: draft.specialist_id,
          obligations: structuredClone(draft.obligations), question: draft.question, scope: draft.scope,
          provider_mode: draft.provider_mode, limit_profile: draft.limit_profile,
          expected_graph_revision: choices.graph_snapshot_id, expected_context_id: choices.context_id,
          conversation_id: draft.conversation_id ?? null, parent_id: draft.parent_id ?? null };
        if (new TextEncoder().encode(JSON.stringify(request)).length > policy.request_bytes) {
          const message = `This complete request exceeds ${policy.request_bytes.toLocaleString()} bytes. Shorten it or remove an obligation.`;
          set(options.preserveComposer ? { startError: message } : { obligationDraftError: message });
          return null;
        }
        return startRequest(request, options.preserveComposer === true, options.deferOpen === true);
      } else if (completionObligations !== undefined) return null;
      else if (draft.specialist_id.endsWith('@6')) {
        const { inspection_requirements: requirements, ...qualified } = legacyBase;
        if (requirements !== undefined && requirements.length === 0) return null;
        return startRequest(structuredClone({ ...qualified, schema_version: 3, request_nonce,
          ...(requirements ? { expected_graph_revision: requirements[0].graph_snapshot_id,
          inspection_requirements: requirements } : {}) }), options.preserveComposer === true, options.deferOpen === true);
      } else if (draft.specialist_id.endsWith('@5')) {
        const { expected_context_id, ...inspectionDraft } = legacyBase;
        if (!draft.inspection_requirements?.length || expected_context_id !== undefined) return null;
        return startRequest(structuredClone({ ...inspectionDraft, schema_version: 2, request_nonce,
          expected_graph_revision: draft.inspection_requirements[0].graph_snapshot_id,
          inspection_requirements: draft.inspection_requirements }), options.preserveComposer === true, options.deferOpen === true);
      } else {
        // Preserve legacy schema bytes: requirements are absent, never an empty list.
        const { inspection_requirements: requirements, expected_context_id, ...legacy } = legacyBase;
        if (requirements?.length || expected_context_id !== undefined) return null;
        return startRequest(structuredClone({ ...legacy, schema_version: 1, request_nonce }), options.preserveComposer === true, options.deferOpen === true);
      }
    },
    retryStart: async (options) => {
      const request = get().pendingStart;
      return request ? startRequest(request, pendingStartPreservesComposer, options?.deferOpen === true) : null;
    },
    open: async (id, options) => {
      if (options?.loadingSignal?.aborted) return;
      const beforeOpen = get();
      const selectedSummary = (beforeOpen.detail?.investigation_id === id ? beforeOpen.detail : null) ?? beforeOpen.history.find((item) => item.investigation_id === id) ??
        beforeOpen.conversationItems.find((item) => item.investigation_id === id);
      if (beforeOpen.conversationId && selectedSummary?.conversation_id === beforeOpen.conversationId) {
        set({ conversationPinned: selectedSummary });
      } else if (beforeOpen.conversationId && selectedSummary && selectedSummary.conversation_id !== beforeOpen.conversationId) {
        ++conversationRequest;
        conversationRefreshPending = null;
        conversationRefreshFirstPagePending = false;
        set({ conversationId: null, conversationItems: [], conversationPagePrefixCount: 0, conversationPageBookmarks: [], conversationDirtyPages: [], conversationPinned: null, conversationCursor: null,
          conversationLoading: false, conversationError: null });
      } else if (beforeOpen.conversationId) set({ conversationPinned: null });
      ++detailRequest;
      ++reviewRequest;
      set({ findingReviews: [], reviewLoading: false, reviewBusy: false, reviewError: null });
      ++curationRequest;
      set({ findingCurations: [], curationLoading: false, curationBusy: false, curationError: null });
      citationOwnerCleanup?.(); citationOwnerCleanup = undefined;
      ++citationRequest;
      ++actionRequest;
      ++locationRequest;
      set((state) => ({ selectedId: id, selectionGeneration: state.selectionGeneration + 1,
        detail: null, result: null, chat: null, chatStatus: 'loading', chatError: null,
        events: [], consent: null, consentOpen: false,
        loading: false, error: null, actionBusy: false, actionError: null,
        citationId: null, citationRead: null, citationLoading: false, citationError: null }));
      set({ inspectionLocation: null, inspectionLocationTargetId: null, inspectionLocationLoading: false, inspectionLocationError: null });
      await get().refreshSelected(options);
    },

    refreshSelected: async (options) => {
      if (options?.loadingSignal?.aborted) return;
      const { selectedId: id, selectionGeneration, events: previousEvents } = get();
      if (!id) return;
      const request = ++detailRequest;
      // This read starts after any notification already seen, so it covers it.
      detailRefreshPending = null;
      const ownsSelection = () => get().selectedId === id && get().selectionGeneration === selectionGeneration && request === detailRequest;
      const current = () => ownsSelection() && !options?.loadingSignal?.aborted;
      const releaseLoading = () => {
        if (ownsSelection()) set({ loading: false, chat: null, chatStatus: 'error',
          chatError: 'Chat loading stopped when its pane closed. Refresh to load the saved transcript.' });
      };
      set({ loading: true, error: null, chatStatus: 'loading', chatError: null });
      // The signal releases UI ownership, never the underlying IPC/read slot.
      options?.loadingSignal?.addEventListener('abort', releaseLoading, { once: true });
      if (options?.loadingSignal?.aborted) {
        releaseLoading();
        options.loadingSignal.removeEventListener('abort', releaseLoading);
        return;
      }
      let transcriptSettled: Promise<unknown> | undefined;
      try {
        const chatObservation = invokeOr<InvestigationChat | null | undefined>('get_investigation_chat', undefined,
          { investigationId: id }).then(
          (value) => ({ kind: 'value' as const, value }),
          () => ({ kind: 'error' as const }),
        );
        transcriptSettled = chatObservation;
        const [detail, result, consent] = await Promise.all([
          invokeOr<InvestigationDetail | null>('get_investigation', null, { investigationId: id }),
          invokeOr<InvestigationResult | null>('investigation_result', null, { investigationId: id }),
          invokeOr<InvestigationConsent | null>('investigation_consent', null, { investigationId: id }),
        ]);
        if (!current()) return;
        if (!validSummary(detail, id)) throw new Error('Wrong detail.');
        if ('reported_usage_exact' in detail && !validReportedTaskUsage(detail.reported_usage_exact)) {
          throw new Error('Invalid exact usage projection.');
        }
        if ('runtime' in detail && !validRunRuntime(detail.runtime)) throw new Error('Invalid runtime projection.');
        if (get().conversationId && detail.conversation_id !== get().conversationId) {
          ++conversationRequest;
          conversationRefreshPending = null;
          conversationRefreshFirstPagePending = false;
          set({ conversationId: null, conversationItems: [], conversationPagePrefixCount: 0, conversationPageBookmarks: [], conversationDirtyPages: [], conversationPinned: null, conversationCursor: null,
            conversationLoading: false, conversationError: null });
        }
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
          set({ loading: options?.awaitChat === true, chat: null, chatStatus: 'error',
            chatError: 'The transcript observation was older than this task. Refresh to load a consistent version.' });
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
        // Publish the usable task/result snapshot independently of transcript I/O.
        // A stalled or unavailable chat read must not hold back existing findings.
        updateSummary(detail);
        set((state) => ({ detail, result: result ?? savedResult, chat: null, chatStatus: 'loading', chatError: null,
          events, consent,
          conversationPinned: state.conversationId === detail.conversation_id ? detail : state.conversationPinned,
          consentOpen: state.consentOpen && state.consent?.step_id === consent?.step_id &&
            state.consent?.preview.payload_hash === consent?.preview.payload_hash,
          loading: options?.awaitChat === true, error: null }));
        if (current()) { void get().refreshReviews(); void get().refreshCurations(); }

        transcriptSettled = chatObservation.then((chatRead) => {
          if (!current()) return;
          if (get().detail && get().detail!.revision > detail.revision) {
            set({ chat: null, chatStatus: 'error',
              chatError: 'The transcript observation was older than this task. Refresh to load a consistent version.' });
            return;
          }
          let chat: InvestigationChat | null = null;
          let chatStatus: InvestigationChatStatus = 'error';
          let chatError: string | null = null;
          if (chatRead.kind === 'error') {
            chatError = 'The saved transcript could not be read. Refresh the selected investigation to try again.';
          } else if (chatRead.value === undefined) {
            // invokeOr uses undefined only for the non-Tauri browser preview; host null is legacy absence.
            chatStatus = 'preview';
          } else if (chatRead.value === null) {
            chatStatus = 'legacy';
          } else if (validInvestigationChat(chatRead.value, id, detail, result)) {
            chat = chatRead.value;
            chatStatus = 'saved';
          } else {
            chatError = 'The saved transcript did not match this task and result. Refresh to reconnect.';
          }
          set({ chat, chatStatus, chatError });
        }).catch(() => {
          // Transcript failures stay local to the transcript; confirmed task and
          // findings state remains available even if detached validation rejects.
          if (current()) set({ chat: null, chatStatus: 'error',
            chatError: 'The saved transcript could not be read. Refresh the selected investigation to try again.' });
        });
      } catch {
        if (current()) set({ loading: options?.awaitChat === true, chat: null, chatStatus: 'error',
          chatError: 'The transcript is unavailable until this investigation refreshes consistently.', consent: null, consentOpen: false,
          reviewError: 'Saved investigation binding could not be refreshed. Refresh before reviewing; historical findings remain visible.',
          error: 'The investigation could not be refreshed consistently. Saved findings and activity remain visible; refresh to reconnect.' });
      } finally {
        // Docking owns one physical chat read through settlement, even if detail
        // failed or selection changed. Ordinary findings remain nonblocking.
        if (options?.awaitChat) {
          await transcriptSettled;
          if (current()) set({ loading: false });
        }
        options?.loadingSignal?.removeEventListener('abort', releaseLoading);
        const pending = detailRefreshPending;
        if (pending && request === detailRequest && !get().loading &&
            get().selectedId === pending.id && get().selectionGeneration === pending.generation) {
          detailRefreshPending = null;
          void get().refreshSelected();
        }
      }
    },

    invalidate: (message) => {
      const state = get();
      if (message.investigation_id === state.selectedId &&
          (message.revision > (state.detail?.revision ?? -1) ||
            message.last_event_sequence > (state.events.at(-1)?.sequence ?? 0))) {
        // A notification during a load (for example the terminal transition of
        // a failed run) must not be dropped, or the pane keeps the older state.
        if (state.loading) detailRefreshPending = { id: message.investigation_id, generation: state.selectionGeneration };
        else void state.refreshSelected();
      }
      if (state.historyActiveOnly) {
        // Membership depends on mutable status. Restart the current selection
        // even when this task was not in the loaded page or a request is pending.
        void state.loadHistory();
      } else if (message.investigation_id !== state.selectedId && !state.historyLoading &&
          message.revision > (state.history.find((item) => item.investigation_id === message.investigation_id)?.revision ?? -1)) {
        void state.loadHistory();
      }
      if (state.conversationId && (message.investigation_id === state.selectedId ||
          state.conversationItems.some((item) => item.investigation_id === message.investigation_id))) {
        void refreshConversation(state.conversationId, message.investigation_id);
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

    readCitation: async (citationId, options) => {
      if (options?.signal?.aborted) return;
      const { selectedId: id, selectionGeneration, detail } = get();
      const citation = detail?.citations.find((item) => item.citation_id === citationId);
      if (!id || !citation) return;
      citationOwnerCleanup?.(); citationOwnerCleanup = undefined;
      const request = ++citationRequest;
      const current = () => get().selectedId === id && get().selectionGeneration === selectionGeneration && request === citationRequest;
      set({ citationId, citationRead: null, citationLoading: true, citationError: null });
      if (!current()) return;
      if (options?.signal) {
        const signal = options.signal;
        // Release this UI reader only; a newer shared reader owns its own request.
        const release = () => { if (current()) get().clearCitation(); };
        signal.addEventListener('abort', release, { once: true });
        citationOwnerCleanup = () => signal.removeEventListener('abort', release);
        if (signal.aborted) { release(); return; }
      }
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
      citationOwnerCleanup?.(); citationOwnerCleanup = undefined;
      ++citationRequest;
      set({ citationId: null, citationRead: null, citationLoading: false, citationError: null });
    },
  };
});
