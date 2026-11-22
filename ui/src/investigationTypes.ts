/** SPEC-10 transport DTOs. Findings/history are separate from edge proposals.
 * Snake-case fields mirror the host; command argument names remain Tauri camelCase.
 * No execution token, live source path, raw transcript or model reasoning is exposed.
 */
import type { EgressPreview } from './components/EgressConsentDialog';
import type { FactKey } from './primarySourceStore';
import type { EvidenceRef, TaskSourceBasisV2 } from './store';

export type { SpecialistId } from './generated/specialistTable';
import type { SpecialistId } from './generated/specialistTable';
export type InvestigationProviderMode = 'local' | 'cloud';
/** The existing context-hub QueryScope wire shape, not a filesystem scope. */
export type InvestigationScope = { type: 'all' } |
  { type: 'neighborhood'; anchor: string; hops: number };
/** Immutable UI context captured from one validated saved parent before follow-up dispatch. */
export interface InvestigationFollowUpContext {
  parent_id: string;
  conversation_id: string;
  specialist_id: SpecialistId;
  scope: InvestigationScope;
  provider_mode: InvestigationProviderMode;
  saved_provider_id: string;
  saved_model: string;
}
export type InvestigationTool = 'query_context' | 'read_evidence' | 'read_coverage_inventory';

export interface InvestigationSpecialist {
  id: SpecialistId;
  name: string;
  version: number;
  prompt_fingerprint: string;
  purpose: string;
  operations: (InvestigationTool | 'replay_query' | 'finish')[];
  tier: 'Agentic';
  confidence_tier: 'InferredWeak';
}

export interface InvestigationProvider {
  mode: InvestigationProviderMode;
  provider_id: string;
  model: string;
  endpoint: string;
  deployment: string | null;
  /** Absent in older records; never infer the current protocol for that history. */
  protocol_version?: string;
  available: boolean;
  unavailable_reason: string | null;
}

/** The host may narrow these ceilings; the UI never promises exact token/cost use. */
export interface InvestigationLimits {
  profile: string;
  model_invocations: number;
  tool_actions: number;
  selected_facts: number;
  evidence_requests: number;
  evidence_items: number;
  evidence_bytes: number;
  captured_validation_bytes: number;
  generated_tokens_per_invocation: number;
  generated_token_reservations: number;
  active_seconds: number;
  consent_wait_seconds: number;
  wall_seconds: number;
}

export interface InvestigationPolicy {
  limits: InvestigationLimits;
  request_schema_version: 4 | 5 | 6 | 7;
  envelope_schema_version: 1 | 2;
  envelope_contract: string;
  max_completion_obligations: number;
  request_bytes: number;
  envelope_bytes: number;
  specialist_ids: SpecialistId[];
}

export interface InvestigationCatalog {
  schema_version: 1;
  specialists: InvestigationSpecialist[];
  providers: InvestigationProvider[];
  limits: InvestigationLimits;
  policies?: InvestigationPolicy[];
  /** Offered specialists whose desktop runs use the saved run limits (#1757). */
  configured_limit_specialists?: SpecialistId[];
}

export type InvestigationRangeRole = 'provenance' | 'rule_exit' | 'condition_branch' | 'condition_expression' |
  'return_value' | 'throw_value' | 'dependency' | 'dependency_declaration' | 'redaction' |
  'definition_declaration' | 'definition_use' | 'definition_initializer' | 'definition_expression' |
  'definition_dependency' | 'definition_dependency_declaration';

/** Host-resolved immutable target. The viewer inventory ordinal is not role/index. */
export interface InvestigationInspectionTarget {
  schema_version: 1;
  target_id: string;
  graph_snapshot_id: string;
  fact: FactKey;
  fact_digest: string;
  registered_source_id: string;
  receipt_id: string;
  receipt_inventory_index: number;
  role: InvestigationRangeRole;
  index: number;
  source: EvidenceRef;
  captured: Extract<TaskSourceBasisV2['evidence'][number]['origin'], { kind: 'captured_primary_source' }>['captured'];
}

export interface InvestigationInspectionAssessment {
  schema_version: 1;
  requirements_hash: string;
  input_ledger_hash: string;
  finishing_step_id: string;
  disclosure_hash: string;
  requirements: {
    target_id: string;
    target: InvestigationInspectionTarget;
    status: 'supplied' | 'unmet';
    acquired: boolean;
    supplied: boolean;
    cited: boolean;
  }[];
}

/** Host-computed coordinates from the exact retained file. Never checkout lines. */
export interface InvestigationCapturedLocation {
  target_id: string;
  captured: InvestigationInspectionTarget['captured'];
  byte_start: number;
  byte_end: number;
  start_line: number;
  start_column: number;
  end_line: number;
  end_column: number;
  column_unit: 'utf8_bytes';
  end_exclusive: true;
}

