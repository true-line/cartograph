import { describe, expect, it } from 'vitest';
import {
  assignBands,
  buildAtlasScene,
  displayClusterKey,
  expandAcross,
  GROUP_BUDGET,
  revealExpansion,
  type AtlasScene,
} from './atlasLayout';
import type { AtlasSnapshot, GraphEdge, GraphNode } from './store';
import { createRepositoryLabeler } from './repositoryLabels';

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

// AC-0651 (#1821): a configuration read is banded with the code that reads
// it and clusters with its file's module, not under Unclassified.
describe('assignBands — configuration reads', () => {
  it('bands a ConfigRead in Server code, in the cluster of its file', () => {
    const read = node('cfgread:local/app@src/db.ts:env:DATABASE_URL', 'ConfigRead', { key: 'DATABASE_URL' });
    const unnamed = node('cfgread:local/app@config.ts:env:?', 'ConfigRead', { dynamic: true });
    const symbol = node('sym:local/app@src/db.ts#connect', 'Symbol');
    const snapshot: AtlasSnapshot = {
      nodes: [read, unnamed, symbol, node('file:local/app@config.ts', 'File', { path: 'config.ts' })],
      edges: [edge(symbol.id, read.id, 'READS_CONFIG')],
    };
    const bands = assignBands(snapshot);
    expect(bands.get(read.id)).toBe('server');
    expect(bands.get(unnamed.id)).toBe('server');
    const scene = buildAtlasScene(snapshot, new Set());
    expect(scene.bands.map((band) => band.label)).toEqual(['Server code']);
    const cluster = (id: string) => scene.clusters.find((item) => item.members.includes(id))?.key;
    expect(cluster(read.id)).toBe('local/app · src');
    expect(cluster(read.id)).toBe(cluster(symbol.id));
    expect(cluster(unnamed.id)).toBe(cluster('file:local/app@config.ts'));
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

// AC-0366 (#1079): at estate scale a band folds its clusters into groups
// (repository, then sorted key ranges) so no level opens to more than
// GROUP_BUDGET tiles, edges aggregate between the nearest visible groups,
// and every cluster is reachable in at most three clicks.
describe('buildAtlasScene — estate-scale grouping', () => {
  // `repos` repositories × `perRepo` clusters, two Symbols per cluster, and
  // one CALLS edge from each cluster into the next repository's cluster.
  function estate(repos: number, perRepo: number): AtlasSnapshot {
    const nodes: GraphNode[] = [];
    const edges: GraphEdge[] = [];
    for (let r = 0; r < repos; r += 1) {
      for (let c = 0; c < perRepo; c += 1) {
        const dir = `d${String(c).padStart(3, '0')}`;
        for (const fn of ['a', 'b']) {
          nodes.push(node(`sym:local/r${r}@${dir}/m.ts#${fn}`, 'Symbol'));
        }
        const next = (r + 1) % repos;
        edges.push(edge(`sym:local/r${r}@${dir}/m.ts#a`, `sym:local/r${next}@${dir}/m.ts#b`, 'CALLS'));
      }
    }
    return { nodes, edges };
  }
  const tiles = (scene: AtlasScene) => scene.nodes.filter((item) => item.cluster || item.group);

  // Breadth-first over clicks: each click opens one visible group or cluster.
  function clicksToReach(snapshot: AtlasSnapshot, clusterId: string): number {
    let frontier: Set<string>[] = [new Set()];
    for (let clicks = 0; clicks <= 4; clicks += 1) {
      const next: Set<string>[] = [];
      for (const open of frontier) {
        const scene = buildAtlasScene(snapshot, open);
        const tile = scene.nodes.find((item) => item.id === clusterId);
        if (tile) return clicks + 1;
        for (const group of scene.collapsedGroups) {
          if (group.clusters.some((cluster) => cluster.id === clusterId)) {
            next.push(new Set([...open, group.id]));
          }
        }
      }
      frontier = next;
    }
    return Infinity;
  }

  it('opens one 500-cluster repository as at most GROUP_BUDGET range tiles', () => {
    const snapshot = estate(1, 500);
    const scene = buildAtlasScene(snapshot, new Set());
    expect(scene.clusters).toHaveLength(500);
    expect(tiles(scene).length).toBeLessThanOrEqual(GROUP_BUDGET);
    expect(scene.collapsedGroups.length).toBeGreaterThan(1);
    expect(scene.collapsedGroups.every((group) => group.children.length <= GROUP_BUDGET)).toBe(true);
    expect(scene.collapsedGroups.reduce((n, group) => n + group.clusters.length, 0)).toBe(500);
    expect(scene.collapsedGroups[0].key).toBe('local/r0 · d000 … d055');
  });

  it('folds many repositories by repository, then by range inside each', () => {
    const snapshot = estate(12, 90);
    const scene = buildAtlasScene(snapshot, new Set());
    expect(tiles(scene).map((item) => item.label)).toContain('local/r0 · 90 clusters · 180 nodes');
    expect(tiles(scene).length).toBeLessThanOrEqual(GROUP_BUDGET);
    const repo = scene.collapsedGroups.find((group) => group.key === 'local/r0');
    const opened = buildAtlasScene(snapshot, new Set([repo!.id]));
    expect(opened.openGroups.map((group) => group.id)).toEqual([repo!.id]);
    const inside = tiles(opened).filter((item) => item.group && item.id.startsWith('group:server:local/r0 / '));
    expect(inside).toHaveLength(2);
  });

  it('names repositories in tile labels while keys and ids stay exact (#1601)', () => {
    const snapshot = estate(12, 90);
    const repoLabel = createRepositoryLabeler([
      { repo: 'local/r0', display_name: 'simple-login' },
      { repo: 'local/r1', display_name: 'app' },
      { repo: 'local/r2', display_name: 'app' },
    ]);
    const scene = buildAtlasScene(snapshot, new Set(), repoLabel);
    const labels = tiles(scene).map((item) => item.label);
    expect(labels).toContain('simple-login · 90 clusters · 180 nodes');
    // Two repositories sharing a name keep their keys visible (AC-0144).
    expect(labels).toContain('app (local/r1) · 90 clusters · 180 nodes');
    expect(scene.collapsedGroups.find((group) => group.key === 'local/r0')?.id).toMatch(/^group:[a-z]+:local\/r0$/u);
    const plain = buildAtlasScene(snapshot, new Set());
    expect(scene.clusters.map((cluster) => cluster.id)).toEqual(plain.clusters.map((cluster) => cluster.id));
  });

  it('labels each repository part of range and nested keys', () => {
    const repoLabel = createRepositoryLabeler([{ repo: 'local/src_ab12', display_name: 'api' }]);
    expect(displayClusterKey('local/src_ab12 · d000 … d055', repoLabel)).toBe('api · d000 … d055');
    expect(displayClusterKey('local/src_ab12 / local/src_ab12 · a … local/src_ab12 · z', repoLabel)).toBe(
      'api / api · a … api · z',
    );
    expect(displayClusterKey('local/src_ab12', repoLabel)).toBe('api');
    expect(displayClusterKey('Http', repoLabel)).toBe('Http');
    expect(displayClusterKey('local/src_ab12 · users')).toBe('local/src_ab12 · users');
  });

  it('reaches every cluster in at most three clicks', () => {
    for (const [snapshot, stride] of [
      [estate(1, 500), 37],
      [estate(12, 90), 37],
      // Both dimensions past the budget: ranges, not repository → range nests.
      [estate(61, 90), 1373],
    ] as const) {
      const scene = buildAtlasScene(snapshot, new Set());
      const sample = scene.clusters.filter((_, index) => index % stride === 0);
      for (const cluster of sample) {
        expect(clicksToReach(snapshot, cluster.id)).toBeLessThanOrEqual(3);
      }
    }
  });

  it('keeps every opened level within GROUP_BUDGET tiles', () => {
    for (const snapshot of [estate(1, 500), estate(61, 90)]) {
      let open = new Set<string>();
      let scene = buildAtlasScene(snapshot, open);
      expect(tiles(scene).length).toBeLessThanOrEqual(GROUP_BUDGET);
      while (scene.collapsedGroups.length > 0) {
        open = new Set([...open, scene.collapsedGroups[0].id]);
        scene = buildAtlasScene(snapshot, open);
        expect(tiles(scene).length).toBeLessThanOrEqual(GROUP_BUDGET);
        expect(scene.unfocused.length).toBeGreaterThan(0);
      }
      expect(scene.openGroups.length).toBeLessThanOrEqual(2);
    }
  });

  it('opens one group from an aggregated edge and folds back to the overview', () => {
    const snapshot = estate(12, 90);
    const overview = buildAtlasScene(snapshot, new Set());
    const between = overview.edges.find(
      (item) => item.source === 'group:server:local/r0' && item.target === 'group:server:local/r1',
    )!;
    const open = expandAcross(new Set(), between.source, between.target);
    expect([...open]).toEqual(['group:server:local/r0']);
    const focused = buildAtlasScene(snapshot, open);
    expect(focused.openGroups.map((group) => group.id)).toEqual(['group:server:local/r0']);
    // Folding the focus back (the trail drops it and its descendants)
    // returns to the overview, not to a latent sibling.
    const folded = focused.openGroups[0].id;
    const back = buildAtlasScene(
      snapshot,
      new Set([...open].filter((id) => id !== folded && !id.startsWith(`${folded} / `))),
    );
    expect(back.openGroups).toEqual([]);
    expect(tiles(back).map((item) => item.id)).toEqual(tiles(overview).map((item) => item.id));
    expect([...expandAcross(new Set(), 'cluster:a', 'group:b')]).toEqual(['cluster:a', 'group:b']);
  });

  it('aggregates edges between the nearest visible groups', () => {
    const snapshot = estate(12, 90);
    const scene = buildAtlasScene(snapshot, new Set());
    expect(scene.edges.every((item) => item.edge === undefined)).toBe(true);
    const between = scene.edges.find(
      (item) => item.source === 'group:server:local/r0' && item.target === 'group:server:local/r1',
    );
    expect(between?.count).toBe(90);
    expect(scene.edges.length).toBeLessThanOrEqual(GROUP_BUDGET * 2);
  });

  it('wraps tiles into a grid instead of one strip, deterministically', () => {
    const snapshot = estate(1, 500);
    const scene = buildAtlasScene(snapshot, new Set());
    const shuffled = buildAtlasScene(
      { nodes: [...snapshot.nodes].reverse(), edges: [...snapshot.edges].reverse() },
      new Set(),
    );
    const at = (s: AtlasScene) => s.nodes.map((item) => `${item.id}@${item.position.x},${item.position.y}`);
    expect(at(shuffled)).toEqual(at(scene));
    const range = scene.collapsedGroups[0];
    const opened = buildAtlasScene(snapshot, new Set([range.id]));
    const rows = new Set(tiles(opened).map((item) => item.position.y));
    expect(rows.size).toBeGreaterThan(1);
  });

  it('keeps small graphs ungrouped', () => {
    const scene = buildAtlasScene(estate(1, 40), new Set());
    expect(scene.groups).toEqual([]);
  });
});

// AC-0715 (#1666): Find reveals a result folded inside groups.
describe('revealExpansion', () => {
  const estate: AtlasSnapshot = {
    nodes: Array.from({ length: 1000 }, (_, index) => ({
      id: `sym:local/estate@d${String(Math.floor(index / 2)).padStart(3, '0')}/m.ts#f${index % 2}`,
      label: 'Symbol',
      props: { name: `f${index % 2}` },
    })),
    edges: [],
  };

  it('opens the group path and the cluster, so the node renders as itself', () => {
    const id = 'sym:local/estate@d300/m.ts#f1';
    expect(buildAtlasScene(estate, new Set()).nodes.some((n) => n.id === id)).toBe(false);
    const expanded = revealExpansion(estate, id);
    const scene = buildAtlasScene(estate, expanded);
    expect(scene.nodes.some((n) => n.node?.id === id)).toBe(true);
    expect(scene.openGroups.map((g) => g.id)).toEqual([...expanded].filter((e) => e.startsWith('group:')));
    expect(scene.nodes.length).toBeLessThanOrEqual(GROUP_BUDGET + 2);
  });

  it('reveals nothing for a node that is not in the scene', () => {
    expect(revealExpansion(estate, 'missing:node').size).toBe(0);
  });
});
