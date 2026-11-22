import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { assignBands, buildAtlasScene, clusterKeyFor } from '../atlasLayout';
import type { AtlasSnapshot, GraphNode, Provenance, Tier } from '../store';
import {
  AtlasCanvas,
  CY_STYLE,
  displayName,
  filterAtlasGraph,
  focusAtlasGraph,
  nodeShapeClass,
  UNNAMED_CONFIG_KEY,
} from './AtlasCanvas';

function prov(confidence_tier: Tier, path: string): Provenance {
  return {
    tier:
      confidence_tier === 'Confirmed'
        ? 'Deterministic'
        : confidence_tier === 'InferredStrong'
          ? 'Semantic'
          : confidence_tier === 'InferredWeak'
            ? 'Agentic'
            : 'Deterministic',
    confidence_tier,
    evidence: [
      {
        repo: 'local/shop',
        path,
        byte_start: 0,
        byte_end: 12,
        commit_sha: 'abc123',
      },
    ],
    extractor_id: 'atlas.story',
    content_hash: `${confidence_tier}-${path}`,
  };
}

const atlasNodes: GraphNode[] = [
  {
    id: 'res:local/shop@aws_sqs_queue.orders',
    label: 'Resource',
    props: { type: 'aws_sqs_queue', logical_id: 'orders', prov: prov('Confirmed', 'infra.tf') },
  },
  {
    id: 'chan:sqs-queue:orders',
    label: 'Channel',
    props: { identity: 'orders queue', prov: prov('InferredStrong', 'publisher.ts') },
  },
  {
    id: 'ep:local/shop@POST:/orders',
    label: 'Endpoint',
    props: { method: 'POST', path: '/orders', prov: prov('Confirmed', 'api.ts') },
  },
  {
    id: 'component:local/shop@Checkout',
    label: 'Component',
    props: { name: 'Checkout', prov: prov('InferredWeak', 'Checkout.tsx') },
  },
  {
    id: 'gap:local/shop@orders-target',
    label: 'Gap',
    props: { reason: 'unresolved call target', prov: prov('Gap', 'api.ts') },
  },
];

const atlasFixture: AtlasSnapshot = {
  nodes: atlasNodes,
  edges: [
    {
      src: atlasNodes[0].id,
      dst: atlasNodes[1].id,
      label: 'BACKS',
      props: { prov: prov('Confirmed', 'state.json') },
    },
    {
      src: atlasNodes[2].id,
      dst: atlasNodes[1].id,
      label: 'PUBLISHES',
      props: { prov: prov('InferredStrong', 'publisher.ts') },
    },
    {
      src: atlasNodes[3].id,
      dst: atlasNodes[2].id,
      label: 'FETCHES',
      props: { prov: prov('InferredWeak', 'Checkout.tsx') },
    },
    {
      src: atlasNodes[2].id,
      dst: atlasNodes[4].id,
      label: 'CALLS',
      props: { prov: prov('Gap', 'api.ts') },
    },
  ],
};

const meta = {
  title: 'Atlas/AtlasCanvas',
  component: AtlasCanvas,
  args: { snapshot: atlasFixture, onSelect: fn(), onSelectEdge: fn(), onLayerChange: fn() },
} satisfies Meta<typeof AtlasCanvas>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LayerFilters: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges');

    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('1 nodes · 0 edges'));
    await expect(canvas.getByRole('button', { name: 'orders' })).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'orders queue' })).not.toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Events' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('3 nodes · 2 edges'));
    await expect(canvas.getByRole('button', { name: 'orders queue' })).toBeInTheDocument();
  },
};

