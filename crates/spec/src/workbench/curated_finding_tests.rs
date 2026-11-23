//! AC-0845 (T-0845): findings accepted as curated in `curated.md`. Both
//! export modes, the citations through the fact index, the findings that
//! are not eligible and a project with no accepted finding.

use super::*;
use crate::{
    ExportMode, FactIndex, SpecArtifact, SpecBundle, Supplied, check_citations, cited_fact_digests,
    try_compile_spec_supplied,
};
use context_hub::curated::{
    CURATED_POLICY_VERSION, FindingCuration, ProjectionInput, ReviewedFinding, SourceSupport,
    project,
};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

const HEADING: &str = "\n## Findings accepted as curated\n";
const STATEMENT: &str = "A refund is issued only for an approved return.";

fn prov(tier: Tier, confidence: ConfidenceTier, id: &str) -> Provenance {
    Provenance::new(
        tier,
        confidence,
        vec![EvidenceRef {
            repo: "local/shop".into(),
            path: "src/app.ts".into(),
            byte_start: 1,
            byte_end: 9,
            commit_sha: "abc123".into(),
        }],
        "spec.curated.finding.test",
        id.as_bytes(),
    )
    .unwrap()
}

/// `sym:b` is a T2 inference, so the Workbench can reject it.
fn nodes() -> Vec<Node> {
    [
        ("sym:a", Tier::Deterministic, ConfidenceTier::Confirmed),
        ("sym:b", Tier::Semantic, ConfidenceTier::InferredStrong),
    ]
    .into_iter()
    .map(|(id, tier, confidence)| Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({ "name": id, "prov": prov(tier, confidence, id) }),
    })
    .collect()
}

fn edges() -> Vec<Edge> {
    vec![Edge {
        src: "sym:a".into(),
        dst: "sym:b".into(),
        label: "CALLS".into(),
        props: json!({
            "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "sym:a CALLS sym:b")
        }),
    }]
}

fn node_id(id: &str) -> String {
    citation_id(&FactKey::Node { id: id.into() })
}

fn edge_id() -> String {
    citation_id(&FactKey::Edge {
        source: "sym:a".into(),
        label: "CALLS".into(),
        destination: "sym:b".into(),
    })
}

/// The current digest of every fact of the fixture, by citation id.
fn current() -> BTreeMap<String, String> {
    let cited = BTreeSet::from([node_id("sym:a"), node_id("sym:b"), edge_id()]);
    let digests = cited_fact_digests(&nodes(), &edges(), &cited);
    assert_eq!(digests.len(), 3);
    digests
}

/// A finding accepted as curated that cites `cited` as the graph holds
/// them now.
fn finding(id: &str, cited: &[String]) -> ReviewedFinding {
    let current = current();
    ReviewedFinding {
        finding_ref: format!("ref-{id}"),
        investigation_id: "investigation-1".into(),
        result_id: "result-1".into(),
        finding_id: id.into(),
        role: "domain-analyst@2".into(),
        kind: "implemented_behavior".into(),
        title: format!("Title of {id}"),
        statement: format!("{STATEMENT} ({id})"),
        limitations: vec!["Partial refunds were not read.".into()],
        cited_digests: cited
            .iter()
            .map(|id| (id.clone(), current[id].clone()))
            .collect(),
        support: SourceSupport {
            captured: 0,
            unverified: cited.len(),
        },
        decision: Some(FindingCuration::Accepted),
    }
}

fn projection(findings: &[ReviewedFinding]) -> CuratedProjection {
    project(&ProjectionInput {
        nodes: &nodes(),
        edges: &edges(),
        graph_content_hash: "graph-hash-1",
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[],
        cited_fact_hashes: &BTreeMap::new(),
        findings,
        cited_fact_digests: &current(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap()
}

fn compile(
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    projection: &CuratedProjection,
) -> SpecBundle {
    try_compile_spec_supplied(
        &nodes(),
        &edges(),
        &[],
        mode,
        rejected,
        Supplied {
            repo_names: &[],
            knowledge: None,
            coverage: &[],
            stale_coverage: &[],
            curated: Curated::Computed(projection),
            graph_content_hash: None,
            source_revisions: None,
        },
    )
    .unwrap()
}

fn curated(bundle: &SpecBundle) -> &SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == ARTIFACT_ID)
        .unwrap()
}

fn index(bundle: &SpecBundle) -> FactIndex {
    let artifact = bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == "fact-index-json")
        .unwrap();
    serde_json::from_str(&artifact.content).unwrap()
}

