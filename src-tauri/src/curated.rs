//! The host side of the curated projection (SPEC-12 slice 2, ADR-0061,
//! AC-0653): the reads that feed `context_hub::curated::project`.
//!
//! It runs in two steps, because the SPEC-09 assessment takes the source
//! registry, the retention leases and the graph lock itself and so cannot run
//! while an export holds the graph:
//!
//! 1. [`gather`], with no graph or decision lock held: one bounded read of
//!    the staging store and one of the section draft store (SPEC-15 slice 5,
//!    AC-0667), then one assessment for each accepted record that could be
//!    eligible. Nothing is persisted.
//! 2. [`project`], over the snapshot the export read under its own graph and
//!    decision locks: the assessments are checked against the store revision
//!    of that snapshot, and the pure projection is computed over it. It takes
//!    no store lock, so an export calls it after releasing them (#1834).
//!
//! An MCP read session (slice 4, AC-0654) runs the same two steps over the
//! snapshot it froze, and takes its reviews through [`ReviewSource`]. It
//! gathers only after its own bounded graph read succeeded, and it never
//! makes the compile of [`DerivedDecisions::Compile`].

use agents::ProposalDecision;
use agents::staging::{MAX_STAGED_PAGE_ITEMS, MAX_STAGED_REFERENCE_SCAN};
use context_hub::curated::{
    self as core, BasisAssessment, BasisDimensions, CuratedProjection, EvidenceAvailability,
    EvidenceStatus, ProjectionInput, ReviewedAttested, ReviewedDraft, ReviewedFinding,
    ReviewedProposal,
};
use core_graph::{Edge, Node, SqliteGraphStore, StoreRevision};
use core_prov::{ConfidenceTier, Provenance, Tier};
use spec::CuratedFailure;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use crate::AppState;
use crate::task_evidence::assessment::{self, Availability, BasisComparison};
use crate::task_evidence::reconcile;

/// Most staged records one projection reads, pending and reviewed together.
/// A store that holds more fails the read; nothing is projected from a part
/// of it.
pub(crate) const MAX_CURATED_RECORD_SCAN: usize = MAX_STAGED_REFERENCE_SCAN;
/// Most accepted records one projection takes. Each one that could be
/// eligible costs one SPEC-09 assessment, which reads its retained bytes
/// and its source associations, so this bounds the work of one read. The
/// graph is copied once for all of them while the store does not move
/// (`assessment::SharedObservation`). More than this fails the read.
pub(crate) const MAX_CURATED_ACCEPTED: usize = 256;
/// Most bytes of accepted section drafts one projection takes, measured as
/// each draft's encoded payload: as much as [`MAX_CURATED_ACCEPTED`] drafts
/// of the largest size a store admits. A draft costs no assessment, so many
/// small drafts are read as long as their sum fits; the projection copies
/// their claims, so the sum bounds that copy. More fails the read (#2040).
pub(crate) const MAX_CURATED_ACCEPTED_DRAFT_BYTES: usize =
    MAX_CURATED_ACCEPTED * agents::draft_store::MAX_DRAFT_RECORD_BYTES;
/// The same for accepted curated findings (#2040).
pub(crate) const MAX_CURATED_ACCEPTED_FINDING_BYTES: usize =
    MAX_CURATED_ACCEPTED * agents::finding_curation::MAX_CURATED_FINDING_BYTES;
/// Most bytes of stored attested records one projection takes, measured as
/// each record's encoded payload, whatever its standing: every record is
/// printed or listed. The projection and a read copy them, so the sum
/// bounds that copy. More fails the read before the set is kept.
pub(crate) const MAX_CURATED_ATTESTED_BYTES: usize = 16 * 1024 * 1024;