// #723: rule exits and rule gaps joined by DEPENDS_ON are not infrastructure.
// A layer edge only pulls in a neighbor of a fact that natively belongs there.
const ruleGapFixture: AtlasSnapshot = {
  nodes: [
    atlasNodes[0],
    {
      id: 'rule:local/shop@Checkout#exit-1',
      label: 'BusinessRule',
      props: { name: 'guarded exit', prov: prov('InferredStrong', 'Checkout.tsx') },
    },
    {
      id: 'gap:rule:local/shop@Checkout#unresolved_call',
      label: 'Gap',
      props: { reason: 'unresolved_call', prov: prov('Gap', 'Checkout.tsx') },
    },
    {
      id: 'gap:local/shop@orders-dlq',
      label: 'Gap',
      props: { reason: 'unresolved dead-letter target', prov: prov('Gap', 'infra.tf') },
    },
  ],
  edges: [
    {
      src: 'rule:local/shop@Checkout#exit-1',
      dst: 'gap:rule:local/shop@Checkout#unresolved_call',
      label: 'DEPENDS_ON',
      props: { prov: prov('Gap', 'Checkout.tsx') },
    },
    {
      src: atlasNodes[0].id,
      dst: 'gap:local/shop@orders-dlq',
      label: 'DEPENDS_ON',
      props: { prov: prov('Gap', 'infra.tf') },
    },
  ],
};

export const InfrastructureExcludesRuleGaps: Story = {
  args: { snapshot: ruleGapFixture },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    // The resource and the gap on its own dependency stay; the rule exit and
    // its rule gap do not.
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));
  },
};

// #734: observed infrastructure. A Resource the code is seen using brings
// its calling symbol into Infrastructure through USES; a Gap on a computed
// identity has no native endpoint there, so it stays out.
const observedFixture: AtlasSnapshot = {
  nodes: [
    {
      id: 'res:local/app@observed:indexeddb:inventory',
      label: 'Resource',
      props: { type: 'indexeddb', kind: 'browser-storage', observed: true, prov: prov('Confirmed', 'db.ts') },
    },
    { id: 'sym:local/app@db.ts#openDb', label: 'Symbol', props: { name: 'openDb', prov: prov('Confirmed', 'db.ts') } },
    { id: 'sym:local/app@db.ts#connect', label: 'Symbol', props: { name: 'connect', prov: prov('Confirmed', 'db.ts') } },
    {
      id: 'gap:observed:local/app@db.ts@90',
      label: 'Gap',
      props: { reason: 'runtime-computed mongodb identity', prov: prov('Gap', 'db.ts') },
    },
  ],
  edges: [
    {
      src: 'sym:local/app@db.ts#openDb',
      dst: 'res:local/app@observed:indexeddb:inventory',
      label: 'USES',
      props: { prov: prov('Confirmed', 'db.ts') },
    },
    {
      src: 'sym:local/app@db.ts#connect',
      dst: 'gap:observed:local/app@db.ts@90',
      label: 'USES',
      props: { prov: prov('Gap', 'db.ts') },
    },
  ],
};

export const InfrastructureShowsObservedResourceUsers: Story = {
  args: { snapshot: observedFixture },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));
  },
};

const extensionFixture: AtlasSnapshot = {
  nodes: [
    { id: 'ext:local/addon@src', label: 'Extension', props: { name: 'addon', prov: prov('Confirmed', 'manifest.json') } },
    { id: 'perm:local/addon@storage', label: 'Permission', props: { name: 'storage', prov: prov('Confirmed', 'manifest.json') } },
    { id: 'cmd:local/addon@open', label: 'Command', props: { name: 'open', prov: prov('Confirmed', 'manifest.json') } },
    { id: 'ctx:local/addon@background', label: 'ExtensionContext', props: { name: 'background', prov: prov('Confirmed', 'manifest.json') } },
  ],
  edges: [
    { src: 'ext:local/addon@src', dst: 'perm:local/addon@storage', label: 'GRANTS', props: { prov: prov('Confirmed', 'manifest.json') } },
    { src: 'cmd:local/addon@open', dst: 'ctx:local/addon@background', label: 'HANDLES', props: { prov: prov('Confirmed', 'manifest.json') } },
  ],
};

export const ClientKeepsExtensionRelationships: Story = {
  args: { snapshot: extensionFixture },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Client code' }));
    // GRANTS and HANDLES are labelled cloud/server, but both of their
    // endpoints are client-native, so they stay with the Client layer.
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 2 edges'));
  },
};

export const InfrastructureEmptyNamesSources: Story = {
  args: { snapshot: { nodes: [atlasNodes[3]], edges: [] } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await expect(await canvas.findByText(/No infrastructure recovered/)).toBeInTheDocument();
    await expect(canvas.getByText(/Terraform, Pulumi/)).toBeInTheDocument();
  },
};

