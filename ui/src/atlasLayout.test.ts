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

  // Scaling, not wall-clock: ten times the Gaps and edges must cost far less
  // than the hundredfold a per-Gap edge scan takes. Best of three runs damps
  // scheduler and GC noise on a busy runner.
  it('scales linearly with tens of thousands of Gaps', () => {
    const graph = (gaps: number) => {
      const nodes: GraphNode[] = [];
      const edges: GraphEdge[] = [];
      for (let i = 0; i < gaps; i++) {
        nodes.push(node(`sym:local/app@f${i}`, 'Symbol'), node(`gap:${i}`, 'Gap'));
        edges.push(edge(`sym:local/app@f${i}`, `gap:${i}`, 'HAS_GAP'));
      }
      // Edges no Gap touches: the old per-Gap scan walked all of these too.
      for (let i = 0; i < gaps * 3; i++) {
        edges.push(edge(`sym:local/app@f${i % gaps}`, `sym:local/app@f${(i * 7) % gaps}`, 'CALLS'));
      }
      return { nodes, edges };
    };
    const best = (snapshot: AtlasSnapshot) => {
      let fastest = Infinity;
      for (let run = 0; run < 3; run++) {
        const started = performance.now();
        assignBands(snapshot);
        fastest = Math.min(fastest, performance.now() - started);
      }
      return fastest;
    };
    const small = graph(3_000);
    const large = graph(30_000);
    expect(assignBands(large).get('gap:29999')).toBe('server');
    expect(best(large) / Math.max(best(small), 1)).toBeLessThan(30);
  });
});
