import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import {
  exportSpecBundle,
  incompleteArtifactsMessage,
  isSameArtifact,
  resolveArtifactForCopy,
  specBundleFileContent,
  withFullArtifacts,
  type ReadSpecArtifacts,
} from './specExport';
import type { SpecArtifact, SpecBundle } from './store';
import bundleFileInput from './fixtures/spec-bundle-file.input.json';
import bundleFileExpected from './fixtures/spec-bundle-file.expected.json?raw';

const artifact = (id: string, overrides: Partial<SpecArtifact> = {}): SpecArtifact => ({
  id, file_name: `${id}.md`, title: id, format: 'markdown', schema: `cartograph.spec/${id}@1`,
  content_id: `spec-artifact-v1:${id}`, content: `${id} preview`,
  content_truncated: false, content_byte_len: 7, assertions: [], assertions_truncated: false,
  assertions_total: 0, ...overrides,
});

describe('withFullArtifacts aborts on a failed on-demand fetch (PR #533 review thread)', () => {
  it('passes untruncated artifacts through without calling readSpecArtifact', async () => {
    const whole = artifact('whole');
    const calls: string[] = [];
    const result = await withFullArtifacts([whole], async (ids) => {
      calls.push(...ids);
      return ids.map((id) => artifact(id));
    });
    expect(result).toEqual([whole]);
    expect(calls).toEqual([]);
  });

  it('replaces a truncated artifact with its fetched full content on success', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const full = artifact('gap-register', { content: 'the complete register' });
    const result = await withFullArtifacts([capped], async () => [full]);
    expect(result).toEqual([full]);
  });

  it('aborts the whole export (returns null) when any truncated artifact fetch fails, instead of falling back to the capped preview', async () => {
    const ok = artifact('drift-register', { content_truncated: true });
    const failing = artifact('gap-register', { assertions_truncated: true });
    const result = await withFullArtifacts([ok, failing], async (ids) =>
      ids.includes('gap-register') ? null : ids.map((id) => artifact(id)),
    );
    expect(result).toBeNull();
  });
});

describe('resolveArtifactForCopy aborts on a failed on-demand fetch (PR #533 review thread)', () => {
  it('returns the artifact as-is when its content was never truncated', async () => {
    const whole = artifact('whole');
    const result = await resolveArtifactForCopy(whole, async () => {
      throw new Error('should not be called');
    });
    expect(result).toBe(whole);
  });

  it('returns the fetched full artifact when the preview was truncated', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const full = artifact('gap-register', { content: 'the complete register' });
    const result = await resolveArtifactForCopy(capped, async () => full);
    expect(result).toBe(full);
  });

  it('returns null instead of the capped preview when the on-demand fetch fails', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const result = await resolveArtifactForCopy(capped, async () => null);
    expect(result).toBeNull();
  });
});

describe('artifact identity guards copy and export (AC-0242)', () => {
  const identified = (id: string, contentId: string, overrides: Partial<SpecArtifact> = {}) =>
    artifact(id, { schema: `cartograph.spec/${id}@1`, content_id: contentId, ...overrides });

  it('accepts a fetched full artifact with the preview identity', async () => {
    const capped = identified('gap-register', 'spec-artifact-v1:aa', { content_truncated: true });
    const full = identified('gap-register', 'spec-artifact-v1:aa', { content: 'complete' });
    const errors: string[] = [];
    expect(await resolveArtifactForCopy(capped, async () => full, (m) => errors.push(m))).toEqual(full);
    expect(await withFullArtifacts([capped], async () => [full], (m) => errors.push(m))).toEqual([full]);
    expect(errors).toEqual([]);
  });

  it('refuses a fetched artifact whose content identity changed, and says why', async () => {
    const capped = identified('gap-register', 'spec-artifact-v1:aa', { content_truncated: true });
    const changed = identified('gap-register', 'spec-artifact-v1:bb', { content: 'recompiled' });
    const errors: string[] = [];
    expect(await resolveArtifactForCopy(capped, async () => changed, (m) => errors.push(m))).toBeNull();
    expect(await withFullArtifacts([capped], async () => [changed], (m) => errors.push(m))).toBeNull();
    expect(errors).toHaveLength(2);
    expect(errors[0]).toContain('gap-register.md changed since this preview was compiled');
  });

  it('refuses a fetched artifact with a different schema version', async () => {
    const capped = identified('gap-register', 'spec-artifact-v1:aa', { assertions_truncated: true });
    const newer = identified('gap-register', 'spec-artifact-v1:aa', {
      schema: 'cartograph.spec/gap-register@2',
    });
    expect(isSameArtifact(capped, newer)).toBe(false);
    expect(await withFullArtifacts([capped], async () => [newer])).toBeNull();
  });
});

