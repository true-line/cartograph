import { describe, expect, it } from 'vitest';
import { eventStage, formatDuration, investigationProgress } from './investigationProgress';
import { investigationEvents } from './investigationFixtures';

describe('investigationProgress (#1631)', () => {
  const events = investigationEvents();
  const now = Date.parse('2026-09-10T10:00:20Z');
  it('reports the current step and elapsed times for an active task', () => {
    expect(investigationProgress('running', false, events.slice(0, 4), now)).toEqual({
      headline: 'Waiting for the model to reply', sinceLastEventMs: 17_000, totalMs: 20_000 });
    expect(investigationProgress('queued', false, [], now)).toEqual({
      headline: 'Queued — waiting to start', sinceLastEventMs: null, totalMs: null });
  });
  it('prefers cancellation and consent over the last event', () => {
    expect(investigationProgress('running', true, events.slice(0, 4), now)?.headline).toBe('Cancelling — the current step will stop');
    expect(investigationProgress('awaiting_consent', false, events.slice(0, 4), now)?.headline)
      .toBe('Waiting for your approval of the next cloud action');
  });
  it('shows nothing for finished tasks', () => {
    for (const status of ['completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'] as const)
      expect(investigationProgress(status, false, events, now)).toBeNull();
  });
  it('distinguishes proposed, executed and refused actions', () => {
    expect(eventStage('action_admitted')).toBe('proposed');
    expect(eventStage('tool_completed')).toBe('executed');
    expect(eventStage('finish_refused')).toBe('not_executed');
    expect(eventStage('action_refused')).toBe('not_executed');
    expect(eventStage('query_refused')).toBe('not_executed');
  });
  it('names refused queries and actions as not executed in the current step (#1632 review)', () => {
    const at = (kind: 'action_refused' | 'query_refused') =>
      investigationProgress('running', false, [...events.slice(0, 5), { ...events[4], sequence: 99, kind }], now)?.headline;
    expect(at('query_refused')).toBe('The model’s query was refused (not executed) — asking it to try again');
    expect(at('action_refused')).toBe('The model’s proposal was refused (not executed) — continuing');
    expect(eventStage('created')).toBeNull();
  });
  it('names an artifact read in the current step (AC-1815)', () => {
    const started = { ...events[4], sequence: 99, kind: 'tool_started' as const, tool: 'read_artifact' as const };
    expect(investigationProgress('running', false, [...events.slice(0, 5), started], now)?.headline)
      .toBe('Running: reading a compiled document');
  });
  it('formats durations', () => {
    expect(formatDuration(14_900)).toBe('14 s');
    expect(formatDuration(125_000)).toBe('2 min 5 s');
  });
});