export const ConfidenceOverlay: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    for (const label of ['Confirmed', 'Inferred strong', 'Inferred weak', 'Gap']) {
      await expect(canvas.getByText(label)).toBeInTheDocument();
    }
    // #164: the legend explains confidence tiers in place.
    await expect(
      canvas.getByRole('button', { name: 'What is confidence tiers?' }),
    ).toBeInTheDocument();
    const toggle = canvas.getByRole('button', { name: 'Confidence overlay' });
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(toggle);
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  },
};

export const NodeSelection: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await expect(args.onSelect).toHaveBeenCalledWith(atlasNodes[2]);
  },
};

export const FocusModeRootsAndBacksOut: Story = {
  // AC-0086 (#160): Enter/CTA roots the selection to its ego graph, each
  // level stacks, Esc backs out exactly one level, ending at the full graph.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on POST \/orders/ }));
    // Ego graph: the endpoint plus its direct connections (channel, gap,
    // Checkout) — the SQS Resource is two hops away and disappears from
    // the projection and the entity index.
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    const path = canvas.getByRole('navigation', { name: 'Focus path' });
    await expect(within(path).getByText('▸ POST /orders')).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'orders' })).not.toBeInTheDocument();

    // A second level roots the channel within the first projection.
    await userEvent.click(canvas.getByRole('button', { name: 'orders queue' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on orders queue/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));

    // Esc pops one level at a time — never straight to the full graph.
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges'));
  },
};

export const FocusIsKeyboardOperableAndAnnounced: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await expect(
      canvas.getByText(/POST \/orders selected — press Enter to focus/),
    ).toBeInTheDocument();

    // Selection hands the keyboard to the canvas (#190 review): the next
    // Enter focuses immediately — no manual tabbing to the canvas needed.
    await expect(document.activeElement?.className).toContain('atlas-cytoscape');
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    await expect(
      canvas.getByText(/Focused on POST \/orders — 4 nodes at level 1\. Esc backs out/),
    ).toBeInTheDocument();

    // The breadcrumb also backs out without a keyboard.
    await userEvent.click(canvas.getByRole('button', { name: 'Full graph' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges'));

    // The projection is a pure function: same inputs, same ego graph,
    // regardless of input order; an unknown root changes nothing.
    const ego = focusAtlasGraph(atlasFixture, atlasNodes[2].id);
    const reversed = focusAtlasGraph(
      { nodes: [...atlasFixture.nodes].reverse(), edges: [...atlasFixture.edges].reverse() },
      atlasNodes[2].id,
    );
    await expect(new Set(reversed.nodes.map((n) => n.id))).toEqual(
      new Set(ego.nodes.map((n) => n.id)),
    );
    await expect(ego.nodes.map((n) => n.id).sort()).toEqual(
      [atlasNodes[1].id, atlasNodes[2].id, atlasNodes[3].id, atlasNodes[4].id].sort(),
    );
    await expect(focusAtlasGraph(atlasFixture, 'missing:node')).toEqual(atlasFixture);
  },
};

export const ShapeEncodesKindNeverColorAlone: Story = {
  // #106: octagon dashed red = Gap (no longer colliding with the gateway
  // diamond), diamond = channel/gateway, rectangle = everything else.
  play: async ({ canvasElement }) => {
    await expect(nodeShapeClass(atlasNodes[4])).toBe('atlas-gap');
    await expect(nodeShapeClass(atlasNodes[1])).toBe('kind-channel');
    await expect(
      nodeShapeClass({ id: 'gw:api', label: 'Gateway', props: {} }),
    ).toBe('kind-channel');
    await expect(nodeShapeClass(atlasNodes[2])).toBe('kind-box');
    // The canvas itself rendered (shape classes feed Cytoscape styles).
    await expect(
      within(canvasElement).getByTestId('atlas-canvas'),
    ).toBeInTheDocument();
  },
};

export const EdgeChipsOpenEdgeEvidence: Story = {
  // #106: every edge carries a mono tier+relation chip and is clickable —
  // the chip names the producing tier, GAP edges say so.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const relations = within(canvas.getByLabelText('Visible relations'));
    await expect(relations.getByText('T0 BACKS')).toBeInTheDocument();
    await expect(relations.getByText('T2 PUBLISHES')).toBeInTheDocument();
    await expect(relations.getByText('T3 FETCHES')).toBeInTheDocument();
    await expect(relations.getByText('GAP CALLS')).toBeInTheDocument();

    await userEvent.click(relations.getByText('GAP CALLS'));
    await expect(args.onSelectEdge).toHaveBeenCalledWith(atlasFixture.edges[3]);
  },
};

