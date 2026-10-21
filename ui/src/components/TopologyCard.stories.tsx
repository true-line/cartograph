import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { GraphEdge, GraphNode, Provenance, SpecArtifact, SpecDiagramView } from '../store';
import { STRUCTURED_VIEW_UNAVAILABLE_NOTICE } from './SpecDiagramView';
import { TopologyCard } from './TopologyCard';

const SAMPLE = `flowchart LR
    res_aws_lambda_function_fulfill["aws_lambda_function.fulfill"]
    res_aws_sqs_queue_orders["aws_sqs_queue.orders"]
    res_aws_sqs_queue_orders -->|TRIGGERS| res_aws_lambda_function_fulfill
`;

const TOPOLOGY_NODES: GraphNode[] = [
  {
    id: 'res:aws_sqs_queue.orders',
    label: 'Resource',
    props: {
      name: 'aws_sqs_queue.orders',
      prov: {
        tier: 'Dynamic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/orders.tf',
            byte_start: 12,
            byte_end: 84,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't1.aws-describe',
        content_hash: 'a'.repeat(64),
      },
    },
  },
  {
    id: 'res:aws_lambda_function.fulfill',
    label: 'Resource',
    props: {
      name: 'aws_lambda_function.fulfill',
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/lambda.tf',
            byte_start: 90,
            byte_end: 164,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't0.hcl',
        content_hash: 'b'.repeat(64),
      },
    },
  },
  {
    id: 'chan:orders.queue',
    label: 'Channel',
    props: {
      name: 'orders.queue',
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'acme/shop',
            path: 'src/orders.ts',
            byte_start: 40,
            byte_end: 96,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't0.events',
        content_hash: 'c'.repeat(64),
      },
    },
  },
  {
    id: 'gap:infra:dlq',
    label: 'Gap',
    props: {
      name: 'Dead-letter queue target not statically resolved',
      reason: 'dead_letter_target_arn uses dynamic variable interpolation',
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Gap',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/orders.tf',
            byte_start: 100,
            byte_end: 152,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't0.hcl',
        content_hash: 'd'.repeat(64),
      },
    },
  },
];

const TOPOLOGY_EDGES: GraphEdge[] = [
  {
    src: 'res:aws_sqs_queue.orders',
    dst: 'res:aws_lambda_function.fulfill',
    label: 'TRIGGERS',
    props: {
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/orders.tf',
            byte_start: 160,
            byte_end: 210,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't0.hcl',
        content_hash: 'e'.repeat(64),
      },
    },
  },
  {
    src: 'res:aws_sqs_queue.orders',
    dst: 'chan:orders.queue',
    label: 'BACKS',
    props: {
      prov: {
        tier: 'Dynamic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/orders.tf',
            byte_start: 12,
            byte_end: 84,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't1.aws-describe',
        content_hash: 'f'.repeat(64),
      },
    },
  },
  {
    src: 'res:aws_sqs_queue.orders',
    dst: 'gap:infra:dlq',
    label: 'TRIGGERS',
    props: {
      gap_reason: 'dynamic event source mapping',
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Gap',
        evidence: [
          {
            repo: 'acme/infra',
            path: 'infra/orders.tf',
            byte_start: 100,
            byte_end: 152,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't0.hcl',
        content_hash: '1'.repeat(64),
      },
    },
  },
];

const TOPOLOGY_TITLE = 'Resource topology';

const TOPOLOGY_ARTIFACT: SpecArtifact = {
  id: 'topology',
  file_name: 'topology.md',
  title: TOPOLOGY_TITLE,
  format: 'markdown',
  schema: 'cartograph.spec/topology@1',
  content_id: 'spec-artifact-v1:topology-digest-1234567890abcdef',
  content: SAMPLE,
  content_truncated: false,
  content_byte_len: SAMPLE.length,
  assertions: [],
  assertions_truncated: false,
  assertions_total: 0,
};

const TOPOLOGY_VIEW: SpecDiagramView = {
  artifact_id: 'topology',
  schema: TOPOLOGY_ARTIFACT.schema,
  content_id: TOPOLOGY_ARTIFACT.content_id,
  mode: 'best-effort',
  nodes: TOPOLOGY_NODES,
  nodes_total: TOPOLOGY_NODES.length,
  edges: TOPOLOGY_EDGES,
  edges_total: TOPOLOGY_EDGES.length,
  flows_total: 0,
  deployments: [
    {
      resource_id: 'res:aws_sqs_queue.orders',
      source: 't1.aws-describe',
      provenance: TOPOLOGY_NODES[0].props.prov as Provenance,
      backed_channels: ['chan:orders.queue'],
    },
  ],
  truncated: false,
};

