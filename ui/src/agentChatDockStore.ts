import { create } from 'zustand';
import { useInvestigationStore, validReportedTaskUsage, type InvestigationState } from './investigationStore';
import type { AdvisoryAudit, InvestigationChat, InvestigationSummary, InvestigationResult, InvestigationCitation, InvestigationCitationRead, InvestigationScope, ReportedTaskUsage } from './investigationTypes';

export type ChatTurn = Pick<InvestigationSummary, 'investigation_id' | 'conversation_id' | 'parent_id' |
  'question' | 'specialist_id' | 'status' | 'updated_at' | 'revision'>;
export interface ChatDock {
  chat: InvestigationChat; question: string; specialistName: string; turn: ChatTurn; scope: InvestigationScope;
  reportedUsage?: ReportedTaskUsage;
  answer: { advisoryAudit?: AdvisoryAudit; result: InvestigationResult; citations: InvestigationCitation[] } | null;
}
interface CitationInspection {
  taskId: string; citationId: string; read: InvestigationCitationRead | null; loading: boolean; error: string | null;
}
interface DockState {
  docks: ChatDock[];
  focusedTask: string | null;
  focusedGeneration: number | null;
  citation: CitationInspection | null;
  readCitation: (taskId: string, citationId: string) => Promise<void>;
  clearCitation: () => void;
  pendingTask: string | null;
  error: string | null;
  fallbackTask: string | null;
  connect: () => () => void;
  open: (taskId: string) => void;
  openForFollowUp: (taskId: string) => Promise<InvestigationState | null>;
  requestOpenFromNotice: (taskId: string) => void;
  cancelNavigationIntent: () => void;
  close: (taskId: string) => void;
}

/** Normalize the closed host scope shapes without changing exact neighborhood identity. */
function normalizedScope(scope: InvestigationScope): InvestigationScope | null {
  if (!scope || typeof scope !== 'object') return null;
  if (scope.type === 'all') return { type: 'all' };
  // Matches the existing UI validation and context_hub::MAX_NEIGHBORHOOD_HOPS.
  if (scope.type === 'neighborhood' && typeof scope.anchor === 'string' && scope.anchor.trim().length > 0 &&
      Number.isSafeInteger(scope.hops) && scope.hops >= 1 && scope.hops <= 3) {
    return { type: 'neighborhood', anchor: scope.anchor, hops: scope.hops };
  }
  if (scope.type === 'area' && typeof scope.id === 'string' && scope.id.trim().length > 0 && scope.id.length <= 1024) {
    return { type: 'area', id: scope.id };
  }
  return null;
}

function sameScope(left: InvestigationScope, right: InvestigationScope): boolean {
  if (left.type === 'all' || right.type === 'all') return left.type === right.type;
  if (left.type === 'area' || right.type === 'area') return left.type === 'area' && right.type === 'area' && left.id === right.id;
  return left.anchor === right.anchor && left.hops === right.hops;
}

/** Capture only a settled coordinator observation; cached immutable identity survives reopen. */
function captureDock(state: InvestigationState, taskId: string, previous?: ChatDock): ChatDock | null {
  const { detail, chat, result } = state;
  if (state.loading || state.error || state.chatError || state.chatStatus !== 'saved' ||
      state.selectedId !== taskId || detail?.investigation_id !== taskId || chat?.task_id !== taskId) return null;
  if ('reported_usage_exact' in detail && !validReportedTaskUsage(detail.reported_usage_exact)) return null;
  const exactUsage = detail.reported_usage_exact;
  const reportedUsage = exactUsage ? { input_tokens: exactUsage.input_tokens,
    output_tokens: exactUsage.output_tokens, cache_read_tokens: exactUsage.cache_read_tokens } : undefined;
  const scope = normalizedScope(detail.scope);
  if (!scope) return null;
  const reference = chat.messages.find((entry) => entry.message.kind === 'result_reference')?.message;
  if (result ? result.investigation_id !== taskId || reference?.kind !== 'result_reference' ||
      reference.task_id !== taskId || reference.result_id !== result.result_id : reference !== undefined) return null;
  const { investigation_id, conversation_id, parent_id, question, specialist_id, status, updated_at, revision } = detail;
  const referenced = new Set(result?.findings.flatMap((finding) => finding.citation_ids));
  if (detail.advisory_audit?.status === 'completed') {
    for (const { assessment } of detail.advisory_audit.reviews) {
      if ('citation_id' in assessment) referenced.add(assessment.citation_id);
    }
  }
  const answer = result ? { result, ...(detail.advisory_audit ? { advisoryAudit: detail.advisory_audit } : {}), citations: detail.citations.filter((citation) => referenced.has(citation.citation_id)) } : null;
  if (previous && ((revision === previous.turn.revision &&
      JSON.stringify(reportedUsage) !== JSON.stringify(previous.reportedUsage)) ||
      revision < previous.turn.revision || chat.chat_id !== previous.chat.chat_id ||
      question !== previous.question || detail.specialist.name !== previous.specialistName || conversation_id !== previous.turn.conversation_id || parent_id !== previous.turn.parent_id ||
      specialist_id !== previous.turn.specialist_id || !sameScope(scope, previous.scope) ||
      JSON.stringify(chat.producer) !== JSON.stringify(previous.chat.producer) ||
      (previous.answer && (JSON.stringify(answer) !== JSON.stringify(previous.answer) ||
        JSON.stringify(chat) !== JSON.stringify(previous.chat))))) return null;
  return { chat, question, specialistName: detail.specialist.name, answer, scope,
    ...(reportedUsage ? { reportedUsage } : {}),
    turn: { investigation_id, conversation_id, parent_id, question, specialist_id, status, updated_at, revision } };
}

