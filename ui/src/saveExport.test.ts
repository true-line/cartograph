import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { saveExport } from './saveExport';

beforeEach(() => {
  vi.stubGlobal('window', {});
});

afterEach(() => {
  clearMocks();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('saveExport (#729)', () => {
  it('invokes save_export_file in Tauri and returns saved when confirmed', async () => {
    const calls: { command: string; args: unknown }[] = [];
    mockIPC((command, args) => {
      calls.push({ command, args });
      return true;
    });

    const outcome = await saveExport('cartograph-spec-best-effort.json', '{"mode":"best-effort"}');
    expect(outcome).toBe('saved');
    expect(calls).toEqual([
      {
        command: 'save_export_file',
        args: {
          suggestedName: 'cartograph-spec-best-effort.json',
          content: '{"mode":"best-effort"}',
        },
      },
    ]);
  });

  it('returns cancelled when the user cancels the Tauri save dialog', async () => {
    mockIPC((command) => {
      expect(command).toBe('save_export_file');
      return false;
    });

    const outcome = await saveExport('cartograph-work-packet-123.md', '# Packet');
    expect(outcome).toBe('cancelled');
  });

  it('propagates errors when save_export_file fails in Tauri', async () => {
    mockIPC(() => {
      throw new Error('Failed to write export file: permission denied');
    });

    await expect(saveExport('out.json', '{}')).rejects.toThrow(
      'Failed to write export file: permission denied',
    );
  });

  it('falls back to an anchor download outside Tauri and revokes the URL asynchronously', async () => {
    vi.useFakeTimers();
    const click = vi.fn();
    const anchor = { href: '', download: '', click };
    const createElement = vi.fn((tag: string) => {
      expect(tag).toBe('a');
      return anchor;
    });
    vi.stubGlobal('document', { createElement });

    let capturedBlob: Blob | undefined;
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockImplementation((obj) => {
      capturedBlob = obj as Blob;
      return 'blob:cartograph-export-test';
    });
    const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});

    const outcome = await saveExport('cartograph-spec-verified-only.json', '{"mode":"verified-only"}');
    expect(outcome).toBe('saved');
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(await capturedBlob?.text()).toBe('{"mode":"verified-only"}');
    expect(anchor.href).toBe('blob:cartograph-export-test');
    expect(anchor.download).toBe('cartograph-spec-verified-only.json');
    expect(click).toHaveBeenCalledTimes(1);

    // URL revocation is deferred to a macrotask (setTimeout 0), not synchronous.
    expect(revokeObjectURL).not.toHaveBeenCalled();
    vi.runAllTimers();
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:cartograph-export-test');
  });
});
