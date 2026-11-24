//! The curated projection core (SPEC-12 slice 1, ADR-0061, AC-0652).
//!
//! [`project`] is a pure function: no I/O, no clock, no store access. The host
//! reads the base graph, the reviewed staged proposals, the SPEC-09 basis
//! assessment of each one and the Workbench assertion decisions, and passes
//! them in. The same inputs give the same [`CuratedProjection`] and the same
//! revision in any input order.
//!
//! Nothing here writes to the graph or changes a fact. An eligible record is
//! reported beside the recovered facts with the tier and confidence it was
//! produced with (R-INT-1, R-INT-2, R-INT-3; ADR-0036 decision 3).
//!
//! # Cost
//!
//! With `R` proposals, `N` nodes, `E` edges, `A` assessed evidence items and
//! `D` assertion decisions: one pass over the proposals to collect the node
//! ids and edge slots they name, one pass over the nodes and one over the
//! edges that parses provenance only for a named id or slot, one pass over
//! the proposals to assign states, and one sort of the reviewed records. Time
//! is `O(N + E + A + R log R + D log D)` with hash-index lookups; no record is
//! compared with every other record. Extra memory is `O(R + D)`.
//!
//! # Rules this module fixes
//!
//! - **Gap presence.** A Gap is in the graph when a node with the Gap's id
//!   exists and is a Gap by the Workbench predicate (`spec::is_gap_node`):
//!   label `Gap`, or provenance that is missing, invalid or Gap confidence.
//!   A node that kept the id and is no longer a Gap counts as gone.
//! - **State order.** First match wins: `unverified`, `already-covered`,
//!   `unresolvable`, `stale`, `conflicted`, `eligible`.
//! - **Already covered.** A base fact fills the slot when the base graph holds
//!   the exact edge the record proposes (same source, label and target) with
//!   valid provenance whose tier is not Agentic and whose confidence is not
//!   Gap. It is checked before `unresolvable` and `stale` and needs neither
//!   the Gap, the endpoint nodes nor an assessment: when a lower tier fills
//!   the slot the Gap is usually gone. An Agentic edge is another proposal,
//!   not a base fact. An edge from
//!   the same source to a different target is not counted: the graph cannot
//!   tie it to this Gap while slot cardinality is unproven (SPEC-12 open
//!   items).
//! - **Revision.** It covers every reviewed record's identity, decision,
//!   state and reason, so it identifies everything a read lists. A rejected
//!   record enters with no state and no reason.
//! - **Conflict.** Every accepted record counts, whatever its own state. Two
//!   accepted records for one Gap conflict when they differ in source, label
//!   or target. Records that propose the same edge assert the same thing and
//!   do not conflict; each stays eligible if nothing else applies.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

mod attested;
pub use attested::{
    AttestedAnswer, AttestedContribution, AttestedCounts, AttestedStanding, AttestedState,
    AttestedStateReason, CuratedAttested, ReviewedAttested,
};
mod drafts;
pub use drafts::{
    CuratedDraft, DraftAddress, DraftClaim, DraftCounts, DraftReview, DraftState, DraftStateReason,
    ReviewedDraft,
};
mod findings;
pub use findings::{
    CuratedFinding, FindingCounts, FindingCuration, FindingState, FindingStateReason,
    ReviewedFinding,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "curated/draft_tests.rs"]
mod draft_tests;

#[cfg(test)]
#[path = "curated/finding_tests.rs"]
mod finding_tests;

#[cfg(test)]
#[path = "curated/attested_tests.rs"]
mod attested_tests;

/// Version of the eligibility policy in this module. It enters the revision,
/// so a policy change gives a new revision for unchanged inputs.
pub const CURATED_POLICY_VERSION: &str = "curated-policy-v5";

/// Domain prefix of the curated revision. Version 2 added section drafts
/// beside proposals (ADR-0064 decision 7); version 3 adds findings accepted
/// as curated (ADR-0074 decision 6); version 4 adds attested records
/// (ADR-0090 decision 8). Policy v5 reads a record's reconciliation
/// (ADR-0072 slice 4) without changing the revision's framing.
pub const CURATED_REVISION_PREFIX: &str = "curated-revision-v4";

/// Domain prefix of the assertion decision digest.
pub const ASSERTION_DECISION_DIGEST_PREFIX: &str = "curated-assertion-decisions-v1";

/// The staged record version whose evidence can be captured primary source.
const CAPTURED_RECORD_VERSION: u32 = 2;
/// Version 3 carries its receipts' closure states (ADR-0072, AC-1088) and
/// is otherwise read as version 2.
const CLOSURE_RECORD_VERSION: u32 = 3;

