import { describe, expect, it } from 'vitest';
import { artifactSpan } from './investigationArtifactReads';

describe('artifactSpan (AC-1816)', () => {
  const read = { artifact_id: 'topology', title: 'Topology', content_id: 'c', response_hash: 'h', offset: 4096, next_offset: 8192, response_bytes: 4300 };
  it('names a continuing page by its content span', () => {
    expect(artifactSpan(read)).toBe('bytes 4,096–8,192, continues');
  });
  it('runs a last page to the end', () => {
    expect(artifactSpan({ ...read, next_offset: null })).toBe('from byte 4,096 to the end');
  });
});
