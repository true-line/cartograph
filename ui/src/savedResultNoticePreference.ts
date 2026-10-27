export const SAVED_RESULT_NOTICE_CORNERS = ['top-right', 'top-left', 'bottom-right', 'bottom-left'] as const;
export type SavedResultNoticeCorner = typeof SAVED_RESULT_NOTICE_CORNERS[number];

const KEY = 'cartograph.saved-result-notice.corner';
const isCorner = (value: string | null): value is SavedResultNoticeCorner =>
  SAVED_RESULT_NOTICE_CORNERS.some((corner) => corner === value);

export function readSavedResultNoticeCorner(): SavedResultNoticeCorner {
  try {
    const stored = localStorage.getItem(KEY);
    return isCorner(stored) ? stored : 'top-right';
  } catch {
    return 'top-right';
  }
}

export function writeSavedResultNoticeCorner(corner: SavedResultNoticeCorner): void {
  try { localStorage.setItem(KEY, corner); } catch { /* This preference is optional. */ }
}
