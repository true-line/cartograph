//! The host side of the curated projection (SPEC-12 slice 2, ADR-0061,
//! AC-0653): the reads that feed `context_hub::curated::project`.
//!
//! It runs in two steps, because the SPEC-09 assessment takes the source
//! registry, the retention leases and the graph lock itself and so cannot run
//! while an export holds the graph:
//!
//! 1. [`gather`], with no graph or decision lock held: one bounded read of
//!    the staging store, then one assessment for each accepted record that
//!    could be eligible. Nothing is persisted.
//! 2. [`project`], under the export's own graph and decision locks: the
//!    assessments are checked against the store revision the export reads,
//!    and the pure projection is computed over the export's own snapshot.
//!
//! An MCP read session (slice 4, AC-0654) runs the same two steps over the
//! snapshot it froze, and takes its reviews through [`ReviewSource`]. It
//! gathers only after its own bounded graph read succeeded, and it never
//! makes the compile of [`DerivedDecisions::Compile`].

use agents::ProposalDecision;
use agents::staging::{MAX_STAGED_PAGE_ITEMS, MAX_STAGED_REFERENCE_SCAN};
use context_hub::curated::{
    self as core, BasisAssessment, BasisDimensions, CuratedProjection, EvidenceAvailability,
    EvidenceStatus, ProjectionInput, ReviewedProposal,
};
use core_graph::{Edge, Node, SqliteGraphStore, StoreRevision};
use core_prov::{ConfidenceTier, Provenance, Tier};
use spec::CuratedFailure;
use std::collections::{BTreeMap, BTreeSet};

use crate::AppState;
use crate::task_evidence::assessment::{self, Availability, BasisComparison};

/// Most staged records one projection reads, pending and reviewed together.
/// A store that holds more fails the read; nothing is projected from a part
/// of it.
pub(crate) const MAX_CURATED_RECORD_SCAN: usize = MAX_STAGED_REFERENCE_SCAN;
/// Most accepted records one projection takes. Each one that could be
/// eligible costs one SPEC-09 assessment, and each assessment copies the
/// graph, so this bounds the work of one read. More than this fails the
/// read.
pub(crate) const MAX_CURATED_ACCEPTED: usize = 256;

/// What [`gather`] read: the reviewed records and the assessments made for
/// them, or why the read did not finish. The default is a store with no
/// reviewed record.
///
/// A read that fails does not fail the export: the recovered documents do
/// not depend on the review stores. It holds no record, so nothing from it
/// is projected, and `curated.md` states the reason.
#[derive(Debug, Default)]
pub(crate) struct CuratedReview {
    failure: Option<CuratedFailure>,
    proposals: Vec<ReviewedProposal>,
    /// By record identity, with the store revision the assessment's graph
    /// read observed (`None` when that read failed).
    assessments: BTreeMap<String, (BasisAssessment, Option<StoreRevision>)>,
}

/// Whether one artifact's bytes depend on the curated projection: the
/// document itself, and the fact index that lists what it cites.
pub(crate) fn artifact_reads_projection(artifact_id: &str) -> bool {
    matches!(artifact_id, "curated" | "fact-index" | "fact-index-json")
}

fn comparison(value: BasisComparison) -> core::BasisComparison {
    match value {
        BasisComparison::Unchanged => core::BasisComparison::Unchanged,
        BasisComparison::Changed => core::BasisComparison::Changed,
        BasisComparison::LegacyUnverified => core::BasisComparison::LegacyUnverified,
        BasisComparison::OperationalFailure => core::BasisComparison::OperationalFailure,
    }
}

fn availability(value: Availability) -> EvidenceAvailability {
    match value {
        Availability::Available => EvidenceAvailability::Available,
        Availability::Unavailable => EvidenceAvailability::Unavailable,
        Availability::Invalid => EvidenceAvailability::Invalid,
        Availability::OperationalFailure => EvidenceAvailability::OperationalFailure,
        Availability::ValidationByteLimit => EvidenceAvailability::ValidationByteLimit,
        Availability::WorkingTreeUnverified => EvidenceAvailability::WorkingTreeUnverified,
    }
}

