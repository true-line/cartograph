import { useEffect, useRef, useState } from 'react';
import type { SurfaceView } from '../views';
import { useInvestigationStore } from '../investigationStore';
import { createSavedResultNoticeStore, useSavedResultNoticeStore, SAVED_RESULT_TOAST_MS, savedResultNoticeKey } from '../savedResultNoticeStore';

export interface SavedResultNoticesProps {
  view: SurfaceView;
  onOpenChat: (taskId: string) => void;
  onOpenFindings: (taskId: string) => void;
  store?: ReturnType<typeof createSavedResultNoticeStore>;
}

export function SavedResultNotices({ view, onOpenChat, onOpenFindings, store = useSavedResultNoticeStore }: SavedResultNoticesProps) {
  const notices = store();
  const investigation = useInvestigationStore();
  const [listOpen, setListOpen] = useState(false);
  const toastTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  useEffect(() => {
    const current = new Set(notices.toasts);
    for (const [key, timer] of toastTimers.current) {
      if (!current.has(key)) { clearTimeout(timer); toastTimers.current.delete(key); }
    }
    for (const key of current) {
      if (toastTimers.current.has(key)) continue;
      toastTimers.current.set(key, setTimeout(() => {
        toastTimers.current.delete(key);
        store.getState().dismissToast(key);
      }, SAVED_RESULT_TOAST_MS));
    }
  }, [notices.toasts, store]);
  useEffect(() => () => {
    toastTimers.current.forEach((timer) => clearTimeout(timer));
    toastTimers.current.clear();
  }, []);

  const resultReference = investigation.chat?.messages.find((entry) =>
    entry.author === 'specialist' && entry.message.kind === 'result_reference');
  useEffect(() => {
    if (view !== 'investigations' || investigation.loading || investigation.error || investigation.chatError ||
        investigation.chatStatus !== 'saved' ||
        investigation.selectedId !== investigation.detail?.investigation_id ||
        investigation.result?.investigation_id !== investigation.selectedId ||
        investigation.detail?.has_result !== true || !resultReference ||
        resultReference.message.kind !== 'result_reference' ||
        resultReference.message.task_id !== investigation.selectedId ||
        resultReference.message.result_id !== investigation.result.result_id) return;
    store.getState().markVisible(investigation.selectedId, investigation.result.result_id);
  }, [view, investigation.loading, investigation.error, investigation.chatError, investigation.chatStatus,
    investigation.selectedId, investigation.detail, investigation.result, investigation.chat, resultReference, store]);

  const toastNotices = notices.toasts.map((key) => notices.unread.find((notice) => savedResultNoticeKey(notice) === key))
    .filter((notice): notice is NonNullable<typeof notice> => notice !== undefined);
  return (
    <aside className={`saved-result-notices saved-result-notices-${notices.corner}`} aria-label="Saved result notifications">
      {toastNotices.map((notice) => (
        <section className="saved-result-toast" role="status" key={savedResultNoticeKey(notice)}>
          <strong>Saved result ready</strong>
          <span title={notice.question}>{notice.question.slice(0, 100)}</span>
          <button type="button" aria-label={`Open chat for ${notice.task_id}: ${notice.question}`}
            onClick={() => onOpenChat(notice.task_id)}>Open chat</button>
          <button type="button" aria-label={`Open findings for ${notice.task_id}: ${notice.question}`}
            onClick={() => onOpenFindings(notice.task_id)}>Open findings</button>
          <button type="button" aria-label="Dismiss saved result notice"
            onClick={() => store.getState().dismissToast(savedResultNoticeKey(notice))}>×</button>
        </section>
      ))}
      {notices.unread.length > 0 && (
        <div className="saved-result-inbox">
          <button type="button" aria-expanded={listOpen} onClick={() => setListOpen((open) => !open)}>
            New results{notices.incomplete ? ' · some checks incomplete' : ` · ${notices.unread.length}`}
          </button>
          {listOpen && <ul>
            {notices.unread.map((notice) => <li key={savedResultNoticeKey(notice)}>
              <span title={notice.question}>{notice.question.slice(0, 100)}</span>
              <button type="button" aria-label={`Open chat for ${notice.task_id}: ${notice.question}`}
                onClick={() => onOpenChat(notice.task_id)}>Open chat</button>
              <button type="button" aria-label={`Open findings for ${notice.task_id}: ${notice.question}`}
                onClick={() => onOpenFindings(notice.task_id)}>Open findings</button>
            </li>)}
          </ul>}
        </div>
      )}
      {notices.incomplete && notices.unread.length === 0 && <p role="status">Some saved results could not be checked.</p>}
    </aside>
  );
}
