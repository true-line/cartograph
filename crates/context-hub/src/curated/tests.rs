//! AC-0652: the curated projection core. One fixture per state, first-match
//! precedence, revision equality and inequality, determinism under input
//! order.

use super::*;
use serde_json::json;

const GRAPH_HASH: &str = "graph-hash-1";

fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
    let prov = Provenance::new(tier, confidence, vec![], "fixture", id.as_bytes()).unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({ "prov": prov }),
    }
}

fn symbol(id: &str) -> Node {
    node(id, "Symbol", Tier::Deterministic, ConfidenceTier::Confirmed)
}

fn gap(id: &str) -> Node {
    node(id, "Gap", Tier::Deterministic, ConfidenceTier::Gap)
}

fn edge(src: &str, dst: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
    let prov = Provenance::new(tier, confidence, vec![], "fixture", b"edge").unwrap();
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: "CALLS".into(),
        props: json!({ "prov": prov }),
    }
}

/// An accepted version 2 record for `gap:1`: `sym:a -CALLS-> sym:b`, with two
/// captured evidence items.
fn record(id: &str) -> ReviewedProposal {
    ReviewedProposal {
        record_id: id.into(),
        schema_version: 2,
        gap_id: "gap:1".into(),
        source_id: "sym:a".into(),
        target_id: "sym:b".into(),
        edge_label: "CALLS".into(),
        tier: Tier::Agentic,
        confidence: ConfidenceTier::InferredWeak,
        decision: Some(ReviewDecision::Accepted),
        evidence: ["E1", "E2"]
            .into_iter()
            .map(|id| RecordEvidence {
                evidence_id: id.into(),
                captured: true,
            })
            .collect(),
    }
}

fn dimensions() -> BasisDimensions {
    BasisDimensions {
        graph: BasisComparison::Unchanged,
        association: BasisComparison::Unchanged,
        evidence: ["E1", "E2"]
            .into_iter()
            .map(|id| EvidenceStatus {
                evidence_id: id.into(),
                status: EvidenceAvailability::Available,
            })
            .collect(),
        unverified_evidence: 0,
        cited_files: BasisComparison::LegacyUnverified,
    }
}

/// Owned inputs; `run` borrows them into a [`ProjectionInput`].
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    graph_hash: String,
    proposals: Vec<ReviewedProposal>,
    assessments: BTreeMap<String, BasisAssessment>,
    decisions: Vec<AssertionDecisionEntry>,
    current: BTreeSet<String>,
    policy: String,
}

impl Fixture {
    /// The base graph with the given records, each assessed as unchanged.
    fn new(proposals: Vec<ReviewedProposal>) -> Self {
        let assessments = proposals
            .iter()
            .map(|record| {
                (
                    record.record_id.clone(),
                    BasisAssessment::Assessed(dimensions()),
                )
            })
            .collect();
        Self {
            nodes: vec![
                gap("gap:1"),
                gap("gap:2"),
                symbol("sym:a"),
                symbol("sym:b"),
                symbol("sym:c"),
            ],
            edges: vec![edge(
                "sym:a",
                "gap:1",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            )],
            graph_hash: GRAPH_HASH.into(),
            proposals,
            assessments,
            decisions: Vec::new(),
            current: BTreeSet::new(),
            policy: CURATED_POLICY_VERSION.into(),
        }
    }

    fn assess(&mut self, id: &str, change: impl FnOnce(&mut BasisDimensions)) {
        let mut value = dimensions();
        change(&mut value);
        self.assessments
            .insert(id.into(), BasisAssessment::Assessed(value));
    }

    fn run(&self) -> CuratedProjection {
        self.try_run().unwrap()
    }

    fn try_run(&self) -> Result<CuratedProjection, ProjectionError> {
        project(&ProjectionInput {
            nodes: &self.nodes,
            edges: &self.edges,
            graph_content_hash: &self.graph_hash,
            proposals: &self.proposals,
            assessments: &self.assessments,
            assertion_decisions: &self.decisions,
            current_assertion_hashes: &self.current,
            drafts: &[],
            cited_fact_hashes: &std::collections::BTreeMap::new(),
            findings: &[],
            cited_fact_digests: &std::collections::BTreeMap::new(),
            attested: &[],
            policy_version: &self.policy,
        })
    }
}