/// The findings section of `curated.md`.
fn section(content: &str) -> &str {
    let start = content.find(HEADING).unwrap() + HEADING.len();
    let rest = &content[start..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

fn printed_citations(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| word.len() == 18 && word.starts_with("F-"))
        .collect()
}

#[test]
fn best_effort_prints_eligible_findings_marked_and_cited_through_the_fact_index() {
    // Two findings over the same two facts: each fact is asserted once.
    let both = [node_id("sym:a"), edge_id()];
    let projection = projection(&[finding("f2", &both), finding("f1", &both)]);
    assert_eq!(projection.finding_counts.eligible, 2);
    let best = compile(ExportMode::BestEffort, &BTreeSet::new(), &projection);
    let artifact = curated(&best);
    assert!(artifact.verify_identity());
    let text = section(&artifact.content);
    assert!(text.contains("- Accepted findings: 2. Eligible: 2. Stale: 0. Unresolvable: 0."));
    let mut cites = both.to_vec();
    cites.sort();
    for (number, id) in [(1, "f1"), (2, "f2")] {
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("{number}. ")))
            .unwrap();
        assert!(line.contains(&format!("**Title of {id}**")), "{line}");
        assert!(line.contains(&format!("{STATEMENT} ({id})")), "{line}");
        for part in [
            "implemented\\_behavior; T3 / InferredWeak; role `domain-analyst@2`",
            "human-reviewed: accepted as curated",
            "source support: 0 captured, 2 unverified",
            &format!("finding `ref-{id}`"),
            &format!("cites {})", cites.join(", ")),
        ] {
            assert!(line.contains(part), "{part}: {line}");
        }
    }
    assert_eq!(
        text.matches("   - Limitation: Partial refunds were not read.")
            .count(),
        2
    );
    // No accepted finding is other than eligible, so nothing else is listed.
    assert!(!text.contains("### Accepted findings that are not eligible"));

    // ADR-0058 decision 6 over the printed ids.
    let printed = printed_citations(text);
    assert_eq!(printed.len(), 4);
    let index = index(&best);
    check_citations(&index, printed.iter().copied()).unwrap();
    assert_eq!(artifact.assertions.len(), 2);
    for id in &both {
        let row = index
            .rows
            .iter()
            .find(|row| &row.citation_id == id)
            .unwrap();
        assert!(row.cited_by.iter().any(|by| by == ARTIFACT_ID));
    }
    // A finding is not a fact: no row is keyed by one.
    assert!(
        index
            .rows
            .iter()
            .all(|row| !serde_json::to_string(&row.fact).unwrap().contains("ref-f"))
    );
}

#[test]
fn verified_only_prints_no_finding_and_states_how_many_it_left_out() {
    let projection = projection(&[finding("f1", &[node_id("sym:a")])]);
    let verified = compile(ExportMode::VerifiedOnly, &BTreeSet::new(), &projection);
    let artifact = curated(&verified);
    let text = section(&artifact.content);
    assert!(text.contains("- Accepted findings: 1. Eligible: 1. Stale: 0. Unresolvable: 0."));
    assert!(text.contains(
        "No finding is shown: this is a `verified-only` export. Eligible findings left out: 1."
    ));
    assert!(!artifact.content.contains(STATEMENT));
    assert!(!artifact.content.contains("Title of f1"));
    assert!(printed_citations(text).is_empty());
    assert!(artifact.assertions.is_empty());
}

