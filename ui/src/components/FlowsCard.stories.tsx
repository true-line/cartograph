import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { Flow, FlowHop, SpecArtifact, SpecDiagramView, Tier } from '../store';
import { FlowsCard, hopKind, projectedDossier, statusBadge } from './FlowsCard';
import { STRUCTURED_VIEW_UNAVAILABLE_NOTICE } from './SpecDiagramView';

function hop(
  label: string,
  src: string,
  dst: string,
  srcName: string,
  dstName: string,
  confidence: Tier,
  options: Partial<FlowHop> = {},
): FlowHop {
  const tier =
    confidence === 'Confirmed'
      ? 'Deterministic'
      : confidence === 'InferredStrong'
        ? 'Semantic'
        : confidence === 'InferredWeak'
          ? 'Agentic'
          : 'Deterministic';
  return {
    label,
    src,
    dst,
    src_name: srcName,
    dst_name: dstName,
    tier,
    confidence,
    evidence: 'src/app.ts bytes 10..42',
    provenance: {
      tier,
      confidence_tier: confidence,
      evidence: [{
        repo: 'local/shop',
        path: 'src/app.ts',
        byte_start: 10,
        byte_end: 42,
        commit_sha: 'abc123',
      }],
      extractor_id: 'story.flow',
      content_hash: `${src}:${label}:${dst}`,
    },
    gap_reason: null,
    attempted_tiers: [],
    ...options,
  };
}

const VERIFIED: Flow = {
  trigger: 'ep:POST:/orders',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /orders',
  hops: [
    hop('HANDLES', 'ep:POST:/orders', 'sym:orders#create', 'POST /orders', 'createOrder', 'Confirmed'),
    hop('CALLS', 'sym:orders#create', 'sym:orders#persist', 'createOrder', 'persistOrder', 'Confirmed'),
  ],
  status: 'Verified',
  score: 1.0,
  depth_limited: false,
};

const PARTIAL: Flow = {
  trigger: 'ep:POST:/notify',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /notify',
  hops: [
    hop('HANDLES', 'ep:POST:/notify', 'sym:notify#send', 'POST /notify', 'sendNotification', 'Confirmed'),
    hop('CALLS', 'sym:notify#send', 'sym:notify#guess', 'sendNotification', 'guessedTarget', 'InferredWeak'),
    hop(
      'PUBLISHES',
      'sym:notify#send',
      'gap:channel:notify',
      'sendNotification',
      'GAP: runtime-computed channel identity',
      'Gap',
      {
        evidence: 'src/notify.ts bytes 90..128',
        gap_reason: 'runtime-computed channel identity',
        attempted_tiers: ['T0', 'T1', 'T2', 'T3'],
      },
    ),
  ],
  status: 'Partial',
  score: 0.43,
  depth_limited: false,
};

const FLOWS = [VERIFIED, PARTIAL];
const SAMPLE = projectedDossier(FLOWS, 'best-effort');
const BRANCHED: Flow = {
  trigger: 'ep:POST:/branch',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /branch',
  hops: [
    hop('HANDLES', 'ep:POST:/branch', 'sym:branch#handle', 'POST /branch', 'branchHandler', 'Confirmed'),
    hop('CALLS', 'sym:branch#handle', 'sym:branch#helper', 'branchHandler', 'helper', 'Confirmed'),
    hop('PUBLISHES', 'sym:branch#handle', 'channel:orders', 'branchHandler', 'orders.created', 'Confirmed'),
  ],
  status: 'Verified',
  score: 1,
  depth_limited: false,
};
const UNKNOWN_CONFIDENCE: Flow = {
  ...VERIFIED,
  trigger: 'ep:GET:/unknown',
  trigger_name: 'GET /unknown',
  hops: [
    {
      ...VERIFIED.hops[0],
      src: 'ep:GET:/unknown',
      src_name: 'GET /unknown',
      confidence: 'Unrecognized',
    },
  ],
  status: 'Partial',
  score: 0,
};

