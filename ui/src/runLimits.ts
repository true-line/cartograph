import { invoke } from '@tauri-apps/api/core';

/** Investigation time limits from Configure → Run Limits (#1633). */
export interface RunLimits {
  active_seconds: number;
  request_timeout_ms: number;
  default_active_seconds: number;
  default_request_timeout_ms: number;
  max_active_seconds: number;
  max_request_timeout_ms: number;
  customized: boolean;
}

export interface RunLimitsApi {
  load(): Promise<RunLimits>;
  save(activeSeconds: number, requestTimeoutMs: number): Promise<RunLimits>;
  reset(): Promise<RunLimits>;
}

export const tauriRunLimitsApi: RunLimitsApi = {
  load: () => invoke<RunLimits>('get_run_limits'),
  save: (activeSeconds, requestTimeoutMs) =>
    invoke<RunLimits>('set_run_limits', { activeSeconds, requestTimeoutMs }),
  reset: () => invoke<RunLimits>('reset_run_limits'),
};

/** The local completion model for new desktop investigations (#1643). */
export interface LocalModelChoice {
  model: string;
  default_model: string;
  customized: boolean;
}

/** One model installed in Ollama on this Mac. */
export interface InstalledModel {
  name: string;
  digest: string;
}

export interface LocalModelApi {
  load(): Promise<LocalModelChoice>;
  installed(): Promise<InstalledModel[]>;
  save(model: string): Promise<LocalModelChoice>;
  reset(): Promise<LocalModelChoice>;
}

export const tauriLocalModelApi: LocalModelApi = {
  load: () => invoke<LocalModelChoice>('get_local_model'),
  installed: () => invoke<InstalledModel[]>('list_local_models'),
  save: (model) => invoke<LocalModelChoice>('set_local_model', { model }),
  reset: () => invoke<LocalModelChoice>('reset_local_model'),
};

/** Whole minutes in the window; seconds/milliseconds on the wire. */
export const toMinutes = (seconds: number) => seconds / 60;

/** A typed minute value, or an error message when it is outside [min, max] minutes. */
export function parseMinutes(text: string, minMinutes: number, maxMinutes: number): number | string {
  const value = Number(text.trim());
  if (text.trim() === '' || !Number.isFinite(value)) return 'Enter a number of minutes.';
  if (value < minMinutes || value > maxMinutes) return `Must be between ${formatMinutes(minMinutes)} and ${formatMinutes(maxMinutes)} minutes.`;
  return value;
}

export function formatMinutes(minutes: number): string {
  return Number.isInteger(minutes) ? String(minutes) : minutes.toFixed(1);
}
