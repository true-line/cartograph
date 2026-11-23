import { describe, expect, it } from 'vitest';
import { manifestSuggestions } from './manifestSuggestions';
import type { SpecAssertion } from './store';

const gap = (id: string, reason: string): SpecAssertion => ({
  id: `node:${id}`, subject_id: id, subject_kind: 'Gap', summary: `Gap: ${reason}`,
  provenance: { tier: 'Deterministic', confidence_tier: 'Gap', evidence: [], extractor_id: 't0.events', content_hash: 'x' },
});

describe('manifest suggestions from client-call gaps (#2474)', () => {
  it('groups unresolved keys by key with their call sites, largest first', () => {
    const suggestions = manifestSuggestions([
      gap('gap:fetch:local/src_a@static/js/app.js@10', 'runtime base URL `window.API_URL` of loadAliases is not declared in the system manifest'),
      gap('gap:fetch:local/src_a@static/js/app.js@90', 'runtime base URL `window.API_URL` of saveAlias is not declared in the system manifest'),
      gap('gap:fetch:local/src_a@web/api.ts@5', 'env key VITE_API not found in any env file in the repo'),
      gap('gap:fetch:local/src_a@web/x.ts@7', 'runtime-computed fetch URL'),
      gap('gap:fetch:local/src_a@web/x.ts@80', 'runtime-computed fetch URL'),
      gap('gap:fetch:local/src_a@web/y.ts@8', 'no recovered endpoint matches GET /a'),
      gap('gap:other', 'runtime-computed fetch URL'),
    ]);
    expect(suggestions.keys).toEqual([
      { key: 'window.API_URL', calls: 2, sites: ['static/js/app.js'] },
      { key: 'VITE_API', calls: 1, sites: ['web/api.ts'] },
    ]);
    expect(suggestions.computedCalls).toBe(2);
    expect(suggestions.computedSites).toEqual(['web/x.ts']);
  });

  it('suggests nothing without client-call gaps', () => {
    expect(manifestSuggestions([])).toEqual({ keys: [], computedCalls: 0, computedSites: [] });
  });
});