/// State and reason of the one record `id`, wherever it is reported.
fn outcome(projection: &CuratedProjection, id: &str) -> (RecordState, StateReason) {
    let mut found = projection
        .items
        .iter()
        .chain(&projection.listed)
        .filter(|entry| entry.record_id == id);
    let entry = found.next().unwrap();
    assert!(found.next().is_none(), "{id} is reported once");
    (entry.state, entry.reason)
}

fn decision(hash: &str, decision: AssertionReview) -> AssertionDecisionEntry {
    AssertionDecisionEntry {
        content_hash: hash.into(),
        decision,
    }
}

// AC-0652: an accepted record whose every condition holds is a curated item
// and carries the DR-4 fields.
#[test]
fn eligible_record_is_an_item_with_dr4_fields() {
    let projection = Fixture::new(vec![record("r1")]).run();
    assert_eq!(projection.view, CuratedView::Curated);
    assert_eq!(projection.graph_basis, GRAPH_HASH);
    assert!(projection.revision.starts_with("curated-revision-v4:"));
    assert!(projection.listed.is_empty());
    let [item] = projection.items.as_slice() else {
        panic!("one item");
    };
    assert_eq!(item.record_id, "r1");
    assert_eq!(item.review_status, ReviewDecision::Accepted);
    assert_eq!(item.state, RecordState::Eligible);
    assert_eq!(item.reason, StateReason::AllConditionsHold);
    assert_eq!(
        item.source_support,
        SourceSupport {
            captured: 2,
            unverified: 0
        }
    );
    assert_eq!(
        item.basis,
        BasisReport::Assessed {
            graph: BasisComparison::Unchanged,
            association: BasisComparison::Unchanged,
            available: 2,
            not_available: 0,
            unverified: 0,
        }
    );
}

// AC-0652: `unverified` covers version 1, unknown versions, missing evidence
// and any item that is not captured.
#[test]
fn unverified_state_and_reasons() {
    let mut legacy = record("legacy");
    legacy.schema_version = 1;
    legacy.evidence.iter_mut().for_each(|e| e.captured = false);
    let mut future = record("future");
    future.schema_version = 3;
    let mut empty = record("empty");
    empty.evidence.clear();
    let mut mixed = record("mixed");
    mixed.evidence[1].captured = false;
    let projection = Fixture::new(vec![legacy, future, empty, mixed]).run();
    for (id, reason) in [
        ("legacy", StateReason::LegacyRecord),
        ("future", StateReason::UnsupportedRecordVersion),
        ("empty", StateReason::NoEvidence),
        ("mixed", StateReason::EvidenceNotCaptured),
    ] {
        assert_eq!(
            outcome(&projection, id),
            (RecordState::Unverified, reason),
            "{id}"
        );
    }
    assert!(projection.items.is_empty());
    let mixed = projection
        .listed
        .iter()
        .find(|entry| entry.record_id == "mixed")
        .unwrap();
    assert_eq!(
        mixed.source_support,
        SourceSupport {
            captured: 1,
            unverified: 1
        }
    );
}

// AC-0652: first match wins. A record that is unverified, covered,
// unresolvable, stale and conflicted at once is `unverified`.
#[test]
fn unverified_precedes_every_other_state() {
    let mut first = record("r1");
    first.evidence[0].captured = false;
    first.gap_id = "gap:gone".into();
    let mut fixture = Fixture::new(vec![first, record("r2")]);
    fixture.assess("r1", |d| d.graph = BasisComparison::Changed);
    fixture.edges.push(edge(
        "sym:a",
        "sym:b",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    ));
    assert_eq!(
        outcome(&fixture.run(), "r1"),
        (RecordState::Unverified, StateReason::EvidenceNotCaptured)
    );
}

// AC-0652: `unresolvable` when the Gap or an endpoint is gone.
#[test]
fn unresolvable_state_and_reasons() {
    let mut no_gap = record("no-gap");
    no_gap.gap_id = "gap:gone".into();
    let mut resolved = record("resolved");
    resolved.gap_id = "sym:c".into();
    let mut no_source = record("no-source");
    no_source.gap_id = "gap:2".into();
    no_source.source_id = "sym:gone".into();
    let mut no_target = record("no-target");
    no_target.target_id = "sym:gone".into();
    let projection = Fixture::new(vec![no_gap, resolved, no_source, no_target]).run();
    for (id, reason) in [
        ("no-gap", StateReason::GapMissing),
        ("resolved", StateReason::GapNoLongerGap),
        ("no-source", StateReason::SourceMissing),
        ("no-target", StateReason::TargetMissing),
    ] {
        assert_eq!(
            outcome(&projection, id),
            (RecordState::Unresolvable, reason),
            "{id}"
        );
    }
}

