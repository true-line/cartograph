import { describe, expect, it } from 'vitest';
import { methodBadge, methodLabel } from './components/TierBadge';

// AC-0244 (T-0244): the method is shown beside tier and confidence, and a
// legacy fact without one says so explicitly.
describe('producing method labels', () => {
  it('names each method and marks legacy facts as not recorded', () => {
    expect(methodBadge({ kind: 'rule', rule: 'projection.api-catalog', version: '1' })).toBe('rule-derived');
    expect(methodLabel({ kind: 'rule', rule: 'projection.api-catalog', version: '1' })).toBe(
      'rule projection.api-catalog@1',
    );
    expect(methodBadge({ kind: 'model', model: 'local:llm' })).toBe('model');
    expect(methodLabel({ kind: 'model', model: 'local:llm' })).toBe('model local:llm');
    expect(methodLabel({ kind: 'human', actor: 'owner' })).toBe('human owner');
    expect(methodBadge(undefined)).toBe('method not recorded');
    expect(methodLabel(undefined)).toBe('not recorded');
    expect(methodLabel({ kind: 'unrecorded' })).toBe('not recorded');
  });
});