/** Session snapshots only. The existing coordinator owns all validation and I/O. */
export function createAgentChatDockStore(coordinator = useInvestigationStore) {
  let connected = false;
  let citationRequest = 0;
  let citationUnsubscribe: (() => void) | undefined;
  let citationOwner: AbortController | undefined;
  let adoptedInitialSelection = false;
  let intent = 0;
  let unsubscribe: (() => void) | undefined;
  let liveUnsubscribe: (() => void) | undefined;
  let reading = false;
  let requestedGeneration = 0;
  let navigationIntent: string | null = null;
  let loadingOwner: AbortController | undefined;
  type Consumer = 'followUp' | 'citation';
  type Waiter = { intent: number; taskId: string; resolve: (state: InvestigationState | null) => void };
  const waiters: Partial<Record<Consumer, Waiter>> = {};
  const finishWaiters = (state: InvestigationState | null, request?: number, taskId?: string) => {
    for (const consumer of ['followUp', 'citation'] as const) {
      const waiter = waiters[consumer];
      if (waiter && (request === undefined || waiter.intent === request) && (!taskId || waiter.taskId === taskId)) {
        delete waiters[consumer]; waiter.resolve(state);
      }
    }
  };
  const stop = (releaseLoading = false) => {
    ++intent; unsubscribe?.(); unsubscribe = undefined;
    finishWaiters(null);
    // Release UI ownership only; the physical read still occupies its slot.
    if (releaseLoading) loadingOwner?.abort();
  };
  return create<DockState>((set, get) => {
    const run = () => {
      const taskId = get().pendingTask;
      if (reading || !connected || !taskId) return;
      if (coordinator.getState().selectionGeneration !== requestedGeneration) {
        stop(); set({ pendingTask: null, focusedGeneration: null }); return;
      }
      reading = true;
      const request = intent;
      const generation = coordinator.getState().selectionGeneration + 1;
      const owner = new AbortController();
      loadingOwner = owner;
      const observe = () => {
        if (!connected || intent !== request) return;
        const state = coordinator.getState();
        if (state.selectionGeneration !== generation || state.selectedId !== taskId) {
          if (state.selectionGeneration >= generation) { stop(); set({ pendingTask: null, focusedGeneration: null }); }
          return;
        }
        if (state.loading || state.chatStatus === 'loading' || state.chatStatus === 'idle') return;
        const dock = captureDock(state, taskId, get().docks.find((item) => item.chat.task_id === taskId));
        if (dock) {
          set(({ docks }) => {
            const existing = docks.findIndex((item) => item.chat.chat_id === dock.chat.chat_id);
            return { docks: existing < 0 ? [...docks.slice(-3), dock]
              : docks.map((item, index) => index === existing ? dock : item), pendingTask: null, focusedGeneration: generation };
          });
        } else set({ pendingTask: null, focusedGeneration: null, fallbackTask: state.chatStatus === 'legacy' ? taskId : null,
          error: state.chatError ?? state.error ?? (state.chatStatus === 'legacy'
            ? 'No saved conversation is available for this older investigation.' : 'Saved chat observation did not match the retained task. Try opening it again.') });
        finishWaiters(dock ? state : null, request, taskId);
        unsubscribe?.(); unsubscribe = undefined;
      };
      unsubscribe = coordinator.subscribe(observe);
      void coordinator.getState().open(taskId, { awaitChat: true, loadingSignal: owner.signal }).then(observe).catch(() => {
        if (connected && intent === request) {
          unsubscribe?.(); unsubscribe = undefined;
          set({ pendingTask: null, error: 'Saved chat unavailable. Try opening it again.' });
          finishWaiters(null, request);
        }
      }).finally(() => {
        reading = false;
        if (loadingOwner === owner) loadingOwner = undefined;
        // A single replaceable intent, rather than a promise chain per click.
        if (intent !== request) run();
      });
    };
    const beginOpen = (taskId: string, resolve?: (state: InvestigationState | null) => void, consumer: Consumer = 'followUp') => {
      get().clearCitation();
      stop();
      requestedGeneration = coordinator.getState().selectionGeneration;
      if (resolve) waiters[consumer] = { intent, taskId, resolve };
      set({ focusedTask: taskId, focusedGeneration: null, pendingTask: taskId, error: null, fallbackTask: null });
      run();
    };
    const awaitOpen = (taskId: string, consumer: Consumer) => new Promise<InvestigationState | null>((resolve) => {
      if (!connected) { resolve(null); return; }
      if (get().pendingTask === taskId) {
        waiters[consumer]?.resolve(null);
        waiters[consumer] = { intent, taskId, resolve };
        run(); return;
      }
      beginOpen(taskId, resolve, consumer);
    });
    return {
      docks: [], focusedTask: null, focusedGeneration: null, citation: null, pendingTask: null, error: null, fallbackTask: null,
      clearCitation: () => {
        ++citationRequest; citationUnsubscribe?.(); citationUnsubscribe = undefined;
        const owner = citationOwner; citationOwner = undefined;
        owner?.abort();
        set({ citation: null });
      },
      readCitation: async (taskId, citationId) => {
        const answer = get().docks.find((dock) => dock.chat.task_id === taskId)?.answer;
        const citation = answer?.citations.find((item) => item.citation_id === citationId);
        if (!connected || !answer || !citation) return;
        const opening = awaitOpen(taskId, 'citation');
        const request = ++citationRequest;
        const opened = await opening;
        if (request !== citationRequest || !opened || opened.chatStatus !== 'saved' || opened.chat?.task_id !== taskId ||
            opened.result?.investigation_id !== taskId || opened.selectedId !== taskId || opened.result?.result_id !== answer.result.result_id ||
            JSON.stringify(opened.detail?.citations.find((item) => item.citation_id === citationId)) !== JSON.stringify(citation)) return;
        const generation = opened.selectionGeneration;
        const current = () => connected && request === citationRequest && coordinator.getState().selectedId === taskId &&
          coordinator.getState().selectionGeneration === generation &&
          get().docks.some((dock) => dock.chat.task_id === taskId && dock.answer?.result.result_id === answer.result.result_id);
        if (!current()) return;
        set({ citation: { taskId, citationId, read: null, loading: true, error: null } });
        citationUnsubscribe = coordinator.subscribe(() => { if (!current()) get().clearCitation(); });
        const owner = new AbortController();
        citationOwner = owner;
        await coordinator.getState().readCitation(citationId, { signal: owner.signal });
        if (!current()) return;
        citationUnsubscribe?.(); citationUnsubscribe = undefined;
        const state = coordinator.getState();
        const read = state.citationRead?.investigation_id === taskId && state.citationRead.citation_id === citationId
          ? state.citationRead : null;
        set({ citation: { taskId, citationId, loading: false,
          read,
          error: state.citationError ?? (read ? null : 'Historical evidence could not be inspected.') } });
      },
      connect: () => {
        connected = true;
        liveUnsubscribe?.();
        liveUnsubscribe = coordinator.subscribe((state) => {
          const { focusedTask, focusedGeneration, pendingTask, docks } = get();
          if (!connected || pendingTask || !focusedTask || focusedGeneration !== state.selectionGeneration) return;
          const previous = docks.find((item) => item.chat.task_id === focusedTask);
          if (!previous || (state.chat === previous.chat && state.detail?.revision === previous.turn.revision &&
              state.result === (previous.answer?.result ?? null))) return;
          const updated = captureDock(state, focusedTask, previous);
          if (updated) set({ docks: docks.map((item) => item === previous ? updated : item) });
        });
        const requested = navigationIntent;
        navigationIntent = null;
        const selected = requested ?? get().focusedTask ?? (!adoptedInitialSelection ? coordinator.getState().selectedId : null);
        adoptedInitialSelection = true;
        if (selected) get().open(selected);
        return () => { connected = false; liveUnsubscribe?.(); liveUnsubscribe = undefined;
          get().clearCitation(); navigationIntent = null; stop(true); set({ pendingTask: null, focusedGeneration: null }); };
      },
      open: (taskId) => {
        if (!connected || get().pendingTask === taskId) return;
        beginOpen(taskId);
      },
      openForFollowUp: (taskId) => awaitOpen(taskId, 'followUp'),
      requestOpenFromNotice: (taskId) => {
        if (connected) get().open(taskId);
        else navigationIntent = taskId;
      },
      cancelNavigationIntent: () => { navigationIntent = null; },
      close: (taskId) => {
        if (get().citation?.taskId === taskId) get().clearCitation();
        if (get().pendingTask === taskId || get().focusedTask === taskId) stop(true);
        set((state) => ({ docks: state.docks.filter((dock) => dock.chat.task_id !== taskId),
          pendingTask: state.pendingTask === taskId ? null : state.pendingTask,
          focusedTask: state.focusedTask === taskId ? null : state.focusedTask,
          focusedGeneration: state.focusedTask === taskId ? null : state.focusedGeneration,
          fallbackTask: state.fallbackTask === taskId ? null : state.fallbackTask }));
      },
    };
  });
}

export const useAgentChatDockStore = createAgentChatDockStore();
