import type { AtlasSnapshot, GraphEdge, GraphNode } from './store';

/**
 * Deterministic layer-banded Atlas layout (#159, AC-0081).
 *
 * Nodes are placed into labeled architecture bands (Infrastructure → Cloud →
 * Server → Events → Client → Tools/Build), clustered within each band by
 * repo/module, and positioned purely from sorted ids — the same snapshot
 * always yields the same scene, regardless of input order. Past a size
 * threshold the initial scene renders collapsed clusters that expand on
 * demand, so reading a large graph never requires manual arrangement.
 * At estate scale (#1079) a band with more than {@link GROUP_BUDGET}
 * clusters folds them into groups — repository, then contiguous ranges of
 * the sorted cluster keys — so no band ever opens to more than that many
 * tiles; tiles wrap into a grid rather than one long strip, and edges
 * aggregate between the nearest visible groups.
 * A Confirmed external/internal boundary placeholder
 * (`core-graph::placeholder::mint`, `props.placeholder === true` with
 * `props.boundary` `external`/`internal`) is excluded from bands entirely —
 * pure noise reduction over a fact that is not itself a gap. An unresolved
 * placeholder is a genuine Gap (R-INT-4): it keeps its band and stays
 * visible like any other fact, confidence tier `Gap` and all (#244).
 */

export type Band = 'infra' | 'cloud' | 'server' | 'events' | 'client' | 'tools' | 'other';

export const BAND_ORDER: readonly Band[] = [
  'infra',
  'cloud',
  'server',
  'events',
  'client',
  'tools',
  'other',
];

export const BAND_LABELS: Record<Band, string> = {
  infra: 'Infrastructure',
  cloud: 'Cloud',
  // "Server" alone reads as machines on a system view (#1079): these bands
  // hold recovered server-side and client-side code.
  server: 'Server code',
  events: 'Events',
  client: 'Client code',
  tools: 'Tools/Build',
  other: 'Unclassified',
};

/** Collapse clusters by default above this many nodes (AC-0081). */
export const CLUSTER_THRESHOLD = 200;

/** Never open a band (or a group) to more than this many tiles (#1079). */
export const GROUP_BUDGET = 60;

/** Collapsed tiles per grid row before a band wraps (#1079). */
export const GRID_COLUMNS = 12;

/**
 * A Confirmed external/internal boundary placeholder
 * (`core-graph::placeholder::mint`): the repository provably cannot (or
 * does, just not from a single declaring file) provide this fact, cited
 * with Confirmed evidence. It is excluded from Atlas bands entirely (#244)
 * rather than clustered under whatever label its id scheme implies — that
 * classification is itself the finding, and clutters bands with hundreds of
 * external-dependency tiles otherwise.
 *
 * An *unresolved* placeholder (`props.boundary === 'unresolved'`) is a real
 * Gap (`is_gap` in `core-graph::placeholder`) even though it also carries
 * `props.placeholder === true` — R-INT-4 requires it stay banded and
 * visible like any other fact, so this predicate deliberately returns
 * `false` for it.
 */
export function isBoundaryPlaceholder(node: GraphNode): boolean {
  if (node.props.placeholder !== true) return false;
  const boundary = node.props.boundary;
  return boundary === 'external' || boundary === 'internal';
}

/**
 * The snapshot with every Confirmed external/internal boundary placeholder
 * (and the edges that only reach one) dropped — the same exclusion
 * {@link buildAtlasScene} applies to bands, surfaced once so the node/edge
 * counts and entity indexes a consumer renders alongside the canvas agree
 * with what is actually on it (#244).
 */
export function excludeBoundaryPlaceholders(snapshot: AtlasSnapshot): AtlasSnapshot {
  const nodes = snapshot.nodes.filter((node) => !isBoundaryPlaceholder(node));
  const keep = new Set(nodes.map((node) => node.id));
  return {
    nodes,
    edges: snapshot.edges.filter((edge) => keep.has(edge.src) && keep.has(edge.dst)),
  };
}

/**
 * File node ids that are the target of a `Tool --DEFINED_IN--> File` edge
 * (`crates/ingest/src/toolchain.rs`) — the toolchain's unconditional proof
 * that a File is config evidence for some Tool, independent of which File
 * node ultimately won the id (`src-tauri/src/main.rs`: "Config files an
 * adapter already owns … keep the adapter's richer File node; the
 * DEFINED_IN edge targets the same id either way"). A `.ts`-authored
 * config (`vite.config.ts`, …) is re-parsed by the TS adapter, whose own
 * File node carries only `path`/`prov` — never `props.config` — so that
 * prop alone under-detects adapter-owned configs (#244 review). Scoped to
 * Tool sources only: Symbol/Component nodes also emit `DEFINED_IN` edges
 * to their declaring File for unrelated reasons.
 */