/// Which view a read reports (DR-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratedView {
    /// Base facts only.
    Recovered,
    /// Base facts with the eligible accepted records beside them.
    Curated,
}

/// A human review of a staged proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// The reviewer accepted the proposal.
    Accepted,
    /// The reviewer rejected the proposal; also how acceptance is withdrawn.
    Rejected,
}

/// One evidence item of a staged proposal and whether it is captured
/// primary-source evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEvidence {
    /// Citation identity in the original task.
    pub evidence_id: String,
    /// True for captured primary-source evidence; false for a working-tree
    /// read that nothing verified.
    pub captured: bool,
}

/// A staged proposal as the projection reads it. Review notes, timestamps and
/// review revision counters are not part of this type, so they cannot reach
/// the revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedProposal {
    /// Stage identity of the record.
    pub record_id: String,
    /// Staging envelope version: 1 is legacy, 2 carries a source basis, 3 also
    /// its receipts' closure states.
    pub schema_version: u32,
    /// Gap the proposal would fill.
    pub gap_id: String,
    /// Source of the proposed edge.
    pub source_id: String,
    /// Target of the proposed edge.
    pub target_id: String,
    /// Label of the proposed edge.
    pub edge_label: String,
    /// Producing tier, exactly as produced.
    pub tier: Tier,
    /// Producing confidence, exactly as produced.
    pub confidence: ConfidenceTier,
    /// Current review; `None` is a pending record.
    pub decision: Option<ReviewDecision>,
    /// Every evidence item supplied to the producing task.
    pub evidence: Vec<RecordEvidence>,
}

/// One SPEC-09 comparison result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisComparison {
    /// The compared content matched at the observation.
    Unchanged,
    /// The compared content differs.
    Changed,
    /// A version 1 record: nothing to compare.
    LegacyUnverified,
    /// The comparison could not be made.
    OperationalFailure,
}

/// SPEC-09 availability of one retained evidence item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    /// The retained bytes were read and match the staged fingerprint.
    Available,
    /// The retained bytes are known to be missing or forgotten.
    Unavailable,
    /// The retained selection does not match the staged record.
    Invalid,
    /// The read failed for a busy or storage reason.
    OperationalFailure,
    /// The read would exceed the validation byte bound.
    ValidationByteLimit,
    /// The item is a working-tree read that nothing verified.
    WorkingTreeUnverified,
}

/// Availability of one evidence item, by its citation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceStatus {
    /// Citation identity in the original task.
    pub evidence_id: String,
    /// What the host observed for it.
    pub status: EvidenceAvailability,
}

/// The dimensions of one completed SPEC-09 assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisDimensions {
    /// Current graph against the staged snapshot.
    pub graph: BasisComparison,
    /// Current selected associations against the staged ones.
    pub association: BasisComparison,
    /// Availability of each evidence item.
    pub evidence: Vec<EvidenceStatus>,
    /// Number of items the assessment counted as unverified.
    pub unverified_evidence: usize,
    /// The files the record's captured evidence cites, at the current
    /// source associations, against the ones it was made with (#2251):
    /// `unchanged` when every one is byte-identical, `legacy_unverified`
    /// when they were not compared.
    #[serde(default = "not_compared")]
    pub cited_files: BasisComparison,
}

fn not_compared() -> BasisComparison {
    BasisComparison::LegacyUnverified
}

/// The host's basis assessment of one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BasisAssessment {
    /// The assessment completed with these dimensions.
    Assessed(BasisDimensions),
    /// The assessment itself failed operationally.
    Failed,
    /// The record's latest explicit reconciliation (ADR-0072 slice 4), which
    /// the host supplies in place of the SPEC-09 assessment when it decides
    /// the record: a current `reconciled`, `changed` or `missing` result, or
    /// a stale or unverifiable one for a record the assessment cannot make
    /// eligible.
    Reconciliation(RecordReconciliation),
}

/// A record's derived reconciliation state as the projection reads it
/// (ADR-0072 slice 4, AC-1076).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecordReconciliation {
    /// The latest result is current and every item matched: the record's
    /// evidence is read from it instead of from the staged origin.
    Reconciled {
        /// Captured items whose recaptured span and fact match exactly.
        retained_match: usize,
        /// Working-tree items whose recapture gives the same text.
        recapture_corroborated: usize,
    },
    /// The latest result found a cited item, candidate or slot changed.
    Changed,
    /// The latest result found something missing.
    Missing,
    /// The latest result could not compare everything.
    Unverifiable,
    /// The latest result no longer describes the current recovery.
    Stale,
}

/// A Workbench decision on one inferred assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionReview {
    /// Keep the inference at its own tier.
    Accepted,
    /// Suppress the inference while its content hash matches.
    Rejected,
    /// Keep the inference and attach a note.
    Annotated,
}

