import type { Job } from '../store';
import type { InvestigationProvider } from '../investigationTypes';
import { stageLabel } from '../stageLabels';
import {
  LANE_LABEL, LANE_OUTPUT, agentPool, providerLine, usageLine, usageTotal,
  type AgentLane, type AgentWork, type JobUsage,
} from '../agentPool';

export interface AgentPoolPanelProps {
  jobs: Job[];
  /** The investigation catalog's configured providers; null while unknown. */
  providers: InvestigationProvider[] | null;
  /** Undecided staged proposals in the loaded page; null when not loaded. */
  awaitingCuration: number | null;
  /** Provider-reported usage per job (AC-1703); empty when none is recorded. */
  usage?: JobUsage[];
  onOpenJobs: () => void;
  onOpenCuration: () => void;
  onViewInvestigation?: (id: string) => void;
}

const LANES: AgentLane[] = ['investigation', 'area-expert', 'section-draft', 'escalation'];

function WorkRow({ item, usage, onViewInvestigation }: {
  item: AgentWork;
  usage?: JobUsage;
  onViewInvestigation?: (id: string) => void;
}) {
  const { job } = item;
  const investigation = job.investigation_id;
  return (
    <li className={`agent-pool-row job-${job.status}`}>
      <div>
        <p className="agent-pool-row-title">
          <strong>{LANE_LABEL[item.lane]}</strong>
          {item.subject && <> · <code>{item.subject}</code></>}
          <span className={`job-status job-status-${job.status}`}>{job.status}</span>
          {item.mode && <span className="agent-pool-mode">{item.mode}</span>}
        </p>
        <p className="muted">
          <code>#{job.id}</code>
          {job.status === 'running' && job.stage ? ` · ${stageLabel(job.stage)}` : ''}
          {' · updated '}{job.updated_at}
        </p>
        {usage && <p className="muted" data-testid={`agent-pool-usage-${job.id}`}>{usageLine(usage)}</p>}
        {job.status === 'failed' && job.error && <p className="job-error">{job.error}</p>}
      </div>
      {investigation && onViewInvestigation && (
        <button type="button" className="secondary-button" onClick={() => onViewInvestigation(investigation)}>
          View investigation
        </button>
      )}
    </li>
  );
}

/**
 * The agent pool (#252, AC-1701): the agent work the host has recorded as
 * jobs, the providers it is configured with, and the proposals waiting for a
 * person. Read-only: controls stay on Jobs and in curation.
 */
export function AgentPoolPanel({
  jobs, providers, awaitingCuration, usage = [], onOpenJobs, onOpenCuration, onViewInvestigation,
}: AgentPoolPanelProps) {
  const pool = agentPool(jobs);
  const usageOf = new Map(usage.map((row) => [row.job_id, row]));
  const total = usageTotal([...pool.active, ...pool.recent], usage);
  return (
    <section className="agent-pool" aria-labelledby="agent-pool-title">
      <header className="agent-pool-header">
        <div>
          <h2 id="agent-pool-title">Agent pool</h2>
          <p className="muted">
            Agent work recorded as jobs. A job record does not prove a worker is live. Agent output
            is attached as inferred and is never promoted over T0/T1 facts.
          </p>
        </div>
        <button type="button" className="secondary-button" onClick={onOpenJobs}>Open Jobs</button>
      </header>

      <ul className="agent-pool-lanes" aria-label="Active agent work by kind">
        {LANES.map((lane) => (
          <li key={lane} className="agent-pool-lane">
            <span className="agent-pool-count">{pool.activeByLane[lane]}</span>
            <span>
              <strong>{LANE_LABEL[lane]}</strong>
              <span className="muted"> {LANE_OUTPUT[lane]}</span>
            </span>
          </li>
        ))}
      </ul>

      <section aria-label="Queued and running agent work">
        <h3>Queued and running</h3>
        {pool.active.length === 0
          ? <p className="muted" data-testid="agent-pool-idle">No agent work is queued or running.</p>
          : <ul className="agent-pool-rows">
            {pool.active.map((item) => (
              <WorkRow key={item.job.id} item={item} usage={usageOf.get(item.job.id)} onViewInvestigation={onViewInvestigation} />
            ))}
          </ul>}
      </section>

      {pool.recent.length > 0 && (
        <section aria-label="Recently finished agent work">
          <h3>Recently finished</h3>
          <ul className="agent-pool-rows">
            {pool.recent.map((item) => (
              <WorkRow key={item.job.id} item={item} usage={usageOf.get(item.job.id)} onViewInvestigation={onViewInvestigation} />
            ))}
          </ul>
        </section>
      )}

      <section aria-label="Reported model use">
        <h3>Reported model use</h3>
        <p data-testid="agent-pool-usage-total">
          {total ? `Listed work: ${usageLine(total)}` : 'No model use is recorded for the listed work.'}
        </p>
        <p className="muted">
          Token counts are what each provider reported, not verified billing. Specialist
          investigations keep their usage with each saved task.
        </p>
      </section>

      <section aria-label="Configured providers">
        <h3>Providers</h3>
        {providers === null
          ? <p className="muted">The provider catalog has not loaded.</p>
          : providers.length === 0
            ? <p className="muted">No provider is configured.</p>
            : <ul className="agent-pool-providers">
              {providers.map((provider) => (
                <li key={provider.mode} className={provider.available ? 'provider-ready' : 'provider-unavailable'}>
                  {providerLine(provider)}
                </li>
              ))}
            </ul>}
        <p className="muted">From the configuration the host reads; whether a model answers is not checked here.</p>
      </section>

      <section aria-label="Proposals awaiting curation">
        <h3>Awaiting curation</h3>
        <p data-testid="agent-pool-curation">
          {awaitingCuration === null
            ? 'Staged proposals have not loaded.'
            : awaitingCuration === 0
              ? 'No staged proposal is waiting for a decision in the loaded page.'
              : `${awaitingCuration} staged ${awaitingCuration === 1 ? 'proposal is' : 'proposals are'} waiting for a decision in the loaded page.`}
        </p>
        <button type="button" className="secondary-button" onClick={onOpenCuration}>Open curation</button>
      </section>
    </section>
  );
}
