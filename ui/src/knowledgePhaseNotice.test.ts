// AC-0588 (#1782): every skip or omission the core reports becomes a line with its reason code.
import { describe, expect, it } from 'vitest';
import { knowledgePhaseLines, type KnowledgeReport } from './components/KnowledgePhaseNotice';

const ran: KnowledgeReport = {
  embedded: 3, omitted: 0, omission: null, derived: 2, oversized: 0,
  derivedOmission: null, producerOmissions: [], published: true,
};

describe('knowledgePhaseLines', () => {
  it('says nothing when the phase did everything, has not run, or was superseded', () => {
    expect(knowledgePhaseLines(null)).toEqual([]);
    expect(knowledgePhaseLines({ report: ran, error: null })).toEqual([]);
    expect(knowledgePhaseLines({ report: { ...ran, published: false }, error: null })).toEqual([]);
  });

  it('names each reason the core reports', () => {
    const lines = knowledgePhaseLines({
      report: {
        ...ran, omission: 'knowledge-base-bounds', derivedOmission: 'knowledge-derived-rejected',
        producerOmissions: ['flow-trace-limit'], omitted: 1, oversized: 2,
      },
      error: null,
    });
    expect(lines.map((line) => line.code)).toEqual([
      'knowledge-base-bounds', 'knowledge-derived-rejected', 'flow-trace-limit', 'embedding-unit-cap', 'knowledge-oversized',
    ]);
    expect(lines[0].text).toContain('larger than the knowledge phase can read');
    expect(lines[2].text).toBe('One knowledge producer was skipped (flow-trace-limit).');
    expect(lines[3].text).toBe('1 unit was past the embedding cap and not embedded.');
    expect(lines[4].text).toBe('2 candidate nodes were too large and left out.');
  });

  it('distinguishes T2 off from a missing or failing provider', () => {
    const text = (omission: string) => knowledgePhaseLines({ report: { ...ran, omission }, error: null })[0].text;
    expect(text('semantic-tier-disabled')).toContain('Semantic tier (T2) is off');
    expect(text('embedding-provider-unavailable')).toContain('not installed');
    expect(text('embedding-provider-failed')).toContain('failed');
  });

  it('reports a phase that failed outright', () => {
    expect(knowledgePhaseLines({ report: null, error: 'store closed' })).toEqual([
      { code: 'knowledge-phase-failed', text: 'The knowledge phase failed: store closed' },
    ]);
  });
});