/// A Workbench assertion decision, keyed by the assertion's content hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssertionDecisionEntry {
    /// Content hash of the reviewed assertion.
    pub content_hash: String,
    /// The decision.
    pub decision: AssertionReview,
}

/// Everything [`project`] reads. All of it is borrowed; nothing is fetched.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionInput<'a> {
    /// Base graph nodes.
    pub nodes: &'a [Node],
    /// Base graph edges.
    pub edges: &'a [Edge],
    /// Content hash of that base graph.
    pub graph_content_hash: &'a str,
    /// Staged proposals: pending, accepted and rejected, in any order.
    pub proposals: &'a [ReviewedProposal],
    /// Basis assessment per record identity. An accepted record that needs
    /// one and has none is listed as stale.
    pub assessments: &'a BTreeMap<String, BasisAssessment>,
    /// Workbench assertion decisions, in any order.
    pub assertion_decisions: &'a [AssertionDecisionEntry],
    /// Content hashes of the assertions that can be curated now.
    pub current_assertion_hashes: &'a BTreeSet<String>,
    /// Section drafts: pending, accepted, rejected and returned, in any
    /// order (SPEC-15).
    pub drafts: &'a [ReviewedDraft],
    /// Current content hash of each fact a draft cites, by citation id. An
    /// id that resolves to no fact in the current graph is absent.
    pub cited_fact_hashes: &'a BTreeMap<String, String>,
    /// Saved findings with their curation review, reviewed or not, in any
    /// order (ADR-0074).
    pub findings: &'a [ReviewedFinding],
    /// Current digest of each fact a finding cites, by citation id. An id
    /// that resolves to no fact in the current graph is absent.
    pub cited_fact_digests: &'a BTreeMap<String, String>,
    /// Stored attested records with their standing, in any order
    /// (ADR-0090). Their supporting ids are looked up in
    /// `cited_fact_hashes`.
    pub attested: &'a [ReviewedAttested],
    /// Policy version; pass [`CURATED_POLICY_VERSION`].
    pub policy_version: &'a str,
}

/// The one state of an accepted record (ADR-0061 decision 4). The variants
/// are in first-match order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordState {
    /// Version 1 record, or evidence that is not all captured primary
    /// source, unless a current reconciliation matched every item; or a
    /// reconciliation that did not (ADR-0072 slice 4).
    Unverified,
    /// A base fact already holds the proposed edge, whether or not the Gap
    /// and the endpoints are still in the graph.
    AlreadyCovered,
    /// The Gap, the source or the target is not in the graph.
    Unresolvable,
    /// The basis moved, a captured item is not available, or the assessment
    /// failed.
    Stale,
    /// Another accepted record proposes a different edge for the same Gap.
    Conflicted,
    /// None of the above.
    Eligible,
}

/// Why a record has its state. One reason per record: the first that applies
/// in the order of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateReason {
    /// `unverified`: the latest reconciliation found the record changed.
    ReconciliationChanged,
    /// `unverified`: the latest reconciliation found something missing.
    ReconciliationMissing,
    /// `unverified`: the latest reconciliation could not compare everything.
    ReconciliationUnverifiable,
    /// `unverified`: the latest reconciliation no longer describes the
    /// current recovery.
    ReconciliationStale,
    /// `unverified`: a version 1 record.
    LegacyRecord,
    /// `unverified`: a record version this policy does not know.
    UnsupportedRecordVersion,
    /// `unverified`: the record has no evidence item.
    NoEvidence,
    /// `unverified`: an evidence item is not captured primary source.
    EvidenceNotCaptured,
    /// `already-covered`: the base graph holds the proposed edge.
    BaseFactFillsSlot,
    /// `unresolvable`: no node has the Gap's id.
    GapMissing,
    /// `unresolvable`: the node with the Gap's id is no longer a Gap.
    GapNoLongerGap,
    /// `unresolvable`: the source node is not in the graph.
    SourceMissing,
    /// `unresolvable`: the target node is not in the graph.
    TargetMissing,
    /// `stale`: the host supplied no assessment for the record.
    AssessmentMissing,
    /// `stale`: the assessment failed operationally.
    AssessmentFailed,
    /// `stale`: the graph comparison could not be made.
    GraphComparisonFailed,
    /// `stale`: the graph comparison is not `unchanged`.
    GraphChanged,
    /// `stale`: the association comparison could not be made.
    AssociationComparisonFailed,
    /// `stale`: the association comparison is not `unchanged`.
    AssociationChanged,
    /// `stale`: the graph or the associations changed, but every file the
    /// record's evidence cites is byte-identical at the current associations
    /// and every evidence item is available (#2251).
    CitedFilesUnchanged,
    /// `stale`: a captured item has no availability result.
    EvidenceNotAssessed,
    /// `stale`: a captured item's read failed operationally.
    EvidenceReadFailed,
    /// `stale`: a captured item is not available.
    EvidenceUnavailable,
    /// `stale`: the assessment counts unverified items the record does not.
    AssessmentMismatch,
    /// `conflicted`: another accepted record proposes a different edge.
    DifferentTargetAccepted,
    /// `eligible`: every condition holds.
    AllConditionsHold,
}