const meta = {
  title: 'Atlas/FlowInspector',
  component: FlowsCard,
  args: {
    flows: FLOWS,
    dossier: SAMPLE,
    artifact: {
      id: 'flow-dossiers',
      file_name: 'flow_dossiers.md',
      title: 'Flow dossiers',
      format: 'markdown',
      schema: 'cartograph.spec/flow-dossiers@1',
      content_id: 'spec-artifact-v1:flow-dossiers-digest-abcdef123456',
      content: SAMPLE,
      content_truncated: false,
      content_byte_len: SAMPLE.length,
      assertions: [],
      assertions_truncated: false,
      assertions_total: 0,
    },
    onSelectHop: fn(),
    onOpenResolution: fn(),
  },
} satisfies Meta<typeof FlowsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { flows: [], dossier: '# Flow dossier\n' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no flows traced/i)).toBeInTheDocument();
    // Without an anchor inventory the state still says why, not a hint.
    await expect(canvas.getByText(/no anchor inventory available/i)).toBeInTheDocument();
  },
};

export const EmptyNamesEveryAnchorKindSought: Story = {
  // AC-0084 (#165): zero flows must name the anchor kinds recovery looked
  // for and what it found — never the generic "ingest a repo" shrug.
  args: {
    flows: [],
    dossier: '# Flow dossier\n',
    anchors: [
      { kind: 'screens (routes/pages)', found: 0 },
      { kind: 'extension contexts (popup, background, content scripts)', found: 0 },
      { kind: 'extension commands (keyboard shortcuts)', found: 0 },
      { kind: 'HTTP endpoints', found: 0 },
      { kind: 'externally published event channels', found: 0 },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const probes = canvas.getByLabelText('Anchor kinds sought');
    const rows = within(probes).getAllByRole('listitem');
    await expect(rows).toHaveLength(5);
    await expect(probes).toHaveTextContent('screens (routes/pages)');
    await expect(probes).toHaveTextContent('extension commands (keyboard shortcuts)');
    await expect(within(probes).getAllByText('none found')).toHaveLength(5);
    await expect(canvas.queryByText(/ingest a repo with endpoints/i)).not.toBeInTheDocument();
  },
};

const COMMAND_FLOW: Flow = {
  trigger: 'extcmd:local/ext@.:save-snapshot',
  trigger_kind: 'Command',
  trigger_name: 'Command save-snapshot',
  hops: [
    hop(
      'HANDLES',
      'extcmd:local/ext@.:save-snapshot',
      'extctx:local/ext@.:service-worker:background.js',
      'Command save-snapshot',
      'service-worker · src/background.ts',
      'Confirmed',
    ),
    hop(
      'ENTRY',
      'extctx:local/ext@.:service-worker:background.js',
      'file:local/ext@src/background.ts',
      'service-worker · src/background.ts',
      'src/background.ts',
      'Confirmed',
    ),
    hop(
      'DEFINED_IN',
      'file:local/ext@src/background.ts',
      'sym:background.ts#onSave',
      'src/background.ts',
      'onSave',
      'Confirmed',
    ),
  ],
  status: 'Verified',
  score: 1,
  depth_limited: false,
};

export const ExtensionCommandAnchorsAFlow: Story = {
  // AC-0083 (#165): a manifest command is a user-action anchor whose hops
  // carry provenance like any other flow.
  args: { flows: [COMMAND_FLOW], dossier: projectedDossier([COMMAND_FLOW], 'best-effort') },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { level: 2 })).toHaveTextContent(
      'F-0001 · Command save-snapshot',
    );
    await expect(canvas.getByText('TRIGGER · Command')).toBeInTheDocument();
    const sequence = canvas.getByLabelText('Flow sequence');
    await expect(within(sequence).getByText('CONTEXT')).toBeInTheDocument();
    await expect(within(sequence).getByText('FILE')).toBeInTheDocument();
    await expect(within(sequence).getByText('FUNCTION')).toBeInTheDocument();
    await expect(hopKind(COMMAND_FLOW.hops[0])).toBe('CONTEXT');
    await expect(hopKind(COMMAND_FLOW.hops[1])).toBe('FILE');
  },
};