function toolConfigFileIds(snapshot: AtlasSnapshot): Set<string> {
  const toolIds = new Set(
    snapshot.nodes.filter((node) => node.label === 'Tool').map((node) => node.id),
  );
  const ids = new Set<string>();
  for (const edge of snapshot.edges) {
    if (edge.label === 'DEFINED_IN' && toolIds.has(edge.src)) ids.add(edge.dst);
  }
  return ids;
}

/**
 * A File or Symbol the TS adapter proved to be browser service-worker code
 * (`props.runtime`, #1078, AC-0365): it runs in the browser, so it belongs
 * on the client side whatever its kind says.
 */
export function isServiceWorkerCode(node: GraphNode): boolean {
  return node.props.runtime === 'service-worker';
}

/** An observed browser-storage Resource (Cache Storage, IndexedDB, Web
 *  Storage — `observed_infra.rs`, #734): it lives in the client (#1078). */
export function isBrowserStorage(node: GraphNode): boolean {
  return node.label === 'Resource' && node.props.kind === 'browser-storage';
}

/** The primary band a node kind belongs to; Gaps inherit from neighbors. */
function kindBand(node: GraphNode, configFileIds: ReadonlySet<string>): Band | null {
  switch (node.label) {
    case 'Resource':
      return isBrowserStorage(node) ? 'client' : 'infra';
    case 'Gateway':
      return 'cloud';
    case 'Channel':
      return 'events';
    case 'Screen':
    case 'Component':
    case 'Extension':
    case 'ExtensionContext':
    case 'Command':
    case 'Permission':
      return 'client';
    case 'Tool':
    case 'Config':
      return 'tools';
    case 'Symbol':
      return isServiceWorkerCode(node) ? 'client' : 'server';
    case 'Module':
    case 'Endpoint':
    case 'DataEntity':
      return 'server';
    case 'ConfigRead':
      // A configuration read (ADR-0059, #1821) sits with the code that
      // reads it: its id carries the reading file's path, so it clusters
      // with that file's module instead of landing in Unclassified.
      return 'server';
    case 'File':
      // Config-file evidence (#215): a File proving a Tool node belongs
      // with it in Tools/Build, not scattered into Server (#244). Either
      // the winning File node itself carries `props.config` (the toolchain
      // emitted it directly), or it's the DEFINED_IN target of a Tool node
      // (an adapter's own richer File node superseded the toolchain's,
      // #244 review).
      if (node.props.config === true || configFileIds.has(node.id)) return 'tools';
      return isServiceWorkerCode(node) ? 'client' : 'server';
    case 'Gap':
      return null; // resolved from neighbors below
    default:
      return 'other';
  }
}

/**
 * Band per node id. A Gap sits in the band of its first banded neighbor
 * (neighbors visited in sorted edge order — deterministic), falling back
 * to `other` when nothing anchors it.
 */
export function assignBands(snapshot: AtlasSnapshot): Map<string, Band> {
  const bands = new Map<string, Band>();
  const configFileIds = toolConfigFileIds(snapshot);
  const nodes = [...snapshot.nodes]
    .filter((node) => !isBoundaryPlaceholder(node))
    .sort((a, b) => a.id.localeCompare(b.id));
  const pending: GraphNode[] = [];
  for (const node of nodes) {
    const band = kindBand(node, configFileIds);
    if (band) bands.set(node.id, band);
    else pending.push(node);
  }
  const edges = [...snapshot.edges].sort(
    (a, b) => a.src.localeCompare(b.src) || a.dst.localeCompare(b.dst) || a.label.localeCompare(b.label),
  );
  // Incident edges per node, kept in sorted edge order. Scanning every edge
  // per Gap froze the Atlas on large graphs (#1003).
  const neighbors = new Map<string, string[]>();
  const link = (from: string, to: string) => {
    const list = neighbors.get(from);
    if (list) list.push(to);
    else neighbors.set(from, [to]);
  };
  for (const edge of edges) {
    link(edge.src, edge.dst);
    if (edge.dst !== edge.src) link(edge.dst, edge.src);
  }
  for (const node of pending) {
    let resolved: Band | null = null;
    for (const neighbor of neighbors.get(node.id) ?? []) {
      const band = bands.get(neighbor);
      if (band && band !== 'other') {
        resolved = band;
        break;
      }
    }
    bands.set(node.id, resolved ?? 'other');
  }
  return bands;
}

