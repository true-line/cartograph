import { describe, expect, it } from 'vitest';
import { displayName, filterAtlasGraph } from './AtlasCanvas';
import type { AtlasSnapshot, GraphNode } from '../store';

function node(id: string, label: string, props: GraphNode['props'] = {}): GraphNode {
  return { id, label, props };
}

// AC-0365 (#1078): the layer filter follows the same classification as the
// bands — proven service-worker code and browser storage are Client, not
// Server; browser storage stays visible as observed infrastructure (#734).
describe('filterAtlasGraph — service worker layer', () => {
  const snapshot: AtlasSnapshot = {
    nodes: [
      node('sym:r@sw.js#anon@954', 'Symbol', { name: 'anon@954', runtime: 'service-worker' }),
      node('sym:r@api.js#handler', 'Symbol', { name: 'handler' }),
      node('res:r@cache', 'Resource', { name: 'agent-bot-ui-v1', kind: 'browser-storage' }),
      node('res:r@db', 'Resource', { name: 'postgres', kind: 'database' }),
    ],
    edges: [
      { src: 'sym:r@sw.js#anon@954', dst: 'res:r@cache', label: 'USES', props: {} },
      { src: 'sym:r@api.js#handler', dst: 'res:r@db', label: 'USES', props: {} },
    ],
  };
  const ids = (layer: Parameters<typeof filterAtlasGraph>[1]) =>
    filterAtlasGraph(snapshot, layer)
      .nodes.map((n) => n.id)
      .sort();

  it('shows the worker and its storage under Client, not Server', () => {
    expect(ids('client')).toEqual(['res:r@cache', 'sym:r@sw.js#anon@954']);
    expect(ids('server')).toEqual(['sym:r@api.js#handler']);
  });

  it('keeps browser storage and its users visible under Infrastructure', () => {
    expect(ids('infra')).toEqual(['res:r@cache', 'res:r@db', 'sym:r@api.js#handler', 'sym:r@sw.js#anon@954']);
  });
});

// AC-0365 (#1078): atlas labels prefer the adapter's readable `display_name`
// over the anonymous `anon@<byte>` name, and fall back when it is absent.
describe('displayName', () => {
  it('prefers display_name over the anon id', () => {
    expect(
      displayName(node('s', 'Symbol', { name: 'anon@954', display_name: 'sw.js · install listener' })),
    ).toBe('sw.js · install listener');
  });

  it('falls back to name, then id', () => {
    expect(displayName(node('s', 'Symbol', { name: 'anon@954' }))).toBe('anon@954');
    expect(displayName(node('s', 'Symbol'))).toBe('s');
  });
});