export const SequenceAndTriggerSelection: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // #107 header: flow id, status badge with named gap count, trigger
    // summary and score in prose.
    await expect(canvas.getByRole('heading', { level: 2 })).toHaveTextContent(
      'F-0001 · POST /orders',
    );
    await expect(canvas.getByText('VERIFIED')).toHaveClass('tier-confirmed');
    await expect(canvas.getByText(/Flow score 1\.00/)).toBeInTheDocument();
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 2 hops shown');
    // #164: the projection toggle explains verified-only vs best-effort.
    await expect(
      canvas.getByRole('button', { name: 'What is verified-only vs best-effort?' }),
    ).toBeInTheDocument();
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('createOrder');

    await userEvent.selectOptions(canvas.getByLabelText('Trigger source'), PARTIAL.trigger);
    await expect(canvas.getByRole('heading', { level: 2 })).toHaveTextContent(
      'F-0002 · POST /notify',
    );
    await expect(canvas.getByText('PARTIAL (1 gap)')).toHaveClass('tier-gap');
    await expect(canvas.getByText(/Flow score 0\.43/)).toBeInTheDocument();
    await expect(canvas.getByRole('status')).toHaveTextContent('3 of 3 hops shown');
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('sendNotification');
    await expect(statusBadge(PARTIAL)).toBe('PARTIAL (1 gap)');
  },
};

export const ExplicitGap: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    const gap = within(sequence)
      .getByRole('button', { name: /Unresolved hop/ });
    await expect(gap).toHaveClass('unresolved');
    await expect(gap).toHaveTextContent('runtime-computed channel identity');
    await expect(gap).toHaveTextContent('T0 → T1 → T2 → T3');
    await expect(within(gap).getByText('GAP', { selector: '.tier-badge' })).toBeInTheDocument();
    await expect(hopKind(PARTIAL.hops[2])).toBe('GAP');
  },
};

export const BranchedTraceUsesRecordedEndpoints: Story = {
  args: { flows: [BRANCHED], dossier: projectedDossier([BRANCHED], 'best-effort') },
  play: async ({ canvasElement }) => {
    // Every card and dossier arrow uses the hop's recorded src/dst ids —
    // sequence is never inferred from array position.
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    const publishes = within(sequence).getByRole('button', { name: /^PUBLISHES:/ });
    await expect(publishes).toHaveTextContent('branchHandler → orders.created');
    await expect(hopKind(BRANCHED.hops[2])).toBe('CHANNEL');

    const dossier = projectedDossier([BRANCHED], 'best-effort');
    await expect(dossier).toContain('p1->>p2: CALLS [Confirmed]');
    await expect(dossier).toContain('p1->>p3: PUBLISHES [Confirmed]');
    await expect(dossier).not.toContain('p2->>p3: PUBLISHES [Confirmed]');
  },
};

