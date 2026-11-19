//! AC-0844 (T-0844): the host reads the curation reviews from the section
//! draft store's file and the curated projection gives each accepted
//! finding its state against the graph the read took.

use super::*;
use agents::TaskFactKey;
use agents::draft_store::DraftStore;
use agents::finding_curation::{
    CitedFact, CuratedFindingRecord, CurationDecision, StoredCuratedFinding,
};
use agents::investigation::{FindingReviewSubjectV1, InvestigationClaimKind};
use context_hub::curated::{FindingCuration, FindingState, FindingStateReason};
use core_graph::{FactKey, GraphStore};
use core_prov::EvidenceRef;
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    graph: SqliteGraphStore,
    reviews: StoredReviews,
}

fn rule(id: &str, note: &str) -> Node {
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/app.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Rule".into(),
        props: json!({ "name": id, "note": note, "prov": prov }),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["rule:refund", "rule:return"] {
        graph.put_node(&rule(id, "as read")).unwrap();
    }
    Fixture {
        graph,
        reviews: StoredReviews {
            proposals: Mutex::new(
                agents::ProposalStore::open(dir.path().join("proposals.sqlite")).unwrap(),
            ),
            drafts: Mutex::new(DraftStore::open(dir.path().join("section-drafts.sqlite")).unwrap()),
        },
        _dir: dir,
    }
}

fn citation(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

impl Fixture {
    /// The finding `finding` as an investigation would have saved it: it
    /// cites `cited` with the digest each node has in the graph now.
    fn record(&self, finding: &str, cited: &[&str]) -> CuratedFindingRecord {
        let (nodes, _) = self.graph.read_snapshot().unwrap();
        let mut facts: Vec<CitedFact> = cited
            .iter()
            .map(|id| {
                let node = nodes.iter().find(|node| node.id == *id).unwrap();
                CitedFact {
                    citation: citation(id),
                    fact: TaskFactKey::Node { id: (*id).into() },
                    digest: core_graph::source::node_digest(node).unwrap(),
                    captured: false,
                }
            })
            .collect();
        facts.sort_by(|left, right| left.citation.cmp(&right.citation));
        let digit = finding.len() % 10;
        CuratedFindingRecord {
            subject: FindingReviewSubjectV1 {
                investigation_id: "investigation-1".into(),
                result_id: "result-1".into(),
                finding_id: finding.into(),
                subject_id: format!("investigation-finding-review-v1:{}{digit}", "0".repeat(63)),
            },
            role: "domain-analyst@2".into(),
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: "Refunds need an approved return".into(),
            statement: "A refund is issued only for an approved return.".into(),
            limitations: Vec::new(),
            cited: facts,
        }
    }

    fn review(
        &self,
        record: &CuratedFindingRecord,
        revision: u64,
        decision: CurationDecision,
    ) -> StoredCuratedFinding {
        self.reviews
            .drafts
            .lock()
            .unwrap()
            .finding_curation()
            .review(record, revision, decision)
            .unwrap()
    }

    /// The projection of `review` over the graph as it is now.
    fn project(&self, review: &CuratedReview) -> CuratedProjection {
        let (revision, ((nodes, edges), coverage)) = read_graph(&self.graph).unwrap();
        let content_hash = spec::graph_content_hash(&nodes, &edges);
        project(
            review,
            &Snapshot {
                nodes: &nodes,
                edges: &edges,
                derived_decisions: DerivedDecisions::Compile(&[]),
                coverage: &coverage,
                content_hash: &content_hash,
                revision: &revision,
            },
            &[],
        )
        .unwrap()
    }
}

#[test]
fn an_accepted_finding_is_read_from_the_store_and_follows_the_facts_it_cites() {
    let mut fixture = fixture();
    let record = fixture.record("finding-1", &["rule:refund", "rule:return"]);
    let none = fixture.project(&fixture.reviews.gather());
    assert!(none.findings.is_empty());

    fixture.review(&record, 0, CurationDecision::Accepted);
    let gathered = fixture.reviews.gather();
    let projection = fixture.project(&gathered);
    let [finding] = projection.findings.as_slice() else {
        panic!("one finding");
    };
    assert_eq!(finding.finding_ref, record.subject.subject_id);
    assert_eq!(finding.statement, record.statement);
    assert_eq!(finding.role, "domain-analyst@2");
    assert_eq!(finding.kind, "implemented_behavior");
    assert_eq!(finding.review_status, FindingCuration::Accepted);
    assert_eq!(
        (finding.state, finding.reason),
        (
            FindingState::Eligible,
            FindingStateReason::AllConditionsHold
        )
    );
    assert_eq!(
        (finding.tier, finding.confidence),
        (Tier::Agentic, ConfidenceTier::InferredWeak)
    );
    assert_eq!(projection.finding_counts.eligible, 1);
    assert_ne!(projection.revision, none.revision);
    // The graph is not written: its content hash is the one read before.
    assert_eq!(projection.graph_basis, none.graph_basis);
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(true));

    // Any field of a cited fact changing makes the finding stale.
    fixture
        .graph
        .put_node(&rule("rule:refund", "changed since"))
        .unwrap();
    let stale = fixture.project(&fixture.reviews.gather());
    assert_eq!(
        (stale.findings[0].state, stale.findings[0].reason),
        (FindingState::Stale, FindingStateReason::CitedFactChanged)
    );
    assert_eq!(stale.findings[0].changed, [citation("rule:refund")]);
    assert!(stale.findings[0].missing.is_empty());
    assert_ne!(stale.revision, projection.revision);

    // Accepting it again certifies nothing: the record keeps the digests
    // the run recorded, so it stays stale.
    fixture.review(&record, 1, CurationDecision::Rejected);
    fixture.review(&record, 2, CurationDecision::Accepted);
    let again = fixture.project(&fixture.reviews.gather());
    assert_eq!(again.findings[0].state, FindingState::Stale);
}