// AC-0652: a node without valid provenance is a Gap, as in the Workbench
// register, so a record naming it is not unresolvable.
#[test]
fn node_without_provenance_counts_as_a_gap() {
    let mut fixture = Fixture::new(vec![record("r1")]);
    fixture.nodes[0] = Node {
        id: "gap:1".into(),
        label: "Symbol".into(),
        props: json!({}),
    };
    assert_eq!(outcome(&fixture.run(), "r1").0, RecordState::Eligible);
}

// AC-1038 (#2251): a record whose graph or associations changed but whose
// cited files are all unchanged is stale with its own reason; a failed
// comparison, an unavailable item or changed cited files keep theirs.
#[test]
fn unchanged_cited_files_have_their_own_stale_reason() {
    type Change = fn(&mut BasisDimensions);
    let cases: [(&str, StateReason, Change); 6] = [
        ("graph", StateReason::CitedFilesUnchanged, |d| {
            d.graph = BasisComparison::Changed;
            d.association = BasisComparison::Changed;
            d.cited_files = BasisComparison::Unchanged;
        }),
        ("association", StateReason::CitedFilesUnchanged, |d| {
            d.association = BasisComparison::Changed;
            d.cited_files = BasisComparison::Unchanged;
        }),
        ("changed", StateReason::GraphChanged, |d| {
            d.graph = BasisComparison::Changed;
            d.association = BasisComparison::Changed;
            d.cited_files = BasisComparison::Changed;
        }),
        ("not-compared", StateReason::GraphChanged, |d| {
            d.graph = BasisComparison::Changed;
        }),
        (
            "association-failed",
            StateReason::AssociationComparisonFailed,
            |d| {
                d.graph = BasisComparison::Changed;
                d.association = BasisComparison::OperationalFailure;
                d.cited_files = BasisComparison::Unchanged;
            },
        ),
        ("unavailable", StateReason::EvidenceUnavailable, |d| {
            d.graph = BasisComparison::Changed;
            d.cited_files = BasisComparison::Unchanged;
            d.evidence[1].status = EvidenceAvailability::Unavailable;
        }),
    ];
    let mut fixture = Fixture::new(
        cases
            .iter()
            .map(|(id, _, _)| {
                let mut value = record(id);
                value.gap_id = "gap:2".into();
                value
            })
            .collect(),
    );
    for (id, _, change) in cases {
        fixture.assess(id, change);
    }
    let projection = fixture.run();
    for (id, reason, _) in cases {
        assert_eq!(
            outcome(&projection, id),
            (RecordState::Stale, reason),
            "{id}"
        );
    }
}

// AC-0652: `unresolvable` precedes `stale`.
#[test]
fn unresolvable_precedes_stale() {
    let mut gone = record("r1");
    gone.target_id = "sym:gone".into();
    let mut fixture = Fixture::new(vec![gone]);
    fixture
        .assessments
        .insert("r1".into(), BasisAssessment::Failed);
    assert_eq!(
        outcome(&fixture.run(), "r1"),
        (RecordState::Unresolvable, StateReason::TargetMissing)
    );
}