export const LayerDrivesScopeChip: Story = {
  // #106: the layer filter reports its label so the header scope chip can
  // read `Atlas · <layer>`.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Events' }));
    await expect(args.onLayerChange).toHaveBeenCalledWith('Events');
    await userEvent.click(canvas.getByRole('button', { name: 'All layers' }));
    await expect(args.onLayerChange).toHaveBeenCalledWith('All layers');
  },
};

export const BandedLayoutIsDeterministic: Story = {
  // #159/AC-0081: bands come from kind (Gaps inherit from a neighbor),
  // cluster keys are id-derived, and positions are identical for identical
  // snapshots regardless of input order — layout never jitters.
  play: async () => {
    const bands = assignBands(atlasFixture);
    await expect(bands.get(atlasNodes[0].id)).toBe('infra'); // Resource
    await expect(bands.get(atlasNodes[1].id)).toBe('events'); // Channel
    await expect(bands.get(atlasNodes[2].id)).toBe('server'); // Endpoint
    await expect(bands.get(atlasNodes[3].id)).toBe('client'); // Component
    // The Gap anchors to its calling Endpoint's band, never floats alone.
    await expect(bands.get(atlasNodes[4].id)).toBe('server');

    await expect(clusterKeyFor(atlasNodes[0])).toBe('local/shop · aws_sqs_queue');
    await expect(clusterKeyFor(atlasNodes[1])).toBe('sqs-queue');
    // Endpoint cluster keys are method + route segment, not the trailing-
    // colon id-parse artifact (`local/shop · GET:`) (AC-0081, #244).
    await expect(clusterKeyFor(atlasNodes[2])).toBe('local/shop · POST /orders');
    // Routed screen ids carry a leading slash — the first real segment
    // names the cluster, and the root route stays legible (#173 review).
    await expect(
      clusterKeyFor({ id: 'screen:local/shop@/users/[id]', label: 'Screen', props: {} }),
    ).toBe('local/shop · users');
    await expect(
      clusterKeyFor({ id: 'screen:local/shop@/', label: 'Screen', props: {} }),
    ).toBe('local/shop · /');

    const shuffled: AtlasSnapshot = {
      nodes: [...atlasFixture.nodes].reverse(),
      edges: [...atlasFixture.edges].reverse(),
    };
    const a = buildAtlasScene(atlasFixture, new Set());
    const b = buildAtlasScene(shuffled, new Set());
    const positions = (scene: typeof a) =>
      scene.nodes.map((item) => `${item.id}@${item.position.x},${item.position.y}`).sort();
    await expect(positions(b)).toEqual(positions(a));
    await expect(a.bands.map((band) => band.label)).toEqual([
      'Infrastructure',
      'Server code',
      'Events',
      'Client code',
    ]);
  },
};

const toolNode: GraphNode = {
  id: 'tool:local/spring-petclinic@docker-compose',
  label: 'Tool',
  props: {
    name: 'docker-compose',
    display: 'Docker Compose',
    category: 'container',
    prov: prov('Confirmed', 'docker-compose.yml'),
  },
};

const configFileNode: GraphNode = {
  id: 'file:local/spring-petclinic@.github/workflows/ci.yml',
  label: 'File',
  props: { path: '.github/workflows/ci.yml', config: true, prov: prov('Confirmed', '.github/workflows/ci.yml') },
};

const sourceFileNode: GraphNode = {
  id: 'file:local/spring-petclinic@src/main/App.java',
  label: 'File',
  props: { path: 'src/main/App.java', prov: prov('Confirmed', 'src/main/App.java') },
};

