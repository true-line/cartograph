import { describe, expect, it } from 'vitest';
import { composerScope, investigationScopeLabel, investigationScopeValid } from './investigationTypes';

describe('investigation scopes (#2477)', () => {
  it('builds each scope from the composer', () => {
    const composer = { anchor: 'rule:stock', hops: 2, area: 'subsystem:aliases' };
    expect(composerScope({ ...composer, scopeType: 'all' })).toEqual({ type: 'all' });
    expect(composerScope({ ...composer, scopeType: 'neighborhood' })).toEqual({ type: 'neighborhood', anchor: 'rule:stock', hops: 2 });
    expect(composerScope({ ...composer, scopeType: 'area' })).toEqual({ type: 'area', id: 'subsystem:aliases' });
  });

  it('labels an area by name, or by id when the name is unknown', () => {
    expect(investigationScopeLabel({ type: 'area', id: 'subsystem:aliases' }, 'Aliases')).toBe('Area · Aliases');
    expect(investigationScopeLabel({ type: 'area', id: 'subsystem:aliases' })).toBe('Area · subsystem:aliases');
    expect(investigationScopeLabel({ type: 'neighborhood', anchor: 'rule:stock', hops: 1 })).toBe('Fact neighborhood · rule:stock · 1 hop');
    expect(investigationScopeLabel({ type: 'all' })).toBe('Recovered system');
  });

  it('accepts only a named area within the host bound', () => {
    expect(investigationScopeValid({ type: 'area', id: 'subsystem:aliases' })).toBe(true);
    expect(investigationScopeValid({ type: 'area', id: ' ' })).toBe(false);
    expect(investigationScopeValid({ type: 'area', id: 'x'.repeat(1025) })).toBe(false);
    expect(investigationScopeValid({ type: 'neighborhood', anchor: 'rule:stock', hops: 4 })).toBe(false);
  });
});
