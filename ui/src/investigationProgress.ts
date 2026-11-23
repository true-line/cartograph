import type { InvestigationEvent, InvestigationStatus, InvestigationTool } from './investigationTypes';

/** Where an event sits in the life of a model-proposed action. Derived only from recorded events. */
export type InvestigationEventStage = 'waiting_for_model' | 'model_replied' | 'proposed' | 'running' | 'executed' | 'not_executed';

export const EVENT_STAGE_LABELS: Record<InvestigationEventStage, string> = {
  waiting_for_model: 'Waiting for model', model_replied: 'Model replied', proposed: 'Proposed by model · accepted',
  running: 'Running', executed: 'Executed', not_executed: 'Not executed',
};

export function eventStage(kind: InvestigationEvent['kind']): InvestigationEventStage | null {
  switch (kind) {
    case 'model_started': return 'waiting_for_model';
    case 'model_completed': return 'model_replied';
    case 'action_admitted': return 'proposed';
    case 'tool_started': return 'running';
    case 'tool_completed': return 'executed';
    case 'capacity_refused': case 'finish_refused': case 'source_replay_refused': case 'action_refused': case 'query_refused':
    case 'consent_declined': return 'not_executed';
    default: return null;
  }
}

const TOOL_LABELS: Record<InvestigationTool, string> = {
  query_context: 'searching the code graph', read_evidence: 'reading source evidence',
  read_coverage_inventory: 'reading the coverage list', read_artifact: 'reading a compiled document',
};

const ACTIVE: readonly InvestigationStatus[] = ['queued', 'preparing', 'running', 'awaiting_consent'];

export interface InvestigationProgressView {
  headline: string;
  /** Milliseconds since the last recorded event, or null when no event is loaded. */
  sinceLastEventMs: number | null;
  /** Milliseconds since the first recorded event, or null when no event is loaded. */
  totalMs: number | null;
}

/** Plain-language current step for an active task, or null once the task is no longer active. */
export function investigationProgress(status: InvestigationStatus, cancelRequested: boolean,
  events: readonly InvestigationEvent[], now: number): InvestigationProgressView | null {
  if (!ACTIVE.includes(status)) return null;
  const last = events.at(-1);
  const at = (event: InvestigationEvent | undefined) => {
    const ms = event ? Date.parse(event.created_at) : Number.NaN;
    return Number.isFinite(ms) ? Math.max(0, now - ms) : null;
  };
  return { headline: cancelRequested ? 'Cancelling — the current step will stop'
    : status === 'awaiting_consent' ? 'Waiting for your approval of the next cloud action'
      : headline(last), sinceLastEventMs: at(last), totalMs: at(events[0]) };
}

function headline(last: InvestigationEvent | undefined): string {
  if (!last) return 'Queued — waiting to start';
  switch (last.kind) {
    case 'created': return 'Queued — waiting to start';
    case 'preparation_started': return 'Preparing context from the graph';
    case 'context_prepared': return 'Context ready — about to ask the model';
    case 'model_started': return 'Waiting for the model to reply';
    case 'model_completed': return 'Model replied — checking its proposed action';
    case 'action_admitted': return 'Accepted the model’s proposed action';
    case 'tool_started': return `Running: ${last.tool ? TOOL_LABELS[last.tool] : 'action'}`;
    case 'evidence_validation_reserved': return 'Checking captured source files';
    case 'tool_completed': return 'Action finished — sending the result to the model';
    case 'read_unavailable': return 'Requested evidence was unavailable — continuing';
    case 'read_firewall_refused': return 'The firewall withheld a read from the cloud provider — continuing';
    case 'query_refused': return 'The model’s query was refused (not executed) — asking it to try again';
    case 'capacity_refused': case 'finish_refused': case 'source_replay_refused': case 'action_refused':
      return 'The model’s proposal was refused (not executed) — continuing';
    case 'consent_required': return 'Waiting for your approval of the next cloud action';
    case 'consent_approved': return 'Cloud action approved — continuing';
    case 'consent_declined': return 'Cloud action declined';
    case 'cancel_requested': return 'Cancelling — the current step will stop';
    case 'result_persisted': return 'Saving findings';
    default: return 'Finishing';
  }
}

export function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
}
