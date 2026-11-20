import cytoscape from 'cytoscape';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ArchitectureFact, ArchitectureView, SpecArtifact, Tier } from '../store';
import {
  type ArchitectureLevel,
  type Scene,
  type SceneEdge,
  type SceneNode,
  componentsScene,
  containersScene,
  containersWithComponents,
  contextScene,
} from '../architectureScene';
import { TIER_LABELS, TierBadge } from './TierBadge';
import { shortDiagramContentId } from './SpecDiagramView';

/**
 * The Architecture view (#2049 slice 2): C4 levels 1 to 3 of the recovered
 * system, drawn from the model `architecture.md` was compiled from. Level 1
 * opens; a click on the system shows its containers, a click on a
 * container with components opens them, and every box and arrow shows the
 * facts it rests on with their tier and source span.
 */

/** C4 notation colours (the Structurizr palette `architecture.md` uses). */
const C4_STYLE: cytoscape.StylesheetStyle[] = [
  {
    selector: 'node',
    style: {
      shape: 'round-rectangle',
      width: 220,
      height: 110,
      label: 'data(label)',
      'text-wrap': 'wrap',
      'text-max-width': '200px',
      'text-valign': 'center',
      'text-halign': 'center',
      'font-size': 11,
      'line-height': 1.25,
      color: '#ffffff',
      'background-color': '#438dd5',
      'border-width': 1,
      'border-color': '#2e6295',
    },
  },
  { selector: '.c4-person', style: { shape: 'round-rectangle', 'background-color': '#08427b', 'border-color': '#052e56' } },
  { selector: '.c4-system', style: { 'background-color': '#1168bd', 'border-color': '#0b4884', width: 260, height: 120 } },
  { selector: '.c4-store', style: { shape: 'barrel' } },
  { selector: '.c4-external', style: { 'background-color': '#8c8c8c', 'border-color': '#6b6b6b' } },
  { selector: '.c4-component', style: { 'background-color': '#85bbf0', 'border-color': '#5d82a8', color: '#000000' } },
  {
    selector: '.c4-gap',
    style: {
      'background-color': '#ffffff',
      color: '#222222',
      'border-color': '#c0392b',
      'border-width': 2,
      'border-style': 'dashed',
    },
  },
  // R-INT-2: inferred boxes and arrows are never drawn like confirmed ones.
  { selector: 'node.c4-strong', style: { 'border-style': 'dotted', 'border-width': 3 } },
  { selector: 'node.c4-weak', style: { 'border-style': 'dashed', 'border-width': 3, 'background-opacity': 0.7 } },
  { selector: '.c4-drillable', style: { 'border-color': '#f5c518' } },
  { selector: '.c4-drillable.c4-confirmed', style: { 'border-width': 3 } },
  {
    selector: ':parent',
    style: {
      shape: 'round-rectangle',
      'background-opacity': 0,
      'border-style': 'dashed',
      'border-width': 1.5,
      'border-color': '#8b919f',
      label: 'data(label)',
      'text-valign': 'top',
      'text-halign': 'center',
      'text-margin-y': -6,
      color: '#8b919f',
      'font-size': 12,
      'font-weight': 'bold',
      padding: '28px',
    },
  },
  {
    selector: 'edge',
    style: {
      width: 1.5,
      'curve-style': 'bezier',
      'target-arrow-shape': 'triangle',
      'line-color': '#8b919f',
      'target-arrow-color': '#8b919f',
      label: 'data(label)',
      'font-size': 9,
      color: '#24292f',
      'text-wrap': 'wrap',
      'text-max-width': '140px',
      'text-background-color': '#ffffff',
      'text-background-opacity': 0.9,
      'text-background-padding': '2px',
    },
  },
  { selector: 'edge.c4-strong', style: { 'line-style': 'dotted' } },
  { selector: 'edge.c4-weak', style: { 'line-style': 'dashed' } },
  {
    selector: 'edge.c4-gap',
    style: { 'line-style': 'dashed', 'line-color': '#c0392b', 'target-arrow-color': '#c0392b' },
  },
  // A busy level hides arrow labels until a box is selected, and a
  // selection brings its own arrows and neighbours forward.
  { selector: 'edge.c4-quiet', style: { 'text-opacity': 0, opacity: 0.45 } },
  { selector: 'edge.c4-focus', style: { 'text-opacity': 1, opacity: 1, width: 2.5, 'z-index': 10 } },
  { selector: 'edge.c4-faded', style: { opacity: 0.08, 'text-opacity': 0 } },
  { selector: 'node.c4-faded', style: { opacity: 0.3 } },
  { selector: ':selected', style: { 'border-width': 4, 'border-color': '#f5c518', 'line-color': '#f5c518' } },
];