/// The SPEC-09 assessment as projection input. The assessment reports one
/// status per recorded origin, and one citation id can have several origins;
/// the projection takes one status per id. An id is available only when
/// every origin is, and otherwise carries its first status that is not.
fn dimensions(assessed: assessment::BasisAssessment) -> BasisDimensions {
    let mut evidence: Vec<EvidenceStatus> = Vec::new();
    for item in assessed.evidence {
        let status = availability(item.status);
        match evidence
            .iter_mut()
            .find(|seen| seen.evidence_id == item.evidence_id)
        {
            Some(seen) if seen.status == EvidenceAvailability::Available => seen.status = status,
            Some(_) => {}
            None => evidence.push(EvidenceStatus {
                evidence_id: item.evidence_id,
                status,
            }),
        }
    }
    BasisDimensions {
        graph: comparison(assessed.graph_comparison),
        association: comparison(assessed.association_comparison),
        evidence,
        unverified_evidence: assessed.unverified_evidence,
    }
}

/// Whether the projection consults an assessment for `record` at all. A
/// record that is not version 2, has no evidence or has an item that is not
/// captured is `unverified` from the record alone (ADR-0061 decision 4), so
/// no retained byte is read for it.
fn needs_assessment(record: &ReviewedProposal) -> bool {
    record.decision == Some(core::ReviewDecision::Accepted)
        && record.schema_version == 2
        && !record.evidence.is_empty()
        && record.evidence.iter().all(|item| item.captured)
}

/// Every reviewed record of `store`, in pages of [`MAX_STAGED_PAGE_ITEMS`],
/// newest first; the caller holds the staging lock across all of them. The
/// projection orders the records by record identity itself.
fn reviewed_records(
    store: &agents::ProposalStore,
) -> Result<Vec<ReviewedProposal>, CuratedFailure> {
    let mut records = Vec::new();
    let (mut scanned, mut accepted) = (0usize, 0usize);
    let mut cursor: Option<String> = None;
    loop {
        let page = store
            .list(MAX_STAGED_PAGE_ITEMS, cursor.as_deref())
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        scanned += page.items.len();
        if scanned > MAX_CURATED_RECORD_SCAN {
            return Err(CuratedFailure::TooManyRecords {
                limit: MAX_CURATED_RECORD_SCAN,
            });
        }
        for staged in &page.items {
            if staged.review_decision.is_none() {
                continue;
            }
            if staged.review_decision == Some(ProposalDecision::Accepted) {
                accepted += 1;
                if accepted > MAX_CURATED_ACCEPTED {
                    return Err(CuratedFailure::TooManyAccepted {
                        limit: MAX_CURATED_ACCEPTED,
                    });
                }
            }
            records.push(agents::curated::curated_record(staged));
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(records),
        }
    }
}

impl CuratedReview {
    /// A review that read no record, with the reason.
    pub(crate) fn failed(failure: CuratedFailure) -> Self {
        Self {
            failure: Some(failure),
            ..Self::default()
        }
    }
}

/// The reviewed records of `store` with no assessment made. Every accepted
/// record that could be eligible is then listed as `stale` for want of one;
/// [`gather`] is the read an export uses.
#[cfg(test)]
pub(crate) fn read_records(store: &agents::ProposalStore) -> CuratedReview {
    match reviewed_records(store) {
        Ok(proposals) => CuratedReview {
            proposals,
            ..CuratedReview::default()
        },
        Err(failure) => CuratedReview::failed(failure),
    }
}

impl CuratedReview {
    /// Whether `records`, read later, are the records this review read. A
    /// review that failed read none, so nothing is compared for it.
    fn same_records(&self, records: &[ReviewedProposal]) -> bool {
        self.failure.is_some() || self.proposals == records
    }
}

/// Where a read gets its reviews (SPEC-12 slice 4, AC-0654). The export
/// calls [`gather`] itself; an MCP read session takes its reviews through
/// this, so the session code names no store it does not already hold.
pub(crate) trait ReviewSource {
    /// [`gather`]. Called with no graph or decision lock held.
    fn gather(&self) -> CuratedReview;
    /// Whether the reviewed records are still the ones `review` read. Takes
    /// the staging lock alone; call it with no other lock held. An `Err` is
    /// a record read that did not finish.
    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure>;
}

impl ReviewSource for AppState {
    fn gather(&self) -> CuratedReview {
        gather(self)
    }

    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure> {
        let store = self
            .proposals
            .lock()
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        Ok(review.same_records(&reviewed_records(&store)?))
    }
}

/// A read with no staging store: no record is reviewed, so the projection is
/// computed over the graph and the Workbench decisions alone.
#[cfg(test)]
pub(crate) struct NoReviews;

#[cfg(test)]
impl ReviewSource for NoReviews {
    fn gather(&self) -> CuratedReview {
        CuratedReview::default()
    }

    fn unchanged(&self, _: &CuratedReview) -> Result<bool, CuratedFailure> {
        Ok(true)
    }
}

