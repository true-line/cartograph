import { describe, expect, it } from 'vitest';
import { relabelRepoText, systemDisplayName } from './repositoryLabels';

describe('system and source names (#2469, #2471)', () => {
  it('names one source by its label and several by their count', () => {
    expect(systemDisplayName([])).toBeNull();
    expect(systemDisplayName([{ repo: 'local/src_ab', display_name: 'simplelogin' }])).toBe('simplelogin');
    expect(systemDisplayName([{ repo: 'acme/shop' }])).toBe('acme/shop');
    expect(systemDisplayName([{ repo: 'a/b' }, { repo: 'c/d' }])).toBe('2 repos as one system');
  });

  it('shows the label wherever host text names the exact key', () => {
    expect(relabelRepoText('9 files of local/src_ab@e64f; declare it for local/src_ab', 'local/src_ab', 'simplelogin'))
      .toBe('9 files of simplelogin@e64f; declare it for simplelogin');
    expect(relabelRepoText('files of acme/shop', 'acme/shop', 'acme/shop')).toBe('files of acme/shop');
  });
});