/** Past this many arrows a level hides their labels until a box is selected. */
const BUSY_EDGES = 16;

/** The smallest zoom at which a box's 11px text stays legible. */
const READABLE_ZOOM = 0.75;

/** Canvas height for a scene: tall enough to show it at a readable zoom,
 *  within the window. */
export function canvasHeight(scene: Scene | null): number {
  if (!scene || scene.nodes.length === 0) return 420;
  const ys = scene.nodes.map((n) => n.y);
  const span = Math.max(...ys) - Math.min(...ys) + 200;
  return Math.round(Math.min(900, Math.max(420, span * READABLE_ZOOM + 80)));
}

const KIND_LABEL: Record<SceneNode['kind'], string> = {
  person: 'Person',
  container: 'Container',
  store: 'Data store',
  external: 'External system',
  component: 'Component',
  system: 'Software system',
  boundary: 'Boundary',
};

function nodeLabel(n: SceneNode): string {
  const kind = n.technology ? `[${KIND_LABEL[n.kind]}: ${n.technology}]` : `[${KIND_LABEL[n.kind]}]`;
  if (n.kind === 'boundary') return n.name;
  const description = n.description.length > 90 ? `${n.description.slice(0, 87)}…` : n.description;
  const tier = n.confidence === 'Confirmed' ? '' : `· ${TIER_LABELS[n.confidence]} ·`;
  return [n.name, kind, description, tier].filter(Boolean).join('\n');
}

function edgeLabel(e: SceneEdge): string {
  const label = e.technology ? `${e.label}\n[${e.technology}]` : e.label;
  return e.confidence === 'Confirmed' ? label : `${label}\n(${TIER_LABELS[e.confidence]})`;
}

const TIER_CLASS: Record<Tier, string> = {
  Confirmed: 'c4-confirmed',
  InferredStrong: 'c4-strong',
  InferredWeak: 'c4-weak',
  Gap: 'c4-gap',
};

function evidence(fact: ArchitectureFact): string {
  const first = fact.provenance.evidence[0];
  if (!first) return 'no source span';
  return `${first.path} bytes ${first.byte_start}..${first.byte_end}`;
}

type Selected = { kind: 'node'; node: SceneNode } | { kind: 'edge'; edge: SceneEdge } | null;

export interface ArchitectureCanvasProps {
  view: ArchitectureView;
  artifact: Pick<SpecArtifact, 'id' | 'schema' | 'content_id'> | null;
}

