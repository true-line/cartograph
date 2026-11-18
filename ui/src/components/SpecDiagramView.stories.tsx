import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { GraphEdge, GraphNode, Provenance, SpecArtifact, SpecDiagramView } from '../store';
import { SpecDiagramCanvas } from './SpecDiagramView';

/** The canvas itself, without a card around it (#1769). The cards that embed
 *  it (TopologyCard, FlowsCard) cover the Mermaid fallback; these stories
 *  cover the canvas's own identity check, paging and evidence panel. */

function prov(
  confidence: Provenance['confidence_tier'],
  path: string,
  hash: string,
  extractor = 't0.hcl',
): Provenance {
  return {
    tier: 'Deterministic',
    confidence_tier: confidence,
    evidence: [
      { repo: 'acme/infra', path, byte_start: 10, byte_end: 80, commit_sha: 'abc123' },
    ],
    extractor_id: extractor,
    content_hash: hash.repeat(64),
  };
}

const NODES: GraphNode[] = [
  {
    id: 'res:aws_sqs_queue.orders',
    label: 'Resource',
    props: {
      name: 'aws_sqs_queue.orders',
      prov: prov('Confirmed', 'infra/orders.tf', 'a', 't1.aws-describe'),
    },
  },
  {
    id: 'res:aws_lambda_function.fulfill',
    label: 'Resource',
    props: {
      name: 'aws_lambda_function.fulfill',
      prov: prov('Confirmed', 'infra/lambda.tf', 'b'),
    },
  },
  {
    id: 'gap:infra:dlq',
    label: 'Gap',
    props: {
      name: 'Dead-letter queue target not statically resolved',
      reason: 'dead_letter_target_arn uses dynamic variable interpolation',
      prov: prov('Gap', 'infra/orders.tf', 'c'),
    },
  },
];

const EDGES: GraphEdge[] = [
  {
    src: 'res:aws_sqs_queue.orders',
    dst: 'res:aws_lambda_function.fulfill',
    label: 'TRIGGERS',
    props: { prov: prov('Confirmed', 'infra/orders.tf', 'd') },
  },
  {
    src: 'res:aws_sqs_queue.orders',
    dst: 'gap:infra:dlq',
    label: 'TRIGGERS',
    props: {
      gap_reason: 'dynamic event source mapping',
      prov: prov('Gap', 'infra/orders.tf', 'e'),
    },
  },
];

const ARTIFACT: Pick<SpecArtifact, 'id' | 'schema' | 'content_id'> = {
  id: 'topology',
  schema: 'cartograph.spec/topology@1',
  content_id: 'spec-artifact-v1:topology-digest-1234567890abcdef',
};

const VIEW: SpecDiagramView = {
  artifact_id: ARTIFACT.id,
  schema: ARTIFACT.schema,
  content_id: ARTIFACT.content_id,
  mode: 'best-effort',
  nodes: NODES,
  nodes_total: NODES.length,
  edges: EDGES,
  edges_total: EDGES.length,
  flows_total: 0,
  deployments: [
    {
      resource_id: 'res:aws_sqs_queue.orders',
      source: 't1.aws-describe',
      provenance: NODES[0].props.prov as Provenance,
      backed_channels: ['chan:orders.queue'],
    },
  ],
  truncated: false,
};

const CHAIN_NODES: GraphNode[] = Array.from({ length: 5 }, (_, index) => ({
  id: `res:svc_${index + 1}`,
  label: 'Resource',
  props: { name: `service_${index + 1}`, prov: prov('Confirmed', 'infra/svc.tf', 'f') },
}));

const CHAIN_EDGES: GraphEdge[] = Array.from({ length: 4 }, (_, index) => ({
  src: `res:svc_${index + 1}`,
  dst: `res:svc_${index + 2}`,
  label: 'TRIGGERS',
  props: { prov: prov('Confirmed', 'infra/svc.tf', '1') },
}));

const meta = {
  title: 'Spec/SpecDiagramCanvas',
  component: SpecDiagramCanvas,
  args: {
    view: VIEW,
    artifact: ARTIFACT,
    title: 'Resource topology',
    onSelectNode: fn(),
    onSelectEdge: fn(),
  },
} satisfies Meta<typeof SpecDiagramCanvas>;

export default meta;
type Story = StoryObj<typeof meta>;

