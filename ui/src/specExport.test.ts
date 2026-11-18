import { describe, expect, it } from 'vitest';
import {
  incompleteArtifactsMessage,
  isSameArtifact,
  resolveArtifactForCopy,
  withFullArtifacts,
  type ReadSpecArtifacts,
} from './specExport';
import type { SpecArtifact } from './store';

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