// AC-0720 (#1662): every capped artifact of an export is fetched in one host
// request; untruncated ones are not fetched; any result that does not
// answer the request one for one refuses the whole export.
describe('withFullArtifacts fetches every capped artifact in one request (AC-0720)', () => {
  const capped = (id: string) => artifact(id, { content_truncated: true, content: 'capped' });
  const full = (id: string) => artifact(id, { content: `${id} complete` });
  const recorder = (answer: (ids: string[]) => SpecArtifact[] | null) => {
    const requests: string[][] = [];
    const read: ReadSpecArtifacts = async (ids) => {
      requests.push(ids);
      return answer(ids);
    };
    return { requests, read };
  };

  it('makes one request for a mixed bundle and keeps the bundle order', async () => {
    const bundle = [capped('us-tm'), artifact('topology'), capped('data-model'), artifact('adrs')];
    const { requests, read } = recorder((ids) => ids.map(full));
    const result = await withFullArtifacts(bundle, read);
    expect(requests).toEqual([['us-tm', 'data-model']]);
    expect(result).toEqual([full('us-tm'), bundle[1], full('data-model'), bundle[3]]);
  });

  it('makes no request when nothing was capped', async () => {
    const bundle = [artifact('topology'), artifact('adrs')];
    const { requests, read } = recorder(() => null);
    expect(await withFullArtifacts(bundle, read)).toBe(bundle);
    expect(requests).toEqual([]);
  });

  it.each([
    ['missing', () => [full('us-tm')]],
    ['duplicate', () => [full('us-tm'), full('us-tm')]],
    ['reordered', () => [full('data-model'), full('us-tm')]],
    ['unexpected', () => [full('us-tm'), full('data-model'), full('adrs')]],
    ['still truncated', () => [full('us-tm'), capped('data-model')]],
  ])('refuses the whole export for a %s result, never a preview', async (_, answer) => {
    const bundle = [capped('us-tm'), artifact('topology'), capped('data-model')];
    const errors: string[] = [];
    const { requests, read } = recorder(answer);
    expect(await withFullArtifacts(bundle, read, (m) => errors.push(m))).toBeNull();
    expect(requests).toHaveLength(1);
    expect(errors).toEqual([incompleteArtifactsMessage()]);
  });

  it('refuses the whole export when one fetched artifact is stale', async () => {
    const bundle = [capped('us-tm'), capped('data-model')];
    const errors: string[] = [];
    const { read } = recorder(() => [
      full('us-tm'),
      artifact('data-model', { content_id: 'spec-artifact-v1:other' }),
    ]);
    expect(await withFullArtifacts(bundle, read, (m) => errors.push(m))).toBeNull();
    expect(errors).toHaveLength(1);
    expect(errors[0]).toContain('data-model.md changed since this preview was compiled');
  });
});