// AC-0652: `stale` for each SPEC-09 dimension that does not hold, for a failed
// assessment and for a missing one.
#[test]
fn stale_state_and_reasons() {
    type Change = fn(&mut BasisDimensions);
    let cases: [(&str, StateReason, Change); 9] = [
        ("graph", StateReason::GraphChanged, |d| {
            d.graph = BasisComparison::Changed;
        }),
        ("graph-failed", StateReason::GraphComparisonFailed, |d| {
            d.graph = BasisComparison::OperationalFailure;
        }),
        ("association", StateReason::AssociationChanged, |d| {
            d.association = BasisComparison::Changed;
        }),
        (
            "association-failed",
            StateReason::AssociationComparisonFailed,
            |d| d.association = BasisComparison::OperationalFailure,
        ),
        ("unavailable", StateReason::EvidenceUnavailable, |d| {
            d.evidence[1].status = EvidenceAvailability::Unavailable;
        }),
        ("invalid", StateReason::EvidenceUnavailable, |d| {
            d.evidence[0].status = EvidenceAvailability::Invalid;
        }),
        ("read-failed", StateReason::EvidenceReadFailed, |d| {
            d.evidence[0].status = EvidenceAvailability::OperationalFailure;
        }),
        ("not-assessed", StateReason::EvidenceNotAssessed, |d| {
            d.evidence.pop();
        }),
        ("mismatch", StateReason::AssessmentMismatch, |d| {
            d.unverified_evidence = 1;
        }),
    ];
    let mut fixture = Fixture::new(
        cases
            .iter()
            .map(|(id, _, _)| {
                let mut value = record(id);
                // One Gap each, so no record conflicts with another.
                value.gap_id = "gap:2".into();
                value
            })
            .chain([record("failed"), record("missing")])
            .collect(),
    );
    for (id, _, change) in cases {
        fixture.assess(id, change);
    }
    fixture
        .assessments
        .insert("failed".into(), BasisAssessment::Failed);
    fixture.assessments.remove("missing");
    let projection = fixture.run();
    for (id, reason, _) in cases {
        assert_eq!(
            outcome(&projection, id),
            (RecordState::Stale, reason),
            "{id}"
        );
    }
    assert_eq!(
        outcome(&projection, "failed"),
        (RecordState::Stale, StateReason::AssessmentFailed)
    );
    assert_eq!(
        outcome(&projection, "missing"),
        (RecordState::Stale, StateReason::AssessmentMissing)
    );
    assert!(projection.items.is_empty());
    let report = |id: &str| {
        projection
            .listed
            .iter()
            .find(|entry| entry.record_id == id)
            .unwrap()
            .basis
    };
    assert_eq!(report("failed"), BasisReport::Failed);
    assert_eq!(report("missing"), BasisReport::NotSupplied);
    assert_eq!(
        report("unavailable"),
        BasisReport::Assessed {
            graph: BasisComparison::Unchanged,
            association: BasisComparison::Unchanged,
            available: 1,
            not_available: 1,
            unverified: 0,
        }
    );
}

fn covering_edge() -> Edge {
    edge(
        "sym:a",
        "sym:b",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    )
}

// AC-0652: `already-covered` precedes `unresolvable`. When the base graph
// holds the exact proposed edge, a missing Gap or endpoint node does not
// matter.
#[test]
fn already_covered_precedes_unresolvable() {
    let mut fixture = Fixture::new(vec![record("r1")]);
    fixture.edges.push(covering_edge());
    fixture.nodes.retain(|node| node.id != "gap:1");
    assert_eq!(
        outcome(&fixture.run(), "r1"),
        (RecordState::AlreadyCovered, StateReason::BaseFactFillsSlot)
    );
    fixture.nodes.clear();
    assert_eq!(
        outcome(&fixture.run(), "r1"),
        (RecordState::AlreadyCovered, StateReason::BaseFactFillsSlot)
    );
}

// AC-0652: `already-covered` precedes `stale` and needs no assessment.
#[test]
fn already_covered_precedes_stale() {
    let mut fixture = Fixture::new(vec![record("r1")]);
    fixture.edges.push(covering_edge());
    fixture.assess("r1", |d| d.association = BasisComparison::Changed);
    let covered = (RecordState::AlreadyCovered, StateReason::BaseFactFillsSlot);
    assert_eq!(outcome(&fixture.run(), "r1"), covered);
    fixture
        .assessments
        .insert("r1".into(), BasisAssessment::Failed);
    assert_eq!(outcome(&fixture.run(), "r1"), covered);
    fixture.assessments.clear();
    assert_eq!(outcome(&fixture.run(), "r1"), covered);
}

// AC-0652: `unverified` precedes `already-covered`.
#[test]
fn unverified_precedes_already_covered() {
    let mut legacy = record("r1");
    legacy.schema_version = 1;
    let mut fixture = Fixture::new(vec![legacy]);
    fixture.edges.push(covering_edge());
    assert_eq!(
        outcome(&fixture.run(), "r1"),
        (RecordState::Unverified, StateReason::LegacyRecord)
    );
}

