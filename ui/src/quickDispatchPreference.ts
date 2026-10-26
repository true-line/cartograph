const KEY = 'cartograph.quick-dispatch.cmd-j';

export function readQuickDispatchFallback(): boolean {
  try { return localStorage.getItem(KEY) !== 'false'; } catch { return true; }
}

export function writeQuickDispatchFallback(enabled: boolean): void {
  try { localStorage.setItem(KEY, String(enabled)); } catch { /* Preference is optional. */ }
}