/// A staging store that cannot be read.
#[cfg(test)]
pub(crate) struct ReviewsUnreadable;

#[cfg(test)]
impl ReviewSource for ReviewsUnreadable {
    fn gather(&self) -> CuratedReview {
        CuratedReview::failed(CuratedFailure::ReviewsUnavailable)
    }

    fn unchanged(&self, _: &CuratedReview) -> Result<bool, CuratedFailure> {
        Err(CuratedFailure::ReviewsUnavailable)
    }
}

/// A staging store read with no assessment made.
#[cfg(test)]
impl ReviewSource for std::sync::Mutex<agents::ProposalStore> {
    fn gather(&self) -> CuratedReview {
        match self.lock() {
            Ok(store) => read_records(&store),
            Err(_) => CuratedReview::failed(CuratedFailure::ReviewsUnavailable),
        }
    }

    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure> {
        let store = self
            .lock()
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        Ok(review.same_records(&reviewed_records(&store)?))
    }
}

/// Step 1. Call it with no graph or decision lock held. The staging lock is
/// held for the record read alone and released before any assessment. Each
/// assessment then takes the staging lock, the source registry, the
/// retention leases and the graph lock in its own order, one record at a
/// time; none is held between records. Nothing is written.
///
/// It does not fail: a read that could not finish is carried as the reason
/// in the result, with no record.
pub(crate) fn gather(state: &AppState) -> CuratedReview {
    let read = state
        .proposals
        .lock()
        .map_err(|_| CuratedFailure::ReviewsUnavailable)
        .and_then(|store| reviewed_records(&store));
    let proposals = match read {
        Ok(proposals) => proposals,
        Err(failure) => return CuratedReview::failed(failure),
    };
    let mut assessments = BTreeMap::new();
    for record in proposals.iter().filter(|record| needs_assessment(record)) {
        let assessed = match assessment::assess_observed(state, &record.record_id) {
            Ok((assessed, revision)) => (BasisAssessment::Assessed(dimensions(assessed)), revision),
            // The record could not be read back. Not eligible, and listed.
            Err(_) => (BasisAssessment::Failed, None),
        };
        assessments.insert(record.record_id.clone(), assessed);
    }
    CuratedReview {
        failure: None,
        proposals,
        assessments,
    }
}

fn curatable(provenance: &Provenance) -> bool {
    provenance.validate().is_ok()
        && !provenance.evidence.is_empty()
        && matches!(provenance.tier, Tier::Semantic | Tier::Agentic)
        && matches!(
            provenance.confidence_tier,
            ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak
        )
}

/// The decided content hashes a stored fact carries as a curatable
/// inference, moved from `wanted` to `current`.
fn match_stored<'a>(
    props: impl Iterator<Item = &'a serde_json::Value>,
    wanted: &mut BTreeSet<String>,
    current: &mut BTreeSet<String>,
) {
    for props in props {
        if wanted.is_empty() {
            return;
        }
        let Some(prov) = props.get("prov") else {
            continue;
        };
        let Some(hash) = prov.get("content_hash").and_then(|hash| hash.as_str()) else {
            continue;
        };
        if wanted.contains(hash)
            && serde_json::from_value::<Provenance>(prov.clone()).is_ok_and(|prov| curatable(&prov))
            && let Some(hash) = wanted.take(hash)
        {
            current.insert(hash);
        }
    }
}

/// Which of the decided content hashes a current curatable assertion still
/// carries. Only membership of a decided hash is read by the projection, so
/// only decided hashes are looked for.
///
/// A decision on a stored node or edge is found in the graph. Any decision
/// left over was made on a derived statement (a flow hop, a decision record,
/// a finding) or matches nothing; those are settled by compiling the
/// `best-effort` documents once with no rejection and reading their
/// assertions. That second compile runs only when such a decision exists,
/// and only for a reader that compiles ([`DerivedDecisions::Compile`]). For
/// any other reader such a decision leaves the projection not computed.
fn current_assertion_hashes(
    snapshot: &Snapshot<'_>,
    decided: &[core::AssertionDecisionEntry],
) -> Result<BTreeSet<String>, CuratedFailure> {
    let mut wanted: BTreeSet<String> = decided
        .iter()
        .map(|entry| entry.content_hash.clone())
        .collect();
    let mut current = BTreeSet::new();
    match_stored(
        snapshot.nodes.iter().map(|node| &node.props),
        &mut wanted,
        &mut current,
    );
    match_stored(
        snapshot.edges.iter().map(|edge| &edge.props),
        &mut wanted,
        &mut current,
    );
    if wanted.is_empty() {
        return Ok(current);
    }
    let flows = match snapshot.derived_decisions {
        DerivedDecisions::Compile(flows) => flows,
        DerivedDecisions::NotCompiled => {
            return Err(CuratedFailure::DerivedDecisionsNeedCompile);
        }
    };
    let bundle = spec::try_compile_spec_with_coverage(
        snapshot.nodes,
        snapshot.edges,
        flows,
        spec::ExportMode::BestEffort,
        &BTreeSet::new(),
        snapshot.coverage,
    )
    .map_err(|_| CuratedFailure::AssertionsNotCompiled)?;
    for assertion in bundle
        .artifacts
        .iter()
        .flat_map(|artifact| &artifact.assertions)
    {
        if wanted.contains(&assertion.provenance.content_hash)
            && curatable(&assertion.provenance)
            && let Some(hash) = wanted.take(&assertion.provenance.content_hash)
        {
            current.insert(hash);
        }
    }
    Ok(current)
}