/**
 * Cluster key within a band: `repo · first-path-segment` for ids shaped
 * `prefix:{repo}@{tail}`, the channel kind for `chan:{kind}:{identity}`,
 * else the node kind. Purely id-derived, so stable across re-ingests.
 */
export function clusterKeyFor(node: GraphNode): string {
  const channel = node.id.match(/^chan:([^:]+):/u);
  if (channel) return channel[1];
  const scoped = node.id.match(/^([a-z-]+):([^@]+)@(.+)$/u);
  if (scoped) {
    const [, scheme, repo, tail] = scoped;
    // Endpoint ids are `ep:{repo}@{METHOD}:{route}` (crates/adapters-lang-*)
    // — key by method + the route's first segment so `GET /owners`,
    // `GET /owners/{id}` etc. collapse together, instead of the id-parse
    // artifact that put everything before the route's own leading slash
    // (the trailing-colon method fragment) into the key (#244).
    const endpoint = scheme === 'ep' ? tail.match(/^([A-Z]+):\/?(.*)$/u) : null;
    if (endpoint) {
      const [, method, route] = endpoint;
      const segment = route.includes('/') ? route.split('/')[0] : route.split('.')[0];
      return `${repo} · ${method} /${segment}`;
    }
    // Routed ids (screens) carry a leading slash — strip it so the first
    // real segment names the cluster, and a root route stays `/` (#173
    // review: `repo ·  · N` labels).
    const path = tail.split('#')[0].replace(/^\/+/u, '');
    const segment = path.includes('/') ? path.split('/')[0] : path.split('.')[0];
    return `${repo} · ${segment || '/'}`;
  }
  return node.label;
}

export interface AtlasCluster {
  /** Stable id: `cluster:{band}:{key}`. */
  id: string;
  band: Band;
  key: string;
  /** Member node ids, sorted. */
  members: string[];
}

/**
 * A synthetic fold of clusters (#1079): a repository, or a contiguous range
 * of sorted cluster keys. Stable id `group:{band}:{key}`; purely derived
 * from cluster keys, so it adds no facts.
 */
export interface AtlasGroup {
  id: string;
  band: Band;
  key: string;
  /** Direct children, in display order. */
  children: AtlasUnit[];
  /** Every cluster under this group, at any depth. */
  clusters: AtlasCluster[];
  /** Total member nodes under this group. */
  size: number;
}

export type AtlasUnit = { group: AtlasGroup } | { cluster: AtlasCluster };

function unitKey(unit: AtlasUnit): string {
  return 'group' in unit ? unit.group.key : unit.cluster.key;
}

function unitClusters(unit: AtlasUnit): AtlasCluster[] {
  return 'group' in unit ? unit.group.clusters : [unit.cluster];
}

function groupOf(band: Band, key: string, children: AtlasUnit[]): AtlasUnit {
  const clusters = children.flatMap(unitClusters);
  return {
    group: {
      id: `group:${band}:${key}`,
      band,
      key,
      children,
      clusters,
      size: clusters.reduce((total, cluster) => total + cluster.members.length, 0),
    },
  };
}

/**
 * Fold sorted units into at most {@link GROUP_BUDGET} contiguous ranges of
 * at most that many units each, recursing until the top level fits.
 * `scope` keeps range ids distinct between parents.
 */
function foldRanges(band: Band, scope: string, units: AtlasUnit[]): AtlasUnit[] {
  if (units.length <= GROUP_BUDGET) return units;
  const count = Math.ceil(units.length / GROUP_BUDGET);
  const size = Math.ceil(units.length / count);
  const ranges: AtlasUnit[] = [];
  for (let start = 0; start < units.length; start += size) {
    const chunk = units.slice(start, start + size);
    const first = unitKey(chunk[0]);
    const last = unitKey(chunk[chunk.length - 1]);
    // `repo · a … repo · z` reads as `repo · a … z`.
    const repo = first.slice(0, first.indexOf(' · ') + 3);
    const label = `${first} … ${repo.length > 3 && last.startsWith(repo) ? last.slice(repo.length) : last}`;
    ranges.push(groupOf(band, scope ? `${scope} / ${label}` : label, chunk));
  }
  return foldRanges(band, scope, ranges);
}

