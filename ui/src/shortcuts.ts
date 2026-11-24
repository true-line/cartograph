/** Platform-aware keyboard shortcuts. macOS uses Cmd; Windows and Linux use Ctrl. */
export const IS_MAC = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);

/** Label for a primary-modifier shortcut, e.g. `Cmd+K` on macOS and `Ctrl+K` elsewhere. */
export function modShortcut(key: string, mac = IS_MAC): string {
  return `${mac ? 'Cmd' : 'Ctrl'}+${key}`;
}

/** The quick dispatch shortcut labels. macOS: Cmd+Space, plus Cmd+J when the
 *  fallback is on. Windows and Linux: Ctrl+J only, because Ctrl+Space switches
 *  input methods there. */
export function quickDispatchShortcuts(fallbackEnabled: boolean, mac = IS_MAC): string[] {
  if (!mac) return ['Ctrl+J'];
  return fallbackEnabled ? ['Cmd+Space', 'Cmd+J'] : ['Cmd+Space'];
}

/** Whether a keydown is the quick dispatch shortcut on this platform. Cmd+Space
 *  (and Cmd+J with the fallback) is accepted everywhere; Ctrl+J is also the
 *  always-on shortcut off macOS. */
export function isQuickDispatchKey(
  event: Pick<KeyboardEvent, 'altKey' | 'ctrlKey' | 'shiftKey' | 'metaKey' | 'key' | 'code'>,
  fallbackEnabled: boolean,
  mac = IS_MAC,
): boolean {
  if (event.altKey || event.shiftKey) return false;
  const j = event.key.toLowerCase() === 'j';
  if (event.metaKey && !event.ctrlKey) return event.code === 'Space' || (j && fallbackEnabled);
  return !mac && event.ctrlKey && !event.metaKey && j;
}