/// How much of a record's evidence is captured (DR-4 `source_support`).
/// A reconciled record's items are counted as the reconciliation found
/// them, and a corroborated working-tree item is counted apart from a
/// captured one (ADR-0072 slice 4, R-INT-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSupport {
    /// Captured primary-source items (for a reconciled record, those whose
    /// retained span and fact matched).
    pub captured: usize,
    /// Working-tree items a reconciliation's recapture corroborated. Left
    /// out of the encoding when zero, so a record that was never reconciled
    /// reads as before.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub corroborated: usize,
    /// Items that nothing verified.
    pub unverified: usize,
}

/// The SPEC-09 dimensions a record's state was read beside. Reported for
/// every record, whichever condition decided its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BasisReport {
    /// The host supplied no assessment.
    NotSupplied,
    /// The assessment failed operationally.
    Failed,
    /// The host supplied the record's reconciliation instead.
    Reconciliation(RecordReconciliation),
    /// The assessment completed.
    Assessed {
        /// Graph comparison.
        graph: BasisComparison,
        /// Association comparison.
        association: BasisComparison,
        /// Items reported available.
        available: usize,
        /// Items reported with any other status.
        not_available: usize,
        /// Items the assessment counted as unverified.
        unverified: usize,
    },
}

/// One accepted record with its DR-4 fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedRecord {
    /// Stage identity of the record.
    pub record_id: String,
    /// Gap the record names.
    pub gap_id: String,
    /// Source of the proposed edge.
    pub source_id: String,
    /// Target of the proposed edge.
    pub target_id: String,
    /// Label of the proposed edge.
    pub edge_label: String,
    /// Producing tier, unchanged.
    pub tier: Tier,
    /// Producing confidence, unchanged.
    pub confidence: ConfidenceTier,
    /// The review; always accepted for a record in the projection.
    pub review_status: ReviewDecision,
    /// Captured and unverified evidence counts.
    pub source_support: SourceSupport,
    /// The SPEC-09 dimensions read for the record.
    pub basis: BasisReport,
    /// The assigned state.
    pub state: RecordState,
    /// Why that state applies.
    pub reason: StateReason,
}

/// A Workbench decision whose content hash matches no current assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleAssertionDecision {
    /// Content hash the decision was recorded for.
    pub content_hash: String,
    /// The recorded decision.
    pub decision: AssertionReview,
    /// Always [`RecordState::Stale`].
    pub state: RecordState,
}

/// The Workbench assertion decisions as the projection reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionDecisionSummary {
    /// Digest over the rejections applied and the stale decisions.
    pub digest: String,
    /// Rejections whose content hash matches a current assertion.
    pub rejections_applied: usize,
    /// Decisions that match no current assertion, ordered by content hash.
    pub stale: Vec<StaleAssertionDecision>,
}

/// Record totals. `accepted` is the sum of every state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedCounts {
    /// Accepted records.
    pub accepted: usize,
    /// Records that are curated items.
    pub eligible: usize,
    /// Records listed as `unverified`.
    pub unverified: usize,
    /// Records listed as `unresolvable`.
    pub unresolvable: usize,
    /// Records listed as `stale`.
    pub stale: usize,
    /// Records listed as `already-covered`.
    pub already_covered: usize,
    /// Records listed as `conflicted`.
    pub conflicted: usize,
}

/// The curated projection (SPEC-12 "The projection").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedProjection {
    /// Always [`CuratedView::Curated`].
    pub view: CuratedView,
    /// The curated revision.
    pub revision: String,
    /// Base graph content hash the projection was computed over.
    pub graph_basis: String,
    /// Eligible records, ordered by record identity.
    pub items: Vec<CuratedRecord>,
    /// Every other accepted record, ordered by record identity.
    pub listed: Vec<CuratedRecord>,
    /// Every accepted section draft with its state, ordered by identity.
    #[serde(default)]
    pub drafts: Vec<CuratedDraft>,
    /// Draft totals.
    #[serde(default)]
    pub draft_counts: DraftCounts,
    /// Every finding accepted as curated with its state, ordered by
    /// identity.
    #[serde(default)]
    pub findings: Vec<CuratedFinding>,
    /// Finding totals.
    #[serde(default)]
    pub finding_counts: FindingCounts,
    /// Every attested record with its state, ordered by identity.
    #[serde(default)]
    pub attested: Vec<CuratedAttested>,
    /// Attested totals.
    #[serde(default)]
    pub attested_counts: AttestedCounts,
    /// Assertion decision digest and stale decisions.
    pub assertion_decisions: AssertionDecisionSummary,
    /// Record totals.
    pub counts: CuratedCounts,
}

