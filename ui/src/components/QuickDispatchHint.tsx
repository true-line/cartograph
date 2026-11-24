import { IS_MAC, quickDispatchShortcuts } from '../shortcuts';

export interface QuickDispatchHintProps {
  /** Whether Cmd+J also opens quick dispatch on macOS. */
  fallbackEnabled: boolean;
  /** Label the macOS shortcuts; defaults to the running platform. */
  mac?: boolean;
  onOpen: () => void;
  onDismiss: () => void;
}

/**
 * The closed-state hint for quick dispatch (#251, AC-1702): a pill at the
 * bottom of the workspace that names the shortcut and opens the same dialog.
 * It shows until the first confirmed dispatch or until it is dismissed; Settings
 * turns it back on.
 */
export function QuickDispatchHint({ fallbackEnabled, mac = IS_MAC, onOpen, onDismiss }: QuickDispatchHintProps) {
  const [primary, fallback] = quickDispatchShortcuts(fallbackEnabled, mac);
  return (
    <div className="quick-dispatch-hint" role="complementary" aria-label="Quick dispatch hint">
      <button type="button" className="quick-dispatch-hint-open" onClick={onOpen}>
        <span className="material-symbols-outlined" aria-hidden="true">bolt</span>
        <span>Dispatch to an agent</span>
        <kbd>{primary}</kbd>
        {fallback && <span className="muted">or <kbd>{fallback}</kbd></span>}
      </button>
      <button type="button" className="quick-dispatch-hint-dismiss" aria-label="Hide the quick dispatch hint" onClick={onDismiss}>
        <span className="material-symbols-outlined" aria-hidden="true">close</span>
      </button>
    </div>
  );
}