// A Confirmed external boundary placeholder (`core-graph::placeholder::mint`,
// #237/ADR-0031): an ordinary-looking Module with `props.placeholder = true`
// and `props.boundary = 'external'` standing in for a dependency the
// repository provably cannot provide, e.g. spring-petclinic's ~200-node
// `org.springframework.*` cluster (#244). Pure noise for Atlas — excluded.
const externalPlaceholderModule: GraphNode = {
  id: 'mod:local/spring-petclinic@org.springframework.boot',
  label: 'Module',
  props: {
    placeholder: true,
    boundary: 'external',
    reason: 'external dependency, repository cannot provide it',
    prov: prov('Confirmed', 'pom.xml'),
  },
};

// An *unresolved* boundary placeholder also carries `placeholder: true`
// (mint() sets it for every boundary kind) but is a genuine Gap
// (`core-graph::placeholder::is_gap`, R-INT-4) even though it keeps its
// original `Module` label rather than switching to `Gap` — it must stay
// banded and visible like any other fact.
const unresolvedPlaceholderModule: GraphNode = {
  id: 'mod:local/spring-petclinic@com.example.MissingHandler',
  label: 'Module',
  props: {
    placeholder: true,
    boundary: 'unresolved',
    reason: 'unresolved call target',
    prov: prov('Gap', 'App.java'),
  },
};

export const ToolsBandExcludesPlaceholders: Story = {
  // AC-0081, #244: config/build evidence (Tool nodes, and File nodes
  // proving them) lands in one Tools/Build band instead of splitting across
  // Server and Unclassified under the same cluster name; a Confirmed
  // external/internal boundary placeholder is excluded from bands entirely;
  // an unresolved placeholder — a genuine Gap, R-INT-4 — stays banded and
  // visible despite also carrying `placeholder: true`.
  play: async () => {
    const snapshot: AtlasSnapshot = {
      nodes: [toolNode, configFileNode, sourceFileNode, externalPlaceholderModule, unresolvedPlaceholderModule],
      edges: [
        { src: sourceFileNode.id, dst: unresolvedPlaceholderModule.id, label: 'CALLS', props: { prov: prov('Gap', 'App.java') } },
      ],
    };
    const bands = assignBands(snapshot);
    await expect(bands.get(toolNode.id)).toBe('tools');
    await expect(bands.get(configFileNode.id)).toBe('tools');
    // An ordinary source File (no `config` prop) stays in Server.
    await expect(bands.get(sourceFileNode.id)).toBe('server');
    // The Confirmed external placeholder gets no band at all — excluded.
    await expect(bands.has(externalPlaceholderModule.id)).toBe(false);
    // The unresolved placeholder — a real Gap — still bands like any Module.
    await expect(bands.get(unresolvedPlaceholderModule.id)).toBe('server');

    const scene = buildAtlasScene(snapshot, new Set());
    await expect(scene.bands.map((band) => band.label).sort()).toEqual(['Server code', 'Tools/Build']);
    await expect(scene.nodes.some((node) => node.id === externalPlaceholderModule.id)).toBe(false);
    await expect(scene.nodes.some((node) => node.id === unresolvedPlaceholderModule.id)).toBe(true);
  },
};

const configReadNodes: GraphNode[] = [
  {
    id: 'sym:local/shop@src/db.ts#connect',
    label: 'Symbol',
    props: { name: 'connect', prov: prov('Confirmed', 'src/db.ts') },
  },
  {
    id: 'cfgread:local/shop@src/db.ts:env:DATABASE_URL',
    label: 'ConfigRead',
    props: { source: 'env', key: 'DATABASE_URL', language: 'typescript', prov: prov('Confirmed', 'src/db.ts') },
  },
  {
    id: 'cfgread:local/shop@src/db.ts:env:?',
    label: 'ConfigRead',
    props: { source: 'env', dynamic: true, language: 'typescript', prov: prov('Confirmed', 'src/db.ts') },
  },
];

const configReadFixture: AtlasSnapshot = {
  nodes: configReadNodes,
  edges: [
    {
      src: configReadNodes[0].id,
      dst: configReadNodes[1].id,
      label: 'READS_CONFIG',
      props: { prov: prov('Confirmed', 'src/db.ts') },
    },
  ],
};

