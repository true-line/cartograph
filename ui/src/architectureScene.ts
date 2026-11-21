import type {
  ArchitectureElement,
  ArchitectureRelation,
  ArchitectureView,
  Tier,
} from './store';

/**
 * The C4 scenes of the Architecture view (#2049 slice 2): system context,
 * containers, and the components of one container, each laid out from the
 * model alone so the same model always gives the same picture.
 *
 * Rows: people on top, the system (or the container) boundary in the
 * middle, outside systems and stores below. Within a row, elements keep the
 * model's order, which is deterministic.
 */

export type ArchitectureLevel = 'context' | 'containers' | 'components';

export interface SceneNode {
  id: string;
  /** The element it draws; the system box at level 1 has none. */
  element: ArchitectureElement | null;
  kind: ArchitectureElement['kind'] | 'system' | 'boundary';
  name: string;
  technology: string;
  description: string;
  confidence: Tier;
  parent?: string;
  x: number;
  y: number;
  /** A container with components the user can open. */
  drillable: boolean;
}

export interface SceneEdge {
  id: string;
  from: string;
  to: string;
  label: string;
  technology: string;
  confidence: Tier;
  /** The relations it draws: one, or several folded into the system box. */
  relations: ArchitectureRelation[];
}

export interface Scene {
  level: ArchitectureLevel;
  nodes: SceneNode[];
  edges: SceneEdge[];
}

export const SYSTEM_ID = 'sys';
const COLUMN = 280;
const ROW = 190;
const PER_ROW = 5;

const RANK: Record<Tier, number> = { Confirmed: 3, InferredStrong: 2, InferredWeak: 1, Gap: 0 };

/** The lowest of several confidences; Confirmed for none. */
export function lowestTier(tiers: Tier[]): Tier {
  return tiers.reduce<Tier>((low, t) => (RANK[t] < RANK[low] ? t : low), 'Confirmed');
}

function place(ids: string[], top: number): Map<string, { x: number; y: number }> {
  const out = new Map<string, { x: number; y: number }>();
  ids.forEach((id, i) => {
    const row = Math.floor(i / PER_ROW);
    const inRow = Math.min(PER_ROW, ids.length - row * PER_ROW);
    const col = i % PER_ROW;
    out.set(id, { x: (col - (inRow - 1) / 2) * COLUMN, y: top + row * ROW });
  });
  return out;
}

function rows(count: number): number {
  return Math.max(1, Math.ceil(count / PER_ROW));
}

function node(
  element: ArchitectureElement,
  at: { x: number; y: number },
  extra: Partial<SceneNode> = {},
): SceneNode {
  return {
    id: element.id,
    element,
    kind: element.kind,
    name: element.name,
    technology: element.technology,
    description: element.description,
    confidence: element.confidence,
    x: at.x,
    y: at.y,
    drillable: false,
    ...extra,
  };
}

function edge(relation: ArchitectureRelation, index: number): SceneEdge {
  return {
    id: `r${index}:${relation.from}->${relation.to}`,
    from: relation.from,
    to: relation.to,
    label: relation.label,
    technology: relation.technology,
    confidence: relation.confidence,
    relations: [relation],
  };
}

/** The containers whose components can be drawn. */
export function containersWithComponents(view: ArchitectureView): ArchitectureElement[] {
  const withParts = new Set(
    view.components.filter((c) => c.elements.length > 0).map((c) => c.container),
  );
  return view.elements.filter((e) => withParts.has(e.id));
}

/** Level 1: people, the system as one box, and the systems outside it. */
export function contextScene(view: ArchitectureView): Scene {
  const people = view.elements.filter((e) => e.kind === 'person');
  const outside = view.elements.filter((e) => e.kind === 'external');
  const inside = new Set(
    view.elements.filter((e) => e.kind === 'container' || e.kind === 'store').map((e) => e.id),
  );
  const containers = view.elements.filter((e) => e.kind === 'container').length;
  const stores = view.elements.filter((e) => e.kind === 'store').length;
  const top = place(people.map((p) => p.id), 0);
  const systemY = people.length ? ROW * 1.3 : 0;
  const bottom = place(outside.map((e) => e.id), systemY + ROW * 1.3);
  const nodes: SceneNode[] = [
    ...people.map((p) => node(p, top.get(p.id)!)),
    {
      id: SYSTEM_ID,
      element: null,
      kind: 'system',
      name: view.system,
      technology: '',
      description: `${containers} container${containers === 1 ? '' : 's'} and ${stores} data store${stores === 1 ? '' : 's'}`,
      confidence: lowestTier(
        view.elements.filter((e) => inside.has(e.id)).map((e) => e.confidence),
      ),
      x: 0,
      y: systemY,
      drillable: containers > 0,
    },
    ...outside.map((e) => node(e, bottom.get(e.id)!)),
  ];
  // Relations that cross the boundary, folded onto the system box: one
  // arrow per pair of ends, with every relation under it.
  const end = (id: string) => (inside.has(id) ? SYSTEM_ID : id);
  const folded = new Map<string, ArchitectureRelation[]>();
  for (const r of view.relations) {
    const from = end(r.from);
    const to = end(r.to);
    if (from === to) continue;
    const key = `${from}\u0000${to}`;
    folded.set(key, [...(folded.get(key) ?? []), r]);
  }
  const edges = [...folded.entries()].map(([key, relations], i) => {
    const [from, to] = key.split('\u0000');
    const verbs = [...new Set(relations.map((r) => r.label.split(' ')[0]))];
    const technology = [...new Set(relations.map((r) => r.technology).filter(Boolean))];
    return {
      id: `c${i}:${from}->${to}`,
      from,
      to,
      label: verbs.join(', '),
      technology: technology.join(', '),
      confidence: lowestTier(relations.map((r) => r.confidence)),
      relations,
    };
  });
  return { level: 'context', nodes, edges };
}

