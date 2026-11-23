//! AC-0843 (T-0843): findings accepted as curated in the curated projection.

use super::*;

const GRAPH_HASH: &str = "graph-hash-1";

/// A finding accepted as curated, citing `F-1` and `F-2` as the graph holds
/// them.
fn finding(id: &str) -> ReviewedFinding {
    ReviewedFinding {
        finding_ref: id.into(),
        investigation_id: "investigation-1".into(),
        result_id: "result-1".into(),
        finding_id: format!("finding-{id}"),
        role: "analyst@27".into(),
        kind: "implemented".into(),
        title: "Refunds need an approved return".into(),
        statement: "A refund is issued only for a return in the approved state.".into(),
        limitations: vec!["Partial refunds were not read.".into()],
        cited_digests: BTreeMap::from([
            ("F-1".to_owned(), "node-v1:1".to_owned()),
            ("F-2".to_owned(), "edge-v1:2".to_owned()),
        ]),
        support: SourceSupport {
            captured: 1,
            unverified: 1,
        },
        decision: Some(FindingCuration::Accepted),
    }
}

fn current() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("F-1".to_owned(), "node-v1:1".to_owned()),
        ("F-2".to_owned(), "edge-v1:2".to_owned()),
        ("F-3".to_owned(), "node-v1:3".to_owned()),
    ])
}

