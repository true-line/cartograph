import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readQuickDispatchFallback, writeQuickDispatchFallback } from './quickDispatchPreference';

describe('quick dispatch fallback preference', () => {
  let values: Map<string, string>;
  beforeEach(() => {
    values = new Map();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it('defaults enabled and persists an explicit choice locally', () => {
    expect(readQuickDispatchFallback()).toBe(true);
    writeQuickDispatchFallback(false);
    expect(readQuickDispatchFallback()).toBe(false);
    writeQuickDispatchFallback(true);
    expect(readQuickDispatchFallback()).toBe(true);
  });
});
