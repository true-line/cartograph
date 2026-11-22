import cytoscape from 'cytoscape';
import { HelpTip } from './HelpTip';
import { useEffect, useMemo, useRef, useState } from 'react';
import {
  BAND_LABELS,
  buildAtlasScene,
  displayClusterKey,
  excludeBoundaryPlaceholders,
  isBrowserStorage,
  isServiceWorkerCode,
  type AtlasGroup,
  type AtlasScene,
  expandAcross,
  revealExpansion,
} from '../atlasLayout';
import type { AtlasSnapshot, GraphEdge, GraphNode, Tier } from '../store';

export type AtlasLayer = 'all' | 'infra' | 'cloud' | 'server' | 'events' | 'client';
type GraphLayer = Exclude<AtlasLayer, 'all'>;

const LAYERS: ReadonlyArray<{ id: AtlasLayer; label: string }> = [
  { id: 'all', label: 'All layers' },
  { id: 'infra', label: 'Infrastructure' },
  { id: 'cloud', label: 'Cloud' },
  { id: 'server', label: 'Server code' },
  { id: 'events', label: 'Events' },
  { id: 'client', label: 'Client code' },
];

const TIERS: ReadonlyArray<{ id: Tier; label: string }> = [
  { id: 'Confirmed', label: 'Confirmed' },
  { id: 'InferredStrong', label: 'Inferred strong' },
  { id: 'InferredWeak', label: 'Inferred weak' },
  { id: 'Gap', label: 'Gap' },
];

export function baseLayers(node: GraphNode): GraphLayer[] {
  switch (node.label) {
    case 'Resource':
      // Browser storage is client state (#1078) that is still observed
      // infrastructure the code talks to (#734): Client and Infrastructure.
      return isBrowserStorage(node) ? ['client', 'infra'] : ['infra', 'cloud'];
    case 'Channel':
      return ['events', 'cloud'];
    case 'Gateway':
      return ['cloud'];
    case 'Screen':
    case 'Component':
    case 'Extension':
    case 'ExtensionContext':
    case 'Command':
    case 'Permission':
      return ['client'];
    case 'Module':
    case 'File':
    case 'Symbol':
      // Proven service-worker code runs in the browser (#1078).
      return isServiceWorkerCode(node) ? ['client'] : ['server'];
    case 'Endpoint':
    case 'DataEntity':
    case 'Config':
    case 'ConfigRead':
      // A configuration read belongs with the code that reads it (#1821).
      return ['server'];
    default:
      return [];
  }
}

function edgeLayers(edge: GraphEdge): GraphLayer[] {
  switch (edge.label) {
    case 'DEPENDS_ON':
    case 'REFERENCES':
    case 'USES':
      return ['infra'];
    case 'TRIGGERS':
    case 'ROUTES':
    case 'GRANTS':
      return ['cloud'];
    case 'BACKS':
      return ['cloud', 'events'];
    case 'PUBLISHES':
    case 'SUBSCRIBES':
      return ['events'];
    case 'RENDERS':
    case 'FETCHES':
      return ['client'];
    case 'DEFINED_IN':
    case 'IMPORTS':
    case 'CALLS':
    case 'HANDLES':
    case 'READS':
    case 'WRITES':
    case 'READS_CONFIG':
      return ['server'];
    default:
      return [];
  }
}

/** Pure O(N+E) layer projection used before handing facts to Cytoscape.
 *  A layer edge brings in its other endpoint only when one endpoint natively
 *  belongs to the layer, so generic relations between unclassified facts
 *  (rule exits, rule gaps joined by DEPENDS_ON) never surface under a layer
 *  they are not part of (#723). An edge whose two endpoints are both native to
 *  the layer also stays, whatever its label's own layer: Extension GRANTS
 *  Permission and Command HANDLES ExtensionContext are client relationships. */