/**
 * A cluster or group key as shown (#1601): each `repo · segment` part names
 * its repository through `repoLabel` (the Workspace labeler, AC-0144), and a
 * bare repository fold names it the same way. Keys and ids stay the exact
 * repository keys; only the text changes.
 */
export function displayClusterKey(key: string, repoLabel?: (repo: string) => string): string {
  if (!repoLabel) return key;
  const part = (text: string): string => {
    const split = text.indexOf(' · ');
    if (split < 0) return repoLabel(text);
    return `${repoLabel(text.slice(0, split))}${text.slice(split)}`;
  };
  return key
    .split(' / ')
    .map((scope) => scope.split(' … ').map(part).join(' … '))
    .join(' / ');
}

/** How many range levels {@link foldRanges} nests `count` units under. */
function foldDepth(count: number): number {
  return count <= GROUP_BUDGET ? 0 : 1 + foldDepth(Math.ceil(count / GROUP_BUDGET));
}

/** The repository part of a cluster key (`repo · segment`), else the key. */
function repoOf(cluster: AtlasCluster): string {
  const split = cluster.key.indexOf(' · ');
  return split < 0 ? cluster.key : cluster.key.slice(0, split);
}

/**
 * A band's top-level units (#1079). Up to {@link GROUP_BUDGET} clusters show
 * as themselves. Beyond that they fold by repository when that separates
 * them, and each fold (and the repository list) folds again into ranges, so
 * every level fits the budget.
 */
export function bandUnits(band: Band, clusters: AtlasCluster[]): AtlasUnit[] {
  const leaves: AtlasUnit[] = clusters.map((cluster) => ({ cluster }));
  if (leaves.length <= GROUP_BUDGET) return leaves;
  const byRepo = new Map<string, AtlasCluster[]>();
  for (const cluster of clusters) {
    const repo = repoOf(cluster);
    const members = byRepo.get(repo);
    if (members) members.push(cluster);
    else byRepo.set(repo, [cluster]);
  }
  // One repository (or one cluster per repository) separates nothing, and a
  // repository fold deeper than two group levels costs more clicks than flat
  // ranges over every cluster.
  const deepest = Math.max(...[...byRepo.values()].map((members) => foldDepth(members.length)));
  if (
    byRepo.size === 1 ||
    byRepo.size === clusters.length ||
    foldDepth(byRepo.size) + 1 + deepest > Math.max(2, foldDepth(clusters.length))
  ) {
    return foldRanges(band, '', leaves);
  }
  const repos: AtlasUnit[] = [...byRepo.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([repo, members]) =>
      members.length === 1
        ? { cluster: members[0] }
        : groupOf(band, repo, foldRanges(band, repo, members.map((cluster) => ({ cluster })))),
    );
  return foldRanges(band, '', repos);
}

/**
 * The expanded set after tapping an aggregated edge: cluster endpoints open,
 * and at most one group endpoint (the source first), because a level shows
 * one open group and a second one would wait invisibly to take over when
 * the first folds back.
 */
export function expandAcross(open: ReadonlySet<string>, source: string, target: string): Set<string> {
  const next = new Set(open);
  let focused = false;
  for (const endpoint of [source, target]) {
    if (endpoint.startsWith('cluster:')) next.add(endpoint);
    else if (endpoint.startsWith('group:') && !focused) {
      next.add(endpoint);
      focused = true;
    }
  }
  return next;
}

export interface ScenePosition {
  x: number;
  y: number;
}

export interface SceneNode {
  id: string;
  label: string;
  band: Band;
  /** Present for synthetic collapsed-cluster nodes. */
  cluster?: AtlasCluster;
  /** Present for synthetic collapsed-group nodes (#1079). */
  group?: AtlasGroup;
  /** Present for real graph nodes. */
  node?: GraphNode;
  position: ScenePosition;
}

export interface SceneEdge {
  id: string;
  source: string;
  target: string;
  /** Real edge (chip label + evidence) — absent on aggregated edges. */
  edge?: GraphEdge;
  /** Aggregated relation count between collapsed endpoints. */
  count?: number;
}

export interface AtlasScene {
  nodes: SceneNode[];
  edges: SceneEdge[];
  clusters: AtlasCluster[];
  collapsed: AtlasCluster[];
  /** Every group on the focused path, open or not (#1079). */
  groups: AtlasGroup[];
  /** Groups shown as a collapsed tile. */
  collapsedGroups: AtlasGroup[];
  /** Groups the viewer opened, outermost first — the drill-down trail. */
  openGroups: AtlasGroup[];
  /** Clusters set aside beside an open group (off canvas, edges dropped). */
  unfocused: AtlasCluster[];
  bands: Array<{ band: Band; label: string; count: number }>;
  /** True when the snapshot is small enough to open fully expanded. */
  autoExpanded: boolean;
}

