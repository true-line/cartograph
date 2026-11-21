// The agent pool (#252, AC-1701): which agent work is queued or running,
// what finished last, which providers are configured, and how many staged
// proposals wait for a person. Everything here is read from records the
// host already keeps (job rows, the investigation catalog, staged
// proposals); nothing is inferred about a live worker or about spend.
import type { Job, StagedProposal } from './store';
import type { InvestigationProvider } from './investigationTypes';

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
