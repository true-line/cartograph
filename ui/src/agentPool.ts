// The agent pool (#252, AC-1701): which agent work is queued or running,
// what finished last, which providers are configured, and how many staged
// proposals wait for a person. Everything here is read from records the
// host already keeps (job rows, the investigation catalog, staged
// proposals); nothing is inferred about a live worker or about spend.
import type { Job, StagedProposal } from './store';
import type { InvestigationProvider, InvestigationSummary } from './investigationTypes';
import { investigationIsActive } from './investigationStore';
import { invokeOr } from './tauri';
import type { LocalLaneStatus } from './components/LocalLane';

/** The kinds of agent work the host records as jobs. */
export type AgentLane = 'investigation' | 'area-expert' | 'section-draft' | 'escalation';

export const LANE_LABEL: Record<AgentLane, string> = {
  investigation: 'Specialist investigation',
  'area-expert': 'Topic expert question',
  'section-draft': 'Section drafting',
  escalation: 'Gap escalation',
};

/** What each lane's output is, as the product records it (R-INT-1): agent
 *  output is attached as inferred and is never promoted over T0/T1 facts. */
export const LANE_OUTPUT: Record<AgentLane, string> = {
  investigation: 'Findings stay T3 / InferredWeak.',
  'area-expert': 'Answers are T3 / InferredWeak readings of a briefing.',
  'section-draft': 'Drafts wait for review before they enter the document.',
  escalation: 'Proposals wait for curation; nothing is written over T0/T1.',
};

export interface AgentWork {
  job: Job;
  lane: AgentLane;
  /** "local" or "cloud" when the job's kind records it; null otherwise. */
  mode: 'local' | 'cloud' | null;
  /** What the work is about, from the job's kind; null when it says nothing. */
  subject: string | null;
}

const ACTIVE = new Set(['queued', 'running']);

function modeOf(segment: string | undefined): AgentWork['mode'] {
  return segment === 'local' || segment === 'cloud' ? segment : null;
}

/** The agent work a job records, or null for non-agent work (ingest, plugin
 *  gates, repository adds). Reads only the kind the host wrote. */
export function agentWorkOf(job: Job): AgentWork | null {
  const parts = job.kind.split(':');
  const [head] = parts;
  const last = parts.length > 1 ? parts[parts.length - 1] : undefined;
  if (job.investigation_id || head === 'investigation-v1') {
    return { job, lane: 'investigation', mode: null, subject: job.investigation_id ?? null };
  }
  if (head === 'area-ask') return { job, lane: 'area-expert', mode: modeOf(parts[1]), subject: null };
  if (head === 'section-compose' || head === 'section-compose-gated') {
    const [, document, section] = parts;
    const subject = document && section ? `${document} § ${section}` : null;
    return { job, lane: 'section-draft', mode: modeOf(last), subject };
  }
  if (head === 'escalate') {
    return { job, lane: 'escalation', mode: modeOf(last), subject: parts.length > 2 ? parts.slice(1, -1).join(':') : null };
  }
  if (head === 'escalate-class') {
    const count = Number(parts[1]);
    const subject = Number.isSafeInteger(count) && count > 0 ? `${count} ${count === 1 ? 'gap' : 'gaps'}` : null;
    return { job, lane: 'escalation', mode: modeOf(last), subject };
  }
  return null;
}

export interface AgentPool {
  /** Queued and running agent work, newest first. */
  active: AgentWork[];
  /** The most recent finished agent work, newest first. */
  recent: AgentWork[];
  /** Active work per lane, every lane present. */
  activeByLane: Record<AgentLane, number>;
}

const RECENT = 8;

function newestFirst(left: AgentWork, right: AgentWork): number {
  return right.job.updated_at.localeCompare(left.job.updated_at) || right.job.id - left.job.id;
}

/** The pool over the job rows the app holds. */
export function agentPool(jobs: readonly Job[]): AgentPool {
  const work = jobs.map(agentWorkOf).filter((item): item is AgentWork => item !== null).sort(newestFirst);
  const active = work.filter((item) => ACTIVE.has(item.job.status));
  const activeByLane: Record<AgentLane, number> = {
    investigation: 0, 'area-expert': 0, 'section-draft': 0, escalation: 0,
  };
  for (const item of active) activeByLane[item.lane] += 1;
  return {
    active,
    recent: work.filter((item) => !ACTIVE.has(item.job.status)).slice(0, RECENT),
    activeByLane,
  };
}

/** Staged proposals in the loaded page that no one has decided yet. */
export function awaitingCuration(proposals: readonly StagedProposal[]): number {
  return proposals.filter((proposal) => proposal.review_decision === null).length;
}

/** One line on a configured provider, as the catalog reports it. A catalog
 *  entry says the host can reach its configuration, not that a model is
 *  loaded or answering. */
export function providerLine(provider: InvestigationProvider): string {
  const where = `${provider.mode} · ${provider.provider_id} · ${provider.model}`;
  return provider.available ? `${where}: configured` : `${where}: unavailable (${provider.unavailable_reason ?? 'no reason given'})`;
}

/** What one job's model calls used, as the providers reported it (AC-1703).
 *  Not verified billing; a call with no reported count is not estimated. */
export interface JobUsage {
  job_id: number;
  calls: number;
  unreported_calls: number;
  input_tokens: number;
  output_tokens: number;
  request_bytes: number;
  model: string;
}

/** The recorded usage of every job; empty when the host cannot say. */
export async function loadJobUsage(): Promise<JobUsage[]> {
  return invokeOr<JobUsage[]>('list_job_usage', []);
}