#[test]
fn a_rejected_finding_is_withdrawn_and_a_later_review_makes_a_read_stale() {
    let fixture = fixture();
    let record = fixture.record("finding-1", &["rule:refund"]);
    let other = fixture.record("finding-22", &["rule:return"]);
    fixture.review(&record, 0, CurationDecision::Accepted);
    let gathered = fixture.reviews.gather();
    let accepted = fixture.project(&gathered);
    assert_eq!(accepted.finding_counts.accepted, 1);

    // A curation review made after the gather is seen.
    fixture.review(&other, 0, CurationDecision::Accepted);
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(false));
    let both = fixture.project(&fixture.reviews.gather());
    assert_eq!(both.finding_counts.eligible, 2);

    fixture.review(&record, 1, CurationDecision::Rejected);
    let withdrawn = fixture.project(&fixture.reviews.gather());
    assert_eq!(
        withdrawn
            .findings
            .iter()
            .map(|finding| finding.finding_id.as_str())
            .collect::<Vec<_>>(),
        ["finding-22"]
    );
    // The rejection enters the revision.
    assert_ne!(withdrawn.revision, both.revision);
    assert_ne!(withdrawn.revision, accepted.revision);
}

#[test]
fn a_finding_whose_cited_fact_is_gone_is_unresolvable() {
    let mut fixture = fixture();
    let record = fixture.record("finding-1", &["rule:refund", "rule:return"]);
    fixture.review(&record, 0, CurationDecision::Accepted);
    fixture.graph.delete_node("rule:return").unwrap();
    let projection = fixture.project(&fixture.reviews.gather());
    assert_eq!(
        (projection.findings[0].state, projection.findings[0].reason),
        (
            FindingState::Unresolvable,
            FindingStateReason::CitedFactMissing
        )
    );
    assert_eq!(projection.findings[0].missing, [citation("rule:return")]);
}