export const VerifiedOnlyProjection: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Best-effort names its weak hops instead of hiding the difference.
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Best-effort: includes 1 InferredWeak hop/,
    );
    // The note's tone must actually resolve (#144 review): a typo'd CSS
    // token invalidates the declaration and silently falls back to the
    // confirmed-green base, marking inferred content as confirmed.
    const bestEffortBorder = getComputedStyle(canvas.getByRole('note')).borderColor;

    await userEvent.click(canvas.getByRole('button', { name: 'verified-only' }));
    await expect(
      getComputedStyle(canvas.getByRole('note')).borderColor,
    ).not.toBe(bestEffortBorder);
    // #107: the excluded hop stays visible as an annotated, non-interactive
    // card; the count and the note make the projection difference explicit,
    // and the Gap node is retained (R-INT-4).
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 3 hops shown');
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Verified-only: InferredWeak hops are excluded \(1 hidden\), but the Gap node is retained/,
    );
    const excludedNote = canvas.getByText('Excluded in verified-only — InferredWeak');
    // --tier-inferred-weak resolved, not fallen back to inherited text color.
    await expect(getComputedStyle(excludedNote).color).toBe('rgb(242, 201, 76)');
    const excluded = excludedNote.closest('.flow-hop-card');
    await expect(excluded).toHaveClass('excluded');
    await expect(excluded).toHaveAttribute('aria-disabled', 'true');
    await expect(excluded?.tagName).not.toBe('BUTTON');
    await expect(
      canvas.getByRole('button', { name: /Unresolved hop/ }),
    ).toBeInTheDocument();

    await userEvent.click(canvas.getByText(/Mermaid \+ provenance dossier/));
    const dossier = canvas.getByTestId('flows-dossier');
    await expect(dossier).not.toHaveTextContent('guessedTarget');
    await expect(dossier).toHaveTextContent('PUBLISHES [Gap]');
  },
};

export const GapHopOpensResolutionStrategy: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Gap hop → Resolution Strategy with the Gap node id; a confirmed hop →
    // evidence drawer with the hop itself (#107).
    await userEvent.click(canvas.getByRole('button', { name: /Unresolved hop/ }));
    await expect(args.onOpenResolution).toHaveBeenCalledWith('gap:channel:notify');
    await expect(args.onSelectHop).not.toHaveBeenCalled();

    await userEvent.click(
      canvas.getByRole('button', { name: 'HANDLES: POST /notify to sendNotification' }),
    );
    await expect(args.onSelectHop).toHaveBeenCalledWith(PARTIAL.hops[0]);
  },
};

export const FitAndZoomNeverScrollHorizontally: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const row = canvas.getByLabelText('Flow sequence');
    const noOverflow = () => row.scrollWidth <= row.clientWidth + 1;
    await expect(noOverflow()).toBe(true);

    await userEvent.click(canvas.getByRole('button', { name: 'Zoom in' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Zoom in' }));
    await expect(noOverflow()).toBe(true);

    await userEvent.click(canvas.getByRole('button', { name: 'Fit hops to view' }));
    await expect(noOverflow()).toBe(true);
    await userEvent.click(canvas.getByRole('button', { name: 'Zoom out' }));
    await expect(noOverflow()).toBe(true);
  },
};

export const UnknownConfidenceFailsClosed: Story = {
  args: { flows: [UNKNOWN_CONFIDENCE], dossier: null },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const sequence = canvas.getByLabelText('Flow sequence');
    const card = within(sequence).getByRole('button', { name: /Unresolved hop/ });
    await expect(card).toHaveClass('unresolved');
    await expect(card).toHaveTextContent('confidence metadata missing or unrecognized');
    // Fail-closed is not escalatable: the destination is not a Gap node, so
    // the click shows evidence instead of a Resolution Strategy.
    await userEvent.click(card);
    await expect(args.onOpenResolution).not.toHaveBeenCalled();
    await expect(args.onSelectHop).toHaveBeenCalled();
  },
};

const DEPLOYED_FLOW: Flow = {
  ...PARTIAL,
  hops: [
    ...PARTIAL.hops,
    hop(
      'BACKS',
      'res:aws_sqs_queue.notify',
      'gap:channel:notify',
      'aws_sqs_queue.notify',
      'GAP: runtime-computed channel identity',
      'Confirmed',
    ),
  ],
};

const FLOW_DOSSIERS_ARTIFACT: SpecArtifact = {
  id: 'flow-dossiers',
  file_name: 'flow_dossiers.md',
  title: 'Flow dossiers',
  format: 'markdown',
  schema: 'cartograph.spec/flow-dossiers@1',
  content_id: 'spec-artifact-v1:flow-dossiers-digest-abcdef123456',
  content: projectedDossier([VERIFIED, DEPLOYED_FLOW], 'best-effort'),
  content_truncated: false,
  content_byte_len: 120,
  assertions: [],
  assertions_truncated: false,
  assertions_total: 0,
};