// AC-0740 (#1582): the shared fixture's expected file is what the Workbench
// built before the host streamed it; `app::spec_bundle_file` tests assert the
// host writes these same bytes.
describe('Export bundle file (#1582, AC-0740)', () => {
  const preview = bundleFileInput.preview as SpecBundle;
  const fullArtifacts = bundleFileInput.full_artifacts as SpecArtifact[];
  const readFull: ReadSpecArtifacts = async (ids) =>
    ids.flatMap((id) => fullArtifacts.filter((full) => full.id === id));

  beforeEach(() => {
    vi.stubGlobal('window', {});
  });

  afterEach(() => {
    clearMocks();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('builds the pinned v2 bundle file for the fixture preview (ADR-0086)', async () => {
    expect(preview.artifacts.some((a) => a.content_truncated)).toBe(true);
    const artifacts = await withFullArtifacts(preview.artifacts, readFull);
    expect(artifacts).not.toBeNull();
    const file = specBundleFileContent(preview, artifacts!);
    expect(file).toBe(bundleFileExpected);
    const parsed = JSON.parse(file);
    expect(parsed.schema).toBe('cartograph.spec-bundle-file/v2');
    expect(Object.keys(parsed.files)).toEqual(artifacts!.map((a) => a.file_name));
    for (const artifact of artifacts!) {
      expect(parsed.files[artifact.file_name]).toBe(artifact.content);
      expect(file.split(JSON.stringify(artifact.content)).length - 1).toBe(1);
    }
  });

  it('keeps the artifact order in files and refuses a repeated file name', async () => {
    const artifacts = (await withFullArtifacts(preview.artifacts, readFull))!;
    const numbered = artifacts.map((a, i) => ({ ...a, file_name: ['7', 'README.md', '0'][i] }));
    const file = specBundleFileContent(preview, numbered);
    const order = [...file.matchAll(/^"(7|README\.md|0)":/gm)].map((m) => m[1]);
    expect(order).toEqual(['7', 'README.md', '0']);
    const twice = artifacts.map((a) => ({ ...a, file_name: 'README.md' }));
    expect(() => specBundleFileContent(preview, twice)).toThrow('two artifacts share the file name "README.md"');
  });

  it('in the desktop app sends only artifact identities and builds no bundle string', async () => {
    const calls: { command: string; args: unknown }[] = [];
    mockIPC((command, args) => {
      calls.push({ command, args });
      return true;
    });
    const reads: string[] = [];
    const stringify = vi.spyOn(JSON, 'stringify');
    const errors: string[] = [];
    await exportSpecBundle(
      preview,
      async (ids) => {
        reads.push(...ids);
        return readFull(ids);
      },
      (message) => errors.push(message),
    );
    const ownStringify = stringify.mock.calls.filter(([value]) => value && typeof value === 'object' && 'files' in value);
    stringify.mockRestore();
    expect(ownStringify).toEqual([]);
    expect(reads).toEqual([]);
    expect(errors).toEqual([]);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual({
      command: 'save_spec_bundle_export',
      args: {
        suggestedName: 'cartograph-spec-best-effort.json',
        expected: {
          mode: preview.mode,
          artifacts: preview.artifacts.map(({ id, file_name, schema, content_id }) => ({
            id,
            file_name,
            schema,
            content_id,
          })),
          assertion_count: preview.assertion_count,
          gap_count: preview.gap_count,
          drift_count: preview.drift_count,
          security_count: preview.security_count,
          graph_basis: preview.graph_basis,
          curated_basis: preview.curated_basis,
        },
      },
    });
    const sent = JSON.stringify(calls[0].args);
    for (const artifact of fullArtifacts) expect(sent).not.toContain(JSON.stringify(artifact.content));
  });

  it('shows the host refusal of a changed selection in the Workbench error banner', async () => {
    mockIPC(() => {
      throw 'gap_register.json changed since this preview was compiled. Refresh the Spec Workbench and try again.';
    });
    const errors: string[] = [];
    await exportSpecBundle(preview, readFull, (message) => errors.push(message));
    expect(errors).toEqual([
      'gap_register.json changed since this preview was compiled. Refresh the Spec Workbench and try again.',
    ]);
  });
});