export const ConfigurationReadsShowTheKey: Story = {
  // AC-0651, #1821: a configuration read is filed under Server code beside
  // the symbol that reads it, not under Unclassified, and is labelled with
  // the key it reads, never its raw id. A read whose key the source does
  // not state as a literal says so.
  args: { snapshot: configReadFixture },
  play: async ({ canvasElement }) => {
    const bands = assignBands(configReadFixture);
    await expect(configReadNodes.map((node) => bands.get(node.id))).toEqual(['server', 'server', 'server']);
    const scene = buildAtlasScene(configReadFixture, new Set());
    await expect(scene.bands.map((band) => band.label)).toEqual(['Server code']);
    await expect(new Set(configReadNodes.map(clusterKeyFor))).toEqual(new Set(['local/shop · src']));
    await expect(configReadNodes.map(displayName)).toEqual(['connect', 'DATABASE_URL', UNNAMED_CONFIG_KEY]);
    await expect(filterAtlasGraph(configReadFixture, 'server').nodes).toHaveLength(3);
    await expect(within(canvasElement).getByTestId('atlas-canvas')).toBeInTheDocument();
  },
};

export const ClusterTileRendersNameAndCount: Story = {
  // AC-0081, #244: the collapsed-cluster tile shows its own content (name +
  // member count centered in the shape), not an empty box with a caption
  // underneath it.
  play: async () => {
    const clusterRule = CY_STYLE.find((rule) => rule.selector === '.atlas-cluster');
    const style = clusterRule?.style as Record<string, unknown> | undefined;
    await expect(style?.['text-valign']).toBe('center');
    await expect(style?.['text-halign']).toBe('center');
    await expect(style?.['text-wrap']).toBe('wrap');
  },
};

const SCALE_NODES: GraphNode[] = [
  ...Array.from({ length: 150 }, (_, index) => ({
    id: `sym:local/shop@src/app_${index}.ts#fn${index}`,
    label: 'Symbol' as const,
    props: { name: `fn${index}`, prov: prov('Confirmed', `src/app_${index}.ts`) },
  })),
  ...Array.from({ length: 90 }, (_, index) => ({
    id: `res:local/shop@aws_lambda_function.worker_${index}`,
    label: 'Resource' as const,
    props: { logical_id: `worker_${index}`, prov: prov('Confirmed', 'infra.tf') },
  })),
];