const NODE_DX = 96;
const NODE_DY = 84;
const CLUSTER_GAP = 72;
const BAND_GAP = 140;
const ROW_WIDTH = GRID_COLUMNS * (NODE_DX + CLUSTER_GAP);

/**
 * Build the deterministic banded scene. `expanded` holds cluster ids the
 * user opened; snapshots at or under {@link CLUSTER_THRESHOLD} nodes are
 * always fully expanded.
 */
export function buildAtlasScene(
  snapshot: AtlasSnapshot,
  expanded: ReadonlySet<string>,
  repoLabel?: (repo: string) => string,
): AtlasScene {
  const bands = assignBands(snapshot);
  // Boundary placeholders are excluded from the scene entirely (#244) — no
  // tile, no band membership; edges that only reach one through `endpointFor`
  // below vanish along with it.
  const visibleNodes = snapshot.nodes.filter((node) => !isBoundaryPlaceholder(node));
  const autoExpanded = visibleNodes.length <= CLUSTER_THRESHOLD;

  const byCluster = new Map<string, AtlasCluster>();
  for (const node of [...visibleNodes].sort((a, b) => a.id.localeCompare(b.id))) {
    const band = bands.get(node.id) ?? 'other';
    const key = clusterKeyFor(node);
    const id = `cluster:${band}:${key}`;
    const cluster = byCluster.get(id) ?? { id, band, key, members: [] };
    cluster.members.push(node.id);
    byCluster.set(id, cluster);
  }
  const clusters = [...byCluster.values()].sort(
    (a, b) => BAND_ORDER.indexOf(a.band) - BAND_ORDER.indexOf(b.band) || a.key.localeCompare(b.key),
  );

  const isOpen = (cluster: AtlasCluster) => autoExpanded || expanded.has(cluster.id);
  const nodeById = new Map(visibleNodes.map((node) => [node.id, node]));
  const sceneNodes: SceneNode[] = [];
  const collapsed: AtlasCluster[] = [];
  const groups: AtlasGroup[] = [];
  const collapsedGroups: AtlasGroup[] = [];
  const openGroups: AtlasGroup[] = [];
  // The outermost closed group hiding each cluster: edges re-target it.
  const hiddenBy = new Map<string, AtlasGroup>();
  // Clusters beside an open group: opening drills in, so they leave the
  // canvas (and its edges) until the group folds back.
  const unfocused: AtlasCluster[] = [];

  let y = 0;
  const bandCounts = new Map<Band, number>();
  for (const band of BAND_ORDER) {
    const bandClusters = clusters.filter((cluster) => cluster.band === band);
    if (bandClusters.length === 0) continue;
    bandCounts.set(
      band,
      bandClusters.reduce((total, cluster) => total + cluster.members.length, 0),
    );
    // Row-wrapped placement: a block that would pass the row width starts
    // a new row below the tallest block of the current one (#1079).
    let x = 0;
    let rowTop = y;
    let rowHeight = NODE_DY;
    const place = (width: number, height: number): ScenePosition => {
      if (x > 0 && x + width > ROW_WIDTH) {
        rowTop += rowHeight + CLUSTER_GAP;
        x = 0;
        rowHeight = NODE_DY;
      }
      const at = { x, y: rowTop };
      x += width + CLUSTER_GAP;
      rowHeight = Math.max(rowHeight, height);
      return at;
    };
    const visit = (unit: AtlasUnit, hidden: AtlasGroup | null) => {
      if ('group' in unit) {
        const group = unit.group;
        groups.push(group);
        const open = !hidden && expanded.has(group.id);
        if (!hidden && !open) {
          collapsedGroups.push(group);
          sceneNodes.push({
            id: group.id,
            label: `${displayClusterKey(group.key, repoLabel)} · ${group.clusters.length} clusters · ${group.size} nodes`,
            band,
            group,
            position: place(NODE_DX, NODE_DY),
          });
        }
        if (open) openGroups.push(group);
        visitLevel(group.children, hidden ?? (open ? null : group));
        return;
      }
      const cluster = unit.cluster;
      if (hidden) {
        hiddenBy.set(cluster.id, hidden);
        return;
      }
      if (isOpen(cluster)) {
        const cols = Math.max(1, Math.ceil(Math.sqrt(cluster.members.length)));
        const rows = Math.ceil(cluster.members.length / cols);
        const at = place(cols * NODE_DX, rows * NODE_DY);
        cluster.members.forEach((memberId, index) => {
          const member = nodeById.get(memberId);
          if (!member) return;
          sceneNodes.push({
            id: memberId,
            label: memberId,
            band,
            node: member,
            position: {
              x: at.x + (index % cols) * NODE_DX,
              y: at.y + Math.floor(index / cols) * NODE_DY,
            },
          });
        });
      } else {
        collapsed.push(cluster);
        sceneNodes.push({
          id: cluster.id,
          label: `${displayClusterKey(cluster.key, repoLabel)} · ${cluster.members.length}`,
          band,
          cluster,
          position: place(NODE_DX, NODE_DY),
        });
      }
    };
    // At each level an open group is the focus: its siblings step aside so
    // the level still shows at most GROUP_BUDGET tiles.
    const visitLevel = (units: AtlasUnit[], hidden: AtlasGroup | null) => {
      const focus = hidden
        ? undefined
        : units.find((unit) => 'group' in unit && expanded.has(unit.group.id));
      for (const unit of units) {
        if (focus && unit !== focus) {
          unfocused.push(...('group' in unit ? unit.group.clusters : [unit.cluster]));
        } else {
          visit(unit, hidden);
        }
      }
    };
    const units: AtlasUnit[] = autoExpanded
      ? bandClusters.map((cluster) => ({ cluster }))
      : bandUnits(band, bandClusters);
    visitLevel(units, null);
    y = rowTop + rowHeight + BAND_GAP;
  }

  // Edges: endpoints re-target their collapsed cluster; edges internal to a
  // collapsed cluster vanish; parallel collapsed links aggregate to a count.
  const clusterOf = new Map<string, AtlasCluster>();
  for (const cluster of clusters) {
    for (const member of cluster.members) clusterOf.set(member, cluster);
  }
  const outOfFocus = new Set(unfocused.map((cluster) => cluster.id));
  const endpointFor = (id: string): string | null => {
    const cluster = clusterOf.get(id);
    if (!cluster || outOfFocus.has(cluster.id)) return null;
    const hidden = hiddenBy.get(cluster.id);
    if (hidden) return hidden.id;
    return isOpen(cluster) ? id : cluster.id;
  };
  const real: SceneEdge[] = [];
  const aggregated = new Map<string, SceneEdge>();
  const sortedEdges = [...snapshot.edges].sort(
    (a, b) => a.src.localeCompare(b.src) || a.dst.localeCompare(b.dst) || a.label.localeCompare(b.label),
  );
  for (const edge of sortedEdges) {
    const source = endpointFor(edge.src);
    const target = endpointFor(edge.dst);
    if (!source || !target || source === target) continue;
    if (source === edge.src && target === edge.dst) {
      real.push({ id: `${edge.src} ${edge.label} ${edge.dst}`, source, target, edge });
      continue;
    }
    const id = `agg:${source} ${target}`;
    const existing = aggregated.get(id);
    if (existing) existing.count = (existing.count ?? 1) + 1;
    else aggregated.set(id, { id, source, target, count: 1 });
  }

  return {
    nodes: sceneNodes,
    edges: [...real, ...aggregated.values()],
    clusters,
    collapsed,
    groups,
    collapsedGroups,
    openGroups,
    unfocused,
    bands: BAND_ORDER.filter((band) => bandCounts.has(band)).map((band) => ({
      band,
      label: BAND_LABELS[band],
      count: bandCounts.get(band) ?? 0,
    })),
    autoExpanded,
  };
}

/**
 * The `expanded` set that reveals `nodeId` in the scene built from
 * `snapshot` (#1666): every group on the path to its cluster, outermost
 * first, plus the cluster itself — the same ids a viewer would open by
 * hand from the indexes. Each opened group is the only open group at its
 * level, so it is the drill-in focus there. A node that is not in the
 * scene (a boundary placeholder, or absent) reveals nothing.
 */
export function revealExpansion(snapshot: AtlasSnapshot, nodeId: string): Set<string> {
  // With nothing expanded, the scene lists every group at every depth.
  const scene = buildAtlasScene(snapshot, new Set());
  const cluster = scene.clusters.find((candidate) => candidate.members.includes(nodeId));
  if (!cluster) return new Set();
  const path = scene.groups
    .filter((group) => group.clusters.some((candidate) => candidate.id === cluster.id))
    .map((group) => group.id);
  return new Set([...path, cluster.id]);
}