/// Input that cannot be projected. The projection fails closed: it never
/// returns a result built from an ambiguous record set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// Two proposals share one record identity.
    #[error("duplicate curated record identity")]
    DuplicateRecord,
    /// Two assertion decisions share one content hash.
    #[error("duplicate assertion decision content hash")]
    DuplicateAssertionDecision,
    /// The revision input could not be encoded; no revision is invented.
    #[error("curated revision could not be encoded")]
    Encoding,
}

type EdgeKey<'a> = (&'a str, &'a str, &'a str);

/// What the base graph says about the ids and slots the accepted records
/// name. Built in one pass over the nodes and one over the edges.
struct GraphIndex<'a> {
    /// Named node id to "is a Gap".
    nodes: HashMap<&'a str, bool>,
    /// Proposed edges the base graph already holds as a base fact.
    covered: HashSet<EdgeKey<'a>>,
}

fn valid_provenance(props: &serde_json::Value) -> Option<Provenance> {
    serde_json::from_value::<Provenance>(props.get("prov")?.clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

/// Whether `node` is a Gap: label `Gap`, or provenance that is missing,
/// invalid or Gap confidence. Mirrors `spec::is_gap_node`; a parity test in
/// `spec`, which sees both, fails when the two differ (SPEC-12 slice 2).
pub fn is_gap(node: &Node) -> bool {
    node.label == "Gap"
        || valid_provenance(&node.props)
            .is_none_or(|provenance| provenance.confidence_tier == ConfidenceTier::Gap)
}

/// A base fact: valid provenance, not produced by an agent, not a Gap.
fn is_base_fact(edge: &Edge) -> bool {
    valid_provenance(&edge.props).is_some_and(|provenance| {
        provenance.tier != Tier::Agentic && provenance.confidence_tier != ConfidenceTier::Gap
    })
}

fn edge_key(record: &ReviewedProposal) -> EdgeKey<'_> {
    (&record.source_id, &record.edge_label, &record.target_id)
}

impl<'a> GraphIndex<'a> {
    fn build(input: &ProjectionInput<'a>, accepted: &[&'a ReviewedProposal]) -> Self {
        let mut named = HashSet::new();
        let mut slots = HashSet::new();
        for record in accepted {
            named.extend([
                record.gap_id.as_str(),
                record.source_id.as_str(),
                record.target_id.as_str(),
            ]);
            slots.insert(edge_key(record));
        }
        let mut nodes = HashMap::new();
        for node in input.nodes {
            if let Some(id) = named.get(node.id.as_str()) {
                *nodes.entry(*id).or_insert(false) |= is_gap(node);
            }
        }
        let mut covered = HashSet::new();
        for edge in input.edges {
            let key = (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str());
            if let Some(slot) = slots.get(&key)
                && is_base_fact(edge)
            {
                covered.insert(*slot);
            }
        }
        Self { nodes, covered }
    }
}

/// Gaps for which accepted records propose more than one distinct edge.
fn conflicted_gaps<'a>(accepted: &[&'a ReviewedProposal]) -> HashSet<&'a str> {
    let mut first: HashMap<&str, EdgeKey<'_>> = HashMap::new();
    let mut conflicted = HashSet::new();
    for record in accepted {
        let key = edge_key(record);
        if *first.entry(&record.gap_id).or_insert(key) != key {
            conflicted.insert(record.gap_id.as_str());
        }
    }
    conflicted
}

/// `unverified`, read from the record's reconciliation when the host
/// supplied one (ADR-0072 slice 4), and otherwise from its staged origin.
fn unverified(
    record: &ReviewedProposal,
    assessment: Option<&BasisAssessment>,
) -> Option<StateReason> {
    match assessment {
        Some(BasisAssessment::Reconciliation(reconciliation)) => match reconciliation {
            RecordReconciliation::Reconciled { .. } if record.evidence.is_empty() => {
                Some(StateReason::NoEvidence)
            }
            RecordReconciliation::Reconciled { .. } => None,
            RecordReconciliation::Changed => Some(StateReason::ReconciliationChanged),
            RecordReconciliation::Missing => Some(StateReason::ReconciliationMissing),
            RecordReconciliation::Unverifiable => Some(StateReason::ReconciliationUnverifiable),
            RecordReconciliation::Stale => Some(StateReason::ReconciliationStale),
        },
        _ => unverified_origin(record),
    }
}