/// How a Workbench decision that matches no stored fact is settled.
#[derive(Clone, Copy)]
pub(crate) enum DerivedDecisions<'a> {
    /// By compiling every document once from these flows, the
    /// `flowtracer::trace` of the snapshot. The export does this.
    Compile(&'a [flowtracer::Flow]),
    /// Not at all: the reader's work is bounded and that compile is not in
    /// its bound (an MCP read session). Such a decision leaves the
    /// projection not computed, with
    /// [`CuratedFailure::DerivedDecisionsNeedCompile`].
    NotCompiled,
}

/// The graph an export read, as the projection needs it.
pub(crate) struct Snapshot<'a> {
    pub nodes: &'a [Node],
    pub edges: &'a [Edge],
    pub derived_decisions: DerivedDecisions<'a>,
    pub coverage: &'a [core_graph::coverage::StoredCoverage],
    /// `spec::graph_content_hash` of `nodes` and `edges`.
    pub content_hash: &'a str,
    /// The store revision read under the same graph lock as the snapshot.
    pub revision: &'a StoreRevision,
}

/// Step 2. The projection over the snapshot an export read, under that
/// export's graph and decision locks. An `Err` is the reason `curated.md`
/// states; it never fails the export.
///
/// An assessment whose graph read observed another store revision than the
/// export's describes a graph the export did not read. It is replaced by a
/// failed assessment, so its record is listed as `stale` and is never
/// eligible on the strength of a graph that has since moved.
pub(crate) fn project(
    review: &CuratedReview,
    snapshot: &Snapshot<'_>,
    decisions: &[agents::AssertionDecisionRecord],
) -> Result<CuratedProjection, CuratedFailure> {
    if let Some(failure) = review.failure {
        return Err(failure);
    }
    let assessments: BTreeMap<String, BasisAssessment> = review
        .assessments
        .iter()
        .map(|(id, (assessed, observed))| {
            let moved = observed
                .as_ref()
                .is_some_and(|observed| observed != snapshot.revision);
            let assessed = if moved {
                BasisAssessment::Failed
            } else {
                assessed.clone()
            };
            (id.clone(), assessed)
        })
        .collect();
    let decided: Vec<_> = decisions
        .iter()
        .map(agents::curated::curated_assertion_decision)
        .collect();
    let current = current_assertion_hashes(snapshot, &decided)?;
    core::project(&ProjectionInput {
        nodes: snapshot.nodes,
        edges: snapshot.edges,
        graph_content_hash: snapshot.content_hash,
        proposals: &review.proposals,
        assessments: &assessments,
        assertion_decisions: &decided,
        current_assertion_hashes: &current,
        policy_version: core::CURATED_POLICY_VERSION,
    })
    .map_err(|error| match error {
        core::ProjectionError::DuplicateRecord => CuratedFailure::DuplicateRecord,
        core::ProjectionError::DuplicateAssertionDecision => {
            CuratedFailure::DuplicateAssertionDecision
        }
        core::ProjectionError::Encoding => CuratedFailure::RevisionNotEncoded,
    })
}

/// The snapshot with its inventories and the store revision of the same
/// read transaction, so the revision an assessment observed is compared with
/// the revision of exactly the rows the export compiles.
pub(crate) fn read_graph(
    graph: &SqliteGraphStore,
) -> Result<(StoreRevision, core_graph::coverage::CoveredSnapshot), String> {
    graph
        .read_snapshot_with_coverage_and_revision()
        .map_err(|error| error.to_string())
}