const FLOW_DOSSIERS_HOST_VIEW: SpecDiagramView = {
  artifact_id: 'flow-dossiers',
  schema: FLOW_DOSSIERS_ARTIFACT.schema,
  content_id: FLOW_DOSSIERS_ARTIFACT.content_id,
  mode: 'best-effort',
  nodes: [
    {
      id: 'ep:POST:/orders',
      label: 'Endpoint',
      props: { name: 'POST /orders', prov: VERIFIED.hops[0].provenance },
    },
    {
      id: 'sym:orders#create',
      label: 'Symbol',
      props: { name: 'createOrder', prov: VERIFIED.hops[0].provenance },
    },
    {
      id: 'sym:orders#persist',
      label: 'Symbol',
      props: { name: 'persistOrder', prov: VERIFIED.hops[1].provenance },
    },
    {
      id: 'ep:POST:/notify',
      label: 'Endpoint',
      props: { name: 'POST /notify', prov: DEPLOYED_FLOW.hops[0].provenance },
    },
    {
      id: 'sym:notify#send',
      label: 'Symbol',
      props: { name: 'sendNotification', prov: DEPLOYED_FLOW.hops[0].provenance },
    },
    {
      id: 'sym:notify#guess',
      label: 'Symbol',
      props: { name: 'guessedTarget', prov: DEPLOYED_FLOW.hops[1].provenance },
    },
    {
      id: 'res:aws_sqs_queue.notify',
      label: 'Resource',
      props: {
        name: 'aws_sqs_queue.notify',
        prov: {
          tier: 'Dynamic',
          confidence_tier: 'Confirmed',
          evidence: [
            {
              repo: 'local/shop',
              path: 'infra/notify.tf',
              byte_start: 8,
              byte_end: 64,
              commit_sha: 'abc123',
            },
          ],
          extractor_id: 't1.aws-describe',
          content_hash: '9'.repeat(64),
        },
      },
    },
    {
      id: 'gap:channel:notify',
      label: 'Gap',
      props: {
        name: 'GAP: runtime-computed channel identity',
        reason: 'runtime-computed channel identity',
        prov: DEPLOYED_FLOW.hops[2].provenance,
      },
    },
  ],
  nodes_total: 8,
  edges: [
    {
      src: 'ep:POST:/orders',
      dst: 'sym:orders#create',
      label: 'HANDLES',
      props: { prov: VERIFIED.hops[0].provenance },
    },
    {
      src: 'sym:orders#create',
      dst: 'sym:orders#persist',
      label: 'CALLS',
      props: { prov: VERIFIED.hops[1].provenance },
    },
    {
      src: 'ep:POST:/notify',
      dst: 'sym:notify#send',
      label: 'HANDLES',
      props: { prov: DEPLOYED_FLOW.hops[0].provenance },
    },
    {
      src: 'sym:notify#send',
      dst: 'sym:notify#guess',
      label: 'CALLS',
      props: { prov: DEPLOYED_FLOW.hops[1].provenance },
    },
    {
      src: 'sym:notify#send',
      dst: 'gap:channel:notify',
      label: 'PUBLISHES',
      props: {
        gap_reason: 'runtime-computed channel identity',
        attempted_tiers: ['T0', 'T1', 'T2', 'T3'],
        prov: DEPLOYED_FLOW.hops[2].provenance,
      },
    },
    {
      src: 'res:aws_sqs_queue.notify',
      dst: 'gap:channel:notify',
      label: 'BACKS',
      props: {
        prov: {
          tier: 'Dynamic',
          confidence_tier: 'Confirmed',
          evidence: [
            {
              repo: 'local/shop',
              path: 'infra/notify.tf',
              byte_start: 8,
              byte_end: 64,
              commit_sha: 'abc123',
            },
          ],
          extractor_id: 't1.aws-describe',
          content_hash: '8'.repeat(64),
        },
      },
    },
  ],
  edges_total: 6,
  flows: [VERIFIED, DEPLOYED_FLOW],
  flows_total: 2,
  deployments: [
    {
      resource_id: 'res:aws_sqs_queue.notify',
      source: 't1.aws-describe',
      provenance: {
        tier: 'Dynamic',
        confidence_tier: 'Confirmed',
        evidence: [
          {
            repo: 'local/shop',
            path: 'infra/notify.tf',
            byte_start: 8,
            byte_end: 64,
            commit_sha: 'abc123',
          },
        ],
        extractor_id: 't1.aws-describe',
        content_hash: '9'.repeat(64),
      },
      backed_channels: ['gap:channel:notify'],
    },
  ],
  truncated: false,
};