fn unverified_origin(record: &ReviewedProposal) -> Option<StateReason> {
    match record.schema_version {
        1 => Some(StateReason::LegacyRecord),
        CAPTURED_RECORD_VERSION | CLOSURE_RECORD_VERSION if record.evidence.is_empty() => {
            Some(StateReason::NoEvidence)
        }
        CAPTURED_RECORD_VERSION | CLOSURE_RECORD_VERSION
            if record.evidence.iter().any(|item| !item.captured) =>
        {
            Some(StateReason::EvidenceNotCaptured)
        }
        CAPTURED_RECORD_VERSION | CLOSURE_RECORD_VERSION => None,
        _ => Some(StateReason::UnsupportedRecordVersion),
    }
}

fn unresolvable(record: &ReviewedProposal, graph: &GraphIndex<'_>) -> Option<StateReason> {
    match graph.nodes.get(record.gap_id.as_str()) {
        None => return Some(StateReason::GapMissing),
        Some(false) => return Some(StateReason::GapNoLongerGap),
        Some(true) => {}
    }
    if !graph.nodes.contains_key(record.source_id.as_str()) {
        return Some(StateReason::SourceMissing);
    }
    if !graph.nodes.contains_key(record.target_id.as_str()) {
        return Some(StateReason::TargetMissing);
    }
    None
}

fn comparison(
    value: BasisComparison,
    failed: StateReason,
    changed: StateReason,
) -> Option<StateReason> {
    match value {
        BasisComparison::Unchanged => None,
        BasisComparison::OperationalFailure => Some(failed),
        BasisComparison::Changed | BasisComparison::LegacyUnverified => Some(changed),
    }
}

/// Called only for a record whose evidence is all captured.
fn stale(record: &ReviewedProposal, assessment: Option<&BasisAssessment>) -> Option<StateReason> {
    let dimensions = match assessment {
        None => return Some(StateReason::AssessmentMissing),
        Some(BasisAssessment::Failed) => return Some(StateReason::AssessmentFailed),
        // A current reconciliation compared the record with this recovery.
        Some(BasisAssessment::Reconciliation(RecordReconciliation::Reconciled { .. })) => {
            return None;
        }
        // Not reached: `unverified` lists every other reconciliation.
        Some(BasisAssessment::Reconciliation(_)) => return Some(StateReason::AssessmentMismatch),
        Some(BasisAssessment::Assessed(dimensions)) => dimensions,
    };
    let graph = comparison(
        dimensions.graph,
        StateReason::GraphComparisonFailed,
        StateReason::GraphChanged,
    );
    let association = comparison(
        dimensions.association,
        StateReason::AssociationComparisonFailed,
        StateReason::AssociationChanged,
    );
    // A failed comparison is never softened, and neither is a change while
    // the record's own cited files were not shown unchanged (#2251).
    let failed = |r: &StateReason| {
        matches!(
            r,
            StateReason::GraphComparisonFailed | StateReason::AssociationComparisonFailed
        )
    };
    if let Some(reason) = graph.into_iter().chain(association).find(failed) {
        return Some(reason);
    }
    let moved = graph.or(association);
    if let Some(reason) = moved
        && dimensions.cited_files != BasisComparison::Unchanged
    {
        return Some(reason);
    }
    let statuses: HashMap<&str, EvidenceAvailability> = dimensions
        .evidence
        .iter()
        .map(|item| (item.evidence_id.as_str(), item.status))
        .collect();
    // A duplicated id would hide one status behind another.
    if statuses.len() != dimensions.evidence.len() {
        return Some(StateReason::AssessmentMismatch);
    }
    let mut reason = None;
    for item in &record.evidence {
        match statuses.get(item.evidence_id.as_str()) {
            Some(EvidenceAvailability::Available) => {}
            None => return Some(StateReason::EvidenceNotAssessed),
            Some(EvidenceAvailability::OperationalFailure) => {
                reason.get_or_insert(StateReason::EvidenceReadFailed);
            }
            Some(_) => reason = Some(StateReason::EvidenceUnavailable),
        }
    }
    reason
        .or((dimensions.unverified_evidence != 0).then_some(StateReason::AssessmentMismatch))
        .or(moved.map(|_| StateReason::CitedFilesUnchanged))
}

