import cytoscape from 'cytoscape';
import { useEffect, useMemo, useRef, useState } from 'react';
import type {
  DeploymentRole,
  GraphEdge,
  GraphNode,
  Provenance,
  SpecArtifact,
  SpecDiagramView,
  Tier,
} from '../store';
import { CY_STYLE, nodeShapeClass } from './AtlasCanvas';
import { TierBadge, methodLabel } from './TierBadge';

export const DEFAULT_DIAGRAM_PAGE_SIZE = 24;

export const STRUCTURED_VIEW_UNAVAILABLE_NOTICE =
  'Structured view unavailable from the core; showing Mermaid text.';

const DOTTED_EDGE_LABELS = new Set(['DEPENDS_ON', 'REFERENCES', 'MAPS_TO']);

const DIAGRAM_STYLE: cytoscape.StylesheetStyle[] = [
  ...CY_STYLE,
  {
    selector: '.diagram-unknown',
    style: {
      shape: 'ellipse',
      'background-color': '#2a2a2a',
      'border-color': '#8b919f',
      'border-style': 'solid',
    },
  },
  {
    selector: '.diagram-deployed',
    style: {
      'border-width': 3,
      'border-style': 'double',
      'border-color': '#27c93f',
    },
  },
  {
    selector: '.diagram-reference',
    style: {
      'line-style': 'dotted',
    },
  },
];

export function shortDiagramContentId(contentId: string): string {
  const [scheme, digest] = contentId.split(':', 2);
  return digest ? `${scheme}:${digest.slice(0, 12)}` : contentId;
}

export function verifyDiagramViewIdentity(
  view: SpecDiagramView,
  artifact?: Pick<SpecArtifact, 'id' | 'schema' | 'content_id'> | null,
): boolean {
  if (!artifact) return false;
  return (
    Boolean(view.schema && view.content_id) &&
    view.artifact_id === artifact.id &&
    view.schema === artifact.schema &&
    view.content_id === artifact.content_id
  );
}

export function participantKindFromId(id: string, isGap = false): string {
  if (isGap || id.startsWith('gap:')) return 'Gap';
  const prefix = id.split(':', 1)[0];
  switch (prefix) {
    case 'ep':
      return 'Endpoint';
    case 'sym':
      return 'Symbol';
    case 'chan':
    case 'channel':
      return 'Channel';
    case 'res':
      return 'Resource';
    case 'component':
      return 'Component';
    case 'screen':
      return 'Screen';
    case 'extctx':
      return 'ExtensionContext';
    case 'extcmd':
      return 'ExtensionCommand';
    case 'file':
      return 'File';
    case 'data':
      return 'DataEntity';
    default:
      return 'Unknown';
  }
}

export function diagramNodeShapeClass(
  node: GraphNode,
): 'atlas-gap' | 'kind-channel' | 'kind-box' | 'diagram-unknown' {
  if (isGapNodeFact(node)) return 'atlas-gap';
  if (node.label === 'Unknown') return 'diagram-unknown';
  return nodeShapeClass(node);
}

export function diagramTruncationNotice(view: SpecDiagramView): string | null {
  if (!view.truncated) return null;
  const flowClause =
    view.flows_total > (view.flows?.length ?? 0)
      ? ` Showing ${view.flows?.length ?? 0} of ${view.flows_total} flows.`
      : '';
  return (
    `Showing ${view.nodes.length} of ${view.nodes_total} nodes, ` +
    `${view.edges.length} of ${view.edges_total} edges.${flowClause} ` +
    'The full artifact text is complete; the diagram is a bounded view.'
  );
}

export function nodeConfidence(node: GraphNode): Tier {
  const tier = node.props.prov?.confidence_tier;
  if (node.label === 'Gap' || node.props.placeholder === true) return 'Gap';
  return tier === 'Confirmed' ||
    tier === 'InferredStrong' ||
    tier === 'InferredWeak' ||
    tier === 'Gap'
    ? tier
    : 'Gap';
}

