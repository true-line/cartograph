import { useEffect, useState } from 'react';
import { tauriRunLimitsApi, usesConfiguredLimits, type RunLimits, type RunLimitsApi } from '../runLimits';

export interface RunLimitFooterProps {
  specialistId: string;
  limits: { model_invocations: number; tool_actions: number; active_seconds: number };
  api?: RunLimitsApi;
}

/**
 * The composer's limit line. Specialists that use Configure → Run Limits show
 * the saved task limit (#1643); it reloads when the app window regains focus,
 * because the limits are edited in a separate window.
 */
export function RunLimitFooter({ specialistId, limits, api = tauriRunLimitsApi }: RunLimitFooterProps) {
  const [runLimits, setRunLimits] = useState<RunLimits | null>(null);
  useEffect(() => {
    let live = true;
    const load = () => { api.load().then((next) => { if (live) setRunLimits(next); }, () => { if (live) setRunLimits(null); }); };
    load();
    window.addEventListener('focus', load);
    return () => { live = false; window.removeEventListener('focus', load); };
  }, [api]);
  const activeSeconds = runLimits && usesConfiguredLimits(specialistId) ? runLimits.active_seconds : limits.active_seconds;
  return <p className="muted">Up to {limits.model_invocations} model invocations and {limits.tool_actions} tool actions;
    {' '}{activeSeconds}s active execution. Source and byte limits apply. Missing evidence is not proof of absence.</p>;
}
