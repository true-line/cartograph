import { useEffect, useState } from 'react';
import { formatDuration, investigationProgress } from '../investigationProgress';
import type { InvestigationEvent, InvestigationLimits, InvestigationStatus, InvestigationUsage } from '../investigationTypes';

export interface InvestigationProgressProps {
  status: InvestigationStatus;
  cancelRequested: boolean;
  events: InvestigationEvent[];
  usage: InvestigationUsage;
  limits: InvestigationLimits;
  /** Fixed clock for stories and tests; the live view ticks every second. */
  now?: number;
}

/** Live status for an active task, derived only from recorded events and usage (#1631). */
export function InvestigationProgress({ status, cancelRequested, events, usage, limits, now }: InvestigationProgressProps) {
  const [clock, setClock] = useState(() => now ?? Date.now());
  const view = investigationProgress(status, cancelRequested, events, now ?? clock);
  const active = view !== null;
  useEffect(() => {
    if (now !== undefined || !active) return;
    const timer = window.setInterval(() => setClock(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [now, active]);
  if (!view) return null;
  return <section className="investigation-progress" role="status" aria-label="Investigation progress">
    <p className="investigation-progress-headline"><span className="investigation-progress-pulse" aria-hidden="true" />
      <strong>{view.headline}</strong>
      {view.sinceLastEventMs !== null && <span className="muted"> · this step {formatDuration(view.sinceLastEventMs)}</span>}
    </p>
    <p className="muted">
      {view.totalMs !== null && <>Working for {formatDuration(view.totalMs)} · </>}
      Model calls: {usage.model_invocations} of {limits.model_invocations} · actions charged: {usage.tool_actions} of {limits.tool_actions}
      {' '}(executed: {events.filter((event) => event.kind === 'tool_completed').length})
    </p>
    <p className="muted">Each model request and reply is logged as it happens: Help → Debug Viewer.</p>
  </section>;
}
