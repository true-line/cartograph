import { describe, expect, it } from 'vitest';
import { agentPool, agentWorkOf, awaitingCuration, chatSessionLine, chatSessions, controlsLine, localServerLine, providerLine, usageLine, usageTotal } from './agentPool';
import type { Job, StagedProposal } from './store';
import type { InvestigationSummary } from './investigationTypes';

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

// AC-1703 (T-1703): reported usage is shown as reported and summed only over listed work.
describe('usageLine and usageTotal', () => {
  const row = (job_id: number, calls: number, unreported: number, input: number, output: number, model = 'm') =>
    ({ job_id, calls, unreported_calls: unreported, input_tokens: input, output_tokens: output, request_bytes: 10, model });

  it('names calls, model and reported tokens, and never estimates a missing count', () => {
    expect(usageLine(row(1, 2, 0, 1200, 30))).toBe('2 calls to m: 1,200 input and 30 output tokens, as the provider reported.');
    expect(usageLine(row(1, 1, 1, 0, 0))).toBe('1 call to m: no token count reported, as the provider reported.');
    expect(usageLine(row(1, 3, 1, 10, 2))).toBe('3 calls to m: 10 input and 2 output tokens; 1 call reported no count, as the provider reported.');
  });

  it('sums the listed jobs only', () => {
    const work = agentPool([job(1, 'area-ask:cloud', 'done'), job(2, 'area-ask:local', 'running')]);
    const total = usageTotal([...work.active, ...work.recent], [row(1, 1, 0, 5, 1, 'a'), row(2, 2, 1, 7, 3, 'b'), row(9, 9, 0, 99, 99)]);
    expect(total).toMatchObject({ calls: 3, unreported_calls: 1, input_tokens: 12, output_tokens: 4, model: '2 models' });
    expect(usageTotal(work.active, [])).toBeNull();
  });
});

// AC-1704 (T-1704): the controls line says what happens to new agent work.
describe('controlsLine', () => {
  it('names pause first, then each mode limit', () => {
    expect(controlsLine({ paused: true, local_limit: 1, cloud_limit: 1 })).toBe('New agent work waits until the pool is resumed. Work already running continues.');
    expect(controlsLine({ paused: false, local_limit: 0, cloud_limit: 4 })).toBe('New agent work starts with no local limit and at most 4 cloud at a time.');
  });
});

// AC-1708 (T-1708): the pool says whether the local server answers, from the local check only.
describe('localServerLine', () => {
  const model = (name: string, installed: boolean) => ({ model: name, purpose: 'p', installed, pull_command: installed ? null : `ollama pull ${name}` });
  it('names a stopped server with its start command, then missing models', () => {
    expect(localServerLine({ reachable: false, start_command: 'ollama serve', models: [] }))
      .toBe('The local model server did not answer. Start it with ollama serve.');
    expect(localServerLine({ reachable: true, start_command: null, models: [model('a', true), model('b', false)] }))
      .toBe('The local model server is running; 1 of 2 models is missing (see Settings, Local lane).');
    expect(localServerLine({ reachable: true, start_command: null, models: [model('a', true)] }))
      .toBe('The local model server is running and every model is installed.');
  });
});

// AC-1710 (T-1710): chat sessions are the loaded history grouped by conversation.
describe('chatSessions', () => {
  const turn = (id: string, conversation: string, status: string, updated: string, extra: Partial<InvestigationSummary> = {}) => ({
    schema_version: 7, investigation_id: id, conversation_id: conversation, parent_id: null, job_id: 1,
    specialist_id: 'domain-analyst@3', question: `q ${id}`, scope: { type: 'all' }, provider_mode: 'local', status,
    revision: 1, cancel_requested: false, invocation_pending: false, actions: { can_cancel: false, can_follow_up: true },
    graph_snapshot_id: null, last_event_sequence: 1, has_result: true, created_at: updated, updated_at: updated, ...extra,
  }) as unknown as InvestigationSummary;
  it('groups turns, marks active sessions and those open in a pane by conversation, and lists active ones first', () => {
    const sessions = chatSessions([
      turn('a1', 'c-a', 'completed', '2026-11-21T10:01:00Z'),
      turn('a2', 'c-a', 'completed', '2026-11-21T10:05:00Z'),
      turn('b1', 'c-b', 'running', '2026-11-21T10:02:00Z'),
      turn('c1', 'c-c', 'completed', '2026-11-21T10:09:00Z', { invocation_pending: true }),
    ], ['c-a', 'c-elsewhere']);
    expect(sessions.map((session) => [session.conversation_id, session.turns, session.active, session.open, session.latest.investigation_id]))
      .toEqual([['c-c', 1, true, false, 'c1'], ['c-b', 1, true, false, 'b1'], ['c-a', 2, false, true, 'a2']]);
    expect(chatSessionLine(sessions[2])).toBe('2 turns · latest completed · local · updated 2026-11-21T10:05:00Z');
    expect(chatSessions([], [])).toEqual([]);
  });
});