/// The encoded size of an accepted payload, counted against its budget. A
/// payload that cannot be encoded counts as the whole budget.
fn payload_bytes(payload: &impl serde::Serialize, budget: usize) -> usize {
    serde_json::to_vec(payload).map_or(budget.saturating_add(1), |bytes| bytes.len())
}

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
    /// Every reviewed section draft (SPEC-15). A pending draft is not kept:
    /// the projection reads none.
    drafts: Vec<ReviewedDraft>,
    /// Every finding with a curation review (ADR-0074), from the section
    /// draft store's file.
    findings: Vec<ReviewedFinding>,
    /// Every stored attested record (ADR-0090), from the same file, each
    /// engineer named from the contributor table. Its attestations are its
    /// review, so none is left out.
    attested: Vec<ReviewedAttested>,
    /// By record identity, with the store revision the assessment's graph
    /// read observed (`None` when that read failed).
    assessments: BTreeMap<String, (BasisAssessment, Option<StoreRevision>)>,
    /// By accepted record identity, the latest reconciliation result id the
    /// read took (`None` when there was none). A record whose results could
    /// not be read is not listed (AC-1076).
    reconciled: BTreeMap<String, Option<String>>,
    /// Where [`project`] keeps what the documents' compile said of the
    /// decisions no stored fact carries: the application's when [`gather`]
    /// made this review, and otherwise one that starts empty.
    settled: Arc<DerivedSettlements>,
}

pub(crate) use app_store::curated_settlements::{DerivedSettlements, Settled};

/// Whether `settled` was made for this snapshot's revision and content and
/// for this set of hashes.
fn answers(settled: &Settled, snapshot: &Snapshot<'_>, wanted: &BTreeSet<String>) -> bool {
    &settled.revision == snapshot.revision
        && settled.content_hash == snapshot.content_hash
        && &settled.wanted == wanted
}

