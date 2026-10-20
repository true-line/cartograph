import detailData from './fixtures/work-packet-detail.json';
import requestData from './fixtures/work-packet-request.json';
import markdown from './fixtures/work-packet-export.md?raw';
import reviewedData from './fixtures/work-packet-reviewed.json';
import reviewedMarkdown from './fixtures/work-packet-reviewed-export.md?raw';
import type { AssertionDecisionRecord, GraphEdge, GraphNode, SpecArtifact, SpecBundle } from './store';
import type { GeneratePacketRequest, PacketDetail, PacketKnown } from './workPacketTypes';

/** Emitted by the Rust PacketStore product fixture test; never hand-authored packet bodies. */
export const packetFixture = detailData as unknown as PacketDetail;
export const packetRequestFixture = requestData as unknown as GeneratePacketRequest;
export const packetExportFixture = markdown;
export const packetReviewedFixture = reviewedData as unknown as PacketDetail;
export const packetReviewedExportFixture = reviewedMarkdown;
const evidence = detailData.body.evidence as unknown as {
  facts: { fact: { kind: 'node' | 'edge'; value: GraphNode | GraphEdge }; review_annotations: { record: AssertionDecisionRecord }[] }[];
  artifacts: { artifact: PacketKnown<SpecArtifact> }[];
  interpretations: { historical_decision: { record: AssertionDecisionRecord } | null }[];
};
export const packetGraphFixture = {
  nodes: evidence.facts.filter((item) => item.fact.kind === 'node').map((item) => item.fact.value as GraphNode),
  edges: evidence.facts.filter((item) => item.fact.kind === 'edge').map((item) => item.fact.value as GraphEdge),
};
export const packetDecisionFixtures = [...evidence.facts.flatMap((item) => item.review_annotations.map((annotation) => annotation.record)),
  ...evidence.interpretations.flatMap((item) => item.historical_decision ? [item.historical_decision.record] : [])]
  .filter((record, index, records) => records.findIndex((candidate) => candidate.assertion.provenance.content_hash === record.assertion.provenance.content_hash) === index);
export const packetBundleFixture: SpecBundle = {
  mode: packetRequestFixture.artifacts[0]?.mode ?? 'best-effort',
  artifacts: evidence.artifacts.flatMap((item) => item.artifact.state === 'supplied' ? [{ ...item.artifact.value,
    assertions_total: item.artifact.value.assertions.length, assertions_truncated: false, content_truncated: false }] : []),
  assertion_count: 0, gap_count: 0, drift_count: 0, security_count: 0,
};