export function filterAtlasGraph(snapshot: AtlasSnapshot, layer: AtlasLayer): AtlasSnapshot {
  if (layer === 'all') return snapshot;

  const native = new Set(
    snapshot.nodes.filter((node) => baseLayers(node).includes(layer)).map((node) => node.id),
  );
  const edges = snapshot.edges.filter(
    (edge) =>
      (native.has(edge.src) && native.has(edge.dst)) ||
      (edgeLayers(edge).includes(layer) && (native.has(edge.src) || native.has(edge.dst))),
  );
  const visibleIds = new Set(native);
  for (const edge of edges) {
    visibleIds.add(edge.src);
    visibleIds.add(edge.dst);
  }

  const nodes = snapshot.nodes.filter((node) => visibleIds.has(node.id));
  const existingIds = new Set(nodes.map((node) => node.id));
  return {
    nodes,
    edges: edges.filter((edge) => existingIds.has(edge.src) && existingIds.has(edge.dst)),
  };
}

/** Ego projection for focus mode (#160): the root, every node one edge
 * away in the active projection, and the edges among that kept set — a
 * deterministic function of (snapshot, root). A root that is not in the
 * snapshot projects nothing away (fail open to the unfocused view). */
export function focusAtlasGraph(snapshot: AtlasSnapshot, root: string): AtlasSnapshot {
  if (!snapshot.nodes.some((node) => node.id === root)) return snapshot;
  const keep = new Set([root]);
  for (const edge of snapshot.edges) {
    if (edge.src === root) keep.add(edge.dst);
    if (edge.dst === root) keep.add(edge.src);
  }
  return {
    nodes: snapshot.nodes.filter((node) => keep.has(node.id)),
    edges: snapshot.edges.filter((edge) => keep.has(edge.src) && keep.has(edge.dst)),
  };
}

/** Mono chip prefix per producing tier (handoff: `T0 TRIGGERS`, `GAP …`). */
function tierCode(props: GraphNode['props'] | GraphEdge['props']): string {
  if (confidence(props) === 'Gap') return 'GAP';
  switch (props.prov?.tier) {
    case 'Dynamic':
      return 'T1';
    case 'Semantic':
      return 'T2';
    case 'Agentic':
      return 'T3';
    default:
      return 'T0';
  }
}

function confidence(props: GraphNode['props'] | GraphEdge['props']): Tier {
  const tier = props.prov?.confidence_tier;
  return tier === 'Confirmed' ||
    tier === 'InferredStrong' ||
    tier === 'InferredWeak' ||
    tier === 'Gap'
    ? tier
    : 'Gap';
}

/** What a configuration read with no recorded key is called (ADR-0059
 *  decision 2): the source does not state the key as a literal. */
export const UNNAMED_CONFIG_KEY = 'key not named in source';

