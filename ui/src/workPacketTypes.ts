import type { FactKey } from './primarySourceStore';
import type { Omission, SpecExportMode } from './store';

export type PacketJson = null | boolean | number | string | PacketJson[] | { [key: string]: PacketJson };
export type PacketKnown<T = PacketJson> = { state: 'supplied'; value: T; origin: { kind: 'requested_intent' } | { kind: 'evidence'; evidence_ids: string[] } } |
  { state: 'missing'; omission: Omission };
export interface PacketFactSelection { reference: FactKey; expected_digest: string }
export interface PacketArtifactSelection { id: string; schema: string; content_id: string; mode: SpecExportMode }
export interface PacketDecisionSelection { content_hash: string; expected_record_hash: string }
export interface PacketSelectionSnapshot {
  snapshot_id: string;
  facts: PacketFactSelection[];
  interpretation_decisions: PacketDecisionSelection[];
}
export interface PacketReview { revision: number; status: 'draft' | 'reviewed' | 'changes_requested'; note: string | null }
export interface PacketFreshness {
  state: 'current' | 'stale' | 'conflicted' | 'unknown'; checked_at: string | null;
  observed_snapshot_id: string | null; checks: PacketJson[]; limitations: PacketJson[];
}
export interface PacketBody {
  schema_version: 1;
  identity: { [key: string]: PacketJson };
  intent: { [key: string]: PacketJson };
  evidence: { [key: string]: PacketJson };
  work: { [key: string]: PacketJson };
  acceptance: { [key: string]: PacketJson };
  handoff: { [key: string]: PacketJson };
  return: { [key: string]: PacketJson };
}
export interface PacketDetail {
  packet_id: string; body: PacketBody; review: PacketReview;
  lineage: { predecessor_ids: string[] }; freshness: PacketFreshness; execution: PacketExecution;
  dispatch?: PacketDispatch;
}
/** `dispatched`: an agent's turn ended without a commit; `submitted`: its change is a local branch commit (ADR-0092). */
export type PacketExecution = 'unoffered' | 'dispatched' | 'submitted';
export interface PacketDispatch {
  reviewed_revision: number; branch: string; base_revision: string; head_revision: string | null; stop: string;
  changed_paths: string[]; changed_paths_omitted: number; refused_requests: number;
}
export interface PacketSummary { packet_id: string; objective?: PacketKnown<string>; review: PacketReview; freshness?: PacketFreshness }
export interface PacketIntent {
  objective: PacketKnown<string>; expected_outcome: PacketKnown<string>; current_behavior: PacketKnown<string>;
  intended_behavior: PacketKnown<string>; scope: PacketKnown<string>; non_goals: PacketKnown<string>; business_rationale: PacketKnown<string>;
  requested_by?: string; requested_actor?: string; requested_journey?: string;
}
export interface PacketComponent { repo_key: string; path: string | null; fact_references: FactKey[] }
export interface PacketDependency { description: string; evidence_ids: string[] }
export interface PacketProcedure { id: string; description: string; steps: string[]; expected_evidence: string[]; evidence_ids: string[] }
export interface PacketObservation { id: string; description: string; evidence_ids: string[] }
export interface PacketWork {
  affected_components: PacketKnown<PacketComponent[]>; proposed_approach: PacketKnown<string[]>;
  dependencies: PacketKnown<PacketDependency[]>; open_decisions: PacketKnown<string[]>;
}
export interface PacketAcceptance {
  positive_cases: PacketKnown<PacketProcedure[]>; negative_cases: PacketKnown<PacketProcedure[]>; proposed_verification: PacketKnown<PacketProcedure[]>;
  discovered_tests: PacketObservation[]; structural_test_associations: PacketObservation[]; execution_evidence: PacketObservation[];
  coverage_evidence: PacketObservation[]; assertion_support: PacketObservation[];
}
export interface PacketHandoff {
  requested_capabilities: PacketKnown<string[]>; checkout_boundaries: PacketKnown<string[]>; tool_and_egress_requests: PacketKnown<string[]>;
  budgets: PacketKnown<{ resource: string; limit: string }[]>; proposed_role: PacketKnown<string>; authorization_references: PacketKnown<string[]>;
}
export interface PacketReturn {
  implementation_revision: PacketKnown<string>; pull_request: PacketKnown<string>; execution_receipts: PacketKnown<string[]>;
  source_configuration_changes: PacketKnown<string[]>; reingest_comparison: PacketKnown<string>; residual_gaps: PacketKnown<string[]>;
}
export interface GeneratePacketRequest {
  expected_snapshot_id: string; facts: PacketFactSelection[]; artifacts: PacketArtifactSelection[];
  interpretation_decisions: PacketDecisionSelection[]; intent: PacketIntent; work: PacketWork;
  acceptance: PacketAcceptance; handoff: PacketHandoff; return: PacketReturn; supersedes?: string[];
}
export const packetFactLabel = (reference: FactKey) => reference.kind === 'node' ? reference.id :
  `${reference.source} → ${reference.label} → ${reference.destination}`;
export const packetFactKey = (reference: FactKey) => JSON.stringify(reference.kind === 'node' ? ['node', reference.id] :
  ['edge', reference.source, reference.label, reference.destination]);
export const missingPacketField = (field: string): PacketKnown => ({ state: 'missing', omission: {
  class: 'missing_input', seam: 'interpretation', detail: `${field} was not supplied`,
} });

export interface PacketExport { packet_id: string; file_name: string; content: string }
