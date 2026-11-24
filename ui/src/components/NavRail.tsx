import { useId } from 'react';
import { IS_MAC } from '../shortcuts';
import { SURFACES, type SurfaceView } from '../views';

export interface NavRailProps {
  active: SurfaceView;
  knownUnreadSavedResults?: number;
  unreadChecksIncomplete?: boolean;
  onNavigate: (view: SurfaceView) => void;
  onOpenPalette: () => void;
}

/** Left rail (54px): logo tile, the eight surfaces, and the ⌘K launcher.
 *  Icon-only controls carry `title` + `aria-label` (handoff §App Shell). */
export function NavRail({ active, onNavigate, onOpenPalette, knownUnreadSavedResults = 0, unreadChecksIncomplete = false }: NavRailProps) {
  const unreadDescriptionId = useId();
  const showUnread = knownUnreadSavedResults > 0 || unreadChecksIncomplete;
  const description = `Known unread saved results: ${knownUnreadSavedResults}.${unreadChecksIncomplete ? ' Some saved-result checks are incomplete.' : ''}`;
  return (
    <nav className="nav-rail" aria-label="Primary surfaces">
      <div className="nav-logo" aria-hidden="true" />
      {SURFACES.map((surface) => (
        <button
          key={surface.id}
          type="button"
          className={`nav-btn${surface.id === active ? ' active' : ''}`}
          title={surface.label}
          aria-label={surface.label}
          aria-describedby={surface.id === 'agent-chats' && showUnread ? unreadDescriptionId : undefined}
          aria-current={surface.id === active ? 'page' : undefined}
          onClick={() => onNavigate(surface.id)}
        >
          <span className="material-symbols-outlined" aria-hidden="true">
            {surface.icon}
          </span>
          {surface.id === 'agent-chats' && showUnread && <>
            <span className="nav-saved-result-badge" aria-hidden="true" title={description}>
              {knownUnreadSavedResults > 0 ? knownUnreadSavedResults : ''}{unreadChecksIncomplete ? '?' : ''}
            </span>
            <span id={unreadDescriptionId} className="nav-unread-description">{description}</span>
          </>}
        </button>
      ))}
      <button
        type="button"
        className="nav-btn nav-palette"
        title={`Command palette (${IS_MAC ? '⌘K' : 'Ctrl+K'})`}
        aria-label="Command palette"
        onClick={onOpenPalette}
      >
        <span className="material-symbols-outlined" aria-hidden="true">
          keyboard_command_key
        </span>
      </button>
    </nav>
  );
}