const LARGE_NODES: GraphNode[] = Array.from({ length: 14 }, (_, index) => ({
  id: `res:svc_${String(index + 1).padStart(2, '0')}`,
  label: 'Resource',
  props: {
    name: `service_${String(index + 1).padStart(2, '0')}`,
    prov: TOPOLOGY_NODES[1].props.prov,
  },
}));

const LARGE_EDGES: GraphEdge[] = Array.from({ length: 13 }, (_, index) => ({
  src: `res:svc_${String(index + 1).padStart(2, '0')}`,
  dst: `res:svc_${String(index + 2).padStart(2, '0')}`,
  label: 'TRIGGERS',
  props: { prov: TOPOLOGY_EDGES[0].props.prov },
}));

const LARGE_TOPOLOGY_VIEW: SpecDiagramView = {
  artifact_id: 'topology',
  schema: TOPOLOGY_ARTIFACT.schema,
  content_id: TOPOLOGY_ARTIFACT.content_id,
  mode: 'best-effort',
  nodes: LARGE_NODES,
  nodes_total: LARGE_NODES.length,
  edges: LARGE_EDGES,
  edges_total: LARGE_EDGES.length,
  flows_total: 0,
  deployments: [],
  truncated: false,
};

const meta = {
  title: 'Atlas/TopologyCard',
  component: TopologyCard,
  args: {
    onSelectNode: fn(),
    onSelectEdge: fn(),
  },
} satisfies Meta<typeof TopologyCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { mermaid: 'flowchart LR\n' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no resources recovered yet/i)).toBeInTheDocument();
  },
};

export const NoBackend: Story = {
  args: { mermaid: null },
};

export const Populated: Story = {
  args: { mermaid: SAMPLE },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.queryByTestId('spec-diagram-canvas')).not.toBeInTheDocument();
    await expect(canvas.getByTestId('diagram-view-unavailable')).toHaveTextContent(
      STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
    );
    const pre = canvas.getByTestId('topology-mermaid');
    await expect(pre.textContent).toContain('flowchart LR');
    await expect(pre.textContent).toContain('-->|TRIGGERS|');
    await expect(canvas.getByRole('button', { name: /copy mermaid/i })).toBeEnabled();
  },
};

export const RenderedTopologyAndDeploymentMatchesIdentity: Story = {
  // AC-0246 (T-0246): topology and deployment views render from structured
  // graph data (never by parsing Mermaid text) and match the exported
  // projection's schema and content_id.
  args: {
    mermaid: 'flowchart LR\n    mermaid_only_sentinel["never parsed into diagram"]\n',
    view: TOPOLOGY_VIEW,
    artifact: TOPOLOGY_ARTIFACT,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('spec-diagram-canvas')).toBeInTheDocument();

    const identity = canvas.getByTestId('diagram-content-identity');
    await expect(identity).toHaveAttribute('data-schema', 'cartograph.spec/topology@1');
    await expect(identity).toHaveAttribute(
      'data-content-id',
      'spec-artifact-v1:topology-digest-1234567890abcdef',
    );
    await expect(identity).toHaveTextContent('spec-artifact-v1:topology-dig');

    const deployments = canvas.getByTestId('diagram-deployments');
    await expect(deployments).toHaveTextContent('aws_sqs_queue.orders');
    await expect(deployments).toHaveTextContent('DEPLOYED (t1.aws-describe)');
    await expect(deployments).toHaveTextContent('backs chan:orders.queue');

    // Diagram nodes come from structured data, never from the Mermaid text.
    const nodeIndex = canvas.getByLabelText('Diagram nodes');
    await expect(nodeIndex).toHaveTextContent('aws_lambda_function.fulfill');
    await expect(nodeIndex).not.toHaveTextContent('mermaid_only_sentinel');

    // Copyable Mermaid remains available as a secondary action.
    await expect(canvas.getByRole('button', { name: /copy mermaid/i })).toBeEnabled();
    await expect(canvas.getByTestId('topology-mermaid')).toHaveTextContent(
      'mermaid_only_sentinel',
    );
  },
};

