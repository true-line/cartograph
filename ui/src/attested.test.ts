import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { answersOf, contributorLine, loadAllAttested, loadAttested, reviewActions, reviewAttested, setEngineer } from './attested';
import type { AttestedContributor, AttestedView } from './attested';

// AC-1081 (T-1081): what the surfaces offer the engineer, and the host calls.
const agent = (verb: string): AttestedContributor => ({ display_name: 'gap-filler@1 (local/fixture)', kind: 'agent', verb, run: 'run:1' });
const engineer = (verb: string): AttestedContributor => ({ display_name: 'Placeholder Engineer', kind: 'engineer', verb, run: null });
const view = (contributors: AttestedContributor[]): AttestedView => ({
  record_id: 'attested-v1:a1',
  citation_id: 'A-00000000000000a1',
  target: { kind: 'gap', gap_id: 'gap:login-owner', slot: 'owner' },
  statement: 'The identity team owns login.',
  standing: 'agent_attested_unconfirmed',
  label: 'agent-attested, unconfirmed',
  attestation_count: contributors.length,
  contributors,
});

describe('attested records', () => {
  beforeEach(() => {
    vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  });
  afterEach(() => {
    clearMocks();
    vi.unstubAllGlobals();
  });

  it('offers confirm until the engineer backs a record, then withdraw', () => {
    expect(reviewActions(view([agent('stated')]))).toEqual(['confirmed', 'disputed']);
    expect(reviewActions(view([agent('stated'), engineer('confirmed')]))).toEqual(['disputed', 'withdrew']);
    expect(reviewActions(view([engineer('stated')]))).toEqual(['disputed', 'withdrew']);
    expect(reviewActions(view([engineer('stated'), engineer('withdrew')]))).toEqual(['confirmed', 'disputed']);
    expect(reviewActions(view([agent('stated'), engineer('disputed')]))).toEqual(['confirmed']);
  });

  it('names every contributor with what they did and finds a Gap’s answers', () => {
    const answered = view([agent('stated'), engineer('confirmed')]);
    expect(contributorLine(answered)).toBe('gap-filler@1 (local/fixture) stated · Placeholder Engineer confirmed');
    expect(answersOf([answered], 'gap:login-owner')).toEqual([answered]);
    expect(answersOf([answered], 'gap:other')).toEqual([]);
  });

  it('reviews at the count it read, from its surface', async () => {
    const calls: [string, unknown][] = [];
    mockIPC((command, args) => {
      calls.push([command, args]);
      if (command === 'list_attested_records') return { items: [view([agent('stated')])], next_before: null };
      return null;
    });
    const page = await loadAttested();
    await reviewAttested(page.items[0], 'confirmed', 'gaps_register');
    await setEngineer('Placeholder Engineer');
    expect(calls).toEqual([
      ['list_attested_records', { limit: 50, before: null }],
      ['review_attested_record', { recordId: 'attested-v1:a1', expectedCount: 1, verb: 'confirmed', source: 'gaps_register', reference: null }],
      ['set_attested_engineer', { displayName: 'Placeholder Engineer', handle: null }],
    ]);
  });

  it('reads every page within the host limit, following its cursor', async () => {
    const asked: unknown[] = [];
    const older = { ...view([agent('stated')]), record_id: 'attested-v1:a0' };
    mockIPC((command, args) => {
      if (command !== 'list_attested_records') return null;
      asked.push(args);
      return (args as { before: number | null }).before === null
        ? { items: [view([agent('stated')])], next_before: 7 }
        : { items: [older], next_before: null };
    });
    const all = await loadAllAttested();
    expect(asked).toEqual([{ limit: 50, before: null }, { limit: 50, before: 7 }]);
    expect(all.map((r) => r.record_id)).toEqual(['attested-v1:a1', 'attested-v1:a0']);
  });
});