export function edgeConfidence(edge: GraphEdge): Tier {
  const tier = edge.props.prov?.confidence_tier;
  if (typeof edge.props.gap_reason === 'string' && edge.props.gap_reason.length > 0) {
    return 'Gap';
  }
  return tier === 'Confirmed' ||
    tier === 'InferredStrong' ||
    tier === 'InferredWeak' ||
    tier === 'Gap'
    ? tier
    : 'Gap';
}

export function isGapNodeFact(node: GraphNode): boolean {
  return (
    nodeConfidence(node) === 'Gap' ||
    node.label === 'Gap' ||
    node.props.placeholder === true
  );
}

export function isGapEdgeFact(edge: GraphEdge): boolean {
  return (
    edgeConfidence(edge) === 'Gap' ||
    edge.label === 'UNRESOLVED' ||
    (typeof edge.props.gap_reason === 'string' && edge.props.gap_reason.length > 0)
  );
}

function producerCode(prov?: Provenance, isGap?: boolean): string {
  if (isGap || prov?.confidence_tier === 'Gap') return 'GAP';
  switch (prov?.tier) {
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

export function nodeDisplayName(node: GraphNode): string {
  const method = typeof node.props.method === 'string' ? node.props.method : null;
  const path = typeof node.props.path === 'string' ? node.props.path : null;
  if (method && path) return `${method} ${path}`;
  for (const key of ['logical_id', 'name', 'identity', 'title', 'route', 'path', 'type']) {
    const value = node.props[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  return node.id.replace(/^(res|chan|ep|sym|data|gap):/, '');
}

export function diagramNodeLabel(node: GraphNode, deployment?: DeploymentRole): string {
  const base = nodeDisplayName(node);
  if (isGapNodeFact(node)) {
    const cleaned = base.replace(/^GAP:\s*/i, '');
    return `GAP · ${cleaned} [UNRESOLVED]`;
  }
  if (deployment) {
    return `${base} [DEPLOYED · T1]`;
  }
  return `${producerCode(node.props.prov)} · ${base}`;
}

export function diagramEdgeLabel(edge: GraphEdge): string {
  const tier = edgeConfidence(edge);
  if (isGapEdgeFact(edge)) {
    const reason =
      typeof edge.props.gap_reason === 'string' && edge.props.gap_reason.length > 0
        ? edge.props.gap_reason
        : typeof edge.props.reason === 'string' && edge.props.reason.length > 0
          ? edge.props.reason
          : 'not statically resolved';
    return `GAP ${edge.label} [UNRESOLVED: ${reason}]`;
  }
  return `${producerCode(edge.props.prov)} ${edge.label} [${tier}]`;
}

function fallbackProvenance(identity: string, confidence: Tier = 'Confirmed'): Provenance {
  return {
    tier: 'Deterministic',
    confidence_tier: confidence,
    evidence: [],
    extractor_id: 'spec.view',
    content_hash: identity,
  };
}

export function diagramSubsetNotice(view: SpecDiagramView): string | null {
  if (!view.subset_of) return null;
  const missing =
    view.subset_of.missing_participants ??
    Math.max(0, view.nodes_total - view.nodes.length);
  const base = `Subset of ${view.artifact_id} view · ${shortDiagramContentId(view.content_id)}`;
  if (missing > 0) {
    return `${base} · ${missing} participant${missing === 1 ? '' : 's'} not in the host view (truncated)`;
  }
  return base;
}

/** Build a per-flow diagram view as a subset of the host `specViews.flows` view,
 *  keeping only that flow's hop endpoints (+ trigger) from the host view's nodes
 *  and reporting missing participants without synthesizing nodes (AC-0246). */
export function subsetFlowDiagramView(
  view: SpecDiagramView,
  flowTrigger: string,
): SpecDiagramView | null {
  const flow = view.flows?.find((candidate) => candidate.trigger === flowTrigger);
  if (!flow) return null;

  const participantIds = new Set<string>([flow.trigger]);
  for (const hop of flow.hops) {
    participantIds.add(hop.src);
    participantIds.add(hop.dst);
  }

  // Deployed resources the host joined to these participants (BACKS,
  // TRIGGERS, ROUTES) stay with the flow, mirroring the host projection.
  const hostById = new Map(view.nodes.map((node) => [node.id, node]));
  const infrastructureIds = new Set<string>();
  for (const edge of view.edges) {
    if (!participantIds.has(edge.src) && !participantIds.has(edge.dst)) continue;
    for (const id of [edge.src, edge.dst]) {
      if (!participantIds.has(id) && hostById.get(id)?.label === 'Resource') {
        infrastructureIds.add(id);
      }
    }
  }
  const nodes = view.nodes.filter(
    (node) => participantIds.has(node.id) || infrastructureIds.has(node.id),
  );
  const retainedNodeIds = new Set(nodes.map((node) => node.id));
  const missingParticipants = [...participantIds].filter(
    (id) => !retainedNodeIds.has(id),
  ).length;

  const edges = view.edges.filter(
    (edge) => retainedNodeIds.has(edge.src) && retainedNodeIds.has(edge.dst),
  );
  const retainedEdgeKeys = new Set(
    edges.map((edge) => `${edge.src}\u0000${edge.label}\u0000${edge.dst}`),
  );
  const expectedEdgeKeys = new Set(
    flow.hops.map((hop) => `${hop.src}\u0000${hop.label}\u0000${hop.dst}`),
  );
  const missingEdges = [...expectedEdgeKeys].filter(
    (key) => !retainedEdgeKeys.has(key),
  ).length;

  const deployments = (view.deployments ?? []).filter((role) =>
    retainedNodeIds.has(role.resource_id),
  );

  const nodes_total = nodes.length + missingParticipants;
  const edges_total = edges.length + missingEdges;
  const truncated = missingParticipants > 0 || missingEdges > 0;

  return {
    artifact_id: view.artifact_id,
    schema: view.schema,
    content_id: view.content_id,
    mode: view.mode,
    nodes,
    nodes_total,
    edges,
    edges_total,
    flows: [flow],
    flows_total: 1,
    ...(deployments.length > 0 ? { deployments } : {}),
    truncated,
    subset_of: {
      flow_trigger: flow.trigger,
      ...(missingParticipants > 0 ? { missing_participants: missingParticipants } : {}),
    },
  };
}

function deterministicPositions(
  nodes: GraphNode[],
  edges: GraphEdge[],
): Map<string, { x: number; y: number }> {
  const rankById = new Map<string, number>();
  for (const node of nodes) {
    const baseRank =
      node.label === 'Endpoint' || node.label === 'Screen'
        ? 0
        : node.label === 'Resource'
          ? 1
          : node.label === 'Channel'
            ? 2
            : node.label === 'DataEntity'
              ? 3
              : isGapNodeFact(node)
                ? 3
                : 1;
    rankById.set(node.id, baseRank);
  }
  for (let pass = 0; pass < 3; pass += 1) {
    for (const edge of edges) {
      const srcRank = rankById.get(edge.src);
      const dstRank = rankById.get(edge.dst);
      if (srcRank !== undefined && dstRank !== undefined && dstRank <= srcRank && edge.src !== edge.dst) {
        rankById.set(edge.dst, Math.min(5, srcRank + 1));
      }
    }
  }
  const byColumn = new Map<number, GraphNode[]>();
  for (const node of [...nodes].sort((a, b) => a.id.localeCompare(b.id))) {
    const col = rankById.get(node.id) ?? 1;
    const list = byColumn.get(col) ?? [];
    list.push(node);
    byColumn.set(col, list);
  }
  const positions = new Map<string, { x: number; y: number }>();
  const cols = [...byColumn.keys()].sort((a, b) => a - b);
  cols.forEach((col, colIndex) => {
    const members = byColumn.get(col) ?? [];
    members.forEach((node, rowIndex) => {
      positions.set(node.id, {
        x: 90 + colIndex * 180,
        y: 64 + rowIndex * 84,
      });
    });
  });
  return positions;
}

type SelectedDiagramFact =
  | { kind: 'node'; node: GraphNode; deployment?: DeploymentRole }
  | { kind: 'edge'; edge: GraphEdge };

export interface SpecDiagramCanvasProps {
  view: SpecDiagramView;
  /** Expected exported artifact to verify schema + content_id against (AC-0246). */
  artifact?: Pick<SpecArtifact, 'id' | 'schema' | 'content_id'> | null;
  title?: string;
  pageSize?: number;
  onSelectNode?: (node: GraphNode) => void;
  onSelectEdge?: (edge: GraphEdge) => void;
}

/** Local Cytoscape diagram renderer for compiled topology, deployment, flow, and
 *  data-model projections (US-0036, AC-0246, AC-0247). */
export function SpecDiagramCanvas({
  view,
  artifact,
  title,
  pageSize = DEFAULT_DIAGRAM_PAGE_SIZE,
  onSelectNode,
  onSelectEdge,
}: SpecDiagramCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const onSelectNodeRef = useRef(onSelectNode);
  const onSelectEdgeRef = useRef(onSelectEdge);
  const [page, setPage] = useState(0);
  const [expandedAll, setExpandedAll] = useState(false);
  const [selectedFact, setSelectedFact] = useState<SelectedDiagramFact | null>(null);

  useEffect(() => {
    onSelectNodeRef.current = onSelectNode;
    onSelectEdgeRef.current = onSelectEdge;
  });

  const identityMatches = verifyDiagramViewIdentity(view, artifact);
  const truncationNotice = diagramTruncationNotice(view);
  const subsetNotice = diagramSubsetNotice(view);
  const deploymentByResource = useMemo(
    () => new Map((view.deployments ?? []).map((item) => [item.resource_id, item])),
    [view.deployments],
  );

  const totalNodes = view.nodes.length;
  const isLarge = totalNodes > pageSize;
  const pageCount = Math.max(1, Math.ceil(totalNodes / pageSize));
  const safePage = Math.min(page, pageCount - 1);

  const visibleNodes = useMemo(() => {
    if (!isLarge || expandedAll) return view.nodes;
    const start = safePage * pageSize;
    return view.nodes.slice(start, start + pageSize);
  }, [view.nodes, isLarge, expandedAll, safePage, pageSize]);

  const visibleNodeIds = useMemo(
    () => new Set(visibleNodes.map((node) => node.id)),
    [visibleNodes],
  );

  const visibleEdges = useMemo(
    () =>
      view.edges.filter(
        (edge) => visibleNodeIds.has(edge.src) && visibleNodeIds.has(edge.dst),
      ),
    [view.edges, visibleNodeIds],
  );

  const hiddenNodeCount = totalNodes - visibleNodes.length;
  const hiddenEdgeCount = view.edges.length - visibleEdges.length;

  const elements = useMemo((): cytoscape.ElementDefinition[] => {
    const positions = deterministicPositions(visibleNodes, visibleEdges);
    const nodeElements: cytoscape.ElementDefinition[] = visibleNodes.map((node) => {
      const tier = nodeConfidence(node);
      const deployment = deploymentByResource.get(node.id);
      const shape = diagramNodeShapeClass(node);
      const deployedClass = deployment ? ' diagram-deployed' : '';
      return {
        data: {
          id: node.id,
          label: diagramNodeLabel(node, deployment),
          kind: node.label,
          tier,
        },
        position: positions.get(node.id) ?? { x: 80, y: 80 },
        classes: `tier-${tier.toLowerCase()} ${shape}${deployedClass}`,
      };
    });
    const edgeElements: cytoscape.ElementDefinition[] = visibleEdges.map((edge, index) => {
      const tier = edgeConfidence(edge);
      const refClass = DOTTED_EDGE_LABELS.has(edge.label) ? ' diagram-reference' : '';
      return {
        data: {
          id: `${edge.src}\u0000${edge.label}\u0000${edge.dst}\u0000${index}`,
          edgeKey: `${edge.src}\u0000${edge.label}\u0000${edge.dst}`,
          source: edge.src,
          target: edge.dst,
          label: diagramEdgeLabel(edge),
          tier,
        },
        classes: `tier-${tier.toLowerCase()}${refClass}`,
      };
    });
    return [...nodeElements, ...edgeElements];
  }, [visibleNodes, visibleEdges, deploymentByResource]);

  useEffect(() => {
    if (!containerRef.current || visibleNodes.length === 0 || !identityMatches) return;
    const nodeById = new Map(visibleNodes.map((node) => [node.id, node]));
    const edgeByKey = new Map(
      visibleEdges.map((edge) => [`${edge.src}\u0000${edge.label}\u0000${edge.dst}`, edge]),
    );
    const cy = cytoscape({
      container: containerRef.current,
      elements,
      style: DIAGRAM_STYLE,
      layout: { name: 'preset', fit: true, padding: 28 },
      // Fitting one or two nodes would otherwise zoom them to fill the canvas.
      maxZoom: 1.5,
      autoungrabify: true,
    });
    cy.on('tap', 'node', (event) => {
      const id = event.target.id();
      const found = nodeById.get(id);
      if (found) {
        setSelectedFact({
          kind: 'node',
          node: found,
          deployment: deploymentByResource.get(found.id),
        });
        onSelectNodeRef.current?.(found);
      }
    });
    cy.on('tap', 'edge', (event) => {
      const key = event.target.data('edgeKey') as string | undefined;
      const found = key ? edgeByKey.get(key) : undefined;
      if (found) {
        setSelectedFact({ kind: 'edge', edge: found });
        onSelectEdgeRef.current?.(found);
      }
    });
    return () => cy.destroy();
  }, [elements, visibleNodes, visibleEdges, identityMatches, deploymentByResource]);

  const handleSelectNode = (node: GraphNode) => {
    setSelectedFact({
      kind: 'node',
      node,
      deployment: deploymentByResource.get(node.id),
    });
    onSelectNode?.(node);
  };

  const handleSelectEdge = (edge: GraphEdge) => {
    setSelectedFact({ kind: 'edge', edge });
    onSelectEdge?.(edge);
  };

  if (!identityMatches) {
    return (
      <div
        className="spec-diagram-error error"
        role="alert"
        data-testid="diagram-identity-mismatch"
      >
        Refused to render diagram: view identity ({view.schema} · {shortDiagramContentId(view.content_id)})
        does not match exported artifact identity ({artifact?.schema ?? 'missing'} ·{' '}
        {artifact ? shortDiagramContentId(artifact.content_id) : 'missing'}).
      </div>
    );
  }

  return (
    <div className="spec-diagram-view" data-testid={`spec-diagram-${view.artifact_id}`}>
      <div className="spec-diagram-header">
        <div className="spec-diagram-meta">
          {title && <strong className="spec-diagram-title">{title}</strong>}
          <p
            className="muted spec-diagram-identity"
            data-testid="diagram-content-identity"
            data-schema={view.schema}
            data-content-id={view.content_id}
          >
            <span>{view.schema}</span> ·{' '}
            <code title={view.content_id}>{shortDiagramContentId(view.content_id)}</code>
          </p>
          {subsetNotice && (
            <p
              className="muted spec-diagram-subset-note"
              data-testid="diagram-subset-notice"
            >
              {subsetNotice}
            </p>
          )}
        </div>

        <div
          className="spec-diagram-scale"
          data-testid="diagram-scale-status"
          aria-label="Diagram scale status"
        >
          {isLarge && !expandedAll ? (
            <>
              <span>
                Showing {safePage * pageSize + 1}–
                {Math.min(totalNodes, (safePage + 1) * pageSize)} of{' '}
                {view.truncated ? `${totalNodes} (${view.nodes_total} total)` : totalNodes} nodes ·{' '}
                <strong>{hiddenNodeCount} hidden</strong>
                {hiddenEdgeCount > 0 ? ` (${hiddenEdgeCount} edges hidden)` : ''} · Page{' '}
                {safePage + 1} of {pageCount}
              </span>
              <span className="spec-diagram-page-controls">
                <button
                  type="button"
                  disabled={safePage === 0}
                  onClick={() => setPage((current) => Math.max(0, current - 1))}
                >
                  Previous page
                </button>
                <button
                  type="button"
                  disabled={safePage >= pageCount - 1}
                  onClick={() => setPage((current) => Math.min(pageCount - 1, current + 1))}
                >
                  Next page
                </button>
                <button type="button" onClick={() => setExpandedAll(true)}>
                  {view.truncated
                    ? `Expand bounded view (${totalNodes} of ${view.nodes_total} nodes)`
                    : `Expand all (${totalNodes} nodes)`}
                </button>
              </span>
            </>
          ) : isLarge && expandedAll ? (
            <>
              <span>
                {view.truncated
                  ? `Showing ${totalNodes} of ${view.nodes_total} nodes (bounded view) · ${view.edges.length} of ${view.edges_total} edges`
                  : `Showing all ${totalNodes} of ${totalNodes} nodes · 0 hidden · ${view.edges.length} edges`}
              </span>
              <button
                type="button"
                onClick={() => {
                  setExpandedAll(false);
                  setPage(0);
                }}
              >
                Collapse to pages ({pageSize} per page)
              </button>
            </>
          ) : (
            <span>
              {view.truncated
                ? `Showing ${visibleNodes.length} of ${view.nodes_total} nodes · ${visibleEdges.length} of ${view.edges_total} edges (bounded view)`
                : `${visibleNodes.length} nodes · ${visibleEdges.length} edges`}
            </span>
          )}
        </div>
      </div>

      {truncationNotice && (
        <p
          className="muted spec-diagram-truncated-note"
          data-testid="diagram-truncated-notice"
        >
          {truncationNotice}
        </p>
      )}

      {view.deployments && view.deployments.length > 0 && (
        <div
          className="spec-diagram-deployments"
          aria-label="Deployment observations"
          data-testid="diagram-deployments"
        >
          <span className="muted">Deployment roles (T1 observed):</span>
          <ul>
            {view.deployments.map((role) => (
              <li key={role.resource_id}>
                <code>{role.resource_id.replace(/^res:/, '')}</code> ·{' '}
                <span className="tier-badge tier-confirmed">DEPLOYED ({role.source})</span>
                {role.backed_channels && role.backed_channels.length > 0 && (
                  <span>
                    {' '}
                    → backs <code>{role.backed_channels.join(', ')}</code>
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {visibleNodes.length === 0 ? (
        <p className="muted spec-diagram-empty">No diagram facts in this projection.</p>
      ) : (
        <>
          <div
            ref={containerRef}
            className="atlas-cytoscape spec-diagram-canvas"
            data-testid="spec-diagram-canvas"
            aria-label={`${title ?? view.artifact_id} diagram canvas`}
          />

          <div className="atlas-entity-index" aria-label="Diagram nodes">
            <span className="muted">Diagram nodes (select to inspect evidence)</span>
            <div>
              {visibleNodes.map((node) => {
                const gap = isGapNodeFact(node);
                const unknown = node.label === 'Unknown';
                const deployment = deploymentByResource.get(node.id);
                const label = diagramNodeLabel(node, deployment);
                return (
                  <button
                    key={node.id}
                    type="button"
                    className={`diagram-node-chip${gap ? ' unresolved tier-gap' : ''}${
                      unknown ? ' kind-unknown' : ''
                    }${deployment ? ' deployed' : ''}`}
                    onClick={() => handleSelectNode(node)}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          </div>

          {visibleEdges.length > 0 && (
            <div className="atlas-entity-index" aria-label="Diagram edges and hops">
              <span className="muted">Diagram edges & hops (select to inspect evidence)</span>
              <div>
                {visibleEdges.map((edge, index) => {
                  const gap = isGapEdgeFact(edge);
                  const label = diagramEdgeLabel(edge);
                  return (
                    <button
                      key={`${edge.src}-${edge.label}-${edge.dst}-${index}`}
                      type="button"
                      className={`diagram-edge-chip${gap ? ' unresolved tier-gap' : ''}`}
                      onClick={() => handleSelectEdge(edge)}
                    >
                      {label}: {edge.src} → {edge.dst}
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </>
      )}

      {selectedFact && (
        <aside
          className="spec-diagram-evidence"
          data-testid="diagram-selected-evidence"
          aria-label="Selected diagram fact evidence"
        >
          {selectedFact.kind === 'node' ? (
            (() => {
              const { node, deployment } = selectedFact;
              const prov = node.props.prov ?? fallbackProvenance(node.id, nodeConfidence(node));
              const gap = isGapNodeFact(node);
              return (
                <div>
                  <div className="spec-assertion-heading">
                    <div>
                      <p className="eyebrow">
                        {gap ? `GAP · ${node.label}` : node.label}
                        {deployment ? ` · DEPLOYED (${deployment.source})` : ''}
                      </p>
                      <strong>{nodeDisplayName(node)}</strong>
                      <code>{node.id}</code>
                    </div>
                    <TierBadge tier={prov.confidence_tier} provenance={prov} />
                  </div>
                  {gap && (
                    <p className="flow-hop-gap-note">
                      Unresolved node —{' '}
                      {typeof node.props.reason === 'string' && node.props.reason.length > 0
                        ? node.props.reason
                        : 'placeholder or unresolved target'}
                    </p>
                  )}
                  <dl className="spec-provenance">
                    <div>
                      <dt>Tier</dt>
                      <dd>{prov.tier}</dd>
                    </div>
                    <div>
                      <dt>Confidence</dt>
                      <dd>{prov.confidence_tier}</dd>
                    </div>
                    <div>
                      <dt>Method</dt>
                      <dd>{methodLabel(prov.method)}</dd>
                    </div>
                    <div>
                      <dt>Extractor</dt>
                      <dd>
                        <code>{prov.extractor_id}</code>
                      </dd>
                    </div>
                    <div className="wide">
                      <dt>Evidence</dt>
                      <dd>
                        {prov.evidence.length === 0 ? (
                          <span>None recorded — treated as unresolved.</span>
                        ) : (
                          <ul>
                            {prov.evidence.map((ev, idx) => (
                              <li key={`${ev.repo}:${ev.path}:${ev.byte_start}:${idx}`}>
                                <code>
                                  {ev.repo}:{ev.path}
                                </code>{' '}
                                <span>
                                  bytes {ev.byte_start}..{ev.byte_end}
                                </span>{' '}
                                <span>@ {ev.commit_sha}</span>
                              </li>
                            ))}
                          </ul>
                        )}
                      </dd>
                    </div>
                  </dl>
                </div>
              );
            })()
          ) : (
            (() => {
              const { edge } = selectedFact;
              const prov =
                edge.props.prov ??
                fallbackProvenance(`${edge.src} ${edge.label} ${edge.dst}`, edgeConfidence(edge));
              const gap = isGapEdgeFact(edge);
              const reason =
                typeof edge.props.gap_reason === 'string'
                  ? edge.props.gap_reason
                  : typeof edge.props.reason === 'string'
                    ? edge.props.reason
                    : null;
              return (
                <div>
                  <div className="spec-assertion-heading">
                    <div>
                      <p className="eyebrow">{gap ? `GAP · ${edge.label}` : edge.label}</p>
                      <strong>
                        {edge.src} → {edge.dst}
                      </strong>
                    </div>
                    <TierBadge tier={prov.confidence_tier} provenance={prov} />
                  </div>
                  {gap && (
                    <p className="flow-hop-gap-note">
                      Unresolved hop — {reason ?? 'not statically resolvable'}
                    </p>
                  )}
                  <dl className="spec-provenance">
                    <div>
                      <dt>Tier</dt>
                      <dd>{prov.tier}</dd>
                    </div>
                    <div>
                      <dt>Confidence</dt>
                      <dd>{prov.confidence_tier}</dd>
                    </div>
                    <div>
                      <dt>Method</dt>
                      <dd>{methodLabel(prov.method)}</dd>
                    </div>
                    <div>
                      <dt>Extractor</dt>
                      <dd>
                        <code>{prov.extractor_id}</code>
                      </dd>
                    </div>
                    <div className="wide">
                      <dt>Evidence</dt>
                      <dd>
                        {prov.evidence.length === 0 ? (
                          <span>None recorded — treated as unresolved.</span>
                        ) : (
                          <ul>
                            {prov.evidence.map((ev, idx) => (
                              <li key={`${ev.repo}:${ev.path}:${ev.byte_start}:${idx}`}>
                                <code>
                                  {ev.repo}:{ev.path}
                                </code>{' '}
                                <span>
                                  bytes {ev.byte_start}..{ev.byte_end}
                                </span>{' '}
                                <span>@ {ev.commit_sha}</span>
                              </li>
                            ))}
                          </ul>
                        )}
                      </dd>
                    </div>
                  </dl>
                </div>
              );
            })()
          )}
        </aside>
      )}
    </div>
  );
}
