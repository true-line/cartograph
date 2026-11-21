import { describe, expect, it } from 'vitest';
import { agentPool, agentWorkOf, awaitingCuration, providerLine } from './agentPool';
import type { Job, StagedProposal } from './store';

// AC-1701 (T-1701): the agent pool reads only the job kinds the host writes.
function job(id: number, kind: string, status: string, updated = `2026-11-21T10:0${id}:00Z`, extra: Partial<Job> = {}): Job {
  return { id, kind, status, execution_tracking: 'recorded', created_at: updated, updated_at: updated, ...extra };
}

describe('agentWorkOf', () => {
  it('reads each agent job kind the host records', () => {
    expect(agentWorkOf(job(1, 'area-ask:cloud', 'running'))).toMatchObject({ lane: 'area-expert', mode: 'cloud', subject: null });
    expect(agentWorkOf(job(2, 'section-compose:spec:3.2:area:local', 'queued')))
      .toMatchObject({ lane: 'section-draft', mode: 'local', subject: 'spec § 3.2' });
    expect(agentWorkOf(job(3, 'section-compose-gated:spec:4:all:local', 'done')))
      .toMatchObject({ lane: 'section-draft', mode: 'local', subject: 'spec § 4' });
    expect(agentWorkOf(job(4, 'escalate:G-0001:local', 'running')))
      .toMatchObject({ lane: 'escalation', mode: 'local', subject: 'G-0001' });
    expect(agentWorkOf(job(5, 'escalate-class:3:cloud', 'running')))
      .toMatchObject({ lane: 'escalation', mode: 'cloud', subject: '3 gaps' });
    expect(agentWorkOf(job(6, 'investigation-v1:abc', 'running', undefined, { investigation_id: 'inv-1' })))
      .toMatchObject({ lane: 'investigation', mode: null, subject: 'inv-1' });
  });

  it('leaves ingest, repository and plugin work out', () => {
    for (const kind of ['ingest-source-v1:src_1', 'ingest:/repo', 'add-repo:https://x', 'add-system:/s', 'plugin-gate:t0', 'noop']) {
      expect(agentWorkOf(job(1, kind, 'running'))).toBeNull();
    }
  });

  it('never invents a mode the kind does not record', () => {
    expect(agentWorkOf(job(1, 'area-ask:other', 'running'))?.mode).toBeNull();
    expect(agentWorkOf(job(2, 'escalate-class:x:local', 'running'))?.subject).toBeNull();
  });
});

describe('agentPool', () => {
  it('splits active from recent work, newest first, and counts every lane', () => {
    const pool = agentPool([
      job(1, 'area-ask:cloud', 'done'),
      job(2, 'ingest:/repo', 'running'),
      job(3, 'escalate:G-1:local', 'running'),
      job(4, 'section-compose:spec:1:all:local', 'queued'),
      job(5, 'area-ask:local', 'failed'),
    ]);
    expect(pool.active.map((item) => item.job.id)).toEqual([4, 3]);
    expect(pool.recent.map((item) => item.job.id)).toEqual([5, 1]);
    expect(pool.activeByLane).toEqual({ investigation: 0, 'area-expert': 0, 'section-draft': 1, escalation: 1 });
  });

  it('keeps only the eight most recent finished items', () => {
    const jobs = Array.from({ length: 10 }, (_, index) => job(index + 1, 'area-ask:local', 'done', `2026-11-21T10:${String(index).padStart(2, '0')}:00Z`));
    expect(agentPool(jobs).recent.map((item) => item.job.id)).toEqual([10, 9, 8, 7, 6, 5, 4, 3]);
  });
});

describe('awaitingCuration and providerLine', () => {
  it('counts only undecided proposals', () => {
    const proposals = [{ review_decision: null }, { review_decision: 'accepted' }, { review_decision: null }] as StagedProposal[];
    expect(awaitingCuration(proposals)).toBe(2);
  });

  it('says a configured provider is configured, not that it answers', () => {
    const base = { mode: 'local' as const, provider_id: 'ollama', model: 'qwen3:8b', endpoint: 'http://127.0.0.1:11434', deployment: null };
    expect(providerLine({ ...base, available: true, unavailable_reason: null })).toBe('local · ollama · qwen3:8b: configured');
    expect(providerLine({ ...base, available: false, unavailable_reason: 'no key' }))
      .toBe('local · ollama · qwen3:8b: unavailable (no key)');
  });
});