export const SelectNodeAndEdgeShowsEvidence: Story = {
  // AC-0246/AC-0247: the canvas renders the structured view whose identity
  // matches the exported artifact, and selecting a node or a hop opens its
  // tier, method, extractor and cited evidence.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('spec-diagram-canvas')).toBeInTheDocument();
    await expect(canvas.getByTestId('diagram-content-identity')).toHaveAttribute(
      'data-content-id',
      ARTIFACT.content_id,
    );
    await expect(canvas.getByTestId('diagram-scale-status')).toHaveTextContent('3 nodes · 2 edges');
    await expect(canvas.getByTestId('diagram-deployments')).toHaveTextContent(
      'aws_sqs_queue.orders · DEPLOYED (t1.aws-describe) → backs chan:orders.queue',
    );

    const nodes = within(canvas.getByLabelText('Diagram nodes'));
    await userEvent.click(nodes.getByRole('button', { name: /aws_lambda_function\.fulfill/ }));
    await expect(args.onSelectNode).toHaveBeenCalledWith(NODES[1]);
    const evidence = canvas.getByTestId('diagram-selected-evidence');
    await expect(evidence).toHaveTextContent('res:aws_lambda_function.fulfill');
    await expect(evidence).toHaveTextContent('acme/infra:infra/lambda.tf');

    // A gap node states why it is unresolved.
    await userEvent.click(nodes.getByRole('button', { name: /Dead-letter queue/ }));
    await expect(canvas.getByTestId('diagram-selected-evidence')).toHaveTextContent(
      'Unresolved node — dead_letter_target_arn uses dynamic variable interpolation',
    );

    // A gap hop states its gap reason.
    const edges = within(canvas.getByLabelText('Diagram edges and hops'));
    const hops = edges.getAllByRole('button');
    await expect(hops).toHaveLength(2);
    await userEvent.click(hops[1]);
    await expect(args.onSelectEdge).toHaveBeenCalledWith(EDGES[1]);
    await expect(canvas.getByTestId('diagram-selected-evidence')).toHaveTextContent(
      'Unresolved hop — dynamic event source mapping',
    );
  },
};

export const IdentityMismatchRefusesToRender: Story = {
  // AC-0246: a view whose content_id does not match the exported artifact is
  // refused, with both identities stated, and no canvas is drawn.
  args: {
    artifact: { ...ARTIFACT, content_id: 'spec-artifact-v1:other-digest-abcdef1234567890' },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const alert = canvas.getByRole('alert');
    await expect(alert).toHaveTextContent('Refused to render diagram');
    await expect(alert).toHaveTextContent('spec-artifact-v1:topology-dig');
    await expect(alert).toHaveTextContent('spec-artifact-v1:other-digest');
    await expect(canvas.queryByTestId('spec-diagram-canvas')).not.toBeInTheDocument();
  },
};

export const MissingArtifactRefusesToRender: Story = {
  args: { artifact: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('diagram-identity-mismatch')).toHaveTextContent(
      '(missing · missing)',
    );
  },
};

export const PagesLargeViewAndExpands: Story = {
  // AC-0247: a projection past the page size is paged, the hidden node and
  // edge counts are stated, and it can be expanded and collapsed again.
  args: {
    pageSize: 2,
    view: {
      ...VIEW,
      nodes: CHAIN_NODES,
      nodes_total: CHAIN_NODES.length,
      edges: CHAIN_EDGES,
      edges_total: CHAIN_EDGES.length,
      deployments: [],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const status = canvas.getByTestId('diagram-scale-status');
    await expect(status).toHaveTextContent(
      'Showing 1–2 of 5 nodes · 3 hidden (3 edges hidden) · Page 1 of 3',
    );
    await expect(canvas.getByRole('button', { name: 'Previous page' })).toBeDisabled();

    await userEvent.click(canvas.getByRole('button', { name: 'Next page' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Next page' }));
    await expect(status).toHaveTextContent('Showing 5–5 of 5 nodes · 4 hidden');
    await expect(canvas.getByRole('button', { name: 'Next page' })).toBeDisabled();

    await userEvent.click(canvas.getByRole('button', { name: 'Expand all (5 nodes)' }));
    await expect(status).toHaveTextContent('Showing all 5 of 5 nodes · 0 hidden · 4 edges');

    await userEvent.click(
      canvas.getByRole('button', { name: 'Collapse to pages (2 per page)' }),
    );
    await expect(status).toHaveTextContent('Page 1 of 3');
  },
};

export const EmptyProjection: Story = {
  args: {
    view: { ...VIEW, nodes: [], nodes_total: 0, edges: [], edges_total: 0, deployments: [] },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No diagram facts in this projection.')).toBeInTheDocument();
    await expect(canvas.queryByTestId('spec-diagram-canvas')).not.toBeInTheDocument();
  },
};
