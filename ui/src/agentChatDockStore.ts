import { create } from 'zustand';
import { useInvestigationStore } from './investigationStore';
import type { InvestigationChat } from './investigationTypes';

interface ChatDock { chat: InvestigationChat; question: string }
interface DockState {
  docks: ChatDock[];
  focusedTask: string | null;
  pendingTask: string | null;
  error: string | null;
  fallbackTask: string | null;
  connect: () => () => void;
  open: (taskId: string) => void;
  close: (taskId: string) => void;
}

/** Session snapshots only. The existing coordinator owns all validation and I/O. */
export function createAgentChatDockStore(coordinator = useInvestigationStore) {
  let connected = false;
  let intent = 0;
  let unsubscribe: (() => void) | undefined;
  let reading = false;
  let requestedGeneration = 0;
  const stop = () => { ++intent; unsubscribe?.(); unsubscribe = undefined; };
  return create<DockState>((set, get) => {
    const run = () => {
      const taskId = get().pendingTask;
      if (reading || !connected || !taskId) return;
      if (coordinator.getState().selectionGeneration !== requestedGeneration) {
        stop(); set({ pendingTask: null }); return;
      }
      reading = true;
      const request = intent;
      const generation = coordinator.getState().selectionGeneration + 1;
      const observe = () => {
        if (!connected || intent !== request) return;
        const state = coordinator.getState();
        if (state.selectionGeneration !== generation || state.selectedId !== taskId) {
          if (state.selectionGeneration >= generation) { stop(); set({ pendingTask: null }); }
          return;
        }
        if (state.chatStatus === 'loading' || state.chatStatus === 'idle') return;
        if (state.chatStatus === 'saved' && state.chat?.task_id === taskId &&
            state.detail?.investigation_id === taskId) {
          const dock = { chat: state.chat, question: state.detail.question };
          set(({ docks }) => {
            const existing = docks.findIndex((item) => item.chat.chat_id === dock.chat.chat_id);
            return { docks: existing < 0 ? [...docks.slice(-3), dock]
              : docks.map((item, index) => index === existing ? dock : item), pendingTask: null };
          });
        } else set({ pendingTask: null, fallbackTask: state.chatStatus === 'legacy' ? taskId : null,
          error: state.chatError ?? state.error ?? (state.chatStatus === 'legacy'
            ? 'No saved conversation is available for this older investigation.' : 'Saved chat unavailable. Try opening it again.') });
        unsubscribe?.(); unsubscribe = undefined;
      };
      unsubscribe = coordinator.subscribe(observe);
      void coordinator.getState().open(taskId).then(observe).catch(() => {
        if (connected && intent === request) {
          unsubscribe?.(); unsubscribe = undefined;
          set({ pendingTask: null, error: 'Saved chat unavailable. Try opening it again.' });
        }
      }).finally(() => {
        reading = false;
        // A single replaceable intent, rather than a promise chain per click.
        if (intent !== request) run();
      });
    };
    return {
      docks: [], focusedTask: null, pendingTask: null, error: null, fallbackTask: null,
      connect: () => {
        connected = true;
        const selected = get().focusedTask ?? coordinator.getState().selectedId;
        if (selected) get().open(selected);
        return () => { connected = false; stop(); set({ pendingTask: null }); };
      },
      open: (taskId) => {
        if (!connected || get().pendingTask === taskId) return;
        stop();
        requestedGeneration = coordinator.getState().selectionGeneration;
        set({ focusedTask: taskId, pendingTask: taskId, error: null, fallbackTask: null });
        run();
      },
      close: (taskId) => {
        if (get().pendingTask === taskId || get().focusedTask === taskId) stop();
        set((state) => ({ docks: state.docks.filter((dock) => dock.chat.task_id !== taskId),
          pendingTask: state.pendingTask === taskId ? null : state.pendingTask,
          focusedTask: state.focusedTask === taskId ? null : state.focusedTask,
          fallbackTask: state.fallbackTask === taskId ? null : state.fallbackTask }));
      },
    };
  });
}

export const useAgentChatDockStore = createAgentChatDockStore();
