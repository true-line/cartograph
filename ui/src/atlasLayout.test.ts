import { describe, expect, it } from 'vitest';
import { assignBands } from './atlasLayout';
import type { AtlasSnapshot, GraphEdge, GraphNode } from './store';

function node(id: string, label: string, props: GraphNode['props'] = {}): GraphNode {
  return { id, label, props };
}

function edge(src: string, dst: string, label: string): GraphEdge {
  return { src, dst, label, props: {} };
}

// #244 review (PRRT_kwDOTOhqY86l15UK): a `.ts`-authored config (vite.config.ts,
// …) is re-parsed by the TS adapter, whose own File node wins over the
// toolchain's and carries only `path`/`prov` — never `props.config`. The
// Tool --DEFINED_IN--> File edge the toolchain always emits is the fallback
// that keeps that File in Tools/Build beside its Tool instead of Server.
describe('assignBands — Tools/Build config File placement', () => {
  it('bands an adapter-owned config File (no props.config) via its Tool DEFINED_IN edge', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('tool:local/app@vite.config.ts', 'Tool'),
        // Adapter-owned File node: only path/prov, no `config` prop
        // (adapters-lang-ts/src/lib.rs File-node emission for every source file).
        node('file:local/app@vite.config.ts', 'File', { path: 'vite.config.ts' }),
      ],
      edges: [edge('tool:local/app@vite.config.ts', 'file:local/app@vite.config.ts', 'DEFINED_IN')],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('tool:local/app@vite.config.ts')).toBe('tools');
    expect(bands.get('file:local/app@vite.config.ts')).toBe('tools');
  });

  it('still bands a toolchain-emitted config File via props.config alone', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [node('file:local/app@tsconfig.json', 'File', { path: 'tsconfig.json', config: true })],
      edges: [],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@tsconfig.json')).toBe('tools');
  });

  it('leaves a plain source File in Server', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [node('file:local/app@src/index.ts', 'File', { path: 'src/index.ts' })],
      edges: [],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@src/index.ts')).toBe('server');
  });

  it('does not band a File in Tools/Build via an unrelated DEFINED_IN edge from a Symbol', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('sym:local/app@src/index.ts#handler', 'Symbol'),
        node('file:local/app@src/index.ts', 'File', { path: 'src/index.ts' }),
      ],
      edges: [edge('sym:local/app@src/index.ts#handler', 'file:local/app@src/index.ts', 'DEFINED_IN')],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:local/app@src/index.ts')).toBe('server');
  });
});

// #1003: Gap banding indexes incident edges instead of scanning every edge per
// Gap, which froze the Atlas on a self-ingest graph with tens of thousands of Gaps.
describe('assignBands — Gap neighbors', () => {
  it('bands a Gap from its first banded neighbor in sorted edge order', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('gap:b', 'Gap'),
        node('res:queue', 'Resource'),
        node('screen:local/app@/', 'Screen'),
        node('gap:a', 'Gap'),
        node('gap:lonely', 'Gap'),
      ],
      edges: [
        edge('screen:local/app@/', 'gap:a', 'HAS_GAP'),
        edge('gap:a', 'res:queue', 'REFERENCES'),
        edge('gap:b', 'gap:a', 'RELATES_TO'),
      ],
    };
    const bands = assignBands(snapshot);
    // Sorted edges: gap:a→res:queue precedes screen→gap:a.
    expect(bands.get('gap:a')).toBe('infra');
    // gap:b's only neighbor is gap:a, resolved earlier in id order.
    expect(bands.get('gap:b')).toBe('infra');
    expect(bands.get('gap:lonely')).toBe('other');
  });

  // The per-Gap edge scan took 5–8s at this size on a laptop; linear banding
  // takes under 0.1s. The best of three runs against a 3s ceiling leaves wide
  // margins both ways, and absorbs scheduler and GC stalls on a busy runner.
  it('stays fast with tens of thousands of Gaps', () => {
    const nodes: GraphNode[] = [];
    const edges: GraphEdge[] = [];
    for (let i = 0; i < 30_000; i++) {
      nodes.push(node(`sym:local/app@f${i}`, 'Symbol'), node(`gap:${i}`, 'Gap'));
      edges.push(edge(`sym:local/app@f${i}`, `gap:${i}`, 'HAS_GAP'));
    }
    // Edges no Gap touches: the old per-Gap scan walked all of these too.
    for (let i = 0; i < 90_000; i++) {
      edges.push(edge(`sym:local/app@f${i % 30_000}`, `sym:local/app@f${(i * 7) % 30_000}`, 'CALLS'));
    }
    let fastest = Infinity;
    for (let run = 0; run < 3; run++) {
      const started = performance.now();
      const bands = assignBands({ nodes, edges });
      fastest = Math.min(fastest, performance.now() - started);
      expect(bands.get('gap:29999')).toBe('server');
    }
    expect(fastest).toBeLessThan(3_000);
  });
});

// AC-0365 (#1078): a File or Symbol the TS adapter proved to be service-worker
// code (`props.runtime`) sits in the Client band, and so does an observed
// browser-storage Resource; unproven code and other Resources keep their bands.
describe('assignBands — service-worker code and browser storage', () => {
  it('bands proven service-worker File/Symbol and browser-storage Resources as client', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [
        node('file:r@web/sw.js', 'File', { path: 'web/sw.js', runtime: 'service-worker' }),
        node('sym:r@web/sw.js#anon@954', 'Symbol', {
          name: 'anon@954',
          runtime: 'service-worker',
        }),
        node('res:r@cache', 'Resource', { name: 'agent-bot-ui-v1', kind: 'browser-storage' }),
        node('file:r@web/app.js', 'File', { path: 'web/app.js' }),
        node('sym:r@web/app.js#run', 'Symbol', { name: 'run' }),
        node('res:r@db', 'Resource', { name: 'postgres', kind: 'database' }),
      ],
      edges: [],
    };
    const bands = assignBands(snapshot);
    expect(bands.get('file:r@web/sw.js')).toBe('client');
    expect(bands.get('sym:r@web/sw.js#anon@954')).toBe('client');
    expect(bands.get('res:r@cache')).toBe('client');
    expect(bands.get('file:r@web/app.js')).toBe('server');
    expect(bands.get('sym:r@web/app.js#run')).toBe('server');
    expect(bands.get('res:r@db')).toBe('infra');
  });

  it('keeps a config File in Tools/Build even when it is marked service-worker', () => {
    const snapshot: AtlasSnapshot = {
      nodes: [node('file:r@sw.config.js', 'File', { config: true, runtime: 'service-worker' })],
      edges: [],
    };
    expect(assignBands(snapshot).get('file:r@sw.config.js')).toBe('tools');
  });
});
