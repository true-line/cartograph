import { describe, expect, it } from 'vitest';
import { classOmissionLabel, isEscalatable, omissionLabel } from './gapClasses';
import type { SpecAssertion } from './store';

const assertion = (overrides: Partial<SpecAssertion> = {}): SpecAssertion => ({
  id: 'node:gap:orders',
  subject_id: 'gap:orders',
  subject_kind: 'Gap',
  summary: 'Gap: unresolved',
  provenance: {
    tier: 'Static',
    confidence_tier: 'Gap',
    evidence: [],
    extractor_id: 'ts',
    content_hash: 'h'.repeat(64),
  },
  ...overrides,
});

describe('isEscalatable (AC-0231, #238)', () => {
  it('offers escalation for a relation the broker allows', () => {
    const gap = assertion({ edge_label: 'PUBLISHES' });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(true);
  });

  it('withholds the offer for a relation outside the broker allowlist', () => {
    const gap = assertion({ edge_label: 'IMPORTS' });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(false);
  });

  it('stays optimistic while capability has not loaded yet', () => {
    const gap = assertion({ edge_label: 'IMPORTS' });
    expect(isEscalatable(gap, null)).toBe(true);
  });

  it('stays optimistic for a Gap node whose eventual relation is not yet known', () => {
    const gap = assertion({ edge_label: null });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(true);
    const withoutField = assertion();
    delete (withoutField as { edge_label?: string | null }).edge_label;
    expect(isEscalatable(withoutField, ['PUBLISHES', 'CALLS'])).toBe(true);
  });

  it('checks an edge-shaped gap by its own edge_label, matching subject_kind', () => {
    const edgeGap = assertion({
      id: 'edge:a->b:IMPORTS',
      subject_kind: 'IMPORTS',
      edge_label: 'IMPORTS',
    });
    expect(isEscalatable(edgeGap, ['PUBLISHES', 'CALLS'])).toBe(false);
    expect(isEscalatable(edgeGap, ['IMPORTS'])).toBe(true);
  });
});

describe('omission labels (AC-0254, #585)', () => {
  const missing = assertion({
    omission: { class: 'missing_input', seam: 'manifest_input', detail: 'env key X not found' },
  });
  const unresolved = assertion({
    id: 'node:gap:b',
    omission: { class: 'unresolved_relationship', seam: 'adapter', detail: 'unresolved call target' },
  });

  it('names the class and the seam that would fill it', () => {
    expect(omissionLabel(missing)).toBe('missing input · filled by manifest input');
    expect(omissionLabel(unresolved)).toBe('unresolved relationship · filled by adapter');
  });

  it('labels a uniform class, says when members differ, and stays silent on legacy rows', () => {
    expect(classOmissionLabel([missing, missing])).toBe('missing input · filled by manifest input');
    expect(classOmissionLabel([missing, unresolved])).toBe('mixed omission classes');
    expect(classOmissionLabel([assertion()])).toBeNull();
    expect(omissionLabel(assertion())).toBeNull();
  });
});
