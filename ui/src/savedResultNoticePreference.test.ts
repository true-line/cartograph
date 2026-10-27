import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readSavedResultNoticeCorner, writeSavedResultNoticeCorner } from './savedResultNoticePreference';
import { createSavedResultNoticeStore } from './savedResultNoticeStore';

describe('saved result notice corner preference', () => {
  let values: Map<string, string>;
  beforeEach(() => {
    values = new Map();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it('defaults top-right and reads every supported local choice', () => {
    expect(readSavedResultNoticeCorner()).toBe('top-right');
    for (const corner of ['top-left', 'bottom-right', 'bottom-left', 'top-right'] as const) {
      writeSavedResultNoticeCorner(corner);
      expect(readSavedResultNoticeCorner()).toBe(corner);
      expect(createSavedResultNoticeStore().getState().corner).toBe(corner);
    }
  });

  it('falls back for invalid or unavailable storage and ignores failed writes', () => {
    values.set('cartograph.saved-result-notice.corner', 'center');
    expect(readSavedResultNoticeCorner()).toBe('top-right');
    expect(createSavedResultNoticeStore().getState().corner).toBe('top-right');
    vi.stubGlobal('localStorage', { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); } });
    expect(readSavedResultNoticeCorner()).toBe('top-right');
    expect(() => writeSavedResultNoticeCorner('bottom-left')).not.toThrow();
    const store = createSavedResultNoticeStore();
    expect(store.getState().corner).toBe('top-right');
    expect(() => store.getState().setCorner('bottom-left')).not.toThrow();
  });
});
