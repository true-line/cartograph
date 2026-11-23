//! AC-1075 (T-1075): attested records in the curated projection.

use super::*;
use core_prov::Provenance;
use serde_json::json;

const GRAPH_HASH: &str = "graph-hash-1";

fn gap(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Gap".into(),
        props: json!({}),
    }
}

/// A node an ingest recovered with `tier`, under a Gap's old id.
fn recovered(id: &str, tier: Tier) -> Node {
    let confidence = if tier == Tier::Agentic {
        ConfidenceTier::InferredWeak
    } else {
        ConfidenceTier::Confirmed
    };
    let provenance = Provenance::new(tier, confidence, vec![], "fixture", id.as_bytes()).unwrap();
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({ "prov": provenance }),
    }
}

fn contribution(engineer: bool, verb: &str) -> AttestedContribution {
    AttestedContribution {
        contributor_id: if engineer {
            "C-00000000000000e1".into()
        } else {
            "C-00000000000000a1".into()
        },
        engineer,
        verb: verb.into(),
        role: (!engineer).then(|| "section-writer@1".into()),
        run: (!engineer).then(|| "run-1".into()),
    }
}

fn attested(id: &str, standing: AttestedStanding) -> ReviewedAttested {
    ReviewedAttested {
        record_id: id.into(),
        citation_id: format!("A-{id:0>16}"),
        answer: AttestedAnswer::Gap {
            gap_id: "gap:login".into(),
            slot: "calls".into(),
        },
        statement: "The login handler is registered at runtime.".into(),
        recorded_hashes: BTreeMap::from([("F-1".to_owned(), "hash-1".to_owned())]),
        standing,
        attestations: match standing {
            AttestedStanding::AgentAttestedUnconfirmed => vec![contribution(false, "stated")],
            _ => vec![
                contribution(false, "stated"),
                contribution(true, "confirmed"),
            ],
        },
    }
}

fn current() -> BTreeMap<String, String> {
    BTreeMap::from([("F-1".to_owned(), "hash-1".to_owned())])
}

fn run(records: &[ReviewedAttested], nodes: &[Node]) -> CuratedProjection {
    run_with(records, nodes, &current()).unwrap()
}

fn run_with(
    records: &[ReviewedAttested],
    nodes: &[Node],
    current: &BTreeMap<String, String>,
) -> Result<CuratedProjection, ProjectionError> {
    project(&ProjectionInput {
        nodes,
        edges: &[],
        graph_content_hash: GRAPH_HASH,
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[],
        cited_fact_hashes: current,
        findings: &[],
        cited_fact_digests: &BTreeMap::new(),
        attested: records,
        policy_version: CURATED_POLICY_VERSION,
    })
}

fn only(projection: &CuratedProjection) -> (AttestedState, AttestedStateReason) {
    assert_eq!(projection.attested.len(), 1);
    let record = &projection.attested[0];
    (record.state, record.reason)
}

#[test]
fn an_engineer_backed_record_is_eligible_with_empty_tier_and_the_gap_still_counted() {
    let nodes = [gap("gap:login")];
    let projection = run(&[attested("a1", AttestedStanding::EngineerBacked)], &nodes);
    assert_eq!(
        only(&projection),
        (AttestedState::Eligible, AttestedStateReason::EngineerBacked)
    );
    let record = &projection.attested[0];
    assert_eq!((record.tier, record.confidence), (None, None));
    assert_eq!(record.citations, vec!["F-1".to_owned()]);
    assert_eq!(record.attestations.len(), 2);
    assert!(projection.revision.starts_with("curated-revision-v4:"));
    assert_eq!(CURATED_POLICY_VERSION, "curated-policy-v4");
    // The base graph is the one passed in, and its Gap is still a Gap.
    assert_eq!(projection.graph_basis, GRAPH_HASH);
    assert!(is_gap(&nodes[0]));
    assert!(projection.items.is_empty() && projection.listed.is_empty());
    assert_eq!(projection.attested_counts.eligible, 1);
    assert_eq!(projection.attested_counts.projected, 1);
}

#[test]
fn agents_alone_are_projected_as_agent_attested_unconfirmed_never_eligible() {
    let projection = run(
        &[attested("a1", AttestedStanding::AgentAttestedUnconfirmed)],
        &[gap("gap:login")],
    );
    assert_eq!(
        only(&projection),
        (
            AttestedState::AgentAttestedUnconfirmed,
            AttestedStateReason::AgentsOnly
        )
    );
    assert_eq!(
        AttestedState::AgentAttestedUnconfirmed.label(),
        "agent-attested, unconfirmed"
    );
    assert_eq!(projection.attested_counts.eligible, 0);
    assert_eq!(projection.attested_counts.agent_attested_unconfirmed, 1);
}