// AC-0652: `stale` precedes `conflicted`.
#[test]
fn stale_precedes_conflicted() {
    let mut other = record("r2");
    other.target_id = "sym:c".into();
    let mut fixture = Fixture::new(vec![record("r1"), other]);
    fixture.assess("r1", |d| d.graph = BasisComparison::Changed);
    let projection = fixture.run();
    assert_eq!(
        outcome(&projection, "r1"),
        (RecordState::Stale, StateReason::GraphChanged)
    );
    assert_eq!(outcome(&projection, "r2").0, RecordState::Conflicted);
}

// AC-0652: `already-covered` when the base graph holds the proposed edge as a
// base fact, at any tier below Agentic.
#[test]
fn already_covered_by_a_base_fact() {
    for (tier, confidence) in [
        (Tier::Deterministic, ConfidenceTier::Confirmed),
        (Tier::Dynamic, ConfidenceTier::Confirmed),
        (Tier::Semantic, ConfidenceTier::InferredStrong),
    ] {
        let mut fixture = Fixture::new(vec![record("r1")]);
        fixture.edges.push(edge("sym:a", "sym:b", tier, confidence));
        assert_eq!(
            outcome(&fixture.run(), "r1"),
            (RecordState::AlreadyCovered, StateReason::BaseFactFillsSlot),
            "{tier:?}"
        );
    }
}