export function displayName(node: GraphNode): string {
  // A configuration read shows the key it reads, never its raw id (#1821).
  if (node.label === 'ConfigRead') {
    const key = node.props.key;
    return typeof key === 'string' && key.length > 0 ? key : UNNAMED_CONFIG_KEY;
  }
  const method = typeof node.props.method === 'string' ? node.props.method : null;
  const path = typeof node.props.path === 'string' ? node.props.path : null;
  if (method && path) return `${method} ${path}`;
  // `display_name` names an anonymous symbol readably; the id stays `anon@…` (#1078).
  for (const key of ['display_name', 'name']) {
    const value = node.props[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  if (node.label === 'Screen' && typeof node.props.route === 'string' && node.props.route.trim()) return node.props.route;
  for (const key of ['identity', 'logical_id', 'type']) {
    const value = node.props[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  return node.id;
}

/** How many search matches the Find list shows at once (#1666). */
export const SEARCH_RESULT_LIMIT = 50;

/** Literal entity lookup over a loaded Atlas snapshot (#1666): a trimmed,
 *  case-insensitive substring match on the display name, the kind and the
 *  stable id. It reads only the snapshot it is given — no graph reads, no
 *  model calls — and orders matches by display name, then id, so the same
 *  snapshot and query always list the same results. A blank query matches
 *  nothing. */
export function searchAtlasEntities(snapshot: AtlasSnapshot, query: string): GraphNode[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];
  return snapshot.nodes
    .map((node) => ({ node, name: displayName(node) }))
    .filter(
      ({ node, name }) =>
        name.toLowerCase().includes(needle) ||
        node.label.toLowerCase().includes(needle) ||
        node.id.toLowerCase().includes(needle),
    )
    .sort((a, b) => a.name.localeCompare(b.name) || a.node.id.localeCompare(b.node.id))
    .map(({ node }) => node);
}

/** Shape encodes kind, never color alone (handoff §Atlas): octagon with a
 *  dashed red border = Gap, diamond = gateway/channel, rectangle = the
 *  rest. Exported so the mapping itself is pinned by tests. */
export function nodeShapeClass(node: GraphNode): 'atlas-gap' | 'kind-channel' | 'kind-box' {
  if (node.label === 'Gap') return 'atlas-gap';
  if (node.label === 'Channel' || node.label === 'Gateway') return 'kind-channel';
  return 'kind-box';
}

/** Ids of every group and cluster nested under `group`. */
function descendantIds(group: AtlasGroup): string[] {
  return group.children.flatMap((child) =>
    'group' in child ? [child.group.id, ...descendantIds(child.group)] : [child.cluster.id],
  );
}

/** Clusters folded inside a collapsed group tile. */
function foldedClusterCount(scene: AtlasScene): number {
  return (
    scene.collapsedGroups.reduce((total, group) => total + group.clusters.length, 0) +
    scene.unfocused.length
  );
}

/** Scene → Cytoscape elements: band compounds, real nodes at deterministic
 *  banded positions, collapsed-cluster proxies, and edges (real ones keep
 *  the tier chip; aggregated ones carry a relation count). */
function elementsFor(scene: AtlasScene, overlay: boolean): cytoscape.ElementDefinition[] {
  const bandParents = scene.bands.map(({ band, label, count }) => ({
    data: { id: `band:${band}`, label: `${label} · ${count}` },
    classes: 'atlas-band',
  }));
  const nodeElements = scene.nodes.map((item) => {
    if (item.cluster || item.group) {
      return {
        data: { id: item.id, label: item.label, parent: `band:${item.band}` },
        position: { ...item.position },
        classes: `${item.group ? 'atlas-group' : 'atlas-cluster'} tier-neutral`,
      };
    }
    const node = item.node as GraphNode;
    const tier = confidence(node.props);
    return {
      data: {
        id: node.id,
        label: displayName(node),
        kind: node.label,
        tier,
        parent: `band:${item.band}`,
      },
      position: { ...item.position },
      classes: `${overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral'} ${nodeShapeClass(node)}`,
    };
  });
  const edgeElements = scene.edges.map((item) => {
    if (!item.edge) {
      return {
        data: {
          id: item.id,
          source: item.source,
          target: item.target,
          label: `${item.count ?? 1} relations`,
        },
        classes: 'atlas-agg tier-neutral',
      };
    }
    const edge = item.edge;
    const tier = confidence(edge.props);
    return {
      data: {
        id: `${edge.src}\u0000${edge.label}\u0000${edge.dst}`,
        source: item.source,
        target: item.target,
        // The clickable mono chip: producing tier + relation.
        label: `${tierCode(edge.props)} ${edge.label}`,
        tier,
      },
      classes: overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral',
    };
  });
  return [...bandParents, ...nodeElements, ...edgeElements];
}

/** Exported so its cluster-tile rules are pinned by a story test (#244). */
export const CY_STYLE: cytoscape.StylesheetStyle[] = [
  {
    selector: 'node',
    style: {
      width: 34,
      height: 34,
      label: 'data(label)',
      'font-size': 9,
      color: '#e5e2e1',
      'text-background-color': '#131313',
      'text-background-opacity': 0.88,
      'text-background-padding': '3px',
      'text-valign': 'bottom',
      'text-margin-y': 7,
      'border-width': 2,
      'background-color': '#2a2a2a',
      'border-color': '#8b919f',
    },
  },
  {
    selector: 'edge',
    style: {
      width: 1.5,
      label: 'data(label)',
      'font-size': 7,
      color: '#888888',
      'curve-style': 'bezier',
      'target-arrow-shape': 'triangle',
      'line-color': '#414753',
      'target-arrow-color': '#414753',
      'text-rotation': 'autorotate',
      'text-background-color': '#131313',
      'text-background-opacity': 0.8,
      'text-background-padding': '2px',
      // The chip IS the click target: without text-events Cytoscape only
      // hit-tests the thin edge geometry (#143 review).
      'text-events': 'yes',
    },
  },
  {
    selector: '.tier-confirmed',
    style: { 'background-color': '#27c93f', 'line-color': '#27c93f', 'target-arrow-color': '#27c93f' },
  },
  {
    selector: '.tier-inferredstrong',
    style: { 'background-color': '#2d9cdb', 'line-color': '#2d9cdb', 'target-arrow-color': '#2d9cdb' },
  },
  {
    selector: '.tier-inferredweak',
    style: { 'background-color': '#f2c94c', 'line-color': '#f2c94c', 'target-arrow-color': '#f2c94c' },
  },
  {
    selector: '.tier-gap',
    style: {
      'background-color': '#131313',
      'border-color': '#eb5757',
      'line-color': '#eb5757',
      'target-arrow-color': '#eb5757',
      'line-style': 'dashed',
    },
  },
  {
    selector: '.kind-box',
    style: { shape: 'round-rectangle' },
  },
  {
    selector: '.kind-channel',
    style: { shape: 'diamond' },
  },
  {
    selector: '.atlas-gap',
    // The red dashed border is the Gap's identity — it must survive the
    // confidence overlay being off (#143 review), so it lives here too.
    style: {
      shape: 'octagon',
      'border-style': 'dashed',
      'border-width': 2,
      'border-color': '#eb5757',
    },
  },
  {
    // Labeled architecture band (compound parent): a quiet container, its
    // label reads top-left; never a selection target of its own.
    selector: '.atlas-band',
    style: {
      shape: 'round-rectangle',
      'background-color': '#191919',
      'background-opacity': 0.4,
      'border-width': 1,
      'border-color': '#333a46',
      label: 'data(label)',
      'font-size': 13,
      color: '#8b919f',
      'text-valign': 'top',
      'text-halign': 'center',
      'text-margin-y': -8,
      'text-background-opacity': 0,
      events: 'no',
    },
  },
  {
    // Collapsed cluster proxy: tap to expand. The name + member count is
    // the tile's own content, not a caption underneath an empty box (#244)
    // — center it inside the shape and let it wrap within the tile width.
    selector: '.atlas-cluster',
    style: {
      shape: 'round-rectangle',
      width: 68,
      height: 52,
      'border-width': 3,
      'border-style': 'double',
      'border-color': '#8b919f',
      'background-color': '#20242c',
      'text-valign': 'center',
      'text-halign': 'center',
      'text-wrap': 'wrap',
      'text-max-width': '60px',
      'text-background-opacity': 0,
      'font-size': 8,
    },
  },
  {
    // Collapsed group of clusters (#1079): a repository or a key range.
    selector: '.atlas-group',
    style: {
      shape: 'round-rectangle',
      width: 76,
      height: 60,
      'border-width': 3,
      'border-style': 'solid',
      'border-color': '#abc7ff',
      'background-color': '#1a2433',
      'text-valign': 'center',
      'text-halign': 'center',
      'text-wrap': 'wrap',
      'text-max-width': '70px',
      'text-background-opacity': 0,
      'font-size': 8,
    },
  },
  {
    // Aggregated relations between collapsed endpoints.
    selector: '.atlas-agg',
    style: {
      'line-style': 'dotted',
      width: 2,
    },
  },
  {
    selector: ':selected',
    style: { 'border-color': '#abc7ff', 'border-width': 4 },
  },
];

export interface AtlasCanvasProps {
  snapshot: AtlasSnapshot;
  onSelect: (node: GraphNode) => void;
  /** Edge tap → evidence drawer for the edge (same contract as nodes). */
  onSelectEdge?: (edge: GraphEdge) => void;
  /** The active layer drives the header scope chip (`Atlas · <layer>`). */
  onLayerChange?: (label: string) => void;
  /** Repository names for cluster and group labels (#1601); keys stay exact. */
  repoLabel?: (repo: string) => string;
}

/** Unified read-only graph with layer and confidence projections (US-0010). */
export function AtlasCanvas({ snapshot, onSelect, onSelectEdge, onLayerChange, repoLabel }: AtlasCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const onSelectRef = useRef(onSelect);
  const onSelectEdgeRef = useRef(onSelectEdge);
  const [layer, setLayer] = useState<AtlasLayer>('all');
  const [overlay, setOverlay] = useState(true);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  // Focus mode (#160): a navigable stack of roots. Each level is the ego
  // graph of its root within the level above; Esc pops one level.
  const [focusStack, setFocusStack] = useState<string[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Find (#1666): literal lookup across the whole loaded snapshot.
  const [query, setQuery] = useState('');
  // Confirmed external/internal boundary placeholders never reach the
  // canvas (#244) — dropped once, up front, so the legend count and the
  // entity/relation indexes below always agree with what the scene renders.
  const withoutPlaceholders = useMemo(() => excludeBoundaryPlaceholders(snapshot), [snapshot]);
  const layered = useMemo(
    () => filterAtlasGraph(withoutPlaceholders, layer),
    [withoutPlaceholders, layer],
  );
  const visible = useMemo(
    () => focusStack.reduce((snap, root) => focusAtlasGraph(snap, root), layered),
    [layered, focusStack],
  );
  // Deterministic banded scene (#159): same snapshot → same positions.
  const scene = useMemo(() => buildAtlasScene(visible, expanded, repoLabel), [visible, expanded, repoLabel]);
  const matches = useMemo(
    () => searchAtlasEntities(withoutPlaceholders, query),
    [withoutPlaceholders, query],
  );
  const searching = query.trim().length > 0;
  const namesById = useMemo(
    () => new Map(snapshot.nodes.map((node) => [node.id, displayName(node)])),
    [snapshot],
  );

  const focusOn = (id: string | null) => {
    if (!id || focusStack[focusStack.length - 1] === id) return;
    if (!visible.nodes.some((node) => node.id === id)) return;
    setFocusStack((stack) => [...stack, id]);
    setExpanded(new Set());
    // Keep the keyboard on the canvas: the control just used may unmount
    // with the new projection, and Esc must keep landing inside the card.
    containerRef.current?.focus();
  };
  const backOut = (depth: number) => {
    setFocusStack((stack) => (stack.length > depth ? stack.slice(0, depth) : stack));
    setExpanded(new Set());
    containerRef.current?.focus();
  };

  // Choosing a Find result reveals the entity through the existing
  // mechanisms (#1666): the layer widens to All layers only when the active
  // layer does not hold it, focus backs out only as many levels as still
  // hide it there (a widened layer keeps every focus level that now shows
  // it), and the groups/cluster around it open. The exact original node
  // goes to the evidence drawer.
  const reveal = (node: GraphNode) => {
    const inLayer = layered.nodes.some((candidate) => candidate.id === node.id);
    const nextLayer: AtlasLayer = inLayer ? layer : 'all';
    const base = inLayer ? layered : withoutPlaceholders;
    // Every focus root is in the active layer, so it is in All layers too.
    let stack = focusStack;
    let projected = base;
    for (; stack.length > 0; stack = stack.slice(0, -1)) {
      projected = stack.reduce((snap, root) => focusAtlasGraph(snap, root), base);
      if (projected.nodes.some((candidate) => candidate.id === node.id)) break;
    }
    if (stack.length === 0) projected = base;
    const unchanged = nextLayer === layer && stack.length === focusStack.length;
    const onCanvas = scene.nodes.some((item) => item.node?.id === node.id);
    if (nextLayer !== layer) {
      setLayer(nextLayer);
      onLayerChange?.(LAYERS.find((item) => item.id === nextLayer)?.label ?? nextLayer);
    }
    if (stack.length !== focusStack.length) setFocusStack(stack);
    // Keep what the viewer opened when the entity is already on screen.
    if (!(unchanged && onCanvas)) setExpanded(revealExpansion(projected, node.id));
    setSelectedId(node.id);
    onSelect(node);
  };

  useEffect(() => {
    onSelectRef.current = onSelect;
    onSelectEdgeRef.current = onSelectEdge;
  }, [onSelect, onSelectEdge]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || visible.nodes.length === 0) return;
    const byId = new Map(visible.nodes.map((node) => [node.id, node]));
    const cy = cytoscape({
      container,
      elements: elementsFor(scene, overlay),
      style: CY_STYLE,
      layout: { name: 'preset', fit: true, padding: 36 },
      minZoom: 0.08,
      maxZoom: 4,
    });
    // Re-fit once compound band boxes have sized around their children.
    cy.fit(undefined, 36);
    const byEdgeId = new Map(
      visible.edges.map((edge) => [`${edge.src}\u0000${edge.label}\u0000${edge.dst}`, edge]),
    );
    cy.on('tap', 'node', (event) => {
      const id = event.target.id() as string;
      // A collapsed cluster or group expands in place; bands are inert
      // containers.
      if (id.startsWith('cluster:') || id.startsWith('group:')) {
        setExpanded((open) => new Set([...open, id]));
        return;
      }
      const node = byId.get(id);
      if (node) {
        setSelectedId(node.id);
        onSelectRef.current(node);
      }
    });
    // Edges (and their label chips) are first-class evidence subjects; an
    // aggregated relation expands the clusters it connects.
    cy.on('tap', 'edge', (event) => {
      const id = event.target.id() as string;
      if (id.startsWith('agg:')) {
        setExpanded((open) =>
          expandAcross(open, String(event.target.data('source')), String(event.target.data('target'))),
        );
        return;
      }
      const edge = byEdgeId.get(id);
      if (edge) onSelectEdgeRef.current?.(edge);
    });
    return () => cy.destroy();
  }, [visible, scene, overlay]);

  // Focus mode is keyboard-first (#160): Enter roots the selection, Esc
  // backs out one level. Buttons and form controls keep Enter for their
  // own activation; Esc works from anywhere inside the card.
  const handleFocusKeys = (event: React.KeyboardEvent) => {
    if (event.key === 'Escape' && focusStack.length > 0) {
      event.preventDefault();
      backOut(focusStack.length - 1);
      return;
    }
    if (event.key === 'Enter' && selectedId) {
      if ((event.target as HTMLElement).closest('button, select, input, a, textarea')) return;
      event.preventDefault();
      focusOn(selectedId);
    }
  };

  const currentRoot = focusStack[focusStack.length - 1];
  const focusStatus =
    focusStack.length > 0
      ? `Focused on ${namesById.get(currentRoot) ?? currentRoot} — ${visible.nodes.length} nodes at level ${focusStack.length}. Esc backs out one level.`
      : selectedId
        ? `${namesById.get(selectedId) ?? selectedId} selected — press Enter to focus its connections.`
        : '';

  return (
    <section className="card atlas-card" aria-labelledby="atlas-title" onKeyDown={handleFocusKeys}>
      <div className="atlas-heading">
        <div>
          <h2 id="atlas-title">Atlas · unified graph</h2>
          <p className="muted">Five layers, one provenance-bearing graph. Select any entity for read-only evidence.</p>
        </div>
        <button
          type="button"
          className={`atlas-overlay-toggle ${overlay ? 'active' : ''}`}
          aria-pressed={overlay}
          onClick={() => setOverlay((value) => !value)}
        >
          Confidence overlay
        </button>
      </div>

      <div className="atlas-toolbar" aria-label="Atlas layer filters">
        {LAYERS.map((item) => (
          <button
            key={item.id}
            type="button"
            className={layer === item.id ? 'active' : ''}
            aria-pressed={layer === item.id}
            onClick={() => {
              setLayer(item.id);
              setExpanded(new Set());
              // A layer switch changes the projection focus was built on —
              // reset rather than carry roots that may not exist there.
              setFocusStack([]);
              setSelectedId(null);
              onLayerChange?.(item.label);
            }}
          >
            {item.label}
          </button>
        ))}
      </div>

      <div className="atlas-find" role="search">
        <label htmlFor="atlas-find-input">Find entity</label>
        <input
          id="atlas-find-input"
          type="search"
          value={query}
          placeholder="Name, kind or stable ID"
          aria-describedby="atlas-find-scope"
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            // Esc clears the query first; with nothing typed it keeps
            // backing out of focus like everywhere else in the card.
            if (event.key === 'Escape' && query) {
              event.preventDefault();
              event.stopPropagation();
              setQuery('');
            }
          }}
        />
        {query && (
          <button type="button" onClick={() => setQuery('')}>
            Clear
          </button>
        )}
        <span id="atlas-find-scope" className="muted">
          Searches all {withoutPlaceholders.nodes.length} entities in the loaded Atlas — every layer,
          focus level and collapsed group; boundary placeholders are not listed.
        </span>
      </div>

      <nav className="atlas-focus-path" aria-label="Focus path">
        <button
          type="button"
          className={focusStack.length === 0 ? 'active' : ''}
          aria-current={focusStack.length === 0 ? 'true' : undefined}
          onClick={() => backOut(0)}
        >
          Full graph
        </button>
        {focusStack.map((id, index) => (
          <button
            key={`${id}:${index}`}
            type="button"
            className={index === focusStack.length - 1 ? 'active' : ''}
            aria-current={index === focusStack.length - 1 ? 'true' : undefined}
            onClick={() => backOut(index + 1)}
          >
            ▸ {namesById.get(id) ?? id}
          </button>
        ))}
        {selectedId && selectedId !== currentRoot && visible.nodes.some((n) => n.id === selectedId) && (
          <button type="button" className="atlas-focus-cta" onClick={() => focusOn(selectedId)}>
            Focus on {namesById.get(selectedId) ?? selectedId} (Enter)
          </button>
        )}
        {focusStack.length > 0 && (
          <button type="button" onClick={() => backOut(focusStack.length - 1)}>
            Back out (Esc)
          </button>
        )}
        <span aria-live="polite" className="muted atlas-focus-status">
          {focusStatus}
        </span>
      </nav>

      <div className="atlas-legend" aria-label="Confidence legend">
        <HelpTip topic="tiers" />
        {TIERS.map((tier) => (
          <span key={tier.id} className={`atlas-legend-${tier.id.toLowerCase()}`}>
            {tier.label}
          </span>
        ))}
        <strong role="status">
          {visible.nodes.length} nodes · {visible.edges.length} edges
          {!scene.autoExpanded &&
            ` · ${scene.collapsed.length + foldedClusterCount(scene)} of ${scene.clusters.length} clusters collapsed`}
          {scene.collapsedGroups.length > 0 && ` · ${scene.collapsedGroups.length} groups`}
        </strong>
        {!scene.autoExpanded && (
          <span className="atlas-cluster-controls">
            <button
              type="button"
              onClick={() =>
                // Opens the clusters on screen; groups stay a drill-in.
                setExpanded((open) => new Set([...open, ...scene.collapsed.map((cluster) => cluster.id)]))
              }
            >
              Expand all clusters
            </button>
            <button type="button" onClick={() => setExpanded(new Set())}>
              Collapse clusters
            </button>
          </span>
        )}
      </div>

      <div className="atlas-surface">
        {visible.nodes.length === 0 ? (
          <p className="muted atlas-empty">
            {layer === 'infra'
              ? 'No infrastructure recovered. Add infrastructure-as-code (Terraform, Pulumi), deployment config or a plugin for this stack to show what the code runs on and talks to.'
              : 'No graph facts in this layer.'}
          </p>
        ) : (
          <div
            ref={containerRef}
            className="atlas-cytoscape"
            data-testid="atlas-canvas"
            tabIndex={0}
            aria-label={`${LAYERS.find((item) => item.id === layer)?.label ?? layer} graph canvas`}
          />
        )}
      </div>

      {searching && (
        <div className="atlas-entity-index atlas-find-results" aria-label="Find results">
          <span className="muted" aria-live="polite">
            {matches.length === 0
              ? `No entities match “${query.trim()}”`
              : `${matches.length} ${matches.length === 1 ? 'match' : 'matches'}`}
          </span>
          {matches.length > 0 && (
            <ul>
              {matches.slice(0, SEARCH_RESULT_LIMIT).map((node) => (
                <li key={node.id}>
                  <button
                    type="button"
                    aria-current={node.id === selectedId ? 'true' : undefined}
                    title={node.id}
                    onClick={() => reveal(node)}
                  >
                    <span>{displayName(node)}</span>
                    <span className="muted">
                      {node.label} · {node.id}
                    </span>
                  </button>
                </li>
              ))}
              {matches.length > SEARCH_RESULT_LIMIT && (
                <li className="muted">
                  +{matches.length - SEARCH_RESULT_LIMIT} more — refine the query
                </li>
              )}
            </ul>
          )}
        </div>
      )}

      {!searching && scene.openGroups.length > 0 && (
        <div className="atlas-entity-index" aria-label="Open groups">
          <span className="muted">Open groups — click to fold back</span>
          <div>
            {scene.openGroups.map((group) => (
              <button
                key={group.id}
                type="button"
                onClick={() =>
                  setExpanded((open) => {
                    // Folding a group also folds everything opened under it.
                    const inside = new Set([group.id, ...descendantIds(group)]);
                    return new Set([...open].filter((id) => !inside.has(id)));
                  })
                }
              >
                ▾ {BAND_LABELS[group.band]} · {displayClusterKey(group.key, repoLabel)}
              </button>
            ))}
          </div>
        </div>
      )}

      {!searching && scene.collapsedGroups.length > 0 && (
        <div className="atlas-entity-index" aria-label="Collapsed groups">
          <span className="muted">Collapsed groups — click to open</span>
          <div>
            {scene.collapsedGroups.map((group) => (
              <button
                key={group.id}
                type="button"
                onClick={() => setExpanded((open) => new Set([...open, group.id]))}
              >
                ▸ {BAND_LABELS[group.band]} · {displayClusterKey(group.key, repoLabel)} · {group.clusters.length} clusters
              </button>
            ))}
          </div>
        </div>
      )}

      {!searching && scene.collapsed.length > 0 && (
        <div className="atlas-entity-index" aria-label="Collapsed clusters">
          <span className="muted">Collapsed clusters — click to expand</span>
          <div>
            {scene.collapsed.map((cluster) => (
              <button
                key={cluster.id}
                type="button"
                onClick={() => setExpanded((open) => new Set([...open, cluster.id]))}
              >
                ▸ {BAND_LABELS[cluster.band]} · {displayClusterKey(cluster.key, repoLabel)} · {cluster.members.length}
              </button>
            ))}
          </div>
        </div>
      )}

      {!searching && visible.nodes.length > 0 && (
        <div className="atlas-entity-index">
          <span className="muted">Visible entities</span>
          <div>
            {visible.nodes.slice(0, 24).map((node) => (
              <button
                key={node.id}
                type="button"
                onClick={() => {
                  setSelectedId(node.id);
                  onSelect(node);
                  // Hand the keyboard to the canvas so the announced
                  // "press Enter to focus" path works immediately —
                  // Enter on the button would only re-select (#190 review).
                  containerRef.current?.focus();
                }}
              >
                {displayName(node)}
              </button>
            ))}
            {visible.nodes.length > 24 && <span className="muted">+{visible.nodes.length - 24} more on canvas</span>}
          </div>
        </div>
      )}

      {!searching && visible.edges.length > 0 && onSelectEdge && (
        <div className="atlas-entity-index" aria-label="Visible relations">
          <span className="muted">Visible relations</span>
          <div>
            {visible.edges.slice(0, 24).map((edge) => (
              <button
                key={`${edge.src} ${edge.label} ${edge.dst}`}
                type="button"
                className="atlas-edge-chip"
                aria-label={`${tierCode(edge.props)} ${edge.label}: ${edge.src} to ${edge.dst}`}
                title={`${edge.src} → ${edge.dst}`}
                onClick={() => onSelectEdge(edge)}
              >
                {tierCode(edge.props)} {edge.label}
              </button>
            ))}
            {visible.edges.length > 24 && (
              <span className="muted">+{visible.edges.length - 24} more on canvas</span>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