/** Level 2: people, the containers and stores inside the system boundary,
 *  and the systems outside it. */
export function containersScene(view: ArchitectureView): Scene {
  const people = view.elements.filter((e) => e.kind === 'person');
  const containers = view.elements.filter((e) => e.kind === 'container');
  const stores = view.elements.filter((e) => e.kind === 'store');
  const outside = view.elements.filter((e) => e.kind === 'external');
  const drillable = new Set(containersWithComponents(view).map((e) => e.id));
  const top = place(people.map((p) => p.id), 0);
  const containerTop = people.length ? ROW * 1.4 : 0;
  const middle = place(containers.map((c) => c.id), containerTop);
  const storeTop = containerTop + rows(containers.length) * ROW;
  const low = place(stores.map((s) => s.id), storeTop);
  const outsideTop = storeTop + (stores.length ? rows(stores.length) * ROW : 0) + ROW * 0.4;
  const bottom = place(outside.map((e) => e.id), outsideTop);
  const nodes: SceneNode[] = [
    ...people.map((p) => node(p, top.get(p.id)!)),
    {
      id: SYSTEM_ID,
      element: null,
      kind: 'boundary',
      name: `${view.system} [Software System]`,
      technology: '',
      description: '',
      confidence: 'Confirmed',
      x: 0,
      y: 0,
      drillable: false,
    },
    ...containers.map((c) =>
      node(c, middle.get(c.id)!, { parent: SYSTEM_ID, drillable: drillable.has(c.id) }),
    ),
    ...stores.map((s) => node(s, low.get(s.id)!, { parent: SYSTEM_ID })),
    ...outside.map((e) => node(e, bottom.get(e.id)!)),
  ];
  return { level: 'containers', nodes, edges: view.relations.map(edge) };
}

/** Level 3: one container's components inside its boundary, with what
 *  calls in above and what they call below. */
export function componentsScene(view: ArchitectureView, containerId: string): Scene | null {
  const container = view.elements.find((e) => e.id === containerId);
  const parts = view.components.find((c) => c.container === containerId);
  if (!container || !parts || parts.elements.length === 0) return null;
  const own = new Set(parts.elements.map((e) => e.id));
  const byId = new Map(view.elements.map((e) => [e.id, e]));
  const callers: string[] = [];
  const callees: string[] = [];
  for (const r of parts.relations) {
    if (!own.has(r.from) && byId.has(r.from) && !callers.includes(r.from)) callers.push(r.from);
    if (!own.has(r.to) && byId.has(r.to) && !callees.includes(r.to) && !callers.includes(r.to)) {
      callees.push(r.to);
    }
  }
  const top = place(callers, 0);
  const inTop = callers.length ? ROW * 1.4 : 0;
  const middle = place(parts.elements.map((e) => e.id), inTop);
  const bottom = place(callees, inTop + rows(parts.elements.length) * ROW + ROW * 0.4);
  const boundary = `${containerId}:boundary`;
  const nodes: SceneNode[] = [
    ...callers.map((id) => node(byId.get(id)!, top.get(id)!)),
    {
      id: boundary,
      element: container,
      kind: 'boundary',
      name: `${container.name} [Container]`,
      technology: container.technology,
      description: container.description,
      confidence: container.confidence,
      x: 0,
      y: 0,
      drillable: false,
    },
    ...parts.elements.map((e) => node(e, middle.get(e.id)!, { parent: boundary })),
    ...callees.map((id) => node(byId.get(id)!, bottom.get(id)!)),
  ];
  return { level: 'components', nodes, edges: parts.relations.map(edge) };
}