#[test]
fn findings_that_are_not_eligible_are_listed_in_both_modes_without_their_statement() {
    let mut stale = finding("f-stale", &[node_id("sym:a"), edge_id()]);
    stale
        .cited_digests
        .insert(node_id("sym:a"), "node-v1:as-read".into());
    let mut gone = finding("f-gone", &[edge_id()]);
    gone.cited_digests
        .insert("F-00000000000000aa".into(), "node-v1:1".into());
    let mut uncited = finding("f-uncited", &[]);
    uncited.cited_digests.clear();
    let projection = projection(&[stale, gone, uncited, finding("f-ok", &[edge_id()])]);
    let mut lists = Vec::new();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile(mode, &BTreeSet::new(), &projection);
        let artifact = curated(&bundle);
        let text = section(&artifact.content);
        assert!(text.contains("- Accepted findings: 4. Eligible: 1. Stale: 1. Unresolvable: 2."));
        let (shown, list) = text
            .split_once("\n### Accepted findings that are not eligible\n")
            .unwrap();
        for (id, row) in [
            (
                "f-stale",
                format!(
                    "| stale | A fact the finding cites changed after the investigation read \
                     it. (`cited_fact_changed`) | {} | — |",
                    node_id("sym:a")
                ),
            ),
            (
                "f-gone",
                "| unresolvable | A fact the finding cites is not in the current graph. \
                 (`cited_fact_missing`) | — | F-00000000000000aa |"
                    .to_owned(),
            ),
            (
                "f-uncited",
                "| unresolvable | The finding cites no fact. (`no_cited_fact`) | — | — |"
                    .to_owned(),
            ),
        ] {
            let line = list
                .lines()
                .find(|line| {
                    line.starts_with(&format!("| ref-{id} | investigation-1 | result-1 |"))
                })
                .unwrap();
            assert!(line.ends_with(&row), "{line}");
            // Its statement and title are printed nowhere.
            assert!(!artifact.content.contains(&format!("({id})")));
            assert!(!artifact.content.contains(&format!("Title of {id}")));
        }
        assert!(!list.contains("ref-f-ok"));
        // Only the eligible finding is printed, and only in best-effort.
        assert_eq!(
            shown.contains("**Title of f-ok**"),
            mode == ExportMode::BestEffort
        );
        // Ids in the list are not citations of this artifact.
        let cited: Vec<&str> = printed_citations(shown);
        check_citations(&index(&bundle), cited.iter().copied()).unwrap();
        lists.push(list.to_owned());
    }
    assert_eq!(lists[0], lists[1]);
}

#[test]
fn an_eligible_finding_citing_a_fact_the_export_withholds_is_named_and_not_printed() {
    let projection = projection(&[
        finding("f-a", &[node_id("sym:a")]),
        finding("f-b", &[node_id("sym:a"), node_id("sym:b")]),
    ]);
    let target = &nodes()[1];
    let rejected = BTreeSet::from([crate::provenance(&target.props, &target.id).content_hash]);
    let best = compile(ExportMode::BestEffort, &rejected, &projection);
    let artifact = curated(&best);
    let text = section(&artifact.content);
    assert!(text.contains("1. **Title of f-a**"));
    assert!(!text.contains("Title of f-b"));
    assert!(text.contains(
        "- Finding `ref-f-b` is eligible and is not printed: it cites a fact this export does \
         not list."
    ));
    assert!(!artifact.content.contains(&node_id("sym:b")));
    let printed = printed_citations(text);
    assert_eq!(printed, [node_id("sym:a").as_str()]);
    check_citations(&index(&best), printed.iter().copied()).unwrap();
    assert_eq!(artifact.assertions.len(), 1);
}

#[test]
fn the_section_is_written_only_when_a_finding_is_accepted() {
    let mut rejected = finding("f1", &[node_id("sym:a")]);
    rejected.decision = Some(FindingCuration::Rejected);
    let mut unreviewed = finding("f2", &[node_id("sym:a")]);
    unreviewed.decision = None;
    for findings in [vec![], vec![rejected], vec![unreviewed]] {
        let projection = projection(&findings);
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let bundle = compile(mode, &BTreeSet::new(), &projection);
            let artifact = curated(&bundle);
            assert!(!artifact.content.contains("Findings accepted as curated"));
            assert!(!artifact.content.contains(STATEMENT));
            assert!(artifact.assertions.is_empty());
        }
    }
}

#[test]
fn finding_states_and_reasons_are_named_as_the_projection_serializes_them() {
    for state in [
        FindingState::Unresolvable,
        FindingState::Stale,
        FindingState::Eligible,
    ] {
        assert_eq!(json!(state), finding_state_name(state));
    }
    let mut sentences = BTreeSet::new();
    for reason in [
        FindingStateReason::NoCitedFact,
        FindingStateReason::CitedFactMissing,
        FindingStateReason::CitedFactChanged,
        FindingStateReason::AllConditionsHold,
    ] {
        let (code, sentence) = finding_reason_text(reason);
        assert_eq!(json!(reason), code);
        assert_eq!(curated_finding_reason_sentence(reason), sentence);
        assert!(sentences.insert(sentence));
    }
    // One rule decides what a read shows.
    let projection = projection(&[finding("f1", &[node_id("sym:a")])]);
    assert!(curated_finding_shown(
        &projection.findings[0],
        ExportMode::BestEffort
    ));
    assert!(!curated_finding_shown(
        &projection.findings[0],
        ExportMode::VerifiedOnly
    ));
}