// AC-0652: an Agentic edge, a Gap edge, an edge without provenance, an edge to
// another target, the reverse edge and another label do not fill the slot.
#[test]
fn only_the_exact_base_edge_covers() {
    let mut other_label = edge(
        "sym:a",
        "sym:b",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    );
    other_label.label = "READS".into();
    let not_covering = [
        edge(
            "sym:a",
            "sym:b",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        ),
        edge("sym:a", "sym:b", Tier::Deterministic, ConfidenceTier::Gap),
        Edge {
            src: "sym:a".into(),
            dst: "sym:b".into(),
            label: "CALLS".into(),
            props: json!({}),
        },
        edge(
            "sym:a",
            "sym:c",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        edge(
            "sym:b",
            "sym:a",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        other_label,
    ];
    for extra in not_covering {
        let mut fixture = Fixture::new(vec![record("r1")]);
        fixture.edges.push(extra.clone());
        assert_eq!(
            outcome(&fixture.run(), "r1").0,
            RecordState::Eligible,
            "{extra:?}"
        );
    }
}

// AC-0652: `already-covered` precedes `conflicted`.
#[test]
fn already_covered_precedes_conflicted() {
    let mut other = record("r2");
    other.target_id = "sym:c".into();
    let mut fixture = Fixture::new(vec![record("r1"), other]);
    fixture.edges.push(edge(
        "sym:a",
        "sym:b",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    ));
    let projection = fixture.run();
    assert_eq!(outcome(&projection, "r1").0, RecordState::AlreadyCovered);
    assert_eq!(
        outcome(&projection, "r2"),
        (
            RecordState::Conflicted,
            StateReason::DifferentTargetAccepted
        )
    );
}

// AC-0652: accepted records naming one Gap with different targets are all
// conflicted; none is chosen.
#[test]
fn different_targets_for_one_gap_conflict() {
    let mut second = record("r2");
    second.target_id = "sym:c".into();
    let mut third = record("r3");
    third.gap_id = "gap:2".into();
    let projection = Fixture::new(vec![record("r1"), second, third]).run();
    assert_eq!(outcome(&projection, "r1").0, RecordState::Conflicted);
    assert_eq!(outcome(&projection, "r2").0, RecordState::Conflicted);
    // Another Gap is another slot.
    assert_eq!(outcome(&projection, "r3").0, RecordState::Eligible);
}

// AC-0652: every accepted record counts for a conflict, whatever its own
// state: an unverified record keeps its state and still conflicts the other.
#[test]
fn an_ineligible_accepted_record_still_conflicts() {
    let mut legacy = record("r2");
    legacy.schema_version = 1;
    legacy.target_id = "sym:c".into();
    let projection = Fixture::new(vec![record("r1"), legacy]).run();
    assert_eq!(outcome(&projection, "r1").0, RecordState::Conflicted);
    assert_eq!(outcome(&projection, "r2").0, RecordState::Unverified);
}

// AC-0652: accepted records for one Gap with the same source, label and
// target assert the same thing; they do not conflict and both stay eligible.
#[test]
fn same_edge_for_one_gap_does_not_conflict() {
    let projection = Fixture::new(vec![record("r1"), record("r2")]).run();
    assert_eq!(outcome(&projection, "r1").0, RecordState::Eligible);
    assert_eq!(outcome(&projection, "r2").0, RecordState::Eligible);
    assert_eq!(projection.items.len(), 2);
}

// AC-0652: pending and rejected records are neither items nor listed, are not
// counted, and do not conflict with an accepted record.
#[test]
fn pending_and_rejected_records_are_absent() {
    let mut pending = record("pending");
    pending.decision = None;
    pending.target_id = "sym:c".into();
    let mut rejected = record("rejected");
    rejected.decision = Some(ReviewDecision::Rejected);
    rejected.target_id = "sym:c".into();
    let projection = Fixture::new(vec![record("r1"), pending, rejected]).run();
    assert_eq!(projection.listed, vec![]);
    assert_eq!(projection.items.len(), 1);
    assert_eq!(projection.items[0].record_id, "r1");
    assert_eq!(projection.counts.accepted, 1);
    assert_eq!(projection.counts.eligible, 1);
}

/// One record in every state, plus a pending and a rejected one.
fn every_state() -> Fixture {
    let mut unverified = record("a-unverified");
    unverified.schema_version = 1;
    unverified.gap_id = "gap:3".into();
    let mut unresolvable = record("b-unresolvable");
    unresolvable.gap_id = "gap:gone".into();
    let mut stale = record("c-stale");
    stale.gap_id = "gap:4".into();
    let mut covered = record("d-covered");
    covered.gap_id = "gap:5".into();
    covered.source_id = "sym:c".into();
    let conflict_one = record("e-conflict-1");
    let mut conflict_two = record("f-conflict-2");
    conflict_two.target_id = "sym:c".into();
    let mut eligible = record("g-eligible");
    eligible.gap_id = "gap:2".into();
    let mut pending = record("h-pending");
    pending.decision = None;
    let mut rejected = record("i-rejected");
    rejected.decision = Some(ReviewDecision::Rejected);
    let mut fixture = Fixture::new(vec![
        unverified,
        unresolvable,
        stale,
        covered,
        conflict_one,
        conflict_two,
        eligible,
        pending,
        rejected,
    ]);
    fixture
        .nodes
        .extend([gap("gap:3"), gap("gap:4"), gap("gap:5")]);
    fixture.edges.push(edge(
        "sym:c",
        "sym:b",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    ));
    fixture.assess("c-stale", |d| d.graph = BasisComparison::Changed);
    fixture.decisions = vec![
        decision("hash-rejected", AssertionReview::Rejected),
        decision("hash-accepted", AssertionReview::Accepted),
        decision("hash-gone", AssertionReview::Rejected),
    ];
    fixture.current = ["hash-rejected", "hash-accepted"].map(String::from).into();
    fixture
}

// AC-0652: counts add up: accepted = eligible + every other state, and equals
// items + listed; both lists are in record-identity order.
#[test]
fn counts_add_up_and_lists_are_ordered() {
    let projection = every_state().run();
    let counts = projection.counts;
    assert_eq!(
        counts,
        CuratedCounts {
            accepted: 7,
            eligible: 1,
            unverified: 1,
            unresolvable: 1,
            stale: 1,
            already_covered: 1,
            conflicted: 2,
        }
    );
    assert_eq!(
        counts.accepted,
        counts.eligible
            + counts.unverified
            + counts.unresolvable
            + counts.stale
            + counts.already_covered
            + counts.conflicted
    );
    assert_eq!(counts.eligible, projection.items.len());
    assert_eq!(
        counts.accepted,
        projection.items.len() + projection.listed.len()
    );
    let listed: Vec<&str> = projection
        .listed
        .iter()
        .map(|entry| entry.record_id.as_str())
        .collect();
    assert_eq!(
        listed,
        [
            "a-unverified",
            "b-unresolvable",
            "c-stale",
            "d-covered",
            "e-conflict-1",
            "f-conflict-2"
        ]
    );
}

// AC-0652: the same inputs in another order give the same projection and the
// same revision.
#[test]
fn projection_is_independent_of_input_order() {
    let fixture = every_state();
    let expected = fixture.run();
    let mut reordered = every_state();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    reordered.proposals.reverse();
    reordered.proposals.swap(1, 6);
    reordered.decisions.reverse();
    for record in &mut reordered.proposals {
        record.evidence.reverse();
    }
    for assessment in reordered.assessments.values_mut() {
        if let BasisAssessment::Assessed(dimensions) = assessment {
            dimensions.evidence.reverse();
        }
    }
    let actual = reordered.run();
    assert_eq!(actual.revision, expected.revision);
    assert_eq!(actual, expected);
    assert_eq!(fixture.run(), expected, "repeatable");
}

// AC-0652: the revision changes when a decision flips, when a state changes,
// when a reason changes, when the graph hash changes and when the policy
// version changes.
#[test]
fn revision_changes_with_content() {
    let base = every_state().run().revision;

    let mut flipped = every_state();
    flipped.proposals[6].decision = Some(ReviewDecision::Rejected);
    assert_ne!(flipped.run().revision, base, "accepted to rejected");

    let mut reviewed = every_state();
    reviewed.proposals[7].decision = Some(ReviewDecision::Rejected);
    assert_ne!(reviewed.run().revision, base, "pending to rejected");

    let mut restored = every_state();
    restored.proposals[8].decision = Some(ReviewDecision::Accepted);
    assert_ne!(restored.run().revision, base, "rejected to accepted");

    let mut state = every_state();
    state.assess("g-eligible", |d| {
        d.evidence[0].status = EvidenceAvailability::Unavailable;
    });
    let changed = state.run();
    assert_eq!(outcome(&changed, "g-eligible").0, RecordState::Stale);
    assert_ne!(changed.revision, base, "eligible to stale");

    // The reason is listed, so it is content: a record that stays `stale`
    // for another reason gives another revision.
    let mut reason = every_state();
    reason.assess("c-stale", |d| d.association = BasisComparison::Changed);
    let changed = reason.run();
    assert_eq!(
        outcome(&changed, "c-stale"),
        (RecordState::Stale, StateReason::AssociationChanged)
    );
    assert_ne!(changed.revision, base, "stale for another reason");

    let mut graph = every_state();
    graph.graph_hash = "graph-hash-2".into();
    assert_ne!(graph.run().revision, base, "graph hash");

    let mut policy = every_state();
    policy.policy = "curated-policy-next".into();
    assert_ne!(policy.run().revision, base, "policy version");
}

// AC-0652: the revision covers content only. A pending record, and a change
// that leaves every decision and state as it was, do not move it.
#[test]
fn revision_ignores_what_is_not_content() {
    let base = every_state().run().revision;

    let mut pending = every_state();
    let mut extra = record("z-pending");
    extra.decision = None;
    pending.proposals.push(extra);
    assert_eq!(pending.run().revision, base, "a pending record");

    // An assessment detail that changes neither state nor reason.
    let mut same_reason = every_state();
    same_reason.assess("c-stale", |d| {
        d.graph = BasisComparison::Changed;
        d.association = BasisComparison::Changed;
    });
    let projection = same_reason.run();
    assert_eq!(
        outcome(&projection, "c-stale"),
        (RecordState::Stale, StateReason::GraphChanged)
    );
    assert_eq!(projection.revision, base, "same state and reason");
}

// AC-0652: field boundaries are framed; moving a character from one field to
// the next gives another revision.
#[test]
fn revision_fields_cannot_run_together() {
    let mut left = Fixture::new(vec![]);
    left.policy = "p1".into();
    left.graph_hash = "g".into();
    let mut right = Fixture::new(vec![]);
    right.policy = "p".into();
    right.graph_hash = "1g".into();
    assert_ne!(left.run().revision, right.run().revision);
}

// AC-0652: a decision whose content hash matches no current assertion is
// listed as stale; applied rejections are reported through the digest.
#[test]
fn assertion_decisions_are_summarized() {
    let fixture = every_state();
    let projection = fixture.run();
    let summary = &projection.assertion_decisions;
    assert!(
        summary
            .digest
            .starts_with("curated-assertion-decisions-v1:")
    );
    assert_eq!(summary.rejections_applied, 1);
    assert_eq!(
        summary.stale,
        vec![StaleAssertionDecision {
            content_hash: "hash-gone".into(),
            decision: AssertionReview::Rejected,
            state: RecordState::Stale,
        }]
    );

    // A rejection that starts to apply changes the digest and the revision.
    let mut applied = every_state();
    applied.current.insert("hash-gone".into());
    let after = applied.run();
    assert_eq!(after.assertion_decisions.rejections_applied, 2);
    assert!(after.assertion_decisions.stale.is_empty());
    assert_ne!(after.assertion_decisions.digest, summary.digest);
    assert_ne!(after.revision, projection.revision);

    // Flipping an applied rejection to accepted removes it from the digest.
    let mut flipped = every_state();
    flipped.decisions[0].decision = AssertionReview::Accepted;
    let after = flipped.run();
    assert_eq!(after.assertion_decisions.rejections_applied, 0);
    assert_ne!(after.assertion_decisions.digest, summary.digest);
    assert_ne!(after.revision, projection.revision);

    // An accepted or annotated decision on a current assertion filters
    // nothing, so it changes neither the digest nor the revision.
    let mut annotated = every_state();
    annotated.decisions[1].decision = AssertionReview::Annotated;
    let after = annotated.run();
    assert_eq!(after.assertion_decisions, *summary);
    assert_eq!(after.revision, projection.revision);

    // A stale decision of another kind is listed and changes the digest.
    let mut stale_note = every_state();
    stale_note
        .decisions
        .push(decision("hash-old", AssertionReview::Annotated));
    let after = stale_note.run();
    assert_eq!(after.assertion_decisions.stale.len(), 2);
    assert_eq!(after.assertion_decisions.stale[1].content_hash, "hash-old");
    assert_ne!(after.revision, projection.revision);
}

// AC-0652: tier and confidence are carried exactly as produced; nothing is
// upgraded or normalized, for items and for listed records.
#[test]
fn tier_and_confidence_are_carried_unchanged() {
    let mut odd = record("r2");
    odd.gap_id = "gap:2".into();
    odd.tier = Tier::Semantic;
    odd.confidence = ConfidenceTier::InferredStrong;
    let mut listed = record("r3");
    listed.schema_version = 1;
    listed.gap_id = "gap:gone".into();
    let inputs = vec![record("r1"), odd, listed];
    let projection = Fixture::new(inputs.clone()).run();
    assert_eq!(projection.items.len(), 2);
    assert_eq!(projection.listed.len(), 1);
    for entry in projection.items.iter().chain(&projection.listed) {
        let produced = inputs
            .iter()
            .find(|record| record.record_id == entry.record_id)
            .unwrap();
        assert_eq!(entry.tier, produced.tier);
        assert_eq!(entry.confidence, produced.confidence);
    }
    assert_eq!(projection.items[0].tier, Tier::Agentic);
    assert_eq!(projection.items[0].confidence, ConfidenceTier::InferredWeak);
}

// AC-0652: an ambiguous record set fails; it is never projected.
#[test]
fn duplicate_identities_fail_closed() {
    let mut pending = record("r1");
    pending.decision = None;
    assert_eq!(
        Fixture::new(vec![record("r1"), pending]).try_run(),
        Err(ProjectionError::DuplicateRecord)
    );
    let mut fixture = Fixture::new(vec![record("r1")]);
    fixture.decisions = vec![
        decision("h", AssertionReview::Rejected),
        decision("h", AssertionReview::Accepted),
    ];
    assert_eq!(
        fixture.try_run(),
        Err(ProjectionError::DuplicateAssertionDecision)
    );
}

// AC-0652: states and reasons are machine-readable on the wire.
#[test]
fn states_and_reasons_serialize_as_stable_codes() {
    let projection = every_state().run();
    let wire = serde_json::to_value(&projection).unwrap();
    assert_eq!(wire["view"], "curated");
    assert_eq!(wire["items"][0]["state"], "eligible");
    assert_eq!(wire["items"][0]["reason"], "all_conditions_hold");
    assert_eq!(wire["items"][0]["tier"], "Agentic");
    assert_eq!(wire["items"][0]["confidence"], "InferredWeak");
    assert_eq!(wire["listed"][3]["state"], "already-covered");
    assert_eq!(wire["listed"][3]["reason"], "base_fact_fills_slot");
    assert_eq!(wire["listed"][2]["basis"]["outcome"], "assessed");
    assert_eq!(wire["listed"][2]["basis"]["graph"], "changed");
    let back: CuratedProjection = serde_json::from_value(wire).unwrap();
    assert_eq!(back, projection);
}