fn run_with(
    findings: &[ReviewedFinding],
    current: &BTreeMap<String, String>,
    graph_hash: &str,
) -> Result<CuratedProjection, ProjectionError> {
    project(&ProjectionInput {
        nodes: &[],
        edges: &[],
        graph_content_hash: graph_hash,
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[],
        cited_fact_hashes: &BTreeMap::new(),
        findings,
        cited_fact_digests: current,
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
}

fn run(findings: &[ReviewedFinding]) -> CuratedProjection {
    run_with(findings, &current(), GRAPH_HASH).unwrap()
}

fn outcome(projection: &CuratedProjection, id: &str) -> (FindingState, FindingStateReason) {
    let mut found = projection.findings.iter().filter(|f| f.finding_ref == id);
    let entry = found.next().unwrap();
    assert!(found.next().is_none(), "{id} is listed once");
    (entry.state, entry.reason)
}

#[test]
fn an_accepted_finding_whose_cited_facts_are_unchanged_is_eligible_and_stays_t3() {
    let projection = run(&[finding("f1")]);
    let [entry] = projection.findings.as_slice() else {
        panic!("one finding");
    };
    let stored = finding("f1");
    assert_eq!(entry.state, FindingState::Eligible);
    assert_eq!(entry.reason, FindingStateReason::AllConditionsHold);
    assert_eq!(entry.tier, Tier::Agentic);
    assert_eq!(entry.confidence, ConfidenceTier::InferredWeak);
    assert_eq!(entry.review_status, FindingCuration::Accepted);
    assert_eq!(entry.source_support, stored.support);
    // The content and its identity are carried as stored.
    assert_eq!(entry.statement, stored.statement);
    assert_eq!(entry.title, stored.title);
    assert_eq!(entry.kind, stored.kind);
    assert_eq!(entry.limitations, stored.limitations);
    assert_eq!(entry.role, stored.role);
    assert_eq!(
        (&entry.investigation_id, &entry.result_id, &entry.finding_id),
        (
            &stored.investigation_id,
            &stored.result_id,
            &stored.finding_id
        )
    );
    assert_eq!(entry.citations, ["F-1", "F-2"]);
    assert!(entry.changed.is_empty() && entry.missing.is_empty());
    assert_eq!(
        projection.finding_counts,
        FindingCounts {
            accepted: 1,
            eligible: 1,
            ..FindingCounts::default()
        }
    );
    // A finding is neither a proposal nor a draft.
    assert!(projection.items.is_empty() && projection.listed.is_empty());
    assert!(projection.drafts.is_empty());
    assert_eq!(projection.counts, CuratedCounts::default());
    assert_eq!(projection.draft_counts, DraftCounts::default());
    assert_eq!(projection.graph_basis, GRAPH_HASH);
    assert!(projection.revision.starts_with("curated-revision-v4:"));
}

#[test]
fn a_finding_is_stale_exactly_when_a_cited_fact_has_another_digest() {
    // An uncited fact changing, or a new fact, leaves the finding eligible.
    let mut other = current();
    other.insert("F-3".into(), "node-v1:3b".into());
    other.insert("F-4".into(), "node-v1:4".into());
    let projection = run_with(&[finding("f1")], &other, GRAPH_HASH).unwrap();
    assert_eq!(outcome(&projection, "f1").0, FindingState::Eligible);

    for cited in ["F-1", "F-2"] {
        let mut changed = current();
        changed.insert(cited.into(), "node-v1:new".into());
        let projection = run_with(&[finding("f1")], &changed, GRAPH_HASH).unwrap();
        assert_eq!(
            outcome(&projection, "f1"),
            (FindingState::Stale, FindingStateReason::CitedFactChanged),
            "{cited}"
        );
        assert_eq!(projection.findings[0].changed, [cited]);
        assert_eq!(projection.finding_counts.stale, 1);
        assert_eq!(projection.finding_counts.eligible, 0);
    }

    // The graph hash alone moving does not make a finding stale.
    let projection = run_with(&[finding("f1")], &current(), "graph-hash-2").unwrap();
    assert_eq!(outcome(&projection, "f1").0, FindingState::Eligible);
}

#[test]
fn a_finding_citing_a_missing_fact_or_no_fact_is_unresolvable_before_stale() {
    let mut gone = current();
    gone.remove("F-2");
    gone.insert("F-1".into(), "node-v1:new".into());
    let projection = run_with(&[finding("f1")], &gone, GRAPH_HASH).unwrap();
    assert_eq!(
        outcome(&projection, "f1"),
        (
            FindingState::Unresolvable,
            FindingStateReason::CitedFactMissing
        )
    );
    // Both lists are reported whatever the state.
    assert_eq!(projection.findings[0].missing, ["F-2"]);
    assert_eq!(projection.findings[0].changed, ["F-1"]);

    let mut uncited = finding("f2");
    uncited.cited_digests.clear();
    let projection = run(&[uncited]);
    assert_eq!(
        outcome(&projection, "f2"),
        (FindingState::Unresolvable, FindingStateReason::NoCitedFact)
    );
    assert_eq!(projection.finding_counts.unresolvable, 1);
}

#[test]
fn only_a_curation_review_lists_a_finding_and_rejection_withdraws_it() {
    let mut unreviewed = finding("f1");
    unreviewed.decision = None;
    let none = run(&[]);
    let pending = run(&[unreviewed]);
    // No curation review: not listed, and the revision does not move.
    assert!(pending.findings.is_empty());
    assert_eq!(pending.revision, none.revision);

    let accepted = run(&[finding("f1")]);
    assert_ne!(accepted.revision, none.revision);

    let mut rejected = finding("f1");
    rejected.decision = Some(FindingCuration::Rejected);
    let withdrawn = run(&[rejected]);
    // Rejected: gone from the content, and the revision says a review moved.
    assert!(withdrawn.findings.is_empty());
    assert_eq!(withdrawn.finding_counts, FindingCounts::default());
    assert_ne!(withdrawn.revision, accepted.revision);
    assert_ne!(withdrawn.revision, none.revision);
}

#[test]
fn two_accepted_findings_never_conflict_and_counts_add_up() {
    // The same statement over the same facts, from two results.
    let mut stale = finding("f3");
    stale
        .cited_digests
        .insert("F-3".into(), "node-v1:old".into());
    let projection = run(&[finding("f2"), stale, finding("f1")]);
    let listed: Vec<_> = projection
        .findings
        .iter()
        .map(|f| (f.finding_ref.as_str(), f.state))
        .collect();
    assert_eq!(
        listed,
        [
            ("f1", FindingState::Eligible),
            ("f2", FindingState::Eligible),
            ("f3", FindingState::Stale),
        ]
    );
    let counts = projection.finding_counts;
    assert_eq!(
        counts.accepted,
        counts.eligible + counts.stale + counts.unresolvable
    );
    assert_eq!((counts.eligible, counts.stale), (2, 1));
}

#[test]
fn the_finding_projection_is_independent_of_input_order_and_fails_on_a_duplicate() {
    let mut rejected = finding("f2");
    rejected.decision = Some(FindingCuration::Rejected);
    let forward = run(&[finding("f1"), rejected.clone(), finding("f3")]);
    let backward = run(&[finding("f3"), rejected, finding("f1")]);
    assert_eq!(forward, backward);

    // One identity names one finding, reviewed or not.
    let mut unreviewed = finding("f1");
    unreviewed.decision = None;
    assert_eq!(
        run_with(&[finding("f1"), unreviewed], &current(), GRAPH_HASH),
        Err(ProjectionError::DuplicateRecord)
    );
}

#[test]
fn a_finding_review_changes_the_revision_shared_with_drafts_and_proposals() {
    let draft = ReviewedDraft {
        draft_id: "d1".into(),
        address: DraftAddress {
            catalogue_version: "1.0".into(),
            document: "D1".into(),
            section: "1.3".into(),
            scope: "system".into(),
        },
        role: "section-writer@1".into(),
        claims: Vec::new(),
        cited_hashes: BTreeMap::new(),
        replaces: None,
        decision: Some(DraftReview::Accepted),
    };
    let with = |findings: &[ReviewedFinding]| {
        project(&ProjectionInput {
            nodes: &[],
            edges: &[],
            graph_content_hash: GRAPH_HASH,
            proposals: &[],
            assessments: &BTreeMap::new(),
            assertion_decisions: &[],
            current_assertion_hashes: &BTreeSet::new(),
            drafts: std::slice::from_ref(&draft),
            cited_fact_hashes: &BTreeMap::new(),
            findings,
            cited_fact_digests: &current(),
            attested: &[],
            policy_version: CURATED_POLICY_VERSION,
        })
        .unwrap()
    };
    let before = with(&[]);
    let after = with(&[finding("f1")]);
    // One revision for every record kind: the draft is unchanged, the
    // revision is not.
    assert_eq!(before.drafts, after.drafts);
    assert_ne!(before.revision, after.revision);
    // A finding id equal to a draft id is another record, not a duplicate.
    let same_id = with(&[finding("d1")]);
    assert_eq!(same_id.findings.len(), 1);
    assert_eq!(same_id.drafts.len(), 1);
}
