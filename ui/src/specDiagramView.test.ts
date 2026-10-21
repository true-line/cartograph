import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { FlowsCard, projectedDossier } from './components/FlowsCard';
import {
  STRUCTURED_VIEW_UNAVAILABLE_NOTICE,
  SpecDiagramCanvas,
  diagramEdgeLabel,
  diagramNodeLabel,
  diagramNodeShapeClass,
  diagramSubsetNotice,
  diagramTruncationNotice,
  isGapEdgeFact,
  isGapNodeFact,
  participantKindFromId,
  subsetFlowDiagramView,
  verifyDiagramViewIdentity,
} from './components/SpecDiagramView';
import { SpecWorkbench } from './components/SpecWorkbench';
import { TopologyCard } from './components/TopologyCard';
import type {
  Flow,
  GraphEdge,
  GraphNode,
  SpecArtifact,
  SpecBundle,
  SpecDiagramView,
} from './store';

// AC-0246, AC-0247 (T-0246, T-0247): structured diagram projection and identity verification.
describe('SpecDiagramView identity and structured projection (AC-0246, AC-0247)', () => {
  const sampleArtifact: SpecArtifact = {
    id: 'topology',
    file_name: 'topology.md',
    title: 'Resource topology',
    format: 'markdown',
    schema: 'cartograph.spec/topology@1',
    content_id: 'spec-artifact-v1:1111222233334444',
    content: '# Resource topology\n',
    content_truncated: false,
    content_byte_len: 20,
    assertions: [],
    assertions_truncated: false,
    assertions_total: 0,
  };

  it('verifies that a diagram view matches the exported artifact schema and content_id', () => {
    const view: SpecDiagramView = {
      artifact_id: 'topology',
      schema: 'cartograph.spec/topology@1',
      content_id: 'spec-artifact-v1:1111222233334444',
      mode: 'best-effort',
      nodes: [],
      nodes_total: 0,
      edges: [],
      edges_total: 0,
      flows_total: 0,
      truncated: false,
    };
    expect(verifyDiagramViewIdentity(view, sampleArtifact)).toBe(true);
    expect(
      verifyDiagramViewIdentity(
        { ...view, content_id: 'spec-artifact-v1:different' },
        sampleArtifact,
      ),
    ).toBe(false);
    expect(
      verifyDiagramViewIdentity(
        { ...view, schema: 'cartograph.spec/topology@2' },
        sampleArtifact,
      ),
    ).toBe(false);
  });

  it('fails closed when the exported artifact is missing or content_id is mismatched', () => {
    const view: SpecDiagramView = {
      artifact_id: 'topology',
      schema: 'cartograph.spec/topology@1',
      content_id: 'spec-artifact-v1:1111222233334444',
      mode: 'best-effort',
      nodes: [
        {
          id: 'res:aws_sqs_queue.orders',
          label: 'Resource',
          props: { name: 'aws_sqs_queue.orders' },
        },
      ],
      nodes_total: 1,
      edges: [],
      edges_total: 0,
      flows_total: 0,
      truncated: false,
    };
    expect(verifyDiagramViewIdentity(view, null)).toBe(false);
    expect(verifyDiagramViewIdentity(view, undefined)).toBe(false);

    const missingHtml = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, { view, artifact: null }),
    );
    expect(missingHtml).toContain('data-testid="diagram-identity-mismatch"');
    expect(missingHtml).toContain('Refused to render diagram');
    expect(missingHtml).toContain('missing');

    const tamperedView: SpecDiagramView = {
      ...view,
      content_id: 'spec-artifact-v1:derived-or-tampered',
    };
    expect(verifyDiagramViewIdentity(tamperedView, sampleArtifact)).toBe(false);
    const tamperedHtml = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, { view: tamperedView, artifact: sampleArtifact }),
    );
    expect(tamperedHtml).toContain('data-testid="diagram-identity-mismatch"');
    expect(tamperedHtml).not.toContain('data-testid="spec-diagram-canvas"');
  });

  it('renders a visible bounded-view notice when truncated and never presents it as complete', () => {
    const truncatedView: SpecDiagramView = {
      artifact_id: 'topology',
      schema: 'cartograph.spec/topology@1',
      content_id: 'spec-artifact-v1:1111222233334444',
      mode: 'best-effort',
      nodes: [
        {
          id: 'res:aws_sqs_queue.orders',
          label: 'Resource',
          props: { name: 'aws_sqs_queue.orders' },
        },
      ],
      nodes_total: 1620,
      edges: [],
      edges_total: 3180,
      flows_total: 0,
      truncated: true,
    };

    expect(diagramTruncationNotice(truncatedView)).toBe(
      'Showing 1 of 1620 nodes, 0 of 3180 edges. The full artifact text is complete; the diagram is a bounded view.',
    );

    const html = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, {
        view: truncatedView,
        artifact: sampleArtifact,
      }),
    );
    expect(html).toContain('data-testid="diagram-truncated-notice"');
    expect(html).toContain(
      'Showing 1 of 1620 nodes, 0 of 3180 edges. The full artifact text is complete; the diagram is a bounded view.',
    );
    expect(html).toContain('Showing 1 of 1620 nodes · 0 of 3180 edges (bounded view)');
    expect(html).not.toContain('Showing all');
    expect(html).not.toContain('0 hidden');
  });

  it('maps unrecognized participant prefixes to Unknown and styles Unknown neutrally', () => {
    expect(participantKindFromId('sym:orders#create')).toBe('Symbol');
    expect(participantKindFromId('ep:POST:/orders')).toBe('Endpoint');
    expect(participantKindFromId('gap:missing')).toBe('Gap');
    expect(participantKindFromId('custom:worker')).toBe('Unknown');
    expect(participantKindFromId('unprefixed')).toBe('Unknown');

    const flowArtifact: SpecArtifact = {
      ...sampleArtifact,
      id: 'flow-dossiers',
      schema: 'cartograph.spec/flow-dossiers@1',
      content_id: 'spec-artifact-v1:flow-hash-123456',
    };
    const unknownNode: GraphNode = {
      id: 'custom:trigger',
      label: participantKindFromId('custom:trigger'),
      props: {
        name: 'Custom trigger',
        prov: {
          tier: 'Deterministic',
          confidence_tier: 'Confirmed',
          evidence: [],
          extractor_id: 't0.custom',
          content_hash: '7'.repeat(64),
        },
      },
    };
    expect(unknownNode.label).toBe('Unknown');
    expect(diagramNodeShapeClass(unknownNode)).toBe('diagram-unknown');

    const view: SpecDiagramView = {
      artifact_id: 'flow-dossiers',
      schema: flowArtifact.schema,
      content_id: flowArtifact.content_id,
      mode: 'best-effort',
      nodes: [unknownNode],
      nodes_total: 1,
      edges: [],
      edges_total: 0,
      flows_total: 1,
      truncated: false,
    };

    const html = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, { view, artifact: flowArtifact }),
    );
    expect(html).toContain('diagram-node-chip kind-unknown');
  });

  it('formats deployed resource labels and keeps Gap nodes and edges textually distinct', () => {
    const deployedNode: GraphNode = {
      id: 'res:aws_sqs_queue.orders',
      label: 'Resource',
      props: {
        name: 'aws_sqs_queue.orders',
        prov: {
          tier: 'Dynamic',
          confidence_tier: 'Confirmed',
          evidence: [],
          extractor_id: 't1.aws-describe',
          content_hash: 'a'.repeat(64),
        },
      },
    };
    expect(
      diagramNodeLabel(deployedNode, {
        resource_id: deployedNode.id,
        source: 't1.aws-describe',
        provenance: deployedNode.props.prov as SpecArtifact['assertions'][number]['provenance'],
        backed_channels: ['chan:orders.queue'],
      }),
    ).toBe('aws_sqs_queue.orders [DEPLOYED · T1]');

    const gapNode: GraphNode = {
      id: 'gap:channel:orders',
      label: 'Gap',
      props: {
        name: 'GAP: dynamic topic',
        reason: 'dynamic topic',
        prov: {
          tier: 'Deterministic',
          confidence_tier: 'Gap',
          evidence: [],
          extractor_id: 't0.events',
          content_hash: 'd'.repeat(64),
        },
      },
    };
    expect(isGapNodeFact(gapNode)).toBe(true);
    expect(diagramNodeLabel(gapNode)).toContain('[UNRESOLVED]');

    const gapEdge: GraphEdge = {
      src: 'ep:POST:/orders',
      dst: 'gap:channel:orders',
      label: 'PUBLISHES',
      props: {
        gap_reason: 'dynamic topic',
        prov: gapNode.props.prov,
      },
    };
    expect(isGapEdgeFact(gapEdge)).toBe(true);
    expect(diagramEdgeLabel(gapEdge)).toBe('GAP PUBLISHES [UNRESOLVED: dynamic topic]');
  });

  it('builds a per-flow subset from the host view, keeps host identity, sets subset_of, and reports missing truncated participants without inventing nodes', () => {
    const flowArtifact: SpecArtifact = {
      ...sampleArtifact,
      id: 'flow-dossiers',
      file_name: 'flow_dossiers.md',
      title: 'Flow dossiers',
      schema: 'cartograph.spec/flow-dossiers@1',
      content_id: 'spec-artifact-v1:flow-digest-abcdef123456',
    };
    const prov = {
      tier: 'Deterministic',
      confidence_tier: 'Confirmed' as const,
      evidence: [],
      extractor_id: 't0.flow',
      content_hash: 'a'.repeat(64),
    };
    const ordersFlow: Flow = {
      trigger: 'ep:POST:/orders',
      trigger_kind: 'Endpoint',
      trigger_name: 'POST /orders',
      status: 'Verified',
      score: 1,
      depth_limited: false,
      hops: [
        {
          label: 'HANDLES',
          src: 'ep:POST:/orders',
          dst: 'sym:orders#create',
          src_name: 'POST /orders',
          dst_name: 'createOrder',
          tier: 'Deterministic',
          confidence: 'Confirmed',
          evidence: null,
          gap_reason: null,
          attempted_tiers: [],
          provenance: prov,
        },
        {
          label: 'PUBLISHES',
          src: 'sym:orders#create',
          dst: 'res:aws_sqs_queue.orders',
          src_name: 'createOrder',
          dst_name: 'aws_sqs_queue.orders',
          tier: 'Deterministic',
          confidence: 'Confirmed',
          evidence: null,
          gap_reason: null,
          attempted_tiers: [],
          provenance: prov,
        },
      ],
    };
    const notifyFlow: Flow = {
      trigger: 'ep:POST:/notify',
      trigger_kind: 'Endpoint',
      trigger_name: 'POST /notify',
      status: 'Verified',
      score: 1,
      depth_limited: false,
      hops: [
        {
          label: 'HANDLES',
          src: 'ep:POST:/notify',
          dst: 'sym:notify#send',
          src_name: 'POST /notify',
          dst_name: 'sendNotify',
          tier: 'Deterministic',
          confidence: 'Confirmed',
          evidence: null,
          gap_reason: null,
          attempted_tiers: [],
          provenance: prov,
        },
      ],
    };

    const hostView: SpecDiagramView = {
      artifact_id: 'flow-dossiers',
      schema: flowArtifact.schema,
      content_id: flowArtifact.content_id,
      mode: 'best-effort',
      nodes: [
        { id: 'ep:POST:/orders', label: 'Endpoint', props: { name: 'POST /orders', prov } },
        { id: 'sym:orders#create', label: 'Symbol', props: { name: 'createOrder', prov } },
        { id: 'res:aws_sqs_queue.orders', label: 'Resource', props: { name: 'aws_sqs_queue.orders', prov } },
        { id: 'ep:POST:/notify', label: 'Endpoint', props: { name: 'POST /notify', prov } },
        { id: 'sym:notify#send', label: 'Symbol', props: { name: 'sendNotify', prov } },
        { id: 'res:aws_sqs_queue.notify', label: 'Resource', props: { name: 'aws_sqs_queue.notify', prov } },
      ],
      nodes_total: 6,
      edges: [
        { src: 'ep:POST:/orders', dst: 'sym:orders#create', label: 'HANDLES', props: { prov } },
        { src: 'sym:orders#create', dst: 'res:aws_sqs_queue.orders', label: 'PUBLISHES', props: { prov } },
        { src: 'ep:POST:/notify', dst: 'sym:notify#send', label: 'HANDLES', props: { prov } },
      ],
      edges_total: 3,
      flows: [ordersFlow, notifyFlow],
      flows_total: 2,
      deployments: [
        {
          resource_id: 'res:aws_sqs_queue.orders',
          source: 't1.aws-describe',
          provenance: prov,
          backed_channels: ['res:aws_sqs_queue.orders', 'chan:unrelated'],
        },
        {
          resource_id: 'res:aws_sqs_queue.notify',
          source: 't1.aws-describe',
          provenance: prov,
          backed_channels: [],
        },
      ],
      truncated: false,
    };

    const subset = subsetFlowDiagramView(hostView, 'ep:POST:/orders');
    expect(subset).not.toBeNull();
    expect(verifyDiagramViewIdentity(subset!, flowArtifact)).toBe(true);
    expect(subset?.artifact_id).toBe(hostView.artifact_id);
    expect(subset?.schema).toBe(hostView.schema);
    expect(subset?.content_id).toBe(hostView.content_id);
    expect(subset?.subset_of).toEqual({ flow_trigger: 'ep:POST:/orders' });
    expect(subset?.nodes.map((node) => node.id)).toEqual([
      'ep:POST:/orders',
      'sym:orders#create',
      'res:aws_sqs_queue.orders',
    ]);
    expect(subset?.nodes_total).toBe(3);
    expect(subset?.edges).toHaveLength(2);
    expect(subset?.edges_total).toBe(2);
    expect(subset?.flows?.map((flow) => flow.trigger)).toEqual(['ep:POST:/orders']);
    expect(subset?.flows_total).toBe(1);
    expect(subset?.truncated).toBe(false);
    expect(subset?.deployments).toEqual([
      {
        resource_id: 'res:aws_sqs_queue.orders',
        source: 't1.aws-describe',
        provenance: prov,
        backed_channels: ['res:aws_sqs_queue.orders', 'chan:unrelated'],
      },
    ]);
    expect(diagramSubsetNotice(subset!)).toBe(
      'Subset of flow-dossiers view · spec-artifact-v1:flow-digest-',
    );

    const subsetHtml = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, { view: subset!, artifact: flowArtifact }),
    );
    expect(subsetHtml).toContain('data-testid="diagram-subset-notice"');
    expect(subsetHtml).toContain(
      'Subset of flow-dossiers view · spec-artifact-v1:flow-digest-',
    );

    // When the host view was truncated and a hop endpoint is missing from view.nodes,
    // subsetFlowDiagramView reports the missing participant count and never invents a node.
    const truncatedHostView: SpecDiagramView = {
      ...hostView,
      nodes: hostView.nodes.filter((node) => node.id !== 'res:aws_sqs_queue.orders'),
      nodes_total: 1500,
      truncated: true,
    };
    const truncatedSubset = subsetFlowDiagramView(truncatedHostView, 'ep:POST:/orders');
    expect(truncatedSubset).not.toBeNull();
    expect(truncatedSubset?.nodes.map((node) => node.id)).toEqual([
      'ep:POST:/orders',
      'sym:orders#create',
    ]);
    expect(truncatedSubset?.nodes_total).toBe(3);
    expect(truncatedSubset?.edges).toHaveLength(1);
    expect(truncatedSubset?.edges_total).toBe(2);
    expect(truncatedSubset?.truncated).toBe(true);
    expect(truncatedSubset?.subset_of).toEqual({
      flow_trigger: 'ep:POST:/orders',
      missing_participants: 1,
    });
    expect(diagramSubsetNotice(truncatedSubset!)).toContain(
      '1 participant not in the host view (truncated)',
    );

    const truncatedHtml = renderToStaticMarkup(
      createElement(SpecDiagramCanvas, { view: truncatedSubset!, artifact: flowArtifact }),
    );
    expect(truncatedHtml).toContain('1 participant not in the host view (truncated)');
    expect(truncatedHtml).not.toContain('aws_sqs_queue.orders');
  });

  it('does not derive a client-side diagram when no host view is available, showing Mermaid fallback and notice instead', () => {
    const mermaid = 'flowchart LR\n    res_orders["aws_sqs_queue.orders"]\n';
    const topologyHtml = renderToStaticMarkup(
      createElement(TopologyCard, {
        view: null,
        artifact: { ...sampleArtifact, content: mermaid },
      }),
    );
    expect(topologyHtml).not.toContain('data-testid="spec-diagram-canvas"');
    expect(topologyHtml).toContain('data-testid="diagram-view-unavailable"');
    expect(topologyHtml).toContain(STRUCTURED_VIEW_UNAVAILABLE_NOTICE);
    expect(topologyHtml).toContain('data-testid="topology-mermaid"');
    expect(topologyHtml).toContain('aws_sqs_queue.orders');

    const sampleFlow: Flow = {
      trigger: 'ep:POST:/orders',
      trigger_kind: 'Endpoint',
      trigger_name: 'POST /orders',
      status: 'Verified',
      score: 1,
      depth_limited: false,
      hops: [],
    };
    const flowsHtml = renderToStaticMarkup(
      createElement(FlowsCard, {
        flows: [sampleFlow],
        view: null,
        artifact: {
          ...sampleArtifact,
          id: 'flow-dossiers',
          file_name: 'flow_dossiers.md',
          schema: 'cartograph.spec/flow-dossiers@1',
          content: projectedDossier([sampleFlow], 'best-effort'),
        },
      }),
    );
    expect(flowsHtml).not.toContain('data-testid="spec-diagram-canvas"');
    expect(flowsHtml).toContain('data-testid="diagram-view-unavailable"');
    expect(flowsHtml).toContain(STRUCTURED_VIEW_UNAVAILABLE_NOTICE);
    expect(flowsHtml).toContain('data-testid="flows-dossier"');

    const bundle: SpecBundle = {
      mode: 'best-effort',
      artifacts: [{ ...sampleArtifact, content: mermaid }],
      assertion_count: 0,
      gap_count: 0,
      drift_count: 0,
      security_count: 0,
    };
    const workbenchHtml = renderToStaticMarkup(
      createElement(SpecWorkbench, {
        bundle,
        views: null,
        mode: 'best-effort',
        decisions: [],
        busy: false,
        error: null,
        canCurate: false,
        onModeChange: () => {},
        onCurate: () => {},
        onCopyArtifact: () => {},
        onExportBundle: () => {},
      }),
    );
    expect(workbenchHtml).not.toContain('data-testid="spec-diagram-canvas"');
    expect(workbenchHtml).toContain('data-testid="diagram-view-unavailable"');
    expect(workbenchHtml).toContain(STRUCTURED_VIEW_UNAVAILABLE_NOTICE);
    expect(workbenchHtml).toContain('data-testid="spec-artifact-source"');
  });

  it('uses official artifact content for copyable source, shows truncation notice when preview is capped, and shows no copyable source when the artifact is missing', () => {
    const sampleFlow: Flow = {
      trigger: 'ep:POST:/orders',
      trigger_kind: 'Endpoint',
      trigger_name: 'POST /orders',
      status: 'Verified',
      score: 1,
      depth_limited: false,
      hops: [],
    };

    const truncatedTopologyHtml = renderToStaticMarkup(
      createElement(TopologyCard, {
        view: null,
        artifact: {
          ...sampleArtifact,
          content: 'flowchart LR\n    res_orders["aws_sqs_queue.orders"]\n',
          content_truncated: true,
          content_byte_len: 600_000,
        },
      }),
    );
    expect(truncatedTopologyHtml).toContain('data-testid="spec-artifact-truncated-note"');
    expect(truncatedTopologyHtml).toContain('600,000 bytes');

    const missingTopologyHtml = renderToStaticMarkup(
      createElement(TopologyCard, {
        view: null,
        artifact: null,
      }),
    );
    expect(missingTopologyHtml).toContain('No resources recovered yet');
    expect(missingTopologyHtml).not.toContain('data-testid="topology-mermaid"');

    const truncatedFlowsHtml = renderToStaticMarkup(
      createElement(FlowsCard, {
        flows: [sampleFlow],
        view: null,
        artifact: {
          ...sampleArtifact,
          id: 'flow-dossiers',
          file_name: 'flow_dossiers.md',
          schema: 'cartograph.spec/flow-dossiers@1',
          content: '# Flow dossiers\n\n## Official projection\n',
          content_truncated: true,
          content_byte_len: 750_000,
        },
      }),
    );
    expect(truncatedFlowsHtml).toContain('data-testid="flows-dossier"');
    expect(truncatedFlowsHtml).toContain('Official projection');
    expect(truncatedFlowsHtml).toContain('data-testid="spec-artifact-truncated-note"');
    expect(truncatedFlowsHtml).toContain('750,000 bytes');

    const missingFlowsHtml = renderToStaticMarkup(
      createElement(FlowsCard, {
        flows: [sampleFlow],
        view: null,
        artifact: null,
      }),
    );
    expect(missingFlowsHtml).toContain('data-testid="diagram-view-unavailable"');
    expect(missingFlowsHtml).not.toContain('data-testid="flows-dossier"');
  });

  it('keeps the deployed resources the host joined to a flow in its subset', () => {
    const flow = {
      trigger: 'ep:POST:/orders',
      trigger_kind: 'Endpoint',
      trigger_name: 'POST /orders',
      hops: [
        {
          label: 'HANDLES',
          src: 'ep:POST:/orders',
          dst: 'sym:orders#create',
          src_name: 'POST /orders',
          dst_name: 'createOrder',
        },
      ],
    } as unknown as Flow;
    const host = {
      artifact_id: 'flow-dossiers',
      schema: 'schema',
      content_id: 'id',
      mode: 'best-effort',
      nodes: [
        { id: 'ep:POST:/orders', label: 'Endpoint', props: {} },
        { id: 'sym:orders#create', label: 'Symbol', props: {} },
        { id: 'res:aws_lambda_function.orders', label: 'Resource', props: {} },
        { id: 'res:aws_s3_bucket.unrelated', label: 'Resource', props: {} },
      ],
      nodes_total: 4,
      edges: [
        { src: 'ep:POST:/orders', dst: 'sym:orders#create', label: 'HANDLES', props: {} },
        { src: 'res:aws_lambda_function.orders', dst: 'sym:orders#create', label: 'BACKS', props: {} },
      ],
      edges_total: 2,
      flows: [flow],
      flows_total: 1,
      truncated: false,
    } as SpecDiagramView;
    const subset = subsetFlowDiagramView(host, 'ep:POST:/orders');
    expect(subset?.nodes.map((node) => node.id)).toEqual([
      'ep:POST:/orders',
      'sym:orders#create',
      'res:aws_lambda_function.orders',
    ]);
    expect(subset?.edges.map((edge) => edge.label)).toEqual(['HANDLES', 'BACKS']);
    expect(subset?.truncated).toBe(false);
  });
});
