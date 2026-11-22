import {
  BUSY_REASON,
  dropSetStatus,
  summaryRepos,
  toDropItems,
  type DropCandidate,
  type DropItem,
} from './dropIntake';
import { create } from 'zustand';
import type { RepoCoverage, RestoredCoverage } from './coverage';
import type { FactKey } from './primarySourceStore';
import { invokeOr } from './tauri';
import type { CuratedDraftCounts, CuratedDraftRow } from './sectionDocument';
import type { SurfaceView } from './views';
import type { EgressPreview } from './components/EgressConsentDialog';
import type { ProjectEntry } from './components/ProjectsCard';

export interface GraphStats {
  nodes: number;
  edges: number;
}

/** A durable job row from the state spine. The v2 fields (#117) are
 *  optional so the UI degrades gracefully against a pre-v2 core. */
export interface Job {
  id: number;
  /** Host-resolved durable association; never inferred from the kind string. */
  investigation_id?: string | null;
  kind: string;
  /** queued | running | done | failed | cancelled | interrupted. */
  status: string;
  /** Ownership protocol participation, never proof of a live worker. Older
   * cores omit this field; those records have unknown execution ownership. */
  execution_tracking?: 'recorded' | 'legacy_unknown';
  /** Current pipeline stage while running. */
  stage?: string | null;
  /** Percent complete (0–100). */
  progress?: number | null;
  /** Failure detail once failed. */
  error?: string | null;
  /** Artifact identifiers produced by a completed job. */
  artifacts?: string[];
  /** Best-effort "what it's doing right now" (current adapter/file), streamed
   *  over `job://detail` (#209) — transient, not persisted, so it's absent
   *  until the next live ping (e.g. right after a restart or `refresh()`). */
  detail?: string | null;
  created_at: string;
  updated_at: string;
}

/** Evidence span as stored by core-prov (SPEC-00 §4.3). */
export interface EvidenceRef {
  repo: string;
  path: string;
  byte_start: number;
  byte_end: number;
  commit_sha: string;
}

export type Tier = 'Confirmed' | 'InferredStrong' | 'InferredWeak' | 'Gap';

/**
 * How a fact was produced (AC-0244), separate from tier and confidence.
 * Absent on the wire for legacy facts and producers that record none.
 */
export type ProducingMethod =
  | { kind: 'rule'; rule: string; version: string }
  | { kind: 'model'; model: string }
  | { kind: 'human'; actor: string }
  | { kind: 'unrecorded' };

export interface Provenance {
  tier: string;
  confidence_tier: Tier;
  evidence: EvidenceRef[];
  extractor_id: string;
  content_hash: string;
  method?: ProducingMethod;
}

/** A graph node as returned by the core (props schema varies by label). */
export interface GraphNode {
  id: string;
  label: string;
  props: { prov?: Provenance; [key: string]: unknown };
}

export interface GraphEdge {
  src: string;
  dst: string;
  label: string;
  props: { prov?: Provenance; [key: string]: unknown };
}