export const ClustersCollapseAtScale: Story = {
  // #159/AC-0081: past the threshold the initial view is collapsed clusters
  // that expand on demand — reading never requires manual arrangement.
  args: {
    snapshot: { nodes: SCALE_NODES, edges: [] },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent(
      '240 nodes · 0 edges · 2 of 2 clusters collapsed',
    );

    // The cluster index expands one cluster in place.
    const index = within(canvas.getByLabelText('Collapsed clusters'));
    await userEvent.click(index.getByRole('button', { name: /Infrastructure · local\/shop · aws_lambda_function · 90/ }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('1 of 2 clusters collapsed'),
    );

    // Expand all, then collapse back — no manual arrangement anywhere.
    await userEvent.click(canvas.getByRole('button', { name: 'Expand all clusters' }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('0 of 2 clusters collapsed'),
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Collapse clusters' }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('2 of 2 clusters collapsed'),
    );
  },
};

export const ClusterIndexesNameRepositories: Story = {
  // AC-1900 (#1601): cluster tiles and indexes name the repository as
  // Workspace does, never its local/src_<hash> key; keys stay exact.
  args: {
    snapshot: { nodes: SCALE_NODES, edges: [] },
    repoLabel: (repo: string) => (repo === 'local/shop' ? 'Shop API' : repo),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const index = within(canvas.getByLabelText('Collapsed clusters'));
    const button = index.getByRole('button', { name: /Infrastructure · Shop API · aws_lambda_function · 90/ });
    await expect(index.queryByRole('button', { name: /local\/shop/ })).not.toBeInTheDocument();
    await userEvent.click(button);
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('1 of 2 clusters collapsed'),
    );
  },
};

// 500 directories × 2 symbols in one repository: 500 clusters (#1079).
const ESTATE_NODES: GraphNode[] = Array.from({ length: 1000 }, (_, index) => {
  const dir = `d${String(Math.floor(index / 2)).padStart(3, '0')}`;
  return {
    id: `sym:local/estate@${dir}/m.ts#f${index % 2}`,
    label: 'Symbol',
    props: { name: `f${index % 2}`, prov: prov('Confirmed', `${dir}/m.ts`) },
  };
});

export const EstateScaleOpensToGroups: Story = {
  // AC-0366 (#1079): 500 clusters open as fewer than GROUP_BUDGET range
  // groups; a group drills in to its clusters (siblings step aside) and
  // folds back from the trail.
  args: {
    snapshot: { nodes: ESTATE_NODES, edges: [] },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent(
      '1000 nodes · 0 edges · 500 of 500 clusters collapsed · 9 groups',
    );
    await expect(canvas.getByRole('button', { name: 'Server code' })).toBeInTheDocument();

    const groups = within(canvas.getByLabelText('Collapsed groups'));
    await userEvent.click(groups.getByRole('button', { name: /Server code · local\/estate · d000 … d055 · 56 clusters/ }));
    await waitFor(() => expect(canvas.queryByLabelText('Collapsed groups')).not.toBeInTheDocument());
    await expect(canvas.getByRole('status')).toHaveTextContent('500 of 500 clusters collapsed');
    await expect(canvas.getByRole('status')).not.toHaveTextContent('groups');
    const clusters = within(canvas.getByLabelText('Collapsed clusters'));
    await expect(clusters.getByRole('button', { name: /local\/estate · d000 · 2/ })).toBeInTheDocument();

    const trail = within(canvas.getByLabelText('Open groups'));
    await userEvent.click(trail.getByRole('button', { name: /local\/estate · d000 … d055/ }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('500 of 500 clusters collapsed · 9 groups'),
    );
  },
};

/** Manual MT-M9-01 scale fixture; excluded from the per-PR browser suite. */
export const TenThousandNodeScale: Story = {
  tags: ['!test'],
  args: {
    snapshot: {
      nodes: Array.from({ length: 10_000 }, (_, index) => ({
        id: `res:scale@aws_lambda_function.worker_${index}`,
        label: 'Resource',
        props: {
          type: 'aws_lambda_function',
          logical_id: `worker_${index}`,
          prov: prov('Confirmed', `modules/worker_${index}.tf`),
        },
      })),
      edges: [],
    },
  },
};

export const RecordedScreenRouteSelection: Story = {
  // AC-0028: the label changes, the exact selected node does not.
  args: { snapshot: { nodes: [{ id: 'screen:local/shop@/main', label: 'Screen', props: { route: '/main', prov: prov('Confirmed', 'router.ts') } }], edges: [] } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: '/main' }));
    await expect(args.onSelect).toHaveBeenCalledWith(args.snapshot.nodes[0]);
  },
};