#[test]
fn states_apply_in_first_match_order() {
    use AttestedStanding as B;
    use AttestedState as S;
    let open = [gap("gap:login")];
    let changed = BTreeMap::from([("F-1".to_owned(), "hash-2".to_owned())]);
    // Withdrawn and disputed win over everything the graph says.
    for (standing, state) in [(B::Withdrawn, S::Withdrawn), (B::Disputed, S::Disputed)] {
        let projection = run_with(&[attested("a1", standing)], &[], &changed).unwrap();
        assert_eq!(only(&projection).0, state);
    }
    // A Gap recovered deterministically is already covered, before a
    // missing or changed citation.
    let projection = run_with(
        &[attested("a1", B::EngineerBacked)],
        &[recovered("gap:login", Tier::Deterministic)],
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        only(&projection),
        (S::AlreadyCovered, AttestedStateReason::GapRecovered)
    );
    // A Gap gone, or now a node an agent produced, is unresolvable.
    let projection = run(&[attested("a1", B::EngineerBacked)], &[]);
    assert_eq!(
        only(&projection),
        (S::Unresolvable, AttestedStateReason::GapMissing)
    );
    let projection = run(
        &[attested("a1", B::EngineerBacked)],
        &[recovered("gap:login", Tier::Agentic)],
    );
    assert_eq!(
        only(&projection),
        (S::Unresolvable, AttestedStateReason::GapNotRecovered)
    );
    // A missing citation is unresolvable before a changed one is stale.
    let mut two = attested("a1", B::EngineerBacked);
    two.recorded_hashes.insert("F-2".into(), "hash-2".into());
    let projection = run_with(&[two], &open, &changed).unwrap();
    assert_eq!(
        only(&projection),
        (S::Unresolvable, AttestedStateReason::CitedFactMissing)
    );
    assert_eq!(projection.attested[0].changed, vec!["F-1".to_owned()]);
    assert_eq!(projection.attested[0].missing, vec!["F-2".to_owned()]);
    let projection = run_with(
        &[attested("a1", B::AgentAttestedUnconfirmed)],
        &open,
        &changed,
    )
    .unwrap();
    assert_eq!(
        only(&projection),
        (S::Stale, AttestedStateReason::CitedFactChanged)
    );
}

#[test]
fn a_section_target_is_resolved_by_its_citations_alone() {
    let mut record = attested("a1", AttestedStanding::EngineerBacked);
    record.answer = AttestedAnswer::Section {
        catalogue_version: "1.0".into(),
        document: "D1".into(),
        section: "1.3".into(),
        scope: "system".into(),
    };
    assert_eq!(
        only(&run(std::slice::from_ref(&record), &[])).0,
        AttestedState::Eligible
    );
    record.recorded_hashes.clear();
    assert_eq!(only(&run(&[record], &[])).0, AttestedState::Eligible);
}

#[test]
fn the_attested_projection_is_order_independent_moves_the_revision_and_fails_on_a_duplicate() {
    let nodes = [gap("gap:login")];
    let first = attested("a1", AttestedStanding::EngineerBacked);
    let second = attested("a2", AttestedStanding::AgentAttestedUnconfirmed);
    let forward = run(&[first.clone(), second.clone()], &nodes);
    let backward = run(&[second.clone(), first.clone()], &nodes);
    assert_eq!(forward, backward);
    assert_eq!(
        forward
            .attested
            .iter()
            .map(|record| record.record_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a1", "a2"]
    );
    let counts = forward.attested_counts;
    assert_eq!(counts.projected, 2);
    assert_eq!(counts.eligible + counts.agent_attested_unconfirmed, 2);
    // No record, or a change of standing, gives another revision.
    assert_ne!(run(&[], &nodes).revision, forward.revision);
    let confirmed = AttestedStanding::EngineerBacked;
    let mut promoted = second.clone();
    promoted.standing = confirmed;
    assert_ne!(
        run(&[first.clone(), promoted], &nodes).revision,
        forward.revision
    );
    assert_eq!(
        run_with(&[first.clone(), first], &nodes, &current()),
        Err(ProjectionError::DuplicateRecord)
    );
}