fn basis_report(assessment: Option<&BasisAssessment>) -> BasisReport {
    match assessment {
        None => BasisReport::NotSupplied,
        Some(BasisAssessment::Failed) => BasisReport::Failed,
        Some(BasisAssessment::Reconciliation(reconciliation)) => {
            BasisReport::Reconciliation(*reconciliation)
        }
        Some(BasisAssessment::Assessed(dimensions)) => {
            let available = dimensions
                .evidence
                .iter()
                .filter(|item| item.status == EvidenceAvailability::Available)
                .count();
            BasisReport::Assessed {
                graph: dimensions.graph,
                association: dimensions.association,
                available,
                not_available: dimensions.evidence.len() - available,
                unverified: dimensions.unverified_evidence,
            }
        }
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// DR-4 `source_support`: from a reconciled result when the host supplied
/// one, and otherwise from the staged origin.
fn source_support(
    record: &ReviewedProposal,
    assessment: Option<&BasisAssessment>,
) -> SourceSupport {
    let total = record.evidence.len();
    match assessment {
        Some(BasisAssessment::Reconciliation(RecordReconciliation::Reconciled {
            retained_match,
            recapture_corroborated,
        })) => SourceSupport {
            captured: *retained_match,
            corroborated: *recapture_corroborated,
            unverified: total.saturating_sub(retained_match + recapture_corroborated),
        },
        _ => {
            let captured = record.evidence.iter().filter(|item| item.captured).count();
            SourceSupport {
                captured,
                corroborated: 0,
                unverified: total - captured,
            }
        }
    }
}

/// The first state that applies, with its reason, in the order `unverified`,
/// `already-covered`, `unresolvable`, `stale`, `conflicted`, `eligible`.
fn assign(
    record: &ReviewedProposal,
    assessment: Option<&BasisAssessment>,
    graph: &GraphIndex<'_>,
    conflicted: &HashSet<&str>,
) -> (RecordState, StateReason) {
    if let Some(reason) = unverified(record, assessment) {
        return (RecordState::Unverified, reason);
    }
    // Before `unresolvable` and `stale`: once a lower tier resolves the slot
    // the Gap node is usually gone and the basis has moved, and "covered" is
    // the true answer. It needs no Gap, no endpoint node and no assessment.
    if graph.covered.contains(&edge_key(record)) {
        return (RecordState::AlreadyCovered, StateReason::BaseFactFillsSlot);
    }
    if let Some(reason) = unresolvable(record, graph) {
        return (RecordState::Unresolvable, reason);
    }
    if let Some(reason) = stale(record, assessment) {
        return (RecordState::Stale, reason);
    }
    if conflicted.contains(record.gap_id.as_str()) {
        return (
            RecordState::Conflicted,
            StateReason::DifferentTargetAccepted,
        );
    }
    (RecordState::Eligible, StateReason::AllConditionsHold)
}

/// `prefix:blake3(json([prefix, value]))`, the framing `mcp-read-basis-v1`
/// uses. A JSON array of strings and arrays has one parse, so fields cannot
/// run into each other.
fn digest(prefix: &str, value: &impl Serialize) -> Result<String, ProjectionError> {
    let bytes = serde_json::to_vec(&(prefix, value)).map_err(|_| ProjectionError::Encoding)?;
    Ok(format!("{prefix}:{}", core_prov::content_hash(&bytes)))
}

fn summarize_assertions(
    input: &ProjectionInput<'_>,
) -> Result<AssertionDecisionSummary, ProjectionError> {
    let mut decisions: Vec<&AssertionDecisionEntry> = input.assertion_decisions.iter().collect();
    decisions.sort_unstable();
    if decisions
        .windows(2)
        .any(|pair| pair[0].content_hash == pair[1].content_hash)
    {
        return Err(ProjectionError::DuplicateAssertionDecision);
    }
    let mut applied = Vec::new();
    let mut stale = Vec::new();
    for entry in decisions {
        if !input.current_assertion_hashes.contains(&entry.content_hash) {
            stale.push(StaleAssertionDecision {
                content_hash: entry.content_hash.clone(),
                decision: entry.decision,
                state: RecordState::Stale,
            });
        } else if entry.decision == AssertionReview::Rejected {
            applied.push(entry.content_hash.as_str());
        }
    }
    let stale_keys: Vec<(&str, AssertionReview)> = stale
        .iter()
        .map(|entry| (entry.content_hash.as_str(), entry.decision))
        .collect();
    Ok(AssertionDecisionSummary {
        digest: digest(ASSERTION_DECISION_DIGEST_PREFIX, &(&applied, &stale_keys))?,
        rejections_applied: applied.len(),
        stale,
    })
}

fn count(counts: &mut CuratedCounts, state: RecordState) {
    counts.accepted += 1;
    *match state {
        RecordState::Unverified => &mut counts.unverified,
        RecordState::Unresolvable => &mut counts.unresolvable,
        RecordState::Stale => &mut counts.stale,
        RecordState::AlreadyCovered => &mut counts.already_covered,
        RecordState::Conflicted => &mut counts.conflicted,
        RecordState::Eligible => &mut counts.eligible,
    } += 1;
}

/// Compute the curated projection (SPEC-12, ADR-0061).
///
/// Pending and rejected proposals are neither items nor listed. A rejected
/// proposal still enters the revision with its decision and no state, so a
/// review of any kind gives a new revision.
///
/// Section drafts are the second record kind (SPEC-15, ADR-0064 decision
/// 7): each accepted draft gets one state, and every accepted or rejected
/// draft enters the revision.
///
/// Findings accepted as curated are the third (ADR-0074): each accepted
/// finding gets one state, and every accepted or rejected finding enters
/// the revision.
///
/// `revision = digest("curated-revision-v4", policy_version,
/// graph_content_hash, [(record_id, decision, state, reason)…],
/// [(draft_id, decision, state, reason)…],
/// [(finding_ref, decision, state, reason)…],
/// [(attested_id, state, reason, attestations)…], assertion_decision_digest)`,
/// records, drafts, findings and attested records each in identity order.
pub fn project(input: &ProjectionInput<'_>) -> Result<CuratedProjection, ProjectionError> {
    let mut reviewed: Vec<(&ReviewedProposal, ReviewDecision)> = input
        .proposals
        .iter()
        .filter_map(|record| Some((record, record.decision?)))
        .collect();
    reviewed.sort_unstable_by(|left, right| left.0.record_id.cmp(&right.0.record_id));
    // Pending records are checked too: one identity must name one record.
    let mut identities = HashSet::with_capacity(input.proposals.len());
    if !input
        .proposals
        .iter()
        .all(|record| identities.insert(record.record_id.as_str()))
    {
        return Err(ProjectionError::DuplicateRecord);
    }
    let accepted: Vec<&ReviewedProposal> = reviewed
        .iter()
        .filter(|(_, decision)| *decision == ReviewDecision::Accepted)
        .map(|(record, _)| *record)
        .collect();
    let graph = GraphIndex::build(input, &accepted);
    let conflicted = conflicted_gaps(&accepted);

    let mut items = Vec::new();
    let mut listed = Vec::new();
    let mut counts = CuratedCounts::default();
    let mut revision_records = Vec::with_capacity(reviewed.len());
    for (record, decision) in reviewed {
        if decision == ReviewDecision::Rejected {
            revision_records.push((record.record_id.as_str(), decision, None, None));
            continue;
        }
        let assessment = input.assessments.get(&record.record_id);
        let (state, reason) = assign(record, assessment, &graph, &conflicted);
        revision_records.push((
            record.record_id.as_str(),
            decision,
            Some(state),
            Some(reason),
        ));
        count(&mut counts, state);
        let source_support = source_support(record, assessment);
        let entry = CuratedRecord {
            record_id: record.record_id.clone(),
            gap_id: record.gap_id.clone(),
            source_id: record.source_id.clone(),
            target_id: record.target_id.clone(),
            edge_label: record.edge_label.clone(),
            tier: record.tier,
            confidence: record.confidence,
            review_status: decision,
            source_support,
            basis: basis_report(assessment),
            state,
            reason,
        };
        if state == RecordState::Eligible {
            items.push(entry);
        } else {
            listed.push(entry);
        }
    }
    let attested =
        attested::project_attested(input.attested, input.nodes, input.cited_fact_hashes)?;
    // A draft may cite a printable attested record by its `A-` id beside
    // facts (ADR-0090 decision 6). The draft recorded the record's identity
    // as that id's hash, and the identity covers the record's content, so
    // the id resolves while the record may be printed and never drifts.
    let mut citable = std::borrow::Cow::Borrowed(input.cited_fact_hashes);
    for record in &attested.records {
        if record.printed_label().is_some() {
            citable
                .to_mut()
                .insert(record.citation_id.clone(), record.record_id.clone());
        }
    }
    let drafts = drafts::project_drafts(input.drafts, &citable)?;
    let findings = findings::project_findings(input.findings, input.cited_fact_digests)?;
    let assertion_decisions = summarize_assertions(input)?;
    let revision = digest(
        CURATED_REVISION_PREFIX,
        &(
            input.policy_version,
            input.graph_content_hash,
            &revision_records,
            &drafts.revision,
            &findings.revision,
            &attested.revision,
            &assertion_decisions.digest,
        ),
    )?;
    Ok(CuratedProjection {
        view: CuratedView::Curated,
        revision,
        graph_basis: input.graph_content_hash.to_string(),
        items,
        listed,
        drafts: drafts.drafts,
        draft_counts: drafts.counts,
        findings: findings.findings,
        finding_counts: findings.counts,
        attested: attested.records,
        attested_counts: attested.counts,
        assertion_decisions,
        counts,
    })
}
