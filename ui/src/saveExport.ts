import { invoke } from '@tauri-apps/api/core';
import { inTauri } from './tauri';

/**
 * Save an exported file through the native host dialog when running inside
 * Tauri, or fall back to an anchor download in browser previews (#729).
 */
export async function saveExport(name: string, content: string): Promise<'saved' | 'cancelled'> {
  if (inTauri()) {
    const saved = await invoke<boolean>('save_export_file', { suggestedName: name, content });
    return saved ? 'saved' : 'cancelled';
  }
  const url = URL.createObjectURL(new Blob([content], { type: 'text/plain;charset=utf-8' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = name;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(url), 0);
  return 'saved';
}
