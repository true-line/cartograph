import { create } from 'zustand';
import { inTauri, invokeOr } from './tauri';
import type { InvestigationChanged, InvestigationChat } from './investigationTypes';

export const SAVED_RESULT_NOTICE_LIMIT = 128;
export const SAVED_RESULT_TOAST_LIMIT = 3;
export const SAVED_RESULT_TOAST_MS = 5_500;

export interface SavedResultNotice {
  task_id: string;
  result_id: string;
  question: string;
}

export interface SavedResultNoticeState {
  unread: SavedResultNotice[];
  toasts: string[];
  incomplete: boolean;
  observe: (event: InvestigationChanged) => void;
  dismissToast: (key: string) => void;
  markVisible: (taskId: string, resultId: string) => void;
}

const identity = (value: unknown): value is string => typeof value === 'string' && value.length > 0 &&
  value.length <= 256 && !value.includes('\0');
const noticeKey = (notice: SavedResultNotice) => `${notice.task_id}\0${notice.result_id}`;

function savedResultReference(value: InvestigationChat | null | undefined, taskId: string): SavedResultNotice | null | undefined {
  if (value === null || value === undefined) return null;
  if (value.task_id !== taskId || !identity(value.chat_id) || !Array.isArray(value.messages) ||
      (value.messages.length !== 1 && value.messages.length !== 2)) return undefined;
  const [question, reference] = value.messages;
  if (!question || !identity(question.id) || question.ordinal !== 0 || question.author !== 'user' ||
      question.message?.kind !== 'user_question' || typeof question.message.question !== 'string' ||
      question.message.question.length === 0 || new TextEncoder().encode(question.message.question).length > 16 * 1024 ||
      (reference !== undefined && (!identity(reference.id) || reference.ordinal !== 1 || reference.author !== 'specialist' ||
      reference.message?.kind !== 'result_reference' ||
      reference.message.task_id !== taskId || !identity(reference.message.result_id) ||
      !/^[0-9a-f]{64}$/.test(reference.message.result_hash)))) return undefined;
  if (!reference) return null;
  if (reference.message.kind !== 'result_reference') return undefined;
  return { task_id: taskId, result_id: reference.message.result_id, question: question.message.question };
}

export function createSavedResultNoticeStore() {
  let inFlight = false;
  let inFlightTask: string | null = null;
  let inFlightRevision = -1;
  const trackedTasks = new Set<string>();
  const queued = new Map<string, InvestigationChanged>();
  const observedRevision = new Map<string, number>();
  const knownResults = new Set<string>();
  const store = create<SavedResultNoticeState>((set, get) => {
    const pump = () => {
      if (inFlight || queued.size === 0) return;
      const [taskId, event] = queued.entries().next().value as [string, InvestigationChanged];
      queued.delete(taskId);
      inFlight = true;
      inFlightTask = taskId;
      inFlightRevision = event.revision;
      void (async () => {
        try {
          if (!inTauri()) return;
          const chat = await invokeOr<InvestigationChat | null>('get_investigation_chat', null, { investigationId: taskId });
          const notice = savedResultReference(chat, taskId);
          if (notice === undefined) {
            set({ incomplete: true });
            return;
          }
          observedRevision.set(taskId, Math.max(observedRevision.get(taskId) ?? -1, event.revision));
          if (!notice) return;
          const key = noticeKey(notice);
          if (knownResults.has(key)) return;
          if (knownResults.size >= SAVED_RESULT_NOTICE_LIMIT || get().unread.length >= SAVED_RESULT_NOTICE_LIMIT) {
            set({ incomplete: true });
            return;
          }
          knownResults.add(key);
          set((state) => ({ unread: [...state.unread, notice],
            toasts: [...state.toasts, key].slice(-SAVED_RESULT_TOAST_LIMIT) }));
        } catch {
          set({ incomplete: true });
        } finally {
          inFlight = false;
          inFlightTask = null;
          inFlightRevision = -1;
          pump();
        }
      })();
    };

    return {
      unread: [], toasts: [], incomplete: false,
      observe: (event) => {
        if (!identity(event?.investigation_id) || !Number.isSafeInteger(event.revision) || event.revision < 0 ||
            !Number.isSafeInteger(event.last_event_sequence) || event.last_event_sequence < 0) return;
        const taskId = event.investigation_id;
        if (!trackedTasks.has(taskId)) {
          if (trackedTasks.size >= SAVED_RESULT_NOTICE_LIMIT) {
            set({ incomplete: true });
            return;
          }
          trackedTasks.add(taskId);
        }
        if (event.revision <= (observedRevision.get(taskId) ?? -1)) return;
        const previous = queued.get(taskId);
        if ((previous && previous.revision >= event.revision) ||
            (taskId === inFlightTask && inFlightRevision >= event.revision)) return;
        if (!previous && queued.size >= SAVED_RESULT_NOTICE_LIMIT) {
          set({ incomplete: true });
          return;
        }
        queued.set(taskId, event);
        pump();
      },
      dismissToast: (key) => set((state) => ({ toasts: state.toasts.filter((item) => item !== key) })),
      markVisible: (taskId, resultId) => {
        if (!identity(taskId) || !identity(resultId)) return;
        const key = noticeKey({ task_id: taskId, result_id: resultId, question: '' });
        set((state) => ({ unread: state.unread.filter((item) => noticeKey(item) !== key),
          toasts: state.toasts.filter((item) => item !== key) }));
        if (knownResults.has(key)) return;
        if (knownResults.size >= SAVED_RESULT_NOTICE_LIMIT ||
            (!trackedTasks.has(taskId) && trackedTasks.size >= SAVED_RESULT_NOTICE_LIMIT)) {
          set({ incomplete: true });
          return;
        }
        knownResults.add(key);
        trackedTasks.add(taskId);
      },
    };
  });
  return store;
}

export const useSavedResultNoticeStore = createSavedResultNoticeStore();
export { noticeKey as savedResultNoticeKey };
