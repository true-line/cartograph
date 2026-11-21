const KEY = 'cartograph.quick-dispatch.cmd-j';

export function readQuickDispatchFallback(): boolean {
  try { return localStorage.getItem(KEY) !== 'false'; } catch { return true; }
}

export function writeQuickDispatchFallback(enabled: boolean): void {
  try { localStorage.setItem(KEY, String(enabled)); } catch { /* Preference is optional. */ }
}

const HINT_KEY = 'cartograph.quick-dispatch.hint';

/** Whether the "Dispatch to an agent" hint shows (#251). It shows until the
 *  first confirmed quick dispatch or until it is turned off in Settings. */
export function readQuickDispatchHint(): boolean {
  try { return localStorage.getItem(HINT_KEY) !== 'false'; } catch { return true; }
}

export function writeQuickDispatchHint(shown: boolean): void {
  try { localStorage.setItem(HINT_KEY, String(shown)); } catch { /* Preference is optional. */ }
}