export const FindRevealsEntityInsideCollapsedGroup: Story = {
  // AC-0715 (#1666): Find searches the whole loaded snapshot, so an entity
  // far past the 24-entry visible index and folded inside a collapsed range
  // group is found by its stable id; choosing it opens the evidence drawer
  // with the exact original node and opens the groups and cluster around it.
  args: {
    snapshot: { nodes: ESTATE_NODES, edges: [] },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const target = ESTATE_NODES[601];
    await expect(canvas.getByText(/\+976 more on canvas/)).toBeInTheDocument();
    await expect(canvas.getByText(/Searches all 1000 entities in the loaded Atlas/)).toBeInTheDocument();

    const find = canvas.getByRole('searchbox', { name: 'Find entity' });
    await userEvent.type(find, '  D300/M.TS#F1 ');
    const results = within(canvas.getByLabelText('Find results'));
    await expect(results.getByText('1 match')).toBeInTheDocument();
    // While a query is active the ordinary indexes step aside.
    await expect(canvas.queryByLabelText('Collapsed groups')).not.toBeInTheDocument();

    await userEvent.click(results.getByRole('button', { name: /sym:local\/estate@d300\/m\.ts#f1/ }));
    await expect(args.onSelect).toHaveBeenCalledTimes(1);
    await expect(args.onSelect).toHaveBeenCalledWith(target);
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('499 of 500 clusters collapsed'),
    );

    // Clearing restores the ordinary controls; the selection stays.
    await userEvent.click(canvas.getByRole('button', { name: 'Clear' }));
    await expect(find).toHaveValue('');
    await expect(canvas.queryByLabelText('Find results')).not.toBeInTheDocument();
    const trail = within(canvas.getByLabelText('Open groups'));
    await expect(trail.getByRole('button', { name: /d280 … d335/ })).toBeInTheDocument();
    await expect(canvas.getByText(/f1 selected — press Enter to focus/)).toBeInTheDocument();
    await expect(args.onSelect).toHaveBeenCalledTimes(1);
  },
};

export const FindRevealsAcrossLayerAndFocus: Story = {
  // AC-0715 (#1666): a result outside the active layer widens it to All
  // layers; a result outside the current focus backs out only the levels
  // that hide it. Name, kind and stable id all match.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const find = canvas.getByRole('searchbox', { name: 'Find entity' });

    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('1 nodes · 0 edges'));
    await userEvent.type(find, 'checkout');
    let results = within(canvas.getByLabelText('Find results'));
    await userEvent.click(results.getByRole('button', { name: /component:local\/shop@Checkout/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges'));
    await expect(canvas.getByRole('button', { name: 'All layers' })).toHaveAttribute('aria-pressed', 'true');
    await expect(args.onLayerChange).toHaveBeenLastCalledWith('All layers');
    await expect(args.onSelect).toHaveBeenLastCalledWith(atlasNodes[3]);

    // Focus two levels deep: POST /orders, then its channel.
    await userEvent.clear(find);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on POST \/orders/ }));
    await userEvent.click(canvas.getByRole('button', { name: 'orders queue' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on orders queue/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));

    // Kind match: Checkout is one level up, so exactly one level pops.
    await userEvent.type(find, 'COMPONENT');
    results = within(canvas.getByLabelText('Find results'));
    await userEvent.click(results.getByRole('button', { name: /Checkout/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    const path = canvas.getByRole('navigation', { name: 'Focus path' });
    await expect(within(path).getByText('▸ POST /orders')).toBeInTheDocument();
    await expect(within(path).queryByText('▸ orders queue')).not.toBeInTheDocument();
    await expect(args.onSelect).toHaveBeenLastCalledWith(atlasNodes[3]);
  },
};

export const FindWideningLayerKeepsFocusThatShowsResult: Story = {
  // AC-0715 (#1666): widening to All layers does not drop focus levels that
  // still show the result there — only levels that hide it back out.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await userEvent.click(canvas.getByRole('button', { name: 'orders' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on orders/ }));
    const path = canvas.getByRole('navigation', { name: 'Focus path' });
    await waitFor(() => expect(within(path).getByText('▸ orders')).toBeInTheDocument());

    // The channel is outside Infrastructure but one edge from the focused
    // queue resource in All layers, so the focus level stays.
    await userEvent.type(canvas.getByRole('searchbox', { name: 'Find entity' }), 'chan:sqs');
    const results = within(canvas.getByLabelText('Find results'));
    await userEvent.click(results.getByRole('button', { name: /chan:sqs-queue:orders/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));
    await expect(canvas.getByRole('button', { name: 'All layers' })).toHaveAttribute('aria-pressed', 'true');
    await expect(within(path).getByText('▸ orders')).toBeInTheDocument();
    await expect(args.onSelect).toHaveBeenLastCalledWith(atlasNodes[1]);
  },
};

export const FindNoMatchAndClearKeepsSelection: Story = {
  // AC-0715 (#1666): an explicit no-match state; Esc in the field clears the
  // query and restores the ordinary controls without changing the selection.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await expect(args.onSelect).toHaveBeenCalledTimes(1);

    const find = canvas.getByRole('searchbox', { name: 'Find entity' });
    await userEvent.type(find, 'no-such-entity');
    await expect(canvas.getByText('No entities match “no-such-entity”')).toBeInTheDocument();
    await expect(canvas.queryByText('Visible entities')).not.toBeInTheDocument();

    // A blank (whitespace) query is not a search.
    await userEvent.clear(find);
    await userEvent.type(find, '   ');
    await expect(canvas.queryByLabelText('Find results')).not.toBeInTheDocument();

    await userEvent.clear(find);
    await userEvent.type(find, 'zzz');
    await userEvent.keyboard('{Escape}');
    await expect(find).toHaveValue('');
    await expect(canvas.getByText('Visible entities')).toBeInTheDocument();
    await expect(canvas.getByText(/POST \/orders selected — press Enter to focus/)).toBeInTheDocument();
    await expect(args.onSelect).toHaveBeenCalledTimes(1);
  },
};
