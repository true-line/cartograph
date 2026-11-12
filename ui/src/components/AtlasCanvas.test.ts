import { describe, expect, it } from 'vitest';
import { baseLayers, displayName, filterAtlasGraph, UNNAMED_CONFIG_KEY } from './AtlasCanvas';
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

// AC-0651 (#1821): a configuration read is filed under Server code with the
// symbol that reads it, and is labelled with its key, never its raw id. A
// read whose key the source does not state as a literal says so.
describe('configuration reads', () => {
  const named = node('cfgread:r@src/db.ts:env:DATABASE_URL', 'ConfigRead', { source: 'env', key: 'DATABASE_URL' });
  const unnamed = node('cfgread:r@src/db.ts:env:?', 'ConfigRead', { source: 'env', dynamic: true });

  it('shows the key, not the id', () => {
    expect(displayName(named)).toBe('DATABASE_URL');
    expect(displayName(unnamed)).toBe(UNNAMED_CONFIG_KEY);
    expect(displayName(node('cfgread:r@src/db.ts:env:X', 'ConfigRead', { key: '' }))).toBe(UNNAMED_CONFIG_KEY);
  });

  it('is on the Server code layer with its reading symbol', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [named, unnamed, node('sym:r@src/db.ts#connect', 'Symbol', { name: 'connect' }), node('res:r@db', 'Resource')],
      edges: [{ src: 'sym:r@src/db.ts#connect', dst: named.id, label: 'READS_CONFIG', props: {} }],
    };
    expect(baseLayers(named)).toEqual(['server']);
    const server = filterAtlasGraph(snapshot, 'server');
    expect(server.nodes.map((n) => n.id).sort()).toEqual([unnamed.id, named.id, 'sym:r@src/db.ts#connect'].sort());
    expect(server.edges).toHaveLength(1);
    expect(filterAtlasGraph(snapshot, 'infra').nodes.map((n) => n.id)).toEqual(['res:r@db']);
  });
});

// AC-0028: recorded routes are display-only; do not parse stable IDs.
describe('recorded screen route labels', () => {
  it('uses root and named routes only for Screens after explicit names', () => {
    expect(displayName(node('screen:r@/', 'Screen', { route: '/' }))).toBe('/');
    expect(displayName(node('screen:r@/main', 'Screen', { route: '/main' }))).toBe('/main');
    for (const key of ['identity', 'logical_id', 'type']) {
      expect(displayName(node('screen:r@/main', 'Screen', { route: '/main', [key]: 'generic' }))).toBe('/main');
      expect(displayName(node('screen:r@/main', 'Screen', { route: '/main', [key]: 'generic', name: 'Home' }))).toBe('Home');
      expect(displayName(node('other', 'Symbol', { route: '/main', [key]: 'generic' }))).toBe('generic');
    }
    expect(displayName(node('screen:r@/main', 'Screen', { route: '/main', name: 'Home' }))).toBe('Home');
    expect(displayName(node('screen:r@/main', 'Screen', { route: '/main', display_name: 'Dashboard' }))).toBe('Dashboard');
    expect(displayName(node('screen:r@/main', 'Screen', { route: '  ' }))).toBe('screen:r@/main');
    expect(displayName(node('screen:r@/main', 'Screen'))).toBe('screen:r@/main');
    expect(displayName(node('other', 'Symbol', { route: '/main' }))).toBe('other');
  });
});