interface InvestigationStartFields {
  request_nonce: string;
  specialist_id: SpecialistId;
  question: string;
  scope: InvestigationScope;
  provider_mode: InvestigationProviderMode;
  limit_profile: string;
  expected_graph_revision?: string;
  conversation_id?: string;
  parent_id?: string;
}

export type StartInvestigationRequest = InvestigationStartFields & (
  { schema_version: 1; inspection_requirements?: never; expected_context_id?: never } |
  { schema_version: 2; inspection_requirements: InvestigationInspectionTarget[]; expected_context_id?: never } |
  { schema_version: 3; inspection_requirements?: InvestigationInspectionTarget[]; expected_context_id?: string; obligations?: never }
) | (Omit<InvestigationStartFields, 'expected_graph_revision' | 'conversation_id' | 'parent_id'> & {
  schema_version: 4 | 5 | 6 | 7; inspection_requirements?: never; obligations: InvestigationObligationEnvelope; expected_context_id: string;
  expected_graph_revision: string | null; conversation_id: string | null; parent_id: string | null;
});

/** Public preparation-time qualification; host-only proof fields are never UI authority. */
export interface InvestigationCoverageQualification {
  contract: 'coverage-qualification@1';
  exhaustive: false;
  coverage_id: string;
  repos: {
    repo: string;
    status: 'recorded' | 'stale' | 'unrecorded';
    commit: string | null;
    inventory_hash: string | null;
    limited: boolean | null;
    counts: { not_selected: number; missing_input: number; not_applicable: number; unsupported: number } | null;
    layer_hints: string[] | null;
    compared: string[];
  }[];
}

export interface InvestigationCoverageContext {
  context_id: string;
  coverage: InvestigationCoverageQualification;
}

export interface InvestigationCoverageAttestation {
  context_id: string;
  coverage_id: string;
  representation_hash: string;
}

export type InvestigationStatus = 'queued' | 'preparing' | 'running' |
  'awaiting_consent' | 'completed' | 'failed' | 'cancelled' | 'interrupted' |
  'outcome_unknown';

/** Bounded history summary. Question is the host's redacted saved representation. */
export interface InvestigationSummary {
  /** V10 uses schema 7; V7–V9 use 4–6, V6 uses 3, V5 uses 2; V1–V4 stay 1. */
  schema_version: 1 | 2 | 3 | 4 | 5 | 6 | 7;
  investigation_id: string;
  conversation_id: string;
  parent_id: string | null;
  job_id: number;
  specialist_id: SpecialistId;
  question: string;
  scope: InvestigationScope;
  provider_mode: InvestigationProviderMode;
  status: InvestigationStatus;
  revision: number;
  cancel_requested: boolean;
  invocation_pending: boolean;
  actions: { can_cancel: boolean; can_follow_up: boolean };
  graph_snapshot_id: string | null;
  last_event_sequence: number;
  has_result: boolean;
  created_at: string;
  updated_at: string;
  /** Absent on legacy history: never synthesize an assessed pass. */
  inspection_requirements?: InvestigationInspectionTarget[];
  inspection_assessment?: InvestigationInspectionAssessment | null;
  inspection_status?: 'pending' | 'supplied' | 'unmet' | 'not_assessed';
  inspection_requirement_count?: number;
  completion_outcome?: InvestigationCompletionOutcome;
  obligation_status?: InvestigationObligationStatus;
}

export interface InvestigationUsage {
  model_invocations: number;
  tool_actions: number;
  selected_facts: number;
  evidence_requests: number;
  evidence_items: number;
  evidence_bytes: number;
  captured_validation_bytes: number;
  generated_token_reservations: number;
  reported_input_tokens: number | null;
  reported_output_tokens: number | null;
  active_milliseconds: number;
}

/** App-only exact reports; numeric durable usage is not an exact JS transport. */
export interface ReportedTaskUsage {
  input_tokens: string | null; output_tokens: string | null; cache_read_tokens: string | null;
}

/** ADR-0085: the tool-calling runtime a run executed on (app-only read). */
export interface RunRuntime {
  lane: 'pi' | 'claude'; runtime: string; version: string; runner: string;
}

/** Definition/provider are the saved identities, not the current catalog entries. */
export type AdvisoryAuditSubject =
  | { kind: 'finding'; finding_index: number }
  | { kind: 'limitation'; limitation_index: number }
  | { kind: 'finding_limitation'; finding_index: number; limitation_index: number };
export type AdvisoryAuditAssessment =
  | { verdict: 'pass' | 'unsupported_scope' | 'indeterminate' }
  | { verdict: 'contradiction' | 'answer_coverage'; citation_id: string };
