import { create } from 'zustand';
import { investigationIsInActiveHistory, validSummary, INVESTIGATION_PAGE_LIMIT } from './investigationStore';
import { inTauri, invokeOr } from './tauri';
import type { InvestigationPage, InvestigationSummary } from './investigationTypes';

export type SpecialistActivityStatus = 'idle' | 'loading' | 'ready' | 'incomplete' | 'preview' | 'error';

export interface SpecialistActivityState {
  status: SpecialistActivityStatus;
  tasks: InvestigationSummary[];
  error: string | null;
  setVisible: (visible: boolean) => void;
  refresh: () => Promise<void>;
  invalidate: () => void;
}

export function createSpecialistActivityStore() {
  let visible = false;
  let generation = 0;
  let trailing = false;
  let inFlight: Promise<void> | null = null;

  const store = create<SpecialistActivityState>((set) => {
    const refresh = (): Promise<void> => {
      if (!visible) return Promise.resolve();
      if (inFlight) {
        trailing = true;
        set({ status: 'loading', tasks: [], error: null });
        return inFlight;
      }

      const run = async () => {
        while (visible) {
          trailing = false;
          const requestGeneration = generation;
          set({ status: 'loading', tasks: [], error: null });
          let observation: Pick<SpecialistActivityState, 'status' | 'tasks' | 'error'>;
          try {
            if (!inTauri()) {
              observation = { status: 'preview', tasks: [], error: null };
            } else {
              const page = await invokeOr<InvestigationPage | null>('list_investigations', null, { activeOnly: true });
              if (!page || !Array.isArray(page.items) || page.items.length > INVESTIGATION_PAGE_LIMIT ||
                  (page.next_cursor !== null && typeof page.next_cursor !== 'string') ||
                  page.items.some((item) => !validSummary(item) ||
                    !investigationIsInActiveHistory(item) || typeof item.cancel_requested !== 'boolean')) {
                throw new Error('Invalid active investigation page.');
              }
              observation = { status: page.next_cursor === null ? 'ready' : 'incomplete', tasks: page.items, error: null };
            }
          } catch {
            observation = { status: 'error', tasks: [], error: 'Task activity could not be refreshed; current activity is unknown.' };
          }

          if (!visible) return;
          if (requestGeneration !== generation || trailing) {
            trailing = true;
            continue;
          }
          set(observation);
          return;
        }
      };

      const request = run().finally(() => {
        inFlight = null;
        if (visible && trailing) void refresh();
      });
      inFlight = request;
      return request;
    };

    return {
      status: 'idle', tasks: [], error: null,
      setVisible: (next) => {
        if (visible === next) return;
        visible = next;
        generation += 1;
        if (next) void refresh();
        else {
          trailing = false;
          set({ status: 'idle', tasks: [], error: null });
        }
      },
      refresh,
      invalidate: () => { if (visible) void refresh(); },
    };
  });
  return store;
}

export const useSpecialistActivityStore = createSpecialistActivityStore();
