import { describe, expect, it } from 'vitest';
import { isQuickDispatchKey, modShortcut, quickDispatchShortcuts } from './shortcuts';

const key = (over: Partial<KeyboardEvent>) => ({ altKey: false, ctrlKey: false, shiftKey: false, metaKey: false, key: '', code: '', ...over });

describe('platform-aware shortcuts', () => {
  it('labels Cmd on macOS and Ctrl elsewhere', () => {
    expect(modShortcut('K', true)).toBe('Cmd+K');
    expect(modShortcut('K', false)).toBe('Ctrl+K');
    expect(quickDispatchShortcuts(true, true)).toEqual(['Cmd+Space', 'Cmd+J']);
    expect(quickDispatchShortcuts(false, true)).toEqual(['Cmd+Space']);
    expect(quickDispatchShortcuts(true, false)).toEqual(['Ctrl+J']);
    expect(quickDispatchShortcuts(false, false)).toEqual(['Ctrl+J']);
  });

  it('fires quick dispatch on Ctrl+J off macOS, never on Ctrl+Space', () => {
    expect(isQuickDispatchKey(key({ ctrlKey: true, key: 'j', code: 'KeyJ' }), false, false)).toBe(true);
    expect(isQuickDispatchKey(key({ ctrlKey: true, key: ' ', code: 'Space' }), true, false)).toBe(false);
    expect(isQuickDispatchKey(key({ ctrlKey: true, shiftKey: true, key: 'J', code: 'KeyJ' }), true, false)).toBe(false);
  });

  it('keeps Cmd+Space and the Cmd+J fallback on macOS, and ignores Ctrl+J there', () => {
    expect(isQuickDispatchKey(key({ metaKey: true, key: ' ', code: 'Space' }), false, true)).toBe(true);
    expect(isQuickDispatchKey(key({ metaKey: true, key: 'j', code: 'KeyJ' }), true, true)).toBe(true);
    expect(isQuickDispatchKey(key({ metaKey: true, key: 'j', code: 'KeyJ' }), false, true)).toBe(false);
    expect(isQuickDispatchKey(key({ ctrlKey: true, key: 'j', code: 'KeyJ' }), true, true)).toBe(false);
  });
});