export type AdvisoryAudit =
  | { status: 'completed'; reviews: { subject: AdvisoryAuditSubject; assessment: AdvisoryAuditAssessment }[] }
  | { status: 'not_completed'; reason: 'transport' | 'timeout' | 'length' | 'invalid_output' };

export interface InvestigationDetail extends InvestigationSummary {
  advisory_audit?: AdvisoryAudit;
  reported_usage_exact?: ReportedTaskUsage;
  runtime?: RunRuntime;
  completion?: InvestigationCompletionDetail;
  coverage_context?: InvestigationCoverageContext;
  context_owner: string;
  origin: 'app';
  specialist: InvestigationSpecialist;
  provider: InvestigationProvider;
  scope_snapshot_id: string | null;
  limits: InvestigationLimits;
  usage: InvestigationUsage;
  citations: InvestigationCitation[];
  error: string | null;
}

export interface InvestigationPage {
  items: InvestigationSummary[];
  next_cursor: string | null;
  answer_match_ids?: string[];
  name_match_ids?: string[];
}

/** Saved provider attribution for the app-only active specialist activity view. */
export interface SavedActivityProvider {
  mode: InvestigationProviderMode;
  provider_id: string;
  model: string;
}

export interface SpecialistActivityItem {
  summary: InvestigationSummary;
  saved_provider: SavedActivityProvider;
  /** Exact provider reports; absent projection and null counts remain unknown. */
  reported_usage?: { input_tokens: string | null; output_tokens: string | null };
  reported_cache_read_tokens?: string | null;
}

export interface SpecialistActivityPage {
  items: SpecialistActivityItem[];
  next_cursor: string | null;
}

export type InvestigationEventKind = 'created' | 'preparation_started' |
  'context_prepared' | 'model_started' | 'model_completed' | 'action_admitted' |
  'tool_started' | 'tool_completed' | 'consent_required' | 'consent_approved' |
  'evidence_validation_reserved' | 'finish_refused' | 'source_replay_refused' | 'capacity_refused' | 'action_refused' | 'query_refused' | 'read_unavailable' | 'read_firewall_refused' |
  'consent_declined' | 'cancel_requested' | 'result_persisted' | 'completed' |
  'failed' | 'cancelled' | 'interrupted' | 'outcome_unknown';

/** Ordered durable activity; summary is fixed/sanitized host metadata, never thinking. */
export interface InvestigationEvent {
  investigation_id: string;
  sequence: number;
  revision: number;
  kind: InvestigationEventKind;
  step_id: string | null;
  tool: InvestigationTool | null;
  summary: string;
  created_at: string;
}

export interface InvestigationEventPage {
  investigation_id: string;
  items: InvestigationEvent[];
  /** Last sequence in this page, or the requested cursor for an empty page. */
  next_sequence: number;
  has_more: boolean;
}

/** Push messages invalidate observations; durable event pages remain authoritative. */
export interface InvestigationChanged {
  investigation_id: string;
  last_event_sequence: number;
  revision: number;
}

export type InvestigationClaimKind = 'implemented_behavior' | 'documented_intent' |
  'inferred_interpretation' | 'proposed_design';

export interface InvestigationFinding {
  finding_id: string;
  claim_kind: InvestigationClaimKind;
  title: string;
  statement: string;
  citation_ids: string[];
  limitations: string[];
  tier: 'Agentic';
  confidence_tier: 'InferredWeak';
}

export interface InvestigationResult {
  schema_version: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
  narrative?: { text: string; finding_ids: string[] };
  completion?: InvestigationCompletionResult;
  coverage?: InvestigationCoverageAttestation;
  investigation_id: string;
  result_id: string;
  input_ledger_hash: string;
  graph_snapshot_id: string;
  /** Schema 2+ binds the exact finishing invocation; legacy history stays absent. */
  invocation_step_id?: string;
  disclosure_hash?: string;
  inspection_assessment?: InvestigationInspectionAssessment;
  findings: InvestigationFinding[];
  knowledge_completeness: 'partial' | 'insufficient_evidence';
  limitations: string[];
  observed_response_model: string | null;
  created_at: string;
}

export type InvestigationChatMessage =
  | { kind: 'user_question'; question: string }
  | { kind: 'result_reference'; task_id: string; result_id: string; result_hash: string };

export interface InvestigationChatEntry {
  id: string;
  ordinal: number;
  author: 'user' | 'specialist';
  message: InvestigationChatMessage;
}

/** Saved task attribution only; never resolved from the current catalog. */
export interface InvestigationChatProducer {
  specialist_id: SpecialistId;
  specialist_version: number;
  prompt_fingerprint: string;
  tier: InvestigationSpecialist['tier'];
  confidence_tier: InvestigationSpecialist['confidence_tier'];
  provider_mode: InvestigationProviderMode;
  provider_id: string;
  model: string;
  protocol_version: string | null;
}

