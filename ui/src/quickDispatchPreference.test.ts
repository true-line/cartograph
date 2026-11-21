import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readQuickDispatchFallback, readQuickDispatchHint, writeQuickDispatchFallback, writeQuickDispatchHint } from './quickDispatchPreference';

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

  // AC-1702 (T-1702): the dispatch hint shows until it is turned off.
  it('shows the dispatch hint by default and keeps it off once turned off', () => {
    expect(readQuickDispatchHint()).toBe(true);
    writeQuickDispatchHint(false);
    expect(readQuickDispatchHint()).toBe(false);
    expect(readQuickDispatchFallback()).toBe(true);
    writeQuickDispatchHint(true);
    expect(readQuickDispatchHint()).toBe(true);
  });
});