export const SelectionOpensEvidenceAndHighlightsGaps: Story = {
  // AC-0247 (T-0247): selecting a node or edge in a rendered view opens its
  // evidence, tier, and confidence, and Gap or partial hops are visibly and
  // textually distinct from confirmed ones (never color alone).
  args: {
    mermaid: SAMPLE,
    view: TOPOLOGY_VIEW,
    artifact: TOPOLOGY_ARTIFACT,
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const nodeIndex = within(canvas.getByLabelText('Diagram nodes'));
    const edgeIndex = within(canvas.getByLabelText('Diagram edges and hops'));

    const confirmedNode = nodeIndex.getByRole('button', {
      name: /aws_sqs_queue\.orders \[DEPLOYED · T1\]/,
    });
    const gapNode = nodeIndex.getByRole('button', {
      name: /GAP · Dead-letter queue target not statically resolved \[UNRESOLVED\]/,
    });
    await expect(gapNode).toHaveClass('unresolved');
    await expect(gapNode).toHaveClass('tier-gap');
    await expect(getComputedStyle(gapNode).borderStyle).toBe('dashed');
    await expect(getComputedStyle(confirmedNode).borderStyle).not.toBe('dashed');

    const gapEdge = edgeIndex.getByRole('button', {
      name: /GAP TRIGGERS \[UNRESOLVED: dynamic event source mapping\]/,
    });
    await expect(gapEdge).toHaveClass('unresolved');
    await expect(gapEdge).toHaveClass('tier-gap');
    await expect(getComputedStyle(gapEdge).borderStyle).toBe('dashed');

    // Selecting a confirmed deployed resource node opens its evidence, tier, and confidence.
    await userEvent.click(confirmedNode);
    await expect(args.onSelectNode).toHaveBeenCalledWith(TOPOLOGY_NODES[0]);
    const evidence = canvas.getByTestId('diagram-selected-evidence');
    await expect(evidence).toHaveTextContent('Dynamic');
    await expect(evidence).toHaveTextContent('Confirmed');
    await expect(evidence).toHaveTextContent('t1.aws-describe');
    await expect(evidence).toHaveTextContent('acme/infra:infra/orders.tf');
    await expect(evidence).toHaveTextContent('bytes 12..84');

    // Selecting the Gap edge opens its evidence, tier, confidence, and unresolved reason.
    await userEvent.click(gapEdge);
    await expect(args.onSelectEdge).toHaveBeenCalledWith(TOPOLOGY_EDGES[2]);
    await expect(evidence).toHaveTextContent('GAP · TRIGGERS');
    await expect(evidence).toHaveTextContent('Unresolved hop — dynamic event source mapping');
    await expect(evidence).toHaveTextContent('Gap');
    await expect(evidence).toHaveTextContent('bytes 100..152');
  },
};

export const LargeTopologyPagesAndExpandsWithHiddenCounts: Story = {
  // AC-0246 (T-0246): large systems page or collapse with a visible count of
  // hidden items instead of truncating silently.
  args: {
    mermaid: SAMPLE,
    view: LARGE_TOPOLOGY_VIEW,
    artifact: TOPOLOGY_ARTIFACT,
    pageSize: 6,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const scale = canvas.getByTestId('diagram-scale-status');
    await expect(scale).toHaveTextContent('Showing 1–6 of 14 nodes');
    await expect(scale).toHaveTextContent('8 hidden');
    await expect(scale).toHaveTextContent('Page 1 of 3');

    await userEvent.click(within(scale).getByRole('button', { name: 'Next page' }));
    await expect(scale).toHaveTextContent('Showing 7–12 of 14 nodes');
    await expect(scale).toHaveTextContent('8 hidden');
    await expect(scale).toHaveTextContent('Page 2 of 3');

    await userEvent.click(
      within(scale).getByRole('button', { name: 'Expand all (14 nodes)' }),
    );
    await expect(scale).toHaveTextContent('Showing all 14 of 14 nodes · 0 hidden');

    await userEvent.click(
      within(scale).getByRole('button', { name: 'Collapse to pages (6 per page)' }),
    );
    await expect(scale).toHaveTextContent('Showing 1–6 of 14 nodes');
  },
};

