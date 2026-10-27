import { create } from 'zustand';
import { investigationIsInActiveHistory, validSummary, INVESTIGATION_PAGE_LIMIT } from './investigationStore';
import { inTauri, invokeOr } from './tauri';
import type { SpecialistActivityItem, SpecialistActivityPage } from './investigationTypes';

export type SpecialistActivityStatus = 'idle' | 'loading' | 'ready' | 'incomplete' | 'preview' | 'error';

export interface SpecialistActivityState {
  status: SpecialistActivityStatus;
  tasks: SpecialistActivityItem[];
  error: string | null;
  refreshing: boolean;
  setVisible: (visible: boolean) => void;
  refresh: () => Promise<void>;
  invalidate: () => void;
}

function validReportedCount(count: unknown): boolean {
  return count === null || (typeof count === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(count) &&
    (count.length < 20 || count <= '18446744073709551615'));
}

function validActivityItem(value: SpecialistActivityItem | null): value is SpecialistActivityItem {
  if (!value || !validSummary(value.summary) || !investigationIsInActiveHistory(value.summary) ||
      typeof value.summary.cancel_requested !== 'boolean') return false;
  if ('reported_usage' in value) {
    const usage = value.reported_usage;
    if (!usage || typeof usage !== 'object' || Array.isArray(usage) ||
        Object.keys(usage).sort().join(',') !== 'input_tokens,output_tokens' ||
        ![usage.input_tokens, usage.output_tokens].every(validReportedCount)) return false;
  }
  if ('reported_cache_read_tokens' in value && !validReportedCount(value.reported_cache_read_tokens)) return false;
  const provider = value.saved_provider;
  if (!provider || typeof provider !== 'object' ||
      Object.keys(provider).sort().join(',') !== 'mode,model,provider_id' ||
      (provider.mode !== 'local' && provider.mode !== 'cloud') || provider.mode !== value.summary.provider_mode) return false;
  return [provider.provider_id, provider.model].every((item) =>
    typeof item === 'string' && item.length > 0 && !item.includes('\0') && new TextEncoder().encode(item).length <= 2048);
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
        set({ refreshing: true });
        return inFlight;
      }

      const run = async () => {
        while (visible) {
          trailing = false;
          const requestGeneration = generation;
          set((state) => ({
            status: state.status === 'idle' ? 'loading' : state.status,
            error: null,
            refreshing: true,
          }));
          let observation: Pick<SpecialistActivityState, 'status' | 'tasks' | 'error'>;
          try {
            if (!inTauri()) {
              observation = { status: 'preview', tasks: [], error: null };
            } else {
              const page = await invokeOr<SpecialistActivityPage | null>('list_specialist_task_activity', null, { cursor: null });
              if (!page || !Array.isArray(page.items) || page.items.length > INVESTIGATION_PAGE_LIMIT ||
                  (page.next_cursor !== null && typeof page.next_cursor !== 'string') ||
                  page.items.some((item) => !validActivityItem(item))) {
                throw new Error('Invalid active investigation page.');
              }
              observation = { status: page.next_cursor === null ? 'ready' : 'incomplete', tasks: page.items, error: null };
            }
          } catch {
            observation = { status: 'error', tasks: store.getState().tasks,
              error: 'Task activity could not be refreshed; current activity is unknown.' };
          }

          if (!visible) return;
          if (requestGeneration !== generation) {
            trailing = true;
            continue;
          }
          set({ ...observation, refreshing: trailing });
          if (trailing) continue;
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
      status: 'idle', tasks: [], error: null, refreshing: false,
      setVisible: (next) => {
        if (visible === next) return;
        visible = next;
        generation += 1;
        if (next) {
          set({ status: 'loading', tasks: [], error: null, refreshing: true });
          void refresh();
        }
        else {
          trailing = false;
          set({ status: 'idle', tasks: [], error: null, refreshing: false });
        }
      },
      refresh,
      invalidate: () => { if (visible) void refresh(); },
    };
  });
  return store;
}

export const useSpecialistActivityStore = createSpecialistActivityStore();