export interface InvestigationChat {
  chat_id: string;
  task_id: string;
  producer: InvestigationChatProducer;
  messages: InvestigationChatEntry[];
}

export interface InvestigationCitation {
  citation_id: string;
  fact: FactKey;
  fact_digest: string;
  source: EvidenceRef | null;
  role: string | null;
  index: number | null;
  text_hash: string | null;
  origin: { kind: 'graph_metadata' } | TaskSourceBasisV2['evidence'][number]['origin'];
}

/** Exact payload exists only while the owning worker can authorize this step. */
export interface InvestigationConsent {
  investigation_id: string;
  step_id: string;
  revision: number;
  preview: EgressPreview;
  provider: InvestigationProvider;
  expires_at: string;
  provider_profile: {
    protocol_version: string;
    provider_id: string;
    locality: 'Local' | 'Cloud';
    endpoint_id: string;
    requested_model: string;
  };
  completion_limits: {
    input_bytes: number;
    request_bytes: number;
    response_bytes: number;
    output_text_bytes: number;
    max_output_tokens: number;
    request_timeout_ms: number;
  };
}

/** Historical lookup uses task/citation identity only; no current-fact fallback. */
export interface InvestigationCitationRead {
  investigation_id: string;
  citation_id: string;
  status: 'available' | 'metadata_only' | 'working_tree_unverified' |
    'unavailable' | 'invalid' | 'operational_failure';
  citation: InvestigationCitation;
  text: string | null;
}

/** Canonical host-resolved completion selections; browser display data is never authority. */
export type InvestigationObligation =
  { kind: 'source'; obligation_id: string; target: InvestigationInspectionTarget } |
  { kind: 'inventory'; obligation_id: string; target: { context_id: string; repo: string; inventory_hash: string } };
export interface InvestigationObligationEnvelope {
  schema_version: 1 | 2;
  contract: 'investigation-completion-obligations@1' | 'investigation-completion-obligations@2';
  obligations: InvestigationObligation[];
}
export interface InvestigationObligationChoices {
  schema_version: 1;
  graph_snapshot_id: string;
  context_id: string;
  inventories: { repo: string; status: 'recorded' | 'stale' | 'unrecorded';
    obligation?: Extract<InvestigationObligation, { kind: 'inventory' }> }[];
}

export type InvestigationCompletionOutcome = 'findings_withheld_after_correction' | 'supported' | 'insufficient_evidence' | 'obligations_unmet';
export type InvestigationObligationStatus = 'not_assessed' | 'satisfied' | 'unmet';
export type InvestigationRefusalReason = 'not_attempted' | 'acquired_not_supplied' | 'redacted' | 'hash_mismatch' | 'unavailable';
export interface InvestigationObligationObservation {
  obligation_id: string;
  kind: 'source' | 'inventory';
  attempted: boolean;
  acquired: boolean;
  exactly_supplied: boolean;
  unmet: InvestigationRefusalReason | null;
  last_attempt_outcome?: 'acquired' | 'unavailable' | 'firewall_refused';
}
export type InvestigationCompletionBasis =
  { kind: 'not_assessed' } |
  { kind: 'finishing_disclosure'; step_id: string; input_ledger_hash: string; disclosure_hash: string; finishing_attempts_hash: string } |
  { kind: 'terminal_observation'; finishing_attempts_hash: string; last_refusal_id: string | null;
    last_refusal_basis: { step_id: string; input_ledger_hash: string; disclosure_hash: string; finish_hash: string } | null };
export interface InvestigationCompletionDetail {
  schema_version: 1;
  request_hash: string;
  obligations_hash: string;
  obligations: InvestigationObligationEnvelope;
  refusal_count: number;
  assessment_basis: InvestigationCompletionBasis;
  assessment: InvestigationObligationObservation[];
}
export interface InvestigationCompletionResult {
  schema_version: 1 | 2;
  withholding?: FindingWithholdingWitness;
  outcome: 'supported' | 'insufficient_evidence' | 'findings_withheld_after_correction';
  request_hash: string;
  obligations_hash: string;
  finishing_attempts_hash: string;
  assessment: InvestigationObligationObservation[];
}

export interface FindingWithholdingWitness {
  schema_version: 1;
  detector_version: 1;
  disposition: 'findings_withheld_after_correction';
  original_finish_hash: string;
  original_response_hash: string;
  request_hash: string;
  obligations_hash: string;
  input_ledger_hash: string;
  disclosure_hash: string;
  prior_correction_witness_hash: string;
  projected_result_hash: string;
  original_finding_count: number;
  withheld_indexes: number[];
  survivor_indexes: number[];
}