export const RenderedFlowAndDeploymentViewMatchesProjection: Story = {
  // AC-0246, AC-0247 (T-0246, T-0247): flow and deployment views render as a
  // per-flow subset of the host projection bound to its content identity;
  // selecting a node or hop opens its evidence, tier, and confidence, and
  // missing host projections fall back to Mermaid text without fabricating a
  // client-derived diagram.
  args: {
    flows: [DEPLOYED_FLOW, VERIFIED],
    dossier: projectedDossier([DEPLOYED_FLOW, VERIFIED], 'best-effort'),
    view: FLOW_DOSSIERS_HOST_VIEW,
    artifact: FLOW_DOSSIERS_ARTIFACT,
    onSelectNode: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const diagram = within(canvas.getByTestId('spec-diagram-flow-dossiers'));
    await expect(diagram.getByTestId('spec-diagram-canvas')).toBeInTheDocument();

    const identity = diagram.getByTestId('diagram-content-identity');
    await expect(identity).toHaveAttribute('data-schema', 'cartograph.spec/flow-dossiers@1');
    await expect(identity).toHaveAttribute(
      'data-content-id',
      'spec-artifact-v1:flow-dossiers-digest-abcdef123456',
    );
    await expect(diagram.getByTestId('diagram-subset-notice')).toHaveTextContent(
      'Subset of flow-dossiers view · spec-artifact-v1:flow-dossier',
    );
    await expect(diagram.getByTestId('diagram-deployments')).toHaveTextContent(
      'DEPLOYED (t1.aws-describe)',
    );

    // Only the selected flow's participants are retained in the subset diagram.
    await expect(diagram.getByLabelText('Diagram nodes')).toHaveTextContent('guessedTarget');
    await expect(diagram.getByLabelText('Diagram nodes')).not.toHaveTextContent('createOrder');

    const edgeIndex = within(diagram.getByLabelText('Diagram edges and hops'));
    const gapHop = edgeIndex.getByRole('button', {
      name: /GAP PUBLISHES \[UNRESOLVED: runtime-computed channel identity\]/,
    });
    await expect(gapHop).toHaveClass('unresolved');
    await expect(getComputedStyle(gapHop).borderStyle).toBe('dashed');

    await userEvent.click(gapHop);
    const evidence = diagram.getByTestId('diagram-selected-evidence');
    await expect(evidence).toHaveTextContent('GAP · PUBLISHES');
    await expect(evidence).toHaveTextContent(
      'Unresolved hop — runtime-computed channel identity',
    );
    await expect(evidence).toHaveTextContent('Gap');
    await expect(args.onOpenResolution).toHaveBeenCalledWith('gap:channel:notify');

    // Switching to verified-only when the host view is best-effort does not
    // fabricate a client-derived diagram; it shows the unavailable notice and
    // keeps the verified-only Mermaid dossier fallback.
    await userEvent.click(canvas.getByRole('button', { name: 'verified-only' }));
    await expect(canvas.queryByTestId('spec-diagram-canvas')).not.toBeInTheDocument();
    await expect(canvas.getByTestId('diagram-view-unavailable')).toHaveTextContent(
      STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
    );
  },
};