/** One line on what a job's calls used, as reported. */
export function usageLine(usage: JobUsage): string {
  const calls = `${usage.calls} ${usage.calls === 1 ? 'call' : 'calls'}`;
  const reported = usage.calls - usage.unreported_calls;
  const tokens = reported > 0
    ? `${usage.input_tokens.toLocaleString('en-US')} input and ${usage.output_tokens.toLocaleString('en-US')} output tokens`
    : 'no token count reported';
  const missing = usage.unreported_calls > 0 && reported > 0
    ? `; ${usage.unreported_calls} ${usage.unreported_calls === 1 ? 'call' : 'calls'} reported no count`
    : '';
  return `${calls} to ${usage.model}: ${tokens}${missing}, as the provider reported.`;
}

/** Usage summed over the listed agent work. */
export function usageTotal(work: readonly AgentWork[], usage: readonly JobUsage[]): JobUsage | null {
  const byJob = new Map(usage.map((row) => [row.job_id, row]));
  const rows = work.map((item) => byJob.get(item.job.id)).filter((row): row is JobUsage => row !== undefined);
  if (rows.length === 0) return null;
  const models = new Set(rows.map((row) => row.model));
  return rows.reduce((sum, row) => ({
    ...sum,
    calls: sum.calls + row.calls,
    unreported_calls: sum.unreported_calls + row.unreported_calls,
    input_tokens: sum.input_tokens + row.input_tokens,
    output_tokens: sum.output_tokens + row.output_tokens,
    request_bytes: sum.request_bytes + row.request_bytes,
  }), { job_id: 0, calls: 0, unreported_calls: 0, input_tokens: 0, output_tokens: 0, request_bytes: 0,
    model: models.size === 1 ? rows[0].model : `${models.size} models` });
}

/** The host's agent pool controls (AC-1704). A limit of 0 means no limit. */
export interface AgentPoolControls {
  paused: boolean;
  local_limit: number;
  cloud_limit: number;
}

/** Largest per-mode limit the host accepts. */
export const AGENT_POOL_MAX_LIMIT = 16;

/** The stored controls; null outside the app or when the host cannot say. */
export async function loadAgentPoolControls(): Promise<AgentPoolControls | null> {
  return invokeOr<AgentPoolControls | null>('get_agent_pool_controls', null);
}

/** Persist the controls and return what the host stored. */
export async function saveAgentPoolControls(controls: AgentPoolControls): Promise<AgentPoolControls | null> {
  return invokeOr<AgentPoolControls | null>('set_agent_pool_controls', null, { controls });
}

/** One line on what the controls do to new agent work. */
export function controlsLine(controls: AgentPoolControls): string {
  if (controls.paused) return 'New agent work waits until the pool is resumed. Work already running continues.';
  const limit = (count: number, mode: string) => (count === 0 ? `no ${mode} limit` : `at most ${count} ${mode}`);
  return `New agent work starts with ${limit(controls.local_limit, 'local')} and ${limit(controls.cloud_limit, 'cloud')} at a time.`;
}

/** One line on whether the local model server answers (#252, AC-1708).
 *  Only the local server is asked; no cloud provider is called. */
export function localServerLine(status: LocalLaneStatus): string {
  if (!status.reachable) {
    return `The local model server did not answer. Start it with ${status.start_command ?? 'ollama serve'}.`;
  }
  const missing = status.models.filter((model) => !model.installed).length;
  return missing === 0
    ? 'The local model server is running and every model is installed.'
    : `The local model server is running; ${missing} of ${status.models.length} models ${missing === 1 ? 'is' : 'are'} missing (see Settings, Local lane).`;
}

/** A saved chat session (#252, AC-1710): the turns of one conversation in
 *  the loaded task history, newest first. */
export interface ChatSession {
  conversation_id: string;
  /** The newest turn; opening the session opens this turn. */
  latest: InvestigationSummary;
  turns: number;
  /** Some turn is queued, running or still has an invocation pending. */
  active: boolean;
  /** The conversation is open in an Agent chats pane, whichever of its
   *  turns the pane shows (it may be outside the loaded page). */
  open: boolean;
}

/** Group the loaded task history into chat sessions: active ones first,
 *  then by the newest turn's update time. Only the loaded page is read. */
export function chatSessions(history: readonly InvestigationSummary[], openConversations: readonly string[]): ChatSession[] {
  const open = new Set(openConversations);
  const byConversation = new Map<string, ChatSession>();
  for (const turn of history) {
    const session = byConversation.get(turn.conversation_id);
    const isActive = investigationIsActive(turn);
    if (!session) {
      byConversation.set(turn.conversation_id, {
        conversation_id: turn.conversation_id, latest: turn, turns: 1, active: isActive, open: open.has(turn.conversation_id),
      });
      continue;
    }
    session.turns += 1;
    session.active ||= isActive;
    if (turn.updated_at > session.latest.updated_at) session.latest = turn;
  }
  return [...byConversation.values()].sort((a, b) =>
    Number(b.active) - Number(a.active) || b.latest.updated_at.localeCompare(a.latest.updated_at));
}

/** One line on a chat session's size and newest turn. */
export function chatSessionLine(session: ChatSession): string {
  const turns = `${session.turns} ${session.turns === 1 ? 'turn' : 'turns'}`;
  const status = session.latest.status.split('_').join(' ');
  return `${turns} · latest ${status} · ${session.latest.provider_mode} · updated ${session.latest.updated_at}`;
}