#[cfg(test)]
thread_local! {
    static DERIVED_COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread made the compile of
/// [`DerivedDecisions::Compile`].
#[cfg(test)]
pub(crate) fn derived_compiles() -> usize {
    DERIVED_COMPILES.with(std::cell::Cell::get)
}

/// Which of `wanted` a curatable derived statement of `snapshot`
/// carries: the kept answer when it was made for this revision, content
/// and set, and otherwise one compile of the `best-effort` documents
/// with no rejection, which is then kept.
///
/// The entry stays locked across the compile, so two exports of one
/// revision compile once between them. No store lock is taken while it
/// is held. A poisoned entry is not read or written: the compile runs.
fn settled_carried(
    settlements: &DerivedSettlements,
    snapshot: &Snapshot<'_>,
    flows: &[flowtracer::Flow],
    wanted: BTreeSet<String>,
) -> Result<BTreeSet<String>, CuratedFailure> {
    let mut entry = settlements.0.lock().ok();
    if let Some(Some(settled)) = entry.as_deref()
        && answers(settled, snapshot, &wanted)
    {
        return Ok(settled.carried.clone());
    }
    #[cfg(test)]
    DERIVED_COMPILES.with(|count| count.set(count.get() + 1));
    let bundle = spec::try_compile_spec_with_coverage(
        snapshot.nodes,
        snapshot.edges,
        flows,
        spec::ExportMode::BestEffort,
        &BTreeSet::new(),
        snapshot.coverage,
    )
    .map_err(|_| CuratedFailure::AssertionsNotCompiled)?;
    let mut left = wanted.clone();
    let mut carried = BTreeSet::new();
    for assertion in bundle
        .artifacts
        .iter()
        .flat_map(|artifact| &artifact.assertions)
    {
        if left.contains(&assertion.provenance.content_hash)
            && curatable(&assertion.provenance)
            && let Some(hash) = left.take(&assertion.provenance.content_hash)
        {
            carried.insert(hash);
        }
    }
    if let Some(entry) = entry.as_deref_mut() {
        *entry = Some(Settled {
            revision: snapshot.revision.clone(),
            content_hash: snapshot.content_hash.to_owned(),
            wanted,
            carried: carried.clone(),
        });
    }
    Ok(carried)
}

/// Whether one artifact's bytes depend on the curated projection: the
/// document itself, the D1 document, whose header carries the curated
/// revision (AC-0663), the fact index that lists what they cite, and the
/// bundle manifest, which states the curated revision and the graph basis
/// the review read gives (AC-0682).
pub(crate) fn artifact_reads_projection(artifact_id: &str) -> bool {
    matches!(
        artifact_id,
        "curated"
            | "d1-product-specification"
            | "fact-index"
            | "fact-index-json"
            | "bundle-manifest"
    )
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
        cited_files: comparison(assessed.cited_files),
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

/// Every reviewed draft of `store`, in pages of
/// [`agents::draft_store::MAX_DRAFT_PAGE_ITEMS`], newest first; the caller
/// holds the draft store lock across all of them. More drafts than the
/// staged-record bound, or accepted ones past
/// [`MAX_CURATED_ACCEPTED_DRAFT_BYTES`], fail the read.
fn reviewed_drafts(
    store: &agents::draft_store::DraftStore,
) -> Result<Vec<ReviewedDraft>, CuratedFailure> {
    reviewed_drafts_within(store, MAX_CURATED_ACCEPTED_DRAFT_BYTES)
}

/// [`reviewed_drafts`] with accepted drafts bounded by `budget` bytes.
fn reviewed_drafts_within(
    store: &agents::draft_store::DraftStore,
    budget: usize,
) -> Result<Vec<ReviewedDraft>, CuratedFailure> {
    use agents::draft_store::{DraftDecision, MAX_DRAFT_PAGE_ITEMS};
    let mut drafts = Vec::new();
    let (mut scanned, mut accepted_bytes) = (0usize, 0usize);
    let mut before: Option<u64> = None;
    loop {
        let page = store
            .list(MAX_DRAFT_PAGE_ITEMS, before)
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        scanned += page.items.len();
        if scanned > MAX_CURATED_RECORD_SCAN {
            return Err(CuratedFailure::TooManyRecords {
                limit: MAX_CURATED_RECORD_SCAN,
            });
        }
        for stored in &page.items {
            match stored.review_decision {
                None => continue,
                Some(DraftDecision::Accepted) => {
                    accepted_bytes += payload_bytes(&stored.draft, budget);
                    if accepted_bytes > budget {
                        return Err(CuratedFailure::AcceptedTooLarge {
                            limit_bytes: budget,
                        });
                    }
                }
                Some(DraftDecision::Rejected | DraftDecision::Returned) => {}
            }
            drafts.push(agents::curated::reviewed_draft(stored));
        }
        match page.next_before {
            Some(next) => before = Some(next),
            None => return Ok(drafts),
        }
    }
}

/// Every finding of `store` with a curation review, oldest first; the
/// caller holds the draft store lock. More findings than the staged-record
/// bound, or accepted ones past [`MAX_CURATED_ACCEPTED_FINDING_BYTES`],
/// fail the read.
fn reviewed_findings(
    store: &agents::draft_store::DraftStore,
) -> Result<Vec<ReviewedFinding>, CuratedFailure> {
    reviewed_findings_within(store, MAX_CURATED_ACCEPTED_FINDING_BYTES)
}

/// [`reviewed_findings`] with accepted findings bounded by `budget` bytes.
fn reviewed_findings_within(
    store: &agents::draft_store::DraftStore,
    budget: usize,
) -> Result<Vec<ReviewedFinding>, CuratedFailure> {
    use agents::finding_curation::{CurationDecision, reviewed_finding};
    let read = store
        .finding_curation()
        .reviewed(MAX_CURATED_RECORD_SCAN)
        .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
    if read.more {
        return Err(CuratedFailure::TooManyRecords {
            limit: MAX_CURATED_RECORD_SCAN,
        });
    }
    let mut accepted_bytes = 0usize;
    for stored in &read.items {
        if stored.review_decision == CurationDecision::Accepted {
            accepted_bytes += payload_bytes(&stored.record, budget);
            if accepted_bytes > budget {
                return Err(CuratedFailure::AcceptedTooLarge {
                    limit_bytes: budget,
                });
            }
        }
    }
    Ok(read.items.iter().map(reviewed_finding).collect())
}

/// Every stored attested record of `store` with its standing, oldest
/// first, each engineer named from the contributor table; the caller holds
/// the draft store lock. More records than the staged-record bound fail the
/// read, as do records past [`MAX_CURATED_ATTESTED_BYTES`].
fn attested_records(
    store: &agents::draft_store::DraftStore,
) -> Result<Vec<ReviewedAttested>, CuratedFailure> {
    attested_records_within(store, MAX_CURATED_ATTESTED_BYTES)
}

/// [`attested_records`] with the records bounded by `budget` bytes.
fn attested_records_within(
    store: &agents::draft_store::DraftStore,
    budget: usize,
) -> Result<Vec<ReviewedAttested>, CuratedFailure> {
    use agents::attested_store::{MAX_ATTESTED_PAGE_ITEMS, reviewed_attested};
    let attested = store.attested();
    let engineers: BTreeMap<String, String> = attested
        .engineer()
        .map_err(|_| CuratedFailure::ReviewsUnavailable)?
        .into_iter()
        .map(|engineer| (engineer.id.0, engineer.display_name))
        .collect();
    let mut records = Vec::new();
    let mut bytes = 0usize;
    let mut before: Option<u64> = None;
    loop {
        let page = attested
            .list(MAX_ATTESTED_PAGE_ITEMS, before)
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        if records.len() + page.items.len() > MAX_CURATED_RECORD_SCAN {
            return Err(CuratedFailure::TooManyRecords {
                limit: MAX_CURATED_RECORD_SCAN,
            });
        }
        for stored in &page.items {
            bytes += payload_bytes(&stored.record, budget);
            if bytes > budget {
                return Err(CuratedFailure::AcceptedTooLarge {
                    limit_bytes: budget,
                });
            }
            records.push(reviewed_attested(stored, &engineers));
        }
        match page.next_before {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    records.reverse();
    Ok(records)
}

/// What one read of the section draft store's file finds, under the one
/// lock its caller holds.
#[derive(Debug, Default, PartialEq, Eq)]
struct DraftStoreReviews {
    drafts: Vec<ReviewedDraft>,
    findings: Vec<ReviewedFinding>,
    attested: Vec<ReviewedAttested>,
}

/// The reviewed drafts, the reviewed findings and the attested records of
/// `store`, read under the one lock its caller holds.
fn reviewed_in_draft_store(
    store: &agents::draft_store::DraftStore,
) -> Result<DraftStoreReviews, CuratedFailure> {
    Ok(DraftStoreReviews {
        drafts: reviewed_drafts(store)?,
        findings: reviewed_findings(store)?,
        attested: attested_records(store)?,
    })
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

/// The reviewed records of `store` and the reviewed drafts of `drafts`,
/// with no assessment made.
#[cfg(test)]
pub(crate) fn read_records_and_drafts(
    store: &agents::ProposalStore,
    drafts: &agents::draft_store::DraftStore,
) -> CuratedReview {
    match reviewed_in_draft_store(drafts) {
        Ok(read) => CuratedReview {
            drafts: read.drafts,
            findings: read.findings,
            attested: read.attested,
            ..read_records(store)
        },
        Err(failure) => CuratedReview::failed(failure),
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

#[cfg(test)]
impl CuratedReview {
    /// This review with nothing kept from an earlier export, as every
    /// export read before #1834: a decision that no stored fact carries is
    /// settled by a compile of its own.
    pub(crate) fn settling_alone(mut self) -> Self {
        self.settled = Arc::default();
        self
    }
}

impl CuratedReview {
    /// Whether `records` and the draft store's reviews, read later, are
    /// the ones this review read. A review that failed read none, so
    /// nothing is compared for it.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn same_records(&self, records: &[ReviewedProposal], later: &DraftStoreReviews) -> bool {
        self.failure.is_some()
            || (self.proposals == records
                && self.drafts == later.drafts
                && self.findings == later.findings
                && self.attested == later.attested)
    }
}

/// Where a read gets its reviews (SPEC-12 slice 4, AC-0654). The export
/// calls [`gather`] itself; an MCP read session takes its reviews through
/// this, so the session code names no store it does not already hold.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) trait ReviewSource {
    /// [`gather`]. Called with no graph or decision lock held.
    fn gather(&self) -> CuratedReview;
    /// Whether the reviewed records and drafts are still the ones `review`
    /// read. Takes the staging lock and then the draft store lock, one at a
    /// time; call it with no other lock held. An `Err` is
    /// a record read that did not finish.
    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure>;
}

impl ReviewSource for AppState {
    fn gather(&self) -> CuratedReview {
        gather(self)
    }

    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure> {
        let (records, reconciled) = {
            let store = self
                .proposals
                .lock()
                .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
            // A record reconciled again since the read would project another
            // result (AC-1076).
            let mut reconciled = true;
            for (id, latest) in &review.reconciled {
                let now = reconcile::latest_result_id(&store, id)
                    .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
                reconciled &= &now == latest;
            }
            (reviewed_records(&store)?, reconciled)
        };
        let later = {
            let store = self
                .drafts
                .lock()
                .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
            reviewed_in_draft_store(&store)?
        };
        Ok(reconciled && review.same_records(&records, &later))
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
        Ok(review.same_records(&reviewed_records(&store)?, &DraftStoreReviews::default()))
    }
}

/// A staging store and a section draft store, read with no assessment made.
#[cfg(test)]
pub(crate) struct StoredReviews {
    pub proposals: std::sync::Mutex<agents::ProposalStore>,
    pub drafts: std::sync::Mutex<agents::draft_store::DraftStore>,
}

#[cfg(test)]
impl ReviewSource for StoredReviews {
    fn gather(&self) -> CuratedReview {
        match (self.proposals.lock(), self.drafts.lock()) {
            (Ok(proposals), Ok(drafts)) => read_records_and_drafts(&proposals, &drafts),
            _ => CuratedReview::failed(CuratedFailure::ReviewsUnavailable),
        }
    }

    fn unchanged(&self, review: &CuratedReview) -> Result<bool, CuratedFailure> {
        let proposals = self
            .proposals
            .lock()
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        let drafts = self
            .drafts
            .lock()
            .map_err(|_| CuratedFailure::ReviewsUnavailable)?;
        let later = reviewed_in_draft_store(&drafts)?;
        Ok(review.same_records(&reviewed_records(&proposals)?, &later))
    }
}

/// Step 1. Call it with no graph or decision lock held. The staging lock is
/// held for the record read alone and released before any assessment; the
/// draft store lock is then held for the draft read alone. Each
/// assessment then takes the staging lock, the source registry, the
/// retention leases and the graph lock in its own order, one record at a
/// time; none is held between records. The assessments share one copy of
/// the graph, and each checks under the graph lock that the store is still
/// at the revision of that copy. Nothing is written.
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
    let read = state
        .drafts
        .lock()
        .map_err(|_| CuratedFailure::ReviewsUnavailable)
        .and_then(|store| reviewed_in_draft_store(&store));
    let read = match read {
        Ok(read) => read,
        Err(failure) => return CuratedReview::failed(failure),
    };
    let mut assessments = BTreeMap::new();
    let mut reconciled = BTreeMap::new();
    // One graph copy and one snapshot identity for all of them (#1834).
    let mut observed = assessment::SharedObservation::default();
    let accepted = proposals
        .iter()
        .filter(|record| record.decision == Some(core::ReviewDecision::Accepted));
    for record in accepted {
        // A record's latest reconciliation decides it when it is current and
        // reconciled, changed or missing. A stale or unverifiable one decides
        // only a record the SPEC-09 assessment cannot make eligible; any
        // other is assessed as before (ADR-0072 slice 4, AC-1076).
        let reconciliation =
            match reconcile::curated_reconciliation(state, &record.record_id, &mut observed) {
                Ok((latest, reconciliation)) => {
                    reconciled.insert(record.record_id.clone(), latest);
                    reconciliation
                }
                // Its results could not be read. Not eligible, and listed.
                Err(_) => {
                    assessments.insert(record.record_id.clone(), (BasisAssessment::Failed, None));
                    continue;
                }
            };
        let assessed = match reconciliation {
            Some((read, revision))
                if !matches!(
                    read,
                    core::RecordReconciliation::Stale | core::RecordReconciliation::Unverifiable
                ) || !needs_assessment(record) =>
            {
                (BasisAssessment::Reconciliation(read), revision)
            }
            _ if !needs_assessment(record) => continue,
            _ => match assessment::assess_observed(state, &record.record_id, &mut observed) {
                Ok((assessed, revision)) => {
                    (BasisAssessment::Assessed(dimensions(assessed)), revision)
                }
                // The record could not be read back. Not eligible, and listed.
                Err(_) => (BasisAssessment::Failed, None),
            },
        };
        assessments.insert(record.record_id.clone(), assessed);
    }
    CuratedReview {
        failure: None,
        proposals,
        drafts: read.drafts,
        findings: read.findings,
        attested: read.attested,
        assessments,
        reconciled,
        settled: Arc::clone(&state.curated_settled),
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
        if props.get("prov").is_none() {
            continue;
        }
        let prov = core_graph::stored::stored_prov(props);
        let Some(hash) = prov.get("content_hash").and_then(|hash| hash.as_str()) else {
            continue;
        };
        if wanted.contains(hash)
            && <Provenance as serde::Deserialize>::deserialize(&*prov)
                .is_ok_and(|prov| curatable(&prov))
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
/// only for a reader that compiles ([`DerivedDecisions::Compile`]), and once
/// for one store revision and set of such decisions ([`DerivedSettlements`]).
/// For any other reader such a decision leaves the projection not computed.
///
/// The export's own compile cannot settle them instead. It leaves out every
/// rejected inference, and in `verified-only` every InferredWeak one, so a
/// rejected derived statement is not among its assertions; and the
/// projection these hashes feed is an input of that compile.
fn current_assertion_hashes(
    snapshot: &Snapshot<'_>,
    decided: &[core::AssertionDecisionEntry],
    settled: &DerivedSettlements,
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
    current.extend(settled_carried(settled, snapshot, flows, wanted)?);
    Ok(current)
}

/// How a Workbench decision that matches no stored fact is settled.
#[derive(Clone, Copy)]
pub(crate) enum DerivedDecisions<'a> {
    /// By compiling every document once from these flows, the
    /// `flowtracer::trace` of the snapshot, unless that compile was already
    /// made for this revision and these decisions. The export does this.
    Compile(&'a [flowtracer::Flow]),
    /// Not at all: the reader's work is bounded and that compile is not in
    /// its bound (an MCP read session). Such a decision leaves the
    /// projection not computed, with
    /// [`CuratedFailure::DerivedDecisionsNeedCompile`].
    #[cfg_attr(not(unix), allow(dead_code))]
    NotCompiled,
}

/// The graph an export read, as the projection needs it.
pub(crate) struct Snapshot<'a> {
    pub nodes: &'a [Node],
    pub edges: &'a [Edge],
    /// The knowledge overlay's Capabilities, Actors and printed areas current
    /// for `revision` (#2040, #2511): a draft may cite them beside the base
    /// facts.
    pub overlay: &'a [&'a Node],
    pub derived_decisions: DerivedDecisions<'a>,
    pub coverage: &'a [core_graph::coverage::StoredCoverage],
    /// `spec::graph_content_hash` of `nodes` and `edges`.
    pub content_hash: &'a str,
    /// The store revision read under the same graph lock as the snapshot.
    pub revision: &'a StoreRevision,
}

/// Step 2. The projection over the snapshot an export read under its graph
/// and decision locks. It reads no store, so it needs neither lock. An `Err`
/// is the reason `curated.md` states; it never fails the export.
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
            let assessed = match assessed {
                // A reconciliation of a graph the export did not read no
                // longer describes it.
                BasisAssessment::Reconciliation(_) if moved => {
                    BasisAssessment::Reconciliation(core::RecordReconciliation::Stale)
                }
                _ if moved => BasisAssessment::Failed,
                assessed => assessed.clone(),
            };
            (id.clone(), assessed)
        })
        .collect();
    let decided: Vec<_> = decisions
        .iter()
        .map(agents::curated::curated_assertion_decision)
        .collect();
    let current = current_assertion_hashes(snapshot, &decided, &review.settled)?;
    // The current hash of each fact an accepted draft cites, over the
    // snapshot's nodes and edges, then the overlay's Capabilities, Actors
    // and printed areas (ADR-0064 decision 8, #2040, #2511). One pass, and
    // none when no draft is accepted. A base fact keeps its own hash.
    let cited: BTreeSet<String> = review
        .drafts
        .iter()
        .filter(|draft| draft.decision == Some(core::DraftReview::Accepted))
        .flat_map(|draft| draft.cited_hashes.keys().cloned())
        // And each fact an attested record rests on (ADR-0090 decision 8).
        .chain(
            review
                .attested
                .iter()
                .flat_map(|record| record.recorded_hashes.keys().cloned()),
        )
        .collect();
    let mut cited_hashes = spec::cited_fact_hashes(snapshot.nodes, snapshot.edges, &cited);
    for (id, hash) in spec::cited_node_hashes(snapshot.overlay.iter().copied(), &cited) {
        cited_hashes.entry(id).or_insert(hash);
    }
    // The current digest of each fact an accepted finding cites, the same
    // way (ADR-0074 decision 5): one pass, and none when no finding is
    // accepted.
    let cited: BTreeSet<String> = review
        .findings
        .iter()
        .filter(|finding| finding.decision == Some(core::FindingCuration::Accepted))
        .flat_map(|finding| finding.cited_digests.keys().cloned())
        .collect();
    let cited_digests = spec::cited_fact_digests(snapshot.nodes, snapshot.edges, &cited);
    core::project(&ProjectionInput {
        nodes: snapshot.nodes,
        edges: snapshot.edges,
        graph_content_hash: snapshot.content_hash,
        proposals: &review.proposals,
        assessments: &assessments,
        assertion_decisions: &decided,
        current_assertion_hashes: &current,
        drafts: &review.drafts,
        cited_fact_hashes: &cited_hashes,
        findings: &review.findings,
        cited_fact_digests: &cited_digests,
        attested: &review.attested,
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

/// The overlay Capabilities, Actors and areas a `best-effort` export of
/// `base` prints, for [`Snapshot::overlay`] (#2040): the knowledge stories
/// validated as the export validates them, so a story the export withholds
/// (it, or a fact it rests on, was rejected) resolves no draft. The
/// `subsystems` the stories are arranged by are the areas D1 prints, which a
/// draft may cite (#2319); each one kept resolves too (#2511). A refused
/// overlay resolves none.
pub(crate) fn overlay_nodes<'a>(
    base: &'a core_graph::RevisionedSnapshot,
    capabilities: &'a [core_graph::overlay::OverlayFact],
    actors: &'a [core_graph::overlay::OverlayFact],
    subsystems: &'a [core_graph::overlay::OverlayFact],
    rejected: &BTreeSet<String>,
) -> Vec<&'a Node> {
    let Some(candidates) = capabilities
        .iter()
        .map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(capability) => Some(spec::CapabilityStory {
                capability,
                cites: &fact.cites,
            }),
            core_graph::overlay::OverlayItem::Edge(_) => None,
        })
        .collect::<Option<Vec<_>>>()
    else {
        return Vec::new();
    };
    if candidates.is_empty() && actors.is_empty() {
        return Vec::new();
    }
    spec::validate_knowledge_stories(
        base,
        &candidates,
        actors,
        spec::ExportMode::BestEffort,
        rejected,
    )
    .map(|stories| {
        let mode = spec::ExportMode::BestEffort;
        let stories = stories.with_areas(base, subsystems, mode, rejected);
        let mut nodes: Vec<&'a Node> = stories.included_nodes().collect();
        nodes.extend(stories.area_nodes());
        nodes
    })
    .unwrap_or_default()
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

/// [`read_graph`] with each fact held as an export's compiler reads it:
/// its `prov` and rule payload as text (`spec::compact_node`), so the whole
/// graph is never decoded at once (#2215).
pub(crate) fn read_graph_compacted(
    graph: &SqliteGraphStore,
) -> Result<(StoreRevision, core_graph::coverage::CoveredSnapshot), String> {
    graph
        .read_snapshot_with_coverage_and_revision_each(spec::compact_node, spec::compact_edge)
        .map_err(|error| error.to_string())
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "curated_drafts_tests.rs"]
mod drafts_tests;

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "curated_findings_tests.rs"]
mod findings_tests;

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "curated_finding_reads_tests.rs"]
mod finding_reads_tests;

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "curated_attested_tests.rs"]
mod attested_tests;
