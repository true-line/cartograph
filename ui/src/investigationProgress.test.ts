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
    expect(eventStage('created')).toBeNull();
  });
  it('formats durations', () => {
    expect(formatDuration(14_900)).toBe('14 s');
    expect(formatDuration(125_000)).toBe('2 min 5 s');
  });
});