export function ArchitectureCanvas({ view, artifact }: ArchitectureCanvasProps) {
  const [level, setLevel] = useState<ArchitectureLevel>('context');
  const drillable = useMemo(() => containersWithComponents(view), [view]);
  const [container, setContainer] = useState<string | null>(drillable[0]?.id ?? null);
  const [selected, setSelected] = useState<Selected>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<cytoscape.Core | null>(null);
  const identityMatches = Boolean(
    artifact &&
      view.artifact_id === artifact.id &&
      view.schema === artifact.schema &&
      view.content_id === artifact.content_id,
  );

  const scene: Scene | null = useMemo(() => {
    if (level === 'context') return contextScene(view);
    if (level === 'containers') return containersScene(view);
    return container ? componentsScene(view, container) : null;
  }, [view, level, container]);

  const open = useCallback((next: ArchitectureLevel, containerId?: string) => {
    setLevel(next);
    if (containerId) setContainer(containerId);
    setSelected(null);
  }, []);

  useEffect(() => {
    if (!containerRef.current || !scene || !identityMatches) return;
    const byId = new Map(scene.nodes.map((n) => [n.id, n]));
    const edgeById = new Map(scene.edges.map((e) => [e.id, e]));
    const busy = scene.edges.length > BUSY_EDGES;
    const elements: cytoscape.ElementDefinition[] = [
      ...scene.nodes.map((n) => ({
        data: { id: n.id, label: nodeLabel(n), ...(n.parent ? { parent: n.parent } : {}) },
        position: { x: n.x, y: n.y },
        classes: [
          `c4-${n.kind}`,
          n.kind === 'boundary' ? '' : TIER_CLASS[n.confidence],
          n.drillable ? 'c4-drillable' : '',
        ]
          .filter(Boolean)
          .join(' '),
      })),
      ...scene.edges
        .filter((e) => byId.has(e.from) && byId.has(e.to))
        .map((e) => ({
          data: { id: e.id, source: e.from, target: e.to, label: edgeLabel(e) },
          classes: [
            TIER_CLASS[e.confidence],
            busy ? 'c4-quiet' : '',
          ]
            .filter(Boolean)
            .join(' '),
        })),
    ];
    const cy = cytoscape({
      container: containerRef.current,
      elements,
      style: C4_STYLE,
      layout: { name: 'preset', fit: true, padding: 36 },
      maxZoom: 1.6,
      minZoom: 0.2,
      autoungrabify: true,
    });
    // A large system fits only at a zoom too small to read: keep the text
    // readable, start at the top and let the user pan.
    if (cy.zoom() < READABLE_ZOOM) {
      const box = cy.elements().boundingBox();
      cy.zoom(READABLE_ZOOM);
      cy.pan({
        x: (cy.width() - box.w * READABLE_ZOOM) / 2 - box.x1 * READABLE_ZOOM,
        y: 24 - box.y1 * READABLE_ZOOM,
      });
    }
    const clearFocus = () => cy.elements().removeClass('c4-focus c4-faded');
    cy.on('tap', 'node', (event) => {
      const found = byId.get(event.target.id());
      if (!found) return;
      setSelected({ kind: 'node', node: found });
      if (found.kind === 'boundary') return;
      clearFocus();
      const own = event.target.connectedEdges();
      const near = own.connectedNodes().union(own.connectedNodes().ancestors());
      own.addClass('c4-focus');
      cy.edges().difference(own).addClass('c4-faded');
      cy.nodes().difference(near).difference(event.target.ancestors()).addClass('c4-faded');
    });
    cy.on('tap', (event) => {
      if (event.target === cy) {
        clearFocus();
        setSelected(null);
      }
    });
    cy.on('dbltap', 'node', (event) => {
      const found = byId.get(event.target.id());
      if (!found?.drillable) return;
      if (found.kind === 'system') open('containers');
      else open('components', found.id);
    });
    cy.on('tap', 'edge', (event) => {
      const found = edgeById.get(event.target.id());
      if (found) setSelected({ kind: 'edge', edge: found });
    });
    cyRef.current = cy;
    return () => {
      cyRef.current = null;
      cy.destroy();
    };
  }, [scene, identityMatches, open]);

  if (!identityMatches) {
    return (
      <div className="spec-diagram-error error" role="alert" data-testid="architecture-identity-mismatch">
        Refused to render the architecture: view identity ({view.schema} ·{' '}
        {shortDiagramContentId(view.content_id)}) does not match the exported artifact.
      </div>
    );
  }

  const containerName = view.elements.find((e) => e.id === container)?.name;
  const nameOf = (id: string) =>
    id === 'sys' ? view.system : (scene?.nodes.find((n) => n.id === id)?.name ?? id);

  return (
    <div className="spec-diagram-view architecture-view" data-testid="architecture-view">
      <div className="spec-diagram-header">
        <div className="spec-diagram-meta">
          <strong className="spec-diagram-title">Architecture of {view.system}</strong>
          <p className="muted spec-diagram-identity" data-testid="architecture-identity">
            <span>{view.schema}</span> · <code title={view.content_id}>{shortDiagramContentId(view.content_id)}</code>
          </p>
        </div>
        <div className="architecture-levels" role="tablist" aria-label="C4 level">
          <button
            type="button"
            role="tab"
            aria-selected={level === 'context'}
            className={level === 'context' ? 'active' : ''}
            onClick={() => open('context')}
          >
            1 · System context
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={level === 'containers'}
            className={level === 'containers' ? 'active' : ''}
            onClick={() => open('containers')}
          >
            2 · Containers
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={level === 'components'}
            className={level === 'components' ? 'active' : ''}
            disabled={drillable.length === 0}
            onClick={() => open('components')}
          >
            3 · Components
          </button>
          {level === 'components' && drillable.length > 0 && (
            <select
              aria-label="Container"
              value={container ?? ''}
              onChange={(event) => open('components', event.target.value)}
            >
              {drillable.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      <p className="muted architecture-hint">
        {level === 'context' &&
          'Who uses the system and what it depends on. Double-click the system to see its containers.'}
        {level === 'containers' &&
          'The parts that run on their own, their stores and what they call. Select a box to bring its arrows forward; double-click a yellow-outlined container to open its components.'}
        {level === 'components' &&
          (containerName
            ? `The subsystem areas of ${containerName} and the calls between them. Areas are inferred (InferredWeak).`
            : '')}
      </p>
      {level === 'components' && drillable.length === 0 && (
        <p className="muted" data-testid="architecture-no-components">
          {view.areas_supplied
            ? 'No container has components in this export.'
            : view.mode === 'verified-only'
              ? 'Components are subsystem areas, which are InferredWeak; a verified-only export draws none.'
              : 'No subsystem areas were supplied with this export, so no components are drawn.'}
        </p>
      )}

      <div className="architecture-body">
        <div
          ref={containerRef}
          className="atlas-cytoscape spec-diagram-canvas architecture-canvas"
          style={{ height: canvasHeight(scene) }}
          data-testid="architecture-canvas"
          aria-label={`Architecture ${level} diagram`}
        />
        <aside className="spec-diagram-evidence architecture-evidence" aria-label="Evidence" data-testid="architecture-evidence">
          {!selected && <p className="muted">Select a box or an arrow to see the facts it rests on.</p>}
          {selected?.kind === 'node' && <NodeEvidence node={selected.node} onOpen={open} />}
          {selected?.kind === 'edge' && (
            <EdgeEvidence edge={selected.edge} from={nameOf(selected.edge.from)} to={nameOf(selected.edge.to)} />
          )}
        </aside>
      </div>

      <ul className="architecture-legend" aria-label="Confidence legend" data-testid="architecture-legend">
        <li><span className="legend-line solid" /> Confirmed</li>
        <li><span className="legend-line dotted" /> Inferred (strong): dotted, labelled</li>
        <li><span className="legend-line dashed" /> Inferred (weak): dashed, faded, labelled</li>
        <li><span className="legend-line gap" /> Gap: red dashed, not recovered</li>
      </ul>

      {scene && (
        <div className="atlas-entity-index" aria-label="Architecture elements">
          <span className="muted">Elements (select to inspect evidence)</span>
          <div>
            {scene.nodes
              .filter((n) => n.kind !== 'boundary')
              .map((n) => (
                <button
                  key={n.id}
                  type="button"
                  className={`diagram-node-chip${n.confidence === 'Gap' ? ' unresolved' : ''}`}
                  onClick={() => {
                    const shown = cyRef.current?.getElementById(n.id);
                    if (shown && shown.nonempty()) shown.emit('tap');
                    else setSelected({ kind: 'node', node: n });
                  }}
                >
                  {n.name}
                </button>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

function FactList({ facts, total }: { facts: ArchitectureFact[]; total: number }) {
  if (total === 0) return <p className="muted">No fact recovered: shown as a Gap.</p>;
  return (
    <>
      <ul className="architecture-facts">
        {facts.map((fact) => (
          <li key={fact.id}>
            <span className="architecture-fact-kind">{fact.kind}</span> <strong>{fact.label}</strong>{' '}
            <TierBadge tier={fact.provenance.confidence_tier} provenance={fact.provenance} />
            <div className="muted architecture-fact-source">
              <code>{evidence(fact)}</code>
            </div>
          </li>
        ))}
      </ul>
      {total > facts.length && (
        <p className="muted">
          {total - facts.length} more fact{total - facts.length === 1 ? '' : 's'} in{' '}
          <code>architecture.md</code>.
        </p>
      )}
    </>
  );
}

function NodeEvidence({
  node,
  onOpen,
}: {
  node: SceneNode;
  onOpen: (level: ArchitectureLevel, containerId?: string) => void;
}) {
  return (
    <div data-testid="architecture-node-evidence">
      <h3>{node.name}</h3>
      <p className="muted">
        {KIND_LABEL[node.kind]}
        {node.technology ? ` · ${node.technology}` : ''}
      </p>
      {node.description && <p>{node.description}</p>}
      <p>
        <TierBadge tier={node.confidence} />
      </p>
      {node.drillable && (
        <button
          type="button"
          onClick={() => (node.kind === 'system' ? onOpen('containers') : onOpen('components', node.id))}
        >
          {node.kind === 'system' ? 'Show containers' : 'Show components'}
        </button>
      )}
      {node.element && <FactList facts={node.element.facts} total={node.element.facts_total} />}
    </div>
  );
}

function EdgeEvidence({ edge, from, to }: { edge: SceneEdge; from: string; to: string }) {
  return (
    <div data-testid="architecture-edge-evidence">
      <h3>
        {from} → {to}
      </h3>
      <p>
        {edge.label}
        {edge.technology ? ` · ${edge.technology}` : ''} <TierBadge tier={edge.confidence} />
      </p>
      {edge.relations.map((relation, i) => (
        <div key={`${relation.from}-${relation.to}-${i}`}>
          {edge.relations.length > 1 && (
            <p className="muted">
              {relation.label}
              {relation.technology ? ` · ${relation.technology}` : ''}
            </p>
          )}
          <FactList facts={relation.facts} total={relation.facts_total} />
        </div>
      ))}
    </div>
  );
}