export interface AtlasSnapshot {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface IngestSummary {
  job_id: number;
  files: number;
  /** Distinct facts the load published — the same source as `graph_stats` (AC-0195). */
  nodes: number;
  edges: number;
  /** Occurrences collapsed into one stored fact per id/relation (AC-0195, #242). */
  merged?: {
    nodes: number;
    edges: number;
    /** Ids emitted with differing facts: distinct declarations sharing one id. */
    node_collisions: number;
  };
  layers: {
    ts: LayerSummary;
    python: LayerSummary;
    go: LayerSummary;
    tf: LayerSummary;
    java: LayerSummary;
    kotlin: LayerSummary;
    /** Compiled JVM classes shipped without source (AC-1215). */
    jvm_classes?: LayerSummary;
    webext: LayerSummary;
    /** Config-file evidence: Tool nodes + DEFINED_IN proofs (#215). */
    tools: LayerSummary;
    /** Declared schedule files: Schedule triggers + ENTRY proofs (#770). */
    schedules?: LayerSummary;
    /** Ordered workflow and Dockerfile steps: Pipeline facts (#1790). */
    pipelines?: LayerSummary;
    /** Environments and workloads from Compose files (#1791). */
    environments?: LayerSummary;
    /** Channel and client-call stitching over every layer's sites (#874). */
    events?: LayerSummary;
  };
  /**
   * A local or GitHub recovery's preflight report, reconciled with its AST
   * proof of eval sites (AC-0200, AC-0209): it replaces the pending
   * pre-recovery view.
   */
  preflight?: PreflightReport;
  /** Where `preflight` stands in its repo's register order (AC-0216, #458);
   *  `null` when a newer preflight's findings were already there, so the
   *  recovery wrote nothing and its report must not replace that one. */
  preflight_register?: RegisterStamp | null;
  /** Set for system-manifest adds: each repo's reconciled report (AC-0209). */
  preflights?: { repo: string; report: PreflightReport }[];
  delta?: {
    recomputed_files: number;
    reused_files: number;
    deleted_files: number;
  };
  /** The recovered repo's identity: a clone with its SHA (AC-0001), or a
   *  local tree, whose commit is always `workdir` (AC-0237). */
  repo?: string;
  commit_sha?: string;
  /** Set for system-manifest adds: `identity@sha12` per repo (AC-0002). */
  repos?: string[];
  /** Each recovered repo's coverage inventory (AC-0238). */
  coverage?: RepoCoverage[];
}

export interface LayerSummary {
  files: number;
  nodes: number;
  edges: number;
}

/** Where the Connect step points Cartograph (handoff §Connect). */
export type IngestSource = 'github' | 'local' | 'manifest';

/** One language × framework × capability cell (`ingest::matrix`,
 * AC-0253). Supported and partial cells are backed by a passing
 * conformance fixture; `note` names what partial/unsupported cells miss. */
export interface CoverageCell {
  language: string;
  framework: string;
  capability: string;
  support: 'supported' | 'partial' | 'unsupported';
  note: string;
}

/** One detected language with adapter coverage (`ingest::preflight`). */
export interface LanguageDetection {
  language: string;
  files: number;
  /** Covering adapter id; null means the whole language is unsupported. */
  adapter: string | null;
  /** Matrix cells of a compiled-in adapter; absent in older reports. */
  coverage?: CoverageCell[];
}

/** One classified construct from local detection (`ingest::preflight`). */
export interface PatternFinding {
  kind: string;
  path: string;
  line: number;
  message: string;
  detector: string;
  /** For uncovered-language findings: the language to request an adapter
   * for — the resolution lane instead of a dead end (#201). */
  request_adapter?: string | null;
}

/** An installed source declaring a coverage claim (`ingest::adapter_lookup`). */
export type AdapterSource =
  | { kind: 'builtin'; adapter: string }
  | { kind: 'plugin'; id: string; version: string };

/** Who declares coverage of one unsupported or partial cell (#1071):
 *  verified sources, sources whose evidence has not passed, or nobody. */
export type AdapterLookup = { cell: CoverageCell } & (
  | { status: 'covered_by'; sources: AdapterSource[] }
  | { status: 'declared_unverified'; sources: AdapterSource[] }
  | { status: 'none' }
);

/** Local preflight output (#116): the three-way classification exists from
 *  first contact — potential gaps and unsupported patterns never conflate. */
export interface PreflightReport {
  languages: LanguageDetection[];
  frameworks: string[];
  unsupported: PatternFinding[];
  potential_gaps: PatternFinding[];
  detector: string;
  /** For each unsupported or partial cell, which installed source declares
   *  coverage of it (#1071). Absent from reports older than the lookup. */
  adapter_lookup?: AdapterLookup[];
  /** Set on `preflight` results: the report's place in the register order.
   *  A scan a recovery reconciled past carries that recovery's report. */
  register?: RegisterStamp;
}

/** One `cartograph.system.toml` entry's preview (#1655, AC-0705): a local
 *  member scanned in place, or a remote/managed one deferred to recovery. */
export type MemberPreflight =
  | { kind: 'local'; entry: string; path: string; report: PreflightReport }
  | { kind: 'deferred'; entry: string };

/** A system manifest's preflight preview, in manifest order. It registers
 *  no source and records no finding; recovery owns both (AC-0706). */
export interface SystemPreflight {
  members: MemberPreflight[];
}

/** A report's place in its repo's register order (AC-0216, #458). The
 *  register holds the write with the highest epoch, so showing, per repo, the
 *  highest-epoch report seen always matches the register. */
export interface RegisterStamp {
  repo: string;
  epoch: number;
}

/** A retried job's row; a retried recovery also carries its reconciled
 *  preflight report, exactly as the recovery command's summary does. */
type RetriedJob = Job & {
  recovery?: { preflight: PreflightReport; preflight_register: RegisterStamp | null };
};

/** One live `preflight://progress` ping (#235): the file the scan is reading
 *  and how far through the walk it is — or, while the tree is still being
 *  listed (`total` is `null`, #453), the directory being entered and how many
 *  files have been found so far. */
export interface PreflightProgress {
  /** The scan this ping belongs to — the id `runPreflight` sent. */
  run: number;
  path: string;
  done: number;
  total: number | null;
}

/** One provenance-bearing resolved hop returned by `flowtracer::Flow`. */
export interface FlowHop {
  label: string;
  src: string;
  dst: string;
  src_name: string;
  dst_name: string;
  tier: string;
  confidence: string;
  evidence: string | null;
  provenance: Provenance;
  gap_reason: string | null;
  attempted_tiers: string[];
}

/** One traced flow as returned by `list_flows` (flowtracer::Flow). */
export interface Flow {
  trigger: string;
  trigger_kind: string;
  trigger_name: string;
  hops: FlowHop[];
  status: 'Verified' | 'Partial' | 'Inferred';
  score: number;
  depth_limited: boolean;
}

/** One anchor kind the tracer sought, with the count found (#165) — the
 * zero-flow empty state names what recovery looked for. */
export interface AnchorProbe {
  kind: string;
  found: number;
}

/** Graph membership with operational labels supplied by the host registry. */
export interface SystemRepo {
  repo: string;
  commit: string;
  display_name?: string | null;
  /** Graph content hash recorded by the latest completed ingest of this
   *  registration at `commit` (#1765); absent when none is recorded. Scoped
   *  per registration — a `local/` key is random per installation, so the
   *  value is not comparable across installations. */
  graph_hash?: string | null;
}

/** One installed deterministic adapter, from the registry Preflight uses
 * (#163) — Settings renders exactly what coverage consults. */
export interface AdapterInfo {
  id: string;
  language: string;
  extensions: string[];
  covers: string;
}

/** A known-but-uninstalled adapter type in the recommendation catalog. */
export interface PlannedAdapter {
  language: string;
  extensions: string[];
}

/** One class-level batch escalation outcome (#167): per-instance staged
 * proposals and failures. Each still needs a review; an accepted one gets
 * its curated state when the projection is read (ADR-0061). */
export interface ClassEscalationOutcome {
  proposals: StagedProposal[];
  failures: { gap_id: string; error: string }[];
  cancelled: boolean;
}

export interface AdapterInventory {
  installed: AdapterInfo[];
  planned: PlannedAdapter[];
  matrix: CoverageCell[];
  detector: string;
}

/** One discovered WASM plugin adapter with per-project state (#198).
 * Disabled unless explicitly enabled — the fail-closed default. */
export interface PluginStatus {
  id: string;
  path: string;
  content_hash: string;
  scope: 'project' | 'user';
  /** Resolved ingest root that supplied a project copy; null for user. */
  project_root: string | null;
  shadowed_user_copy: boolean;
  enabled: boolean;
  /** Conformance-gate verdict for these exact bytes (#200). `ungated` is
   * the proposed state; only `passed` ever joins extraction. */
  gate: 'passed' | 'failed' | 'ungated';
  /** First failing check as `name: detail` when the gate failed. */
  gate_detail: string | null;
  /** Coverage cells the gated corpus declares (AC-0263); empty unless
   * the gate passed for these exact artifact and corpus bytes. */
  coverage: CoverageCell[];
}

export type SpecExportMode = 'verified-only' | 'best-effort';

export interface SpecAssertion {
  id: string;
  subject_id: string;
  subject_kind: string;
  summary: string;
  provenance: Provenance;
  /** The relation an escalation of this assertion would need the bounded T3
   *  broker to propose (#238): an edge/hop's own label, or — for a Gap node
   *  — its adjacent edge's label once one is known server-side. Absent (not
   *  just null) on artifacts that predate this field. `null`/undefined both
   *  read as "unknown"; see `isEscalatable` in `gapClasses.ts`. */
  edge_label?: string | null;
  /** Why a Gap is missing and which seam would fill it (#585, AC-0254).
   *  Present exactly on Gap assertions. */
  omission?: Omission;
}

export type OmissionClass =
  | 'unsupported_extractor'
  | 'missing_input'
  | 'unresolved_relationship'
  | 'contradictory_or_stale'
  | 'product_gap';

export type FillSeam = 'adapter' | 'registry' | 'manifest_input' | 'interpretation' | 'requirement';

/** Mirrors `core_graph::omission::Omission`. */
export interface Omission {
  class: OmissionClass;
  seam: FillSeam;
  /** The specific stop reason, verbatim. */
  detail: string;
}

export interface DraftAdapterPacket {
  language: string;
  content_id: string;
  markdown: string;
}

export interface SpecArtifact {
  id: string;
  file_name: string;
  title: string;
  format: 'markdown' | 'mermaid' | 'json';
  /** Versioned schema id, `cartograph.spec/<id>@<version>` (AC-0242). */
  schema: string;
  /** Content identity of the complete artifact, computed before the IPC
   *  preview cap, so a preview and its full artifact share it (AC-0242). */
  content_id: string;
  /** Capped to a preview when it crosses IPC via `export_spec` (#488); see
   *  `content_truncated`/`content_byte_len`. `read_spec_artifact` returns the
   *  untruncated artifact for copy/export. */
  content: string;
  content_truncated: boolean;
  /** Full content length in bytes, regardless of `content_truncated`. */
  content_byte_len: number;
  /** Capped to a preview when it crosses IPC via `export_spec` (#488); see
   *  `assertions_truncated`/`assertions_total`. */
  assertions: SpecAssertion[];
  assertions_truncated: boolean;
  /** Full assertion count, regardless of `assertions_truncated`. */
  assertions_total: number;
}

/** What an export was compiled from (#1765): the content hash of the whole
 *  base graph and the source revisions it holds. */
export interface GraphBasis {
  content_hash: string;
  sources: { repo: string; commit: string }[];
}

export interface SpecBundle {
  mode: SpecExportMode;
  artifacts: SpecArtifact[];
  assertion_count: number;
  gap_count: number;
  drift_count: number;
  security_count: number;
  /** Carried into the exported bundle file unchanged. */
  graph_basis?: GraphBasis;
  /** The curated revision and counts the bundle was compiled with
   *  (AC-0653). Absent when the projection was not computed. */
  curated_basis?: CuratedBasis;
}

/** Record totals of a curated projection; `accepted` is the sum of the rest. */
export interface CuratedCounts {
  accepted: number;
  eligible: number;
  unverified: number;
  unresolvable: number;
  stale: number;
  already_covered: number;
  conflicted: number;
}

export interface CuratedBasis {
  revision: string;
  counts: CuratedCounts;
}

/** The one state the host assigns an accepted record at projection time
 *  (ADR-0061 decision 4). Never stored, and never assigned by the UI. */
export type CuratedRecordState =
  | 'unverified' | 'already-covered' | 'unresolvable' | 'stale' | 'conflicted' | 'eligible';

/** One accepted record with its DR-4 fields (SPEC-12). The sentences come
 *  from the host: the UI maps no reason code to prose. */
export interface CuratedRecord {
  record_id: string;
  gap_id: string;
  source_id: string;
  target_id: string;
  edge_label: string;
  /** Producing tier, exactly as produced (`Agentic` for a proposal). */
  tier: string;
  /** Producing confidence, exactly as produced. */
  confidence: Tier;
  review_status: 'accepted' | 'rejected';
  source_support: { captured: number; unverified: number };
  state: CuratedRecordState;
  reason: string;
  reason_sentence: string;
  /** The SPEC-09 dimensions the state was read beside. */
  basis_summary: string;
}

/** The state the host gives a finding accepted as curated (ADR-0074). */
export type CuratedFindingState = 'unresolvable' | 'stale' | 'eligible';

/** An eligible finding a curated read shows. Always InferredWeak. */
export interface CuratedFinding {
  finding_ref: string;
  investigation_id: string;
  result_id: string;
  finding_id: string;
  role: string;
  kind: string;
  title: string;
  statement: string;
  limitations: string[];
  /** ADR-0058 citation ids of the facts it cites. */
  citations: string[];
  tier: string;
  confidence: Tier;
  review_status: 'accepted';
  source_support: { captured: number; unverified: number };
  state: CuratedFindingState;
  reason: string;
  reason_sentence: string;
  changed: string[];
  missing: string[];
}

/** An accepted finding that is not eligible: identity, state and the cited
 *  ids behind the state. The host sends no statement and no title. */
export interface CuratedFindingListing {
  finding_ref: string;
  investigation_id: string;
  result_id: string;
  finding_id: string;
  role: string;
  cited_facts: number;
  state: CuratedFindingState;
  reason: string;
  reason_sentence: string;
  changed: string[];
  missing: string[];
}

export interface CuratedFindingCounts {
  accepted: number;
  eligible: number;
  unresolvable: number;
  stale: number;
}

/** A Workbench decision whose content hash matches no current statement. */
export interface StaleAssertionDecision {
  content_hash: string;
  decision: AssertionDecision;
  state: CuratedRecordState;
}

/** The curated projection as `export_spec_with_views` returns it beside the
 *  bundle, for that bundle's export mode (SPEC-12 slice 3, AC-0655). */
export type CuratedRead =
  | {
      status: 'computed';
      export_mode: SpecExportMode;
      view: 'recovered' | 'curated';
      revision: string;
      graph_basis: string;
      /** The curated items this export mode shows; none in `verified-only`. */
      items: CuratedRecord[];
      items_omitted: number;
      listed: CuratedRecord[];
      /** Every accepted section draft with its state (SPEC-15 slice 7,
       *  AC-0669); absent from a host that predates it. No claim. */
      drafts?: CuratedDraftRow[];
      draft_counts?: CuratedDraftCounts;
      /** Findings accepted as curated (ADR-0074, AC-0845): the eligible
       *  ones this export mode shows, the number it leaves out, and every
       *  other accepted finding without its statement. Absent from a host
       *  that predates them. */
      findings?: CuratedFinding[];
      findings_omitted?: number;
      findings_listed?: CuratedFindingListing[];
      finding_counts?: CuratedFindingCounts;
      assertion_decisions: {
        digest: string;
        rejections_applied: number;
        stale: StaleAssertionDecision[];
      };
      counts: CuratedCounts;
    }
  | {
      status: 'not_computed';
      export_mode: SpecExportMode;
      reason: string;
      limit?: number;
      sentence: string;
    };

/** A curated read whose projection was computed. */
export type ComputedCuratedRead = Extract<CuratedRead, { status: 'computed' }>;

export interface DeploymentRole {
  resource_id: string;
  source: string;
  provenance?: Provenance;
  backed_channels?: string[];
}

export interface SpecDiagramView {
  artifact_id: string;
  schema: string;
  content_id: string;
  mode: SpecExportMode;
  nodes: GraphNode[];
  nodes_total: number;
  edges: GraphEdge[];
  edges_total: number;
  flows?: Flow[];
  flows_total: number;
  deployments?: DeploymentRole[];
  truncated: boolean;
  /** Client-only marker when rendering a single-flow subset of the host `flows` view. */
  subset_of?: {
    flow_trigger: string;
    missing_participants?: number;
  };
}

export interface SpecViewBundle {
  mode: SpecExportMode;
  topology: SpecDiagramView;
  flows: SpecDiagramView;
  data_model: SpecDiagramView;
  /** The C4 model of `architecture.md` (#2049); absent from a host that
   *  predates it. */
  architecture?: ArchitectureView;
}

/** One fact a C4 element or relationship rests on (#2049). */
export interface ArchitectureFact {
  /** Node id, or the `src LABEL dst` identity of an edge. */
  id: string;
  /** Node label or edge label. */
  kind: string;
  label: string;
  provenance: Provenance;
}

export interface ArchitectureElement {
  /** `e{n}`, or `e{n}c{m}` for a component; the ids `architecture.dsl` uses. */
  id: string;
  kind: 'person' | 'container' | 'store' | 'external' | 'component';
  name: string;
  technology: string;
  description: string;
  confidence: Tier;
  /** At most twelve; `facts_total` counts them all. */
  facts: ArchitectureFact[];
  facts_total: number;
}

export interface ArchitectureRelation {
  from: string;
  to: string;
  label: string;
  technology: string;
  confidence: Tier;
  facts: ArchitectureFact[];
  facts_total: number;
}

export interface ArchitectureComponents {
  container: string;
  elements: ArchitectureElement[];
  relations: ArchitectureRelation[];
  unassigned_files: number;
}

/** The C4 model behind `architecture.md`, bound to its content identity. */
export interface ArchitectureView {
  artifact_id: string;
  schema: string;
  content_id: string;
  mode: SpecExportMode;
  system: string;
  elements: ArchitectureElement[];
  relations: ArchitectureRelation[];
  components: ArchitectureComponents[];
  areas_supplied: boolean;
}

export interface SpecWithViews {
  bundle: SpecBundle;
  views: SpecViewBundle;
  /** The projection `bundle.curated_basis` was stamped from: the host
   *  computes it once per compile and returns it here, so the section and
   *  the bundle cannot show two revisions (AC-0655). */
  curated?: CuratedRead;
}

function unpackSpecWithViews(payload: SpecWithViews | null): {
  specBundle: SpecBundle | null;
  specViews: SpecViewBundle | null;
  specCurated: CuratedRead | null;
} {
  const curated = payload?.curated;
  return {
    specBundle: payload?.bundle ?? null,
    specViews: payload?.views ?? null,
    // Anything that is not one of the two host shapes is not a read.
    specCurated: curated?.status === 'computed' || curated?.status === 'not_computed' ? curated : null,
  };
}

export type AssertionDecision = 'accepted' | 'rejected' | 'annotated';

export interface CuratableAssertion {
  subject_id: string;
  summary: string;
  provenance: Provenance;
}

export interface AssertionDecisionRecord {
  assertion: CuratableAssertion;
  decision: AssertionDecision;
  note: string | null;
  updated_at: string;
}

export interface EvidenceSource {
  text: string;
  /** Byte offset of the window within the file (large files are windowed). */
  window_start: number;
  /** 1-based file line number of the window's first line; optional so the
   *  UI degrades against a pre-#113 core (assumes 1). */
  window_start_line?: number;
  truncated: boolean;
}

/** Source view state: loading → window, or unavailable (file moved, no root). */
export type SourceState = EvidenceSource | 'loading' | 'unavailable';

/** One persisted register finding (`list_findings`, #116): an unsupported
 *  pattern or a no-evidence question — tool limitations, never Gaps. */
export interface RegisterFinding {
  id: number;
  /** `unsupported` | `no-evidence` (Gap cannot enter by schema). */
  kind: string;
  detector: string;
  repo: string;
  path: string;
  line: number;
  message: string;
  created_at: string;
}

/** Register headline counts (`findings_summary`, #116): one definition —
 *  the spec register's own predicates — reused by every surface. */
export interface FindingsSummary {
  gaps: number;
  /** Subset of gaps the interpretation seam fills (#994); still counted in gaps. */
  interpretation_backlog?: number;
  unsupported: number;
  no_evidence: number;
  drift: number;
  /** gaps + unsupported + no_evidence. */
  open_findings: number;
  /** Total graph facts (nodes + edges). */
  graph_facts: number;
}

/** One per-ingest history record (`ingest_history`, #119): tier tallies,
 *  register counts, and the whole-graph content hash that makes the
 *  determinism invariant observable. */
export interface IngestRecord {
  id: number;
  job_id: number;
  repo: string;
  commit_sha: string;
  confirmed: number;
  inferred_strong: number;
  inferred_weak: number;
  gap: number;
  unsupported: number;
  no_evidence: number;
  graph_facts: number;
  content_hash: string;
  created_at: string;
}

/** Per-extractor coverage for the latest ingest (`extractor_coverage`). */
export interface ExtractorCoverage {
  extractor: string;
  files_in_scope: number;
  files_with_facts: number;
  facts: number;
  /** Percent of scoped files with facts; null when the extractor was not
   *  in this ingest's scope (not applicable, never a misleading 0%). */
  coverage_pct: number | null;
}

/** One paired-eval calibration record (`list_evals`, M7/M10 gates). */
export interface EvalResult {
  /** Descriptive evaluation evidence, absent on legacy calibration rows. */
  evaluation?: {
    schema_version: 1;
    mode: 'calibration_only' | 'independent_evaluation' | 'visible_mechanism_evaluation';
    families: {
      family: { kind: 'Call' | 'Channel'; subtype: string | null };
      status: 'calibration_only' | 'independently_evaluated' | 'abstained';
      reason: 'calibration_only' | 'missing_evidence' | 'incompatible_evidence' | 'insufficient_evidence' | 'below_floor' | 'independently_evaluated';
      staged: number;
      admitted: number;
      ambiguous: number;
      receipt_digest: string | null;
      metrics: {
        precision: number;
        recall: number;
        true_positives: number;
        false_positives: number;
        false_negatives: number;
        unknown: number;
        no_match_false_admissions: number;
        retrieval_recall: number | null;
      } | null;
    }[];
  };
  id: number;
  provider: string;
  precision_floor: number;
  similarity_threshold: number;
  precision: number;
  recall: number;
  passed: boolean;
  proposals: number;
  approved: number;
}

/** Fact counts by confidence tier, derived from the same atlas projection
 *  every surface renders (handoff interaction #3: one source of truth). */
export interface TierDistribution {
  confirmed: number;
  inferredStrong: number;
  inferredWeak: number;
  gap: number;
  /** Facts carrying no provenance — never silently promoted. */
  unattributed: number;
  total: number;
}

export function tierDistribution(atlas: AtlasSnapshot): TierDistribution {
  const distribution: TierDistribution = {
    confirmed: 0,
    inferredStrong: 0,
    inferredWeak: 0,
    gap: 0,
    unattributed: 0,
    total: 0,
  };
  const facts = [...atlas.nodes, ...atlas.edges];
  for (const fact of facts) {
    distribution.total += 1;
    switch (fact.props.prov?.confidence_tier) {
      case 'Confirmed':
        distribution.confirmed += 1;
        break;
      case 'InferredStrong':
        distribution.inferredStrong += 1;
        break;
      case 'InferredWeak':
        distribution.inferredWeak += 1;
        break;
      case 'Gap':
        distribution.gap += 1;
        break;
      default:
        distribution.unattributed += 1;
    }
  }
  return distribution;
}

/** One escalation option (`gap_strategies`, #120). */
export interface StrategyCard {
  id: 'local-slm' | 'cloud-opus';
  tier: string;
  provider: string;
  locality: 'local' | 'cloud';
  egress_bytes: number;
  est_usd: number | null;
  latency: string;
  privacy: string;
  export_impact: string;
  available: boolean;
  unavailable_reason: string | null;
}

/** The Resolution Strategy report for one gap (`gap_strategies`, #120). */
export interface GapStrategyReport {
  gap_id: string;
  summary: string;
  stop_reason: string;
  attempted_tiers: string[];
  required_evidence: string[];
  candidates: number;
  strategies: StrategyCard[];
  source_basis?: TaskSourceBasisV2;
}

export interface TaskSelectionReport {
  metadata_lookahead: number;
  acquisition_attempts: number;
  supplied_evidence: number;
  supplied_candidates: number;
  captured_validation_bytes: number;
  limits: {
    metadata_lookahead: number;
    acquisition_attempts: number;
    selected_facts: number;
    evidence: number;
    candidates: number;
    span_bytes: number;
    total_evidence_bytes: number;
    captured_validation_bytes: number;
  };
  omissions: { request_index: number; fact: FactKey; reason: 'missing_citation' | 'legacy_read_unavailable' | 'captured_span_over_limit' }[];
  metadata_preselected_not_read: number;
  unread_tail: boolean;
  stop_reason: 'candidate_limit' | 'neighborhood_exhausted' | 'attempt_limit' |
    'validation_byte_limit' | 'participating_source_unavailable' | 'invalid_selection' | 'required_membership_missing';
}

/** Metadata of the exact inputs supplied at preparation, never a live assessment. */
export interface TaskSourceBasisV2 {
  schema_version: 2;
  graph_snapshot_id: string;
  selected_facts: {
    fact: FactKey;
    fact_digest: string;
    binding: { repo_key: string; receipt_id: string; emitted_fact_digest: string } | null;
  }[];
  evidence: {
    evidence_id: string;
    fact: FactKey;
    role: string;
    index: number;
    origin: { kind: 'working_tree_unverified' } | {
      kind: 'captured_primary_source';
      registered_source_id: string;
      receipt_id: string;
      receipt_inventory_index: number;
      captured: {
        file: { source_id: string; capture_id: string; path: string; digest: string; byte_len: number };
        byte_start: number;
        byte_end: number;
      };
      scope: 'primary_source_only';
      input_closure: 'input_closure_not_established';
    };
  }[];
  selection: TaskSelectionReport;
}

/** Immutable propose-only content produced by the bounded agent broker. */
export interface AgentProposal {
  gap_id: string;
  source_id: string;
  target_id: string;
  edge_label: string;
  annotation: string;
  basis_hash: string;
  provenance: Provenance;
}

/** Host-persisted proposal returned by staging and revision-checked review. */
export interface StagedProposal extends AgentProposal {
  /** Absent only for older hosts and fixtures. New host rows declare 1 or 2. */
  schema_version?: 1 | 2;
  proposal_id: string;
  review_revision: number;
  review_decision: 'accepted' | 'rejected' | null;
  review_note: string | null;
  evidence_binding: 'working_tree_unverified' | 'per_item';
  source_basis?: TaskSourceBasisV2;
  context_status: 'awaiting_reconciliation';
  created_at: string;
  reviewed_at: string | null;
}

export interface StagedProposalPage {
  items: StagedProposal[];
  next_cursor: string | null;
}

export type BasisComparison = 'unchanged' | 'changed' | 'legacy_unverified' | 'operational_failure';
export type BasisEvidenceStatus = 'available' | 'unavailable' | 'invalid' |
  'operational_failure' | 'validation_byte_limit' | 'working_tree_unverified';

/** A read-only observation, kept apart from the immutable staged proposal. */
export interface BasisAssessment {
  schema_version: 1;
  proposal_id: string;
  current_graph_snapshot_id: string | null;
  graph_comparison: BasisComparison;
  association_comparison: BasisComparison;
  evidence: { evidence_id: string; status: BasisEvidenceStatus }[];
  unverified_evidence: number;
  captured_validation_bytes: number;
  max_captured_validation_bytes: number;
}

export interface BasisAssessmentState {
  proposalId: string;
  requestGeneration: number;
  loading: boolean;
  result: BasisAssessment | null;
  error: string | null;
}

function requireBasisAssessment(result: BasisAssessment | null, proposalId: string): BasisAssessment {
  const comparisons: BasisComparison[] = ['unchanged', 'changed', 'legacy_unverified', 'operational_failure'];
  const statuses: BasisEvidenceStatus[] = ['available', 'unavailable', 'invalid',
    'operational_failure', 'validation_byte_limit', 'working_tree_unverified'];
  if (!result || result.schema_version !== 1 || result.proposal_id !== proposalId ||
      !comparisons.includes(result.graph_comparison) || !comparisons.includes(result.association_comparison) ||
      !(result.current_graph_snapshot_id === null || typeof result.current_graph_snapshot_id === 'string') ||
      !Array.isArray(result.evidence) || result.evidence.length > 12 ||
      result.evidence.some((item) => !item || typeof item.evidence_id !== 'string' || !statuses.includes(item.status)) ||
      [result.unverified_evidence, result.captured_validation_bytes, result.max_captured_validation_bytes]
        .some((value) => !Number.isSafeInteger(value) || value < 0)) {
    throw new Error('Current basis could not be assessed. No proposal or review was changed.');
  }
  return result;
}

/** A broker body alone is not a successful durable stage or review response. */
function requireStagedProposal(proposal: StagedProposal | null): StagedProposal {
  if (!proposal?.proposal_id || !Number.isSafeInteger(proposal.review_revision) ||
      proposal.review_revision < 0 ||
      (proposal.schema_version !== undefined && proposal.schema_version !== 1 && proposal.schema_version !== 2) ||
      (proposal.schema_version === 2
        ? proposal.evidence_binding !== 'per_item' || proposal.source_basis?.schema_version !== 2
        : proposal.evidence_binding !== 'working_tree_unverified' || proposal.source_basis !== undefined)) {
    throw new Error('The core did not return a saved proposal. No review was recorded.');
  }
  return proposal;
}

const PROPOSAL_PAGE_SIZE = 20;

function mergeStagedProposals(current: StagedProposal[], incoming: StagedProposal[]): StagedProposal[] {
  const byId = new Map(current.map((proposal) => [proposal.proposal_id, proposal]));
  for (const proposal of incoming) {
    const previous = byId.get(proposal.proposal_id);
    if (!previous || proposal.review_revision >= previous.review_revision) {
      byId.set(proposal.proposal_id, proposal);
    }
  }
  return [...byId.values()];
}

/** UI state of the Resolution Strategy modal (#113). */
export interface EscalationState {
  gapId: string;
  report: GapStrategyReport | null;
  loading: boolean;
  error: string | null;
  running: boolean;
  /** Cloud one-action consent step: the exact preview awaiting a grant. */
  preview: EgressPreview | null;
  proposal: StagedProposal | null;
  reviewing: boolean;
  /** Set once the user accepted/rejected the proposal. */
  decided: 'accepted' | 'rejected' | null;
  /** Transient observation of this selection; never merged into proposal history. */
  basisAssessment?: BasisAssessmentState;
}

/** Exact redacted payload preview for the one-action consent dialog. */
export type { EgressPreview } from './components/EgressConsentDialog';

/** One configurable recovery tier's persisted state (#118). T0 is absent by
 *  design — it is always on and never configurable. */
export interface TierSettings {
  tier: 'T1' | 'T2' | 'T3';
  enabled: boolean;
  provider: 'local' | 'cloud';
  consented: boolean;
  consent_disclosure: string | null;
  consented_at: string | null;
}

/** "Ingest parallelism" (#236): `setting` 0 = Auto, otherwise a fixed worker
 *  count; the rest is what it resolves to on this machine. Output never
 *  depends on it — only wall-clock time does. */
export interface IngestParallelism {
  setting: number;
  auto_workers: number;
  workers: number;
  max_workers: number;
}

/** Live egress line for the status bar (#118). */
export interface EgressSummary {
  cloud_tiers: string[];
  bytes_sent: number;
  label: string;
}

/** Everything the fail-closed consent panel shows *before* consent
 *  (`llm::anthropic::disclosure`). */
export interface CloudDisclosure {
  provider: string;
  model: string;
  endpoint: string;
  input_usd_per_mtok: number;
  output_usd_per_mtok: number;
  notes: string[];
}

export interface AppStore {
  /** Active shell surface (handoff: the router is a single `view` value). */
  view: SurfaceView;
  /** When set, the Recover surface shows exactly this job (#211 review) —
   *  set by a specific "View live" click so it can't show a different
   *  running/queued job (a concurrent plugin gate, another recovery) by
   *  coincidence. `null` means the generic fallback: whichever recovery job
   *  is currently running/queued (the only kind `startRecovery` itself can
   *  reach, since the id isn't known until the first `job://changed` lands). */
  recoverJobId: number | null;
  /** Last rejected job action, scoped in its message to the action and row. */
  jobActionError: string | null;
  /** Backend liveness: unknown until the first ping resolves. */
  backend: 'unknown' | 'up' | 'browser';
  version: string | null;
  stats: GraphStats | null;
  jobs: Job[];
  endpoints: GraphNode[];
  /** Complete deterministic graph projection for the read-only Atlas. */
  atlas: AtlasSnapshot;
  /** Traced flows as data (status/score per R-INT-2). */
  flowList: Flow[];
  /** Anchor kinds sought/found by the tracer — honest zero-flow state. */
  flowAnchors: AnchorProbe[];
  /** Installed + planned adapters (#163), from the Preflight registry. */
  adapters: AdapterInventory | null;
  /** Discovered plugin adapters with per-project enablement (#198). */
  plugins: PluginStatus[];
  /** Toggle a plugin (bound to its exact artifact hash) and refresh. */
  setPluginEnabled: (plugin: PluginStatus, enabled: boolean) => Promise<void>;
  /** Run the conformance gate for a plugin as a durable job (#200). */
  runPluginGate: (plugin: PluginStatus) => Promise<void>;
  /** Repos whose facts are in the current system graph (#162). */
  systemContents: SystemRepo[];
  /** Full official artifact set under the active R-INT-5 mode. */
  specBundle: SpecBundle | null;
  /** Structured diagram projections under the active R-INT-5 mode (US-0036). */
  specViews: SpecViewBundle | null;
  specMode: SpecExportMode;
  /** The curated projection `specBundle` was compiled with, from the same
   *  response (AC-0655); `null` when there is none. */
  specCurated: CuratedRead | null;
  curation: AssertionDecisionRecord[];
  specBusy: boolean;
  specError: string | null;
  ingestBusy: boolean;
  ingestSummary: IngestSummary | null;
  ingestError: string | null;
  /** Connect-step target kind; drives placeholder copy and preflight scope. */
  ingestSource: IngestSource;
  /** Connect-step target: repo URL, local path, or manifest path. */
  ingestTarget: string;
  /** Folders dropped on Connect and each one's outcome (#587); kept until
   *  the next drop or a dismiss, so completed and unresolved stay visible. */
  dropIntake: DropItem[] | null;
  /** Local detection result; null for remote targets (detected on clone). */
  preflight: PreflightReport | null;
  /** A system manifest's per-member preview (#1655); null otherwise. */
  systemPreflight: SystemPreflight | null;
  preflightBusy: boolean;
  preflightError: string | null;
  /** Latest progress of the running preflight; null when none is running. */
  preflightProgress: PreflightProgress | null;
  clearBusy: boolean;
  clearError: string | null;
  /** New project (#1994): the host is restarting into an empty project. */
  newProjectBusy: boolean;
  newProjectError: string | null;
  /** Every project, the loaded one first (#1994); null with no backend. */
  projects: ProjectEntry[] | null;
  projectsBusy: boolean;
  projectsError: string | null;
  /** Register headline counts; null with no backend. */
  findings: FindingsSummary | null;
  /** Persisted unsupported/no-evidence rows for the register surface. */
  registerFindings: RegisterFinding[];
  /** Ingest history, newest first (#119). */
  ingestHistory: IngestRecord[];
  /** Per-extractor coverage for the latest ingest (#119). */
  coverage: ExtractorCoverage[];
  /** Coverage inventories recorded by earlier ingests, read back from the
   *  store so Workspace keeps them across a relaunch (AC-0700, #1656). */
  recordedCoverage: RestoredCoverage[];
  /** Why the recorded inventories could not be read; no card is shown then. */
  recordedCoverageError: string | null;
  /** Paired-eval calibration records, newest first. */
  evals: EvalResult[];
  /** Resolution Strategy modal state; null while closed (#113). */
  escalation: EscalationState | null;
  /** Edge labels the bounded T3 broker may currently propose (#238),
   *  prefetched like the cloud disclosures. Null until loaded — the
   *  register stays optimistic (offers escalation) rather than
   *  withholding it before capability is known. */
  escalationEdgeLabels: string[] | null;
  /** Bounded host history, restored independently of jobs and graph content. */
  stagedProposals: StagedProposal[];
  stagedNextCursor: string | null;
  stagedLoading: boolean;
  stagedError: string | null;
  /** Persisted tier configuration (T1/T2/T3; T0 is always-on, not stored). */
  tierSettings: TierSettings[];
  /** Live status-bar egress line; null with no backend (shown as local-only). */
  egress: EgressSummary | null;
  /** Persisted extraction worker setting; null with no backend (#236). */
  ingestParallelism: IngestParallelism | null;
  /** Disclosure for the tier currently offered cloud consent, keyed by tier.
   *  Loaded before the consent panel renders — fail closed: no disclosure,
   *  no recordable consent. */
  disclosures: Partial<Record<string, CloudDisclosure>>;
  settingsError: string | null;
  /** Node selected for evidence view, with its source window state. */
  selected: { node: GraphNode; source: SourceState; evidenceIndex: number; fact?: FactKey; requestVersion: number } | null;
  refresh: () => Promise<void>;
  /** Remove terminal (done/failed/cancelled) jobs from the durable spine;
   *  queued, running, and interrupted (resumable) work is kept (AC-0076). */
  clearFinishedJobs: () => Promise<void>;
  /** Run recovery over a target. An explicit `source` (from the Connect
   *  step) picks the command directly; without one it is inferred from the
   *  target string. */
  /** `exactPath` ingests an OS path exactly as supplied (a dropped folder),
   * where a typed target is trimmed. */
  ingest: (
    path: string,
    source?: IngestSource,
    options?: { exactPath?: boolean },
  ) => Promise<void>;
  clearGraph: () => Promise<void>;
  /** Set the loaded system aside and restart into an empty project (#1994). */
  startNewProject: () => Promise<void>;
  loadProjects: () => Promise<void>;
  /** Set the loaded project aside and restart into another (#1994). */
  switchProject: (id: string) => Promise<void>;
  renameProject: (id: string, name: string) => Promise<void>;
  deleteProject: (id: string) => Promise<void>;
  setSpecMode: (mode: SpecExportMode) => Promise<void>;
  /** Recompile the bundle, with its views and curated projection, for the
   *  active mode: after a proposal review, or on request from the Workbench. */
  reloadSpec: () => Promise<void>;
  /** Full artifact (untruncated content and assertions) for copy/export,
   *  after `export_spec`'s IPC-capped preview (#488). `null` on failure —
   *  callers fall back to the already-fetched preview. */
  readSpecArtifact: (artifactId: string) => Promise<SpecArtifact | null>;
  /** Several full artifacts in one host request, in the order asked, for
   *  Export bundle (#1662): one graph snapshot however many were capped.
   *  `null` on failure, with `specError` recorded. */
  readSpecArtifacts: (artifactIds: string[]) => Promise<SpecArtifact[] | null>;
  readSelectedFlowView: (trigger: string, mode: SpecExportMode, contentId: string) => Promise<SpecDiagramView | null>;
  curateAssertion: (
    assertion: SpecAssertion,
    decision: AssertionDecision,
    note?: string,
  ) => Promise<void>;
  /** Open the evidence drawer for a fact; `evidenceIndex` picks among its
   *  supporting evidence spans (default first). */
  select: (node: GraphNode, evidenceIndex?: number, fact?: FactKey) => Promise<void>;
  clearSelection: () => void;
  /** Navigate the shell; clears the evidence selection (handoff §Interactions). */
  setView: (view: SurfaceView) => void;
  /** Navigate to Recover pinned to exactly this job (#211 review) — the
   *  "View live" action on a specific Jobs row, so it can't show a
   *  different running/queued job by coincidence. */
  viewRecoveryJob: (id: number) => void;
  /** Cancel a queued/running job (#117); running work stops at its next
   *  stage boundary. */
  cancelJob: (id: number) => Promise<void>;
  /** Retry a failed/cancelled job or resume an interrupted one (#117). */
  retryJob: (id: number) => Promise<void>;
  /** Apply one job transition pushed by the core (`job://changed`). */
  applyJobEvent: (job: Job) => void;
  /** Apply one live "what it's doing right now" ping (`job://detail`, #209)
   *  — merges into the matching job only; a no-op if the job isn't known yet
   *  (its `job://changed` row hasn't landed). */
  applyJobDetail: (id: number, detail: string) => void;
  setIngestSource: (source: IngestSource) => void;
  setIngestTarget: (target: string) => void;
  /** Classify dropped paths, then ingest each accepted one in turn through
   *  the same `ingest` path a typed target takes (#587, AC-0237). */
  ingestDropped: (paths: string[]) => Promise<void>;
  /** Clear a finished drop's outcomes; an unresolved drop stays. */
  dismissDropIntake: () => void;
  /** Navigate to Preflight and run local detection (#116). Local targets get
   *  a real report and a system manifest a per-member preview (#1655);
   *  GitHub targets are detected during recovery. */
  runPreflight: () => Promise<void>;
  /** Apply one `preflight://progress` ping while a preflight is running. */
  applyPreflightProgress: (progress: PreflightProgress) => void;
  /** Stop the running preflight (#235); it records no findings. */
  cancelPreflight: () => Promise<void>;
  /** Navigate to Recover and run the staged pipeline. Returns to Workspace
   *  on success unless the user has already navigated away (Run in
   *  background); a failure stays on Recover so the error is never hidden. */
  startRecovery: () => Promise<void>;
  /** Enable/disable a configurable tier (#118); refreshes the egress line. */
  setTierEnabled: (tier: string, enabled: boolean) => Promise<void>;
  /** Persist "Ingest parallelism" (0 = Auto) for later extractions (#236). */
  setIngestParallelism: (setting: number) => Promise<void>;
  /** Choose local or cloud for an LLM tier. Choosing cloud loads the full
   *  disclosure so the consent panel can render it; leaving cloud revokes
   *  standing consent in the core. */
  setTierProvider: (tier: string, provider: 'local' | 'cloud') => Promise<void>;
  /** Record standing cloud consent, storing the exact disclosure shown. */
  grantCloudConsent: (tier: string) => Promise<void>;
  /** Revoke standing consent — immediate, the tier stays on cloud unconsented. */
  revokeCloudConsent: (tier: string) => Promise<void>;
  /** Open the Resolution Strategy modal for a gap and load its report. */
  openResolution: (gapId: string) => Promise<void>;
  closeResolution: () => void;
  openStagedProposal: (proposal: StagedProposal) => void;
  assessStagedBasis: () => Promise<void>;
  loadStagedProposals: (loadMore?: boolean) => Promise<void>;
  /** Read the recorded coverage inventories; a superseded read is dropped,
   *  and so is one whose caller began before the coverage epoch `since`
   *  (default: now) was bumped by a clear, ingest or plugin change. */
  loadRecordedCoverage: (since?: number) => Promise<void>;
  /** Run a strategy. Local runs immediately; cloud first loads the exact
   *  egress preview so the one-action consent dialog can show it. */
  runStrategy: (strategyId: 'local-slm' | 'cloud-opus') => Promise<void>;
  /** The user approved the previewed payload — run cloud with its hash. */
  consentAndRun: (preview: EgressPreview) => Promise<void>;
  /** Keep local: dismiss the pending cloud preview without running. */
  dismissPreview: () => void;
  /** Record the human verdict on a staged proposal (R-INT-3). */
  decideProposal: (decision: 'accepted' | 'rejected') => Promise<void>;
  /** Withdraw an accepted proposal by rejecting it (ADR-0061 decision 9):
   *  the reconciliation step for a record that is no longer eligible. */
  withdrawProposal: () => Promise<void>;
  /** Batch-escalate one gap class locally (#167); one durable job. */
  runClassEscalation: (gapIds: string[]) => Promise<ClassEscalationOutcome | null>;
  /** Record accept/reject for one staged proposal (batch triage path). */
  recordProposalDecision: (
    proposal: StagedProposal,
    decision: 'accepted' | 'rejected',
  ) => Promise<boolean>;
  /** Draft adapter work packets for a recovered repo (#1101, AC-0371);
   *  read-only — nothing is written to the graph, packet store, or register. */
  draftAdapterWorkPackets: (repo: string) => Promise<DraftAdapterPacket[]>;
}

async function loadEndpoints(): Promise<GraphNode[]> {
  return invokeOr<GraphNode[]>('list_nodes', [], { label: 'Endpoint' });
}

let evidenceRequestVersion = 0;
let basisAssessmentGeneration = 0;
// A later mode/curation request owns the visible spec result, error, and busy state.
let specRequestVersion = 0;
/** Bumped by every recorded-coverage read: a read that settles after a
 *  newer one began is dropped. */
let recordedCoverageRequest = 0;
/** Bumped when a clear, ingest or plugin change starts and again when it
 *  settles: a read requested from before the bump (a refresh already
 *  running, say) can no longer describe the store or plugin basis. */
let coverageEpoch = 0;
let jobActionVersion = 0;
/** Only the latest preflight may publish its result: a superseded or
 *  cancelled scan settles late and must not overwrite the current one. */
let preflightRun = 0;
/** Per repo, the highest-epoch report any preflight or recovery returned
 *  (AC-0216, #458), so results that settle out of register order can't show
 *  an older classification than the register holds. */
const newestPreflight = new Map<string, { epoch: number; report: PreflightReport }>();
/** The repo whose report the Preflight surface shows, when stamped. */
let shownPreflightRepo: string | null = null;

/** Record a stamped report and return the newest report known for its repo. */
function newestPreflightFor(
  report: PreflightReport,
  stamp: RegisterStamp | null | undefined,
): PreflightReport {
  if (!stamp) return report;
  const known = newestPreflight.get(stamp.repo);
  if (known && known.epoch > stamp.epoch) return known.report;
  newestPreflight.set(stamp.repo, { epoch: stamp.epoch, report });
  return report;
}

/** A recovery's reconciled report replaces the shown one when no preflight
 *  began since the recovery started, or when the shown report is the same
 *  repo's — whichever settled first, the register order decides (#458). */
function recoveredPreflight(
  report: PreflightReport,
  stamp: RegisterStamp | null | undefined,
  since: number,
): PreflightReport | null {
  if (stamp === null) return null;
  const newest = newestPreflightFor(report, stamp);
  const sameRepoShown = stamp !== undefined && shownPreflightRepo === stamp.repo;
  if (since !== preflightRun && !sameRepoShown) return null;
  shownPreflightRepo = stamp?.repo ?? null;
  return newest;
}

export const useAppStore = create<AppStore>((set, get) => ({
  view: 'workspace',
  recoverJobId: null,
  jobActionError: null,
  backend: 'unknown',
  version: null,
  stats: null,
  jobs: [],
  endpoints: [],
  atlas: { nodes: [], edges: [] },
  flowList: [],
  flowAnchors: [],
  systemContents: [],
  adapters: null,
  plugins: [],
  specBundle: null,
  specViews: null,
  specMode: 'best-effort',
  specCurated: null,
  curation: [],
  specBusy: false,
  specError: null,
  ingestBusy: false,
  ingestSummary: null,
  ingestError: null,
  ingestSource: 'github',
  ingestTarget: '',
  dropIntake: null,
  preflight: null,
  systemPreflight: null,
  preflightBusy: false,
  preflightError: null,
  preflightProgress: null,
  clearBusy: false,
  clearError: null,
  newProjectBusy: false,
  newProjectError: null,
  projects: null,
  projectsBusy: false,
  projectsError: null,
  findings: null,
  registerFindings: [],
  ingestHistory: [],
  coverage: [],
  recordedCoverage: [],
  recordedCoverageError: null,
  evals: [],
  escalation: null,
  escalationEdgeLabels: null,
  stagedProposals: [],
  stagedNextCursor: null,
  stagedLoading: false,
  stagedError: null,
  tierSettings: [],
  egress: null,
  ingestParallelism: null,
  disclosures: {},
  settingsError: null,
  selected: null,

  refresh: async () => {
    const coverageSince = coverageEpoch;
    const ping = await invokeOr<{ app: string; version: string } | null>('ping', null);
    if (ping === null) {
      set({
        backend: 'browser',
        version: null,
        stats: null,
        jobs: [],
        endpoints: [],
        atlas: { nodes: [], edges: [] },
        flowList: [],
        flowAnchors: [],
        systemContents: [],
        adapters: null,
        plugins: [],
        specBundle: null,
        specViews: null,
        specCurated: null,
        curation: [],
        findings: null,
        registerFindings: [],
        escalationEdgeLabels: null,
        ingestHistory: [],
        coverage: [],
        recordedCoverage: [],
        recordedCoverageError: null,
        evals: [],
        tierSettings: [],
        egress: null,
        ingestParallelism: null,
        stagedProposals: [],
        stagedNextCursor: null,
        stagedLoading: false,
        stagedError: null,
      });
      return;
    }
    const specVersion = specRequestVersion;
    const mode = get().specMode;
    const [stats, jobs, endpoints, atlas, flowList, flowAnchors, systemContents, adapters, plugins, specWithViews, curation, findings, registerFindings, ingestHistory, coverage, evals, tierSettings, egress, ingestParallelism, disclosureT2, disclosureT3, escalationEdgeLabels] = await Promise.all([
      invokeOr<GraphStats>('graph_stats', { nodes: 0, edges: 0 }),
      invokeOr<Job[]>('list_jobs', []),
      loadEndpoints(),
      invokeOr<AtlasSnapshot>('atlas_snapshot', { nodes: [], edges: [] }),
      invokeOr<Flow[]>('list_flows', []),
      invokeOr<AnchorProbe[]>('list_flow_anchors', []),
      invokeOr<SystemRepo[]>('system_contents', []),
      invokeOr<AdapterInventory | null>('adapter_inventory', null),
      invokeOr<PluginStatus[]>('list_plugins', []),
      invokeOr<SpecWithViews | null>('export_spec_with_views', null, { mode }),
      invokeOr<AssertionDecisionRecord[]>('list_assertion_decisions', []),
      invokeOr<FindingsSummary | null>('findings_summary', null),
      invokeOr<RegisterFinding[]>('list_findings', []),
      invokeOr<IngestRecord[]>('ingest_history', []),
      invokeOr<ExtractorCoverage[]>('extractor_coverage', []),
      invokeOr<EvalResult[]>('list_evals', []),
      invokeOr<TierSettings[]>('get_settings', []),
      invokeOr<EgressSummary | null>('egress_summary', null),
      invokeOr<IngestParallelism | null>('get_ingest_parallelism', null),
      // Disclosures are static per tier — prefetched so the consent panel
      // can always show them before consent is recordable (fail closed).
      invokeOr<CloudDisclosure | null>('cloud_disclosure', null, { tier: 'T2' }),
      invokeOr<CloudDisclosure | null>('cloud_disclosure', null, { tier: 'T3' }),
      // The broker's edge-label allowlist is effectively static — prefetched
      // so the register can compute its escalation offer from real broker
      // capability (#238) instead of showing it unconditionally.
      invokeOr<string[] | null>('escalation_capabilities', null),
    ]);
    set({
      backend: 'up',
      version: ping.version,
      stats,
      jobs,
      endpoints,
      atlas,
      flowList,
      flowAnchors,
      systemContents,
      adapters,
      plugins,
      ...(specVersion === specRequestVersion && mode === get().specMode
        ? unpackSpecWithViews(specWithViews)
        : {}),
      curation,
      findings,
      registerFindings,
      ingestHistory,
      coverage,
      evals,
      tierSettings,
      egress,
      ingestParallelism,
      disclosures: { T2: disclosureT2 ?? undefined, T3: disclosureT3 ?? undefined },
      escalationEdgeLabels,
    });
    await Promise.all([
      get().loadStagedProposals(),
      get().loadRecordedCoverage(coverageSince),
      get().loadProjects(),
    ]);
  },

  loadRecordedCoverage: async (since = coverageEpoch) => {
    // A caller from before a clear, ingest or plugin change neither reads nor
    // supersedes the read that change starts.
    if (since !== coverageEpoch) return;
    const request = ++recordedCoverageRequest;
    const current = () => request === recordedCoverageRequest && since === coverageEpoch;
    try {
      const recordedCoverage = await invokeOr<RestoredCoverage[] | null>('recorded_coverage', []);
      if (current()) {
        set({ recordedCoverage: recordedCoverage ?? [], recordedCoverageError: null });
      }
    } catch (error) {
      // A failed read never leaves earlier cards standing in for it.
      if (current()) {
        set({ recordedCoverage: [], recordedCoverageError: String(error) });
      }
    }
  },

  setPluginEnabled: async (plugin: PluginStatus, enabled: boolean) => {
    // The plugin basis qualifies recorded coverage: requalify after it moves.
    coverageEpoch++;
    try {
      await invokeOr('set_plugin_enabled', null, {
        projectRoot: plugin.project_root ?? 'user',
        pluginId: plugin.id,
        contentHash: plugin.content_hash,
        enabled,
      });
      const plugins = await invokeOr<PluginStatus[]>('list_plugins', []);
      set({ plugins });
    } finally {
      coverageEpoch++;
      await get().loadRecordedCoverage();
    }
  },

  runPluginGate: async (plugin: PluginStatus) => {
    // The verdict lands in the settings store keyed by content hash; the
    // refreshed list reflects passed/failed for these exact bytes.
    // A gate verdict can change which plugins are active, and so the plugin
    // basis recorded coverage is qualified by.
    coverageEpoch++;
    try {
      await invokeOr('run_plugin_gate', null, { pluginId: plugin.id });
      const plugins = await invokeOr<PluginStatus[]>('list_plugins', []);
      set({ plugins });
    } finally {
      coverageEpoch++;
      await get().loadRecordedCoverage();
    }
  },

  clearFinishedJobs: async () => {
    await invokeOr<number | null>('clear_finished_jobs', null);
    await get().refresh();
  },

  ingest: async (path: string, source?: IngestSource, options?: { exactPath?: boolean }) => {
    // Clear prior outcome up front so a failed run never shows a stale summary.
    coverageEpoch++;
    set({ ingestBusy: true, ingestError: null, ingestSummary: null, recordedCoverage: [] });
    // A GitHub reference clones with real identity (US-0001); a topology
    // manifest ingests the whole declared system (AC-0002); anything else
    // ingests as a local tree. The Connect step's explicit source wins over
    // string inference — a manifest can also be a directory holding one.
    const trimmed = options?.exactPath ? path : path.trim();
    const isRepoUrl =
      source === 'github' ||
      (source === undefined && /^(https:\/\/github\.com\/|git@github\.com:)/.test(trimmed));
    const isManifest =
      source === 'manifest' ||
      (source === undefined && trimmed.endsWith('cartograph.system.toml'));
    const command = isManifest ? 'add_system' : isRepoUrl ? 'add_repo' : 'ingest_path';
    // Every preflight bumps the generation, whatever its target, so a
    // recovery that settles after a newer scan began must leave that
    // scan's report alone (#439 review).
    const preflightSince = preflightRun;
    try {
      const summary = await invokeOr<IngestSummary | null>(
        command,
        null,
        isRepoUrl ? { url: trimmed } : { path: trimmed },
      );
      set({ ingestSummary: summary });
      const preflight = summary?.preflight
        ? recoveredPreflight(summary.preflight, summary.preflight_register, preflightSince)
        : null;
      if (preflight) set({ preflight, preflightError: null });
    } catch (e) {
      set({ ingestError: String(e) });
    } finally {
      coverageEpoch++;
      set({ ingestBusy: false });
      await get().refresh();
    }
  },

  clearGraph: async () => {
    // Cards from before the clear never return, even from a read in flight
    // or from a refresh that began before it.
    coverageEpoch++;
    set({ clearBusy: true, clearError: null, recordedCoverage: [], recordedCoverageError: null });
    try {
      const stats = await invokeOr<GraphStats | null>('clear_graph', null);
      if (stats !== null) {
        set({ stats, ingestSummary: null, selected: null });
      }
    } catch (e) {
      set({ clearError: String(e) });
    } finally {
      coverageEpoch++;
      set({ clearBusy: false });
      await get().refresh();
    }
  },

  startNewProject: async () => {
    set({ newProjectBusy: true, newProjectError: null });
    try {
      // On success the host restarts and this page is replaced.
      await invokeOr<null>('start_new_project', null);
    } catch (e) {
      set({ newProjectError: String(e) });
    } finally {
      set({ newProjectBusy: false });
    }
  },

  loadProjects: async () => {
    try {
      set({ projects: await invokeOr<ProjectEntry[] | null>('list_projects', null) });
    } catch (e) {
      set({ projectsError: String(e) });
    }
  },

  switchProject: async (id: string) => {
    set({ projectsBusy: true, projectsError: null });
    try {
      // On success the host restarts and this page is replaced.
      await invokeOr<null>('switch_project', null, { id });
    } catch (e) {
      set({ projectsError: String(e) });
    } finally {
      set({ projectsBusy: false });
    }
  },

  renameProject: async (id: string, name: string) => {
    set({ projectsBusy: true, projectsError: null });
    try {
      await invokeOr<null>('rename_project', null, { id, name });
      await get().loadProjects();
    } catch (e) {
      set({ projectsError: String(e) });
    } finally {
      set({ projectsBusy: false });
    }
  },

  deleteProject: async (id: string) => {
    set({ projectsBusy: true, projectsError: null });
    try {
      await invokeOr<null>('delete_project', null, { id });
      await get().loadProjects();
    } catch (e) {
      set({ projectsError: String(e) });
    } finally {
      set({ projectsBusy: false });
    }
  },

  setSpecMode: async (mode: SpecExportMode) => {
    const request = ++specRequestVersion;
    set({ specMode: mode, specBusy: true, specError: null });
    try {
      const specWithViews = await invokeOr<SpecWithViews | null>(
        'export_spec_with_views',
        null,
        { mode },
      );
      if (request === specRequestVersion) set(unpackSpecWithViews(specWithViews));
    } catch (error) {
      if (request === specRequestVersion) set({ specError: String(error) });
    } finally {
      if (request === specRequestVersion) set({ specBusy: false });
    }
  },

  reloadSpec: () => get().setSpecMode(get().specMode),

  readSelectedFlowView: async (trigger, mode, contentId) =>
    invokeOr<SpecDiagramView | null>('read_selected_flow_view', null, { trigger, mode, contentId }),

  readSpecArtifact: async (artifactId: string) => {
    try {
      return await invokeOr<SpecArtifact | null>('read_spec_artifact', null, {
        mode: get().specMode,
        artifactId,
      });
    } catch (error) {
      set({ specError: String(error) });
      return null;
    }
  },

  readSpecArtifacts: async (artifactIds: string[]) => {
    try {
      return await invokeOr<SpecArtifact[] | null>('read_spec_artifacts', null, {
        mode: get().specMode,
        artifactIds,
      });
    } catch (error) {
      set({ specError: String(error) });
      return null;
    }
  },

  curateAssertion: async (
    assertion: SpecAssertion,
    decision: AssertionDecision,
    note?: string,
  ) => {
    let request = ++specRequestVersion;
    set({ specBusy: true, specError: null });
    try {
      await invokeOr<AssertionDecisionRecord | null>('record_assertion_decision', null, {
        assertion: {
          subject_id: assertion.subject_id,
          summary: assertion.summary,
          provenance: assertion.provenance,
        },
        decision,
        note: note?.trim() || null,
      });
      // Recompile the currently requested mode after the decision is stored.
      request = ++specRequestVersion;
      const mode = get().specMode;
      // A Workbench decision enters the curated revision; the recompiled
      // response carries the new projection with the bundle.
      const [specWithViews, curation] = await Promise.all([
        invokeOr<SpecWithViews | null>('export_spec_with_views', null, { mode }),
        invokeOr<AssertionDecisionRecord[]>('list_assertion_decisions', []),
      ]);
      set({
        curation,
        ...(request === specRequestVersion ? unpackSpecWithViews(specWithViews) : {}),
      });
    } catch (error) {
      if (request === specRequestVersion) set({ specError: String(error) });
    } finally {
      if (request === specRequestVersion) set({ specBusy: false });
    }
  },

  select: async (node: GraphNode, evidenceIndex = 0, fact?: FactKey) => {
    const requestVersion = ++evidenceRequestVersion;
    set({ selected: { node, source: 'loading', evidenceIndex, fact, requestVersion } });
    const done = (source: SourceState) => {
      // Ignore if the user selected something else meanwhile.
      const current = get().selected;
      if (requestVersion === evidenceRequestVersion && current?.node === node && current.evidenceIndex === evidenceIndex) {
        set({ selected: { node, source, evidenceIndex, fact, requestVersion } });
      }
    };
    const ev = node.props.prov?.evidence[evidenceIndex] ?? node.props.prov?.evidence[0];
    if (!ev) return done('unavailable');
    try {
      const source = await invokeOr<EvidenceSource | null>('read_evidence', null, {
        repo: ev.repo,
        path: ev.path,
        byteStart: ev.byte_start,
        byteEnd: ev.byte_end,
      });
      done(source ?? 'unavailable');
    } catch {
      // Unknown, missing or unavailable registration: keep the citation metadata.
      done('unavailable');
    }
  },

  clearSelection: () => set({ selected: null }),

  // Generic navigation always forgets any pinned recovery job (#211 review)
  // — only `viewRecoveryJob` pins one, so a stale pin from a prior click
  // never leaks into an unrelated later visit to Recover.
  setView: (view) => set({ view, selected: null, recoverJobId: null }),

  viewRecoveryJob: (id) => set({ view: 'recover', recoverJobId: id, selected: null }),

  cancelJob: async (id) => {
    const version = ++jobActionVersion;
    try {
      const job = await invokeOr<Job | null>('cancel_job', null, { id });
      if (!job || job.id !== id) throw new Error('The backend did not confirm a job transition.');
      get().applyJobEvent(job);
      if (version === jobActionVersion) set({ jobActionError: null });
    } catch (error) {
      if (version === jobActionVersion) set({ jobActionError: `Cancel for job #${id} was not confirmed: ${String(error)} Refresh Jobs before trying again. Cancellation does not immediately stop a worker.` });
    }
  },

  retryJob: async (id) => {
    const version = ++jobActionVersion;
    const existing = get().jobs.find((job) => job.id === id);
    if (existing && (existing.execution_tracking !== 'recorded' || existing.kind.startsWith('ingest:'))) {
      set({ jobActionError: `Job #${id} cannot resume this historical execution. Start a fresh operation from its source instead.` });
      return;
    }
    // A retried recovery reconciles preflight findings like the first run
    // did (AC-0216, #458); it too must not overwrite a newer scan's report.
    const preflightSince = preflightRun;
    try {
      const retried = await invokeOr<RetriedJob | null>('retry_job', null, { id });
      if (!retried || retried.id !== id) throw new Error('The backend did not confirm a job transition.');
      const { recovery, ...job } = retried;
      get().applyJobEvent(job);
      const preflight = recovery
        ? recoveredPreflight(recovery.preflight, recovery.preflight_register, preflightSince)
        : null;
      if (preflight) set({ preflight, preflightError: null });
      if (version === jobActionVersion) set({ jobActionError: null });
    } catch (error) {
      if (version === jobActionVersion) set({ jobActionError: `Retry for job #${id} was not confirmed: ${String(error)} Wait for the current worker to stop, then refresh Jobs and try again. Legacy records require a fresh operation.` });
      return;
    }
    // Refresh only after a confirmed action. A refresh error must not suggest
    // that an already-confirmed retry failed or restore an older job row.
    try {
      await get().refresh();
    } catch {
      if (version === jobActionVersion) set({ jobActionError: `Retry for job #${id} was confirmed, but refreshed results are unavailable. Refresh Jobs to check the current state.` });
    }
  },

  applyJobEvent: (job) =>
    set((state) => {
      const known = state.jobs.some((existing) => existing.id === job.id);
      const jobs = known
        ? state.jobs.map((existing) => (existing.id === job.id ? job : existing))
        : [job, ...state.jobs];
      return { jobs };
    }),

  applyJobDetail: (id, detail) =>
    set((state) => ({
      jobs: state.jobs.map((existing) => (existing.id === id ? { ...existing, detail } : existing)),
    })),

  setIngestSource: (ingestSource) => {
    // A local scan still running on the backend would otherwise finish and
    // persist findings for a target the user has left (#434 review).
    // A manifest preview is abandoned the same way (#1655). Retire the run
    // id too, as a path change does: a scan whose commit claim already won
    // the race with this cancel still settles, and its result must not
    // reach the store (AC-0198, AC-0707).
    if (ingestSource !== get().ingestSource && get().preflightBusy) {
      preflightRun += 1;
      set({ ingestSource, preflightBusy: false, preflightProgress: null });
      void get().cancelPreflight();
      return;
    }
    set({ ingestSource });
  },

  ingestDropped: async (paths) => {
    if (paths.length === 0) return;
    const current = get().dropIntake;
    if (get().ingestBusy || (current && dropSetStatus(current).unresolved > 0)) {
      // Never silently dropped: each path gets an explicit outcome.
      const busy = paths.map((path) => ({
        path,
        source: null,
        status: 'rejected' as const,
        reason: BUSY_REASON,
        repos: [],
      }));
      set({ dropIntake: [...(current ?? []), ...busy] });
      return;
    }
    // Reserve the intake before awaiting classification, so a second drop
    // sees unresolved items and is rejected rather than racing this one.
    // A settled list stays until dismissed, so this intake appends after it
    // and owns the next `paths.length` items; busy rejections appended
    // meanwhile stay after them.
    const prior = current ?? [];
    const base = prior.length;
    const own = paths.length;
    set({
      dropIntake: [
        ...prior,
        ...paths.map((path) => ({
          path,
          source: null,
          status: 'queued' as const,
          reason: null,
          repos: [],
        })),
      ],
    });
    const later = () => get().dropIntake?.slice(base + own) ?? [];
    let candidates: DropCandidate[] | null;
    try {
      candidates = await invokeOr<DropCandidate[] | null>('classify_drop', null, { paths });
    } catch (e) {
      candidates = paths.map((path) => ({ path, source: null, reason: String(e) }));
    }
    if (!candidates) {
      const rest = [...prior, ...later()];
      set({ dropIntake: rest.length > 0 ? rest : null });
      return;
    }
    const items = toDropItems(candidates);
    set({ dropIntake: [...prior, ...items, ...later()] });
    const update = (index: number, patch: Partial<DropItem>) =>
      set((state) => ({
        dropIntake:
          state.dropIntake?.map((item, at) =>
            at === base + index ? { ...item, ...patch } : item,
          ) ??
          null,
      }));
    for (const [index, item] of items.entries()) {
      if (item.status !== 'queued' || !item.source) continue;
      update(index, { status: 'ingesting' });
      let refreshError: string | null = null;
      try {
        await get().ingest(item.path, item.source, { exactPath: true });
      } catch (e) {
        // `ingest` settles its own outcome before refreshing the graph view;
        // only that refresh can reject here. The recovery is not retried.
        refreshError = String(e);
      }
      const { ingestError, ingestSummary } = get();
      update(
        index,
        ingestError
          ? { status: 'failed', reason: ingestError }
          : ingestSummary
            ? {
                status: 'ingested',
                repos: summaryRepos(ingestSummary),
                reason: refreshError ? `ingested; graph refresh failed: ${refreshError}` : null,
              }
            : { status: 'failed', reason: refreshError ?? 'ingest returned no summary' },
      );
    }
  },

  dismissDropIntake: () => {
    const current = get().dropIntake;
    if (current && dropSetStatus(current).unresolved > 0) return;
    set({ dropIntake: null });
  },

  setIngestTarget: (ingestTarget) => {
    // A local scan still running for the previous path (e.g. after Back
    // during a scan) is abandoned: retire its run id so nothing it settles
    // with reaches the store, and stop the worker so it doesn't persist
    // findings for a target the user has left (#454, AC-0198). Whitespace
    // edits don't change what `runPreflight` scanned.
    if (get().preflightBusy && ingestTarget.trim() !== get().ingestTarget.trim()) {
      preflightRun += 1;
      set({ ingestTarget, preflightBusy: false, preflightProgress: null });
      void get().cancelPreflight();
      return;
    }
    set({ ingestTarget });
  },

  runPreflight: async () => {
    const target = get().ingestTarget.trim();
    const run = ++preflightRun;
    shownPreflightRepo = null;
    set({
      view: 'preflight',
      selected: null,
      preflight: null,
      systemPreflight: null,
      preflightError: null,
      preflightBusy: false,
      preflightProgress: null,
    });
    // A local tree, and a system manifest's local members (#1655), can be
    // detected before recovery. GitHub targets are scanned by their
    // recovery, which writes the reconciled findings to the register
    // (AC-0209). Showing nothing beats inventing a report (three-way honesty
    // starts here).
    const source = get().ingestSource;
    if (source === 'github') return;
    set({ preflightBusy: true });
    try {
      if (source === 'manifest') {
        // A preview records nothing, so it has no register stamp to order;
        // only the current run's complete result is shown (AC-0707).
        const system = await invokeOr<SystemPreflight | null>('preflight_system', null, {
          path: target,
          run,
        });
        if (run === preflightRun) set({ systemPreflight: system });
        return;
      }
      const preflight = await invokeOr<PreflightReport | null>('preflight', null, {
        path: target,
        run,
      });
      // A superseded scan's stamp still counts toward its repo's newest
      // report; only the current scan's result is shown.
      const newest = preflight && newestPreflightFor(preflight, preflight.register);
      if (run === preflightRun) {
        shownPreflightRepo = preflight?.register?.repo ?? null;
        set({ preflight: newest });
      }
    } catch (e) {
      if (run !== preflightRun) return;
      const error = String(e);
      set({
        preflightError:
          error === 'cancelled'
            ? source === 'manifest'
              ? 'Preflight cancelled. Nothing was recorded.'
              : 'Preflight cancelled. No findings were recorded.'
            : error,
      });
    } finally {
      if (run === preflightRun) set({ preflightBusy: false, preflightProgress: null });
    }
  },

  applyPreflightProgress: (progress) => {
    // A superseded scan's last ping must not overwrite the current one's.
    if (get().preflightBusy && progress.run === preflightRun) set({ preflightProgress: progress });
  },

  cancelPreflight: async () => {
    try {
      await invokeOr<null>('cancel_preflight', null);
    } catch (e) {
      set({ preflightError: `Cancel was not confirmed: ${String(e)}` });
    }
  },

  startRecovery: async () => {
    const target = get().ingestTarget.trim();
    // A fresh recovery's job id isn't known yet (job://changed hasn't
    // landed) — always the generic fallback, never a stale pin left over
    // from a previous "View live" click (#211 review).
    set({ view: 'recover', recoverJobId: null, selected: null });
    await get().ingest(target, get().ingestSource);
    if (get().view === 'recover' && !get().ingestError) set({ view: 'workspace' });
  },

  setTierEnabled: async (tier, enabled) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('set_tier_enabled', null, {
        tier,
        enabled,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  setIngestParallelism: async (setting) => {
    set({ settingsError: null });
    try {
      const ingestParallelism = await invokeOr<IngestParallelism | null>(
        'set_ingest_parallelism',
        null,
        { setting },
      );
      if (ingestParallelism) set({ ingestParallelism });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  setTierProvider: async (tier, provider) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('set_tier_provider', null, {
        tier,
        provider,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  grantCloudConsent: async (tier) => {
    const disclosure = get().disclosures[tier];
    // Fail closed: consent is only recordable against a loaded disclosure,
    // and the stored consent carries exactly what the user saw.
    if (!disclosure) {
      set({ settingsError: `no disclosure loaded for ${tier} — consent not recorded` });
      return;
    }
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('grant_cloud_consent', null, {
        tier,
        disclosure: JSON.stringify(disclosure),
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  openResolution: async (gapId) => {
    const opening: EscalationState = {
      gapId,
      report: null,
      loading: true,
      error: null,
      running: false,
      preview: null,
      proposal: null,
      reviewing: false,
      decided: null,
    };
    set({ escalation: opening });
    try {
      const report = await invokeOr<GapStrategyReport | null>('gap_strategies', null, {
        gapId,
      });
      set((state) =>
        state.escalation === opening
          ? { escalation: { ...opening, report, loading: false } }
          : {},
      );
    } catch (e) {
      set((state) =>
        state.escalation === opening
          ? { escalation: { ...opening, error: String(e), loading: false } }
          : {},
      );
    }
  },

  closeResolution: () => set({ escalation: null }),

  openStagedProposal: (proposal) => set({
    escalation: {
      gapId: proposal.gap_id,
      report: null,
      loading: false,
      error: null,
      running: false,
      reviewing: false,
      preview: null,
      proposal,
      decided: proposal.review_decision,
    },
  }),

  assessStagedBasis: async () => {
    const proposalId = get().escalation?.proposal?.proposal_id;
    if (!proposalId) return;
    const requestGeneration = ++basisAssessmentGeneration;
    const request: BasisAssessmentState = {
      proposalId, requestGeneration, loading: true, result: null, error: null,
    };
    set((state) => state.escalation?.proposal?.proposal_id === proposalId
      ? { escalation: { ...state.escalation, basisAssessment: request } } : {});
    const update = (fields: Partial<BasisAssessmentState>) => set((state) => {
      const current = state.escalation;
      if (current?.proposal?.proposal_id !== proposalId ||
          current.basisAssessment?.requestGeneration !== requestGeneration) return {};
      return { escalation: { ...current, basisAssessment: { ...request, ...fields } } };
    });
    try {
      const result = requireBasisAssessment(await invokeOr<BasisAssessment | null>(
        'assess_staged_basis', null, { proposalId },
      ), proposalId);
      update({ loading: false, result });
    } catch {
      update({ loading: false, error: 'Current basis could not be assessed. No proposal or review was changed.' });
    }
  },

  loadStagedProposals: async (loadMore = false) => {
    if (get().stagedLoading) return;
    const cursor = loadMore ? get().stagedNextCursor : null;
    if (loadMore && !cursor) return;
    const beforeRead = new Map(get().stagedProposals.map((proposal) => [proposal.proposal_id, proposal]));
    set({ stagedLoading: true, stagedError: null });
    try {
      const page = await invokeOr<StagedProposalPage | null>('list_staged_proposals', null, {
        limit: PROPOSAL_PAGE_SIZE,
        cursor,
      });
      if (!page) throw new Error('Proposal history is unavailable. Connect to the core and retry.');
      const items = page.items.map(requireStagedProposal);
      set((state) => {
        // A page may have been read before a concurrent stage/review completed.
        // Keep those newer receipts, without retaining unrelated older pages on refresh.
        const newStages = state.stagedProposals.filter((proposal) =>
          !beforeRead.has(proposal.proposal_id) &&
          !items.some((item) => item.proposal_id === proposal.proposal_id));
        const newerVersions = state.stagedProposals.filter((proposal) =>
          items.some((item) => item.proposal_id === proposal.proposal_id &&
            proposal.review_revision > item.review_revision));
        return {
          stagedProposals: loadMore ? mergeStagedProposals(state.stagedProposals, items)
            : mergeStagedProposals([...newStages, ...items], newerVersions),
          stagedNextCursor: page.next_cursor,
          stagedLoading: false,
        };
      });
    } catch (e) {
      set({ stagedLoading: false, stagedError: String(e) });
    }
  },

  runStrategy: async (strategyId) => {
    const current = get().escalation;
    if (!current || current.running || current.proposal) return;
    const gapId = current.gapId;
    // An opened history record may share this gap ID. Only the originating
    // modal receives this run; every successful stage still enters history.
    let active = current;
    const patch = (fields: Partial<EscalationState>) =>
      set((state) => {
        if (state.escalation !== active) return {};
        active = { ...active, ...fields };
        return { escalation: active };
      });
    if (strategyId === 'cloud-opus') {
      // Cloud never runs from this click: load the exact preview so the
      // one-action consent dialog can show precisely what would leave.
      patch({ error: null });
      try {
        const preview = await invokeOr<EgressPreview | null>('escalation_preview', null, {
          gapId,
        });
        patch({ preview });
      } catch (e) {
        patch({ error: String(e) });
      }
      return;
    }
    patch({ running: true, error: null });
    try {
      const proposal = requireStagedProposal(await invokeOr<StagedProposal | null>('run_escalation', null, {
        gapId,
        mode: 'local',
        approvedPayloadHash: null,
      }));
      set((state) => ({ stagedProposals: mergeStagedProposals([proposal], state.stagedProposals) }));
      patch({ proposal, running: false, decided: proposal.review_decision });
    } catch (e) {
      patch({ error: String(e), running: false });
    }
  },

  consentAndRun: async (preview) => {
    const current = get().escalation;
    if (!current || current.running || current.proposal) return;
    const gapId = current.gapId;
    let active = current;
    const patch = (fields: Partial<EscalationState>) =>
      set((state) => {
        if (state.escalation !== active) return {};
        active = { ...active, ...fields };
        return { escalation: active };
      });
    patch({ preview: null, running: true, error: null });
    try {
      const proposal = requireStagedProposal(await invokeOr<StagedProposal | null>('run_escalation', null, {
        gapId,
        mode: 'cloud',
        approvedPayloadHash: preview.payload_hash,
      }));
      set((state) => ({ stagedProposals: mergeStagedProposals([proposal], state.stagedProposals) }));
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      set({ egress });
      patch({ proposal, running: false, decided: proposal.review_decision });
    } catch (e) {
      patch({ error: String(e), running: false });
    }
  },

  dismissPreview: () =>
    set((state) =>
      state.escalation ? { escalation: { ...state.escalation, preview: null } } : {},
    ),

  runClassEscalation: async (gapIds: string[]) => {
    const outcome = await invokeOr<ClassEscalationOutcome | null>('run_class_escalation', null, {
      gapIds,
      mode: 'local',
    });
    if (!outcome) throw new Error('The core did not return a saved class result.');
    const proposals = outcome.proposals.map(requireStagedProposal);
    set((state) => ({ stagedProposals: mergeStagedProposals(proposals, state.stagedProposals) }));
    return { ...outcome, proposals };
  },

  recordProposalDecision: async (proposal, decision) => {
    const proposalId = proposal.proposal_id;
    try {
      requireStagedProposal(proposal);
      const result = requireStagedProposal(await invokeOr<StagedProposal | null>(
        'record_agent_decision', null, {
          proposalId,
          expectedRevision: proposal.review_revision,
          decision,
          note: null,
        },
      ));
      if (result.proposal_id !== proposalId || result.review_decision !== decision ||
          result.review_revision !== proposal.review_revision + 1) {
        throw new Error('The core did not confirm this review. Refresh proposal history before retrying.');
      }
      // The review is durable and gives a new curated revision (ADR-0061):
      // the record's state is the host's, returned with the recompiled bundle.
      set((state) => {
        const opened = state.escalation?.proposal;
        const stagedProposals = mergeStagedProposals(state.stagedProposals, [
          result,
          ...(opened?.proposal_id === proposalId ? [opened] : []),
        ]);
        // Another review may have committed while this receipt was in flight.
        // History and an opened record both keep the highest known host revision.
        const latest = stagedProposals.find((item) => item.proposal_id === proposalId) ?? result;
        return {
          stagedProposals,
          stagedError: null,
          ...(state.escalation?.proposal?.proposal_id === proposalId ? {
            escalation: { ...state.escalation, proposal: latest, decided: latest.review_decision, error: null },
          } : {}),
        };
      });
      void get().reloadSpec();
      return true;
    } catch (e) {
      const error = String(e);
      set((state) => ({
        stagedError: error,
        ...(state.escalation?.proposal?.proposal_id === proposalId ? {
          escalation: { ...state.escalation, error },
        } : {}),
      }));
      return false;
    }
  },

  withdrawProposal: async () => {
    const current = get().escalation;
    if (!current?.proposal || current.reviewing || current.decided !== 'accepted') return;
    const proposalId = current.proposal.proposal_id;
    set({ escalation: { ...current, reviewing: true, error: null } });
    await get().recordProposalDecision(current.proposal, 'rejected');
    set((state) => state.escalation?.proposal?.proposal_id === proposalId
      ? { escalation: { ...state.escalation, reviewing: false } } : {});
  },

  decideProposal: async (decision) => {
    const current = get().escalation;
    if (!current?.proposal || current.reviewing || current.decided) return;
    const proposalId = current.proposal.proposal_id;
    set({ escalation: { ...current, reviewing: true, error: null } });
    await get().recordProposalDecision(current.proposal, decision);
    set((state) => state.escalation?.proposal?.proposal_id === proposalId
      ? { escalation: { ...state.escalation, reviewing: false } } : {});
  },

  revokeCloudConsent: async (tier) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('revoke_cloud_consent', null, {
        tier,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  draftAdapterWorkPackets: async (repo) => {
    const packets = await invokeOr<DraftAdapterPacket[] | null>(
      'draft_adapter_work_packets',
      null,
      { repo },
    );
    if (!packets) {
      throw new Error('unavailable_evidence');
    }
    return packets;
  },
}));
