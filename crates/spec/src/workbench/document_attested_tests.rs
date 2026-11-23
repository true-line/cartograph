//! AC-1077 (T-1077): attested claims in `product-specification.md` and the
//! fact index. A claim citing an `A-` id prints in `best-effort` with the
//! record's label and contributors and gains an attested row in the fact
//! index; `verified-only` prints none and counts them per section.

use super::tests::{d1, flows, nodes, printed_citations, section};
use super::*;
use crate::{
    FactIndex, FactIndexError, SpecBundle, Supplied, check_citations, cited_fact_hashes,
    try_compile_spec_supplied,
};
use context_hub::curated::{
    AttestedAnswer, AttestedContribution, AttestedStanding, CURATED_POLICY_VERSION,
    CuratedProjection, DraftAddress, DraftClaim, DraftReview, ProjectionInput, ReviewedAttested,
    ReviewedDraft, project,
};
use std::collections::{BTreeMap, BTreeSet};

const RECORD_ID: &str = "attested-record-1";
const ATTESTED: &str = "A-00000000000000a1";
const STATEMENT: &str = "A refund over the limit is approved by a supervisor.";

fn id(node: &str) -> String {
    citation_id(&FactKey::Node { id: node.into() })
}

fn contribution(engineer: bool, verb: &str) -> AttestedContribution {
    AttestedContribution {
        contributor_id: if engineer {
            "C-00000000000000e1".into()
        } else {
            "C-00000000000000a1".into()
        },
        // Placeholder contributors only (ADR-0090 decision 4).
        display_name: engineer.then(|| "Placeholder Engineer".into()),
        engineer,
        verb: verb.into(),
        role: (!engineer).then(|| "gap-filler@1".into()),
        run: (!engineer).then(|| "run-7".into()),
    }
}

/// An attested record for §1.5 resting on `rule:limit` as the graph holds it.
fn record(standing: AttestedStanding) -> ReviewedAttested {
    let supporting = BTreeSet::from([id("rule:limit")]);
    ReviewedAttested {
        record_id: RECORD_ID.into(),
        citation_id: ATTESTED.into(),
        answer: AttestedAnswer::Section {
            catalogue_version: CATALOGUE_VERSION.into(),
            document: "D1".into(),
            section: "1.5".into(),
            scope: "system".into(),
        },
        statement: STATEMENT.into(),
        recorded_hashes: cited_fact_hashes(&nodes(), &[], &supporting),
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

/// An accepted §1.5 draft whose claim cites `rule:limit` and the record.
fn draft() -> ReviewedDraft {
    let facts = BTreeSet::from([id("rule:limit")]);
    let mut cited_hashes = cited_fact_hashes(&nodes(), &[], &facts);
    cited_hashes.insert(ATTESTED.into(), RECORD_ID.into());
    ReviewedDraft {
        draft_id: "draft-a".into(),
        address: DraftAddress {
            catalogue_version: CATALOGUE_VERSION.into(),
            document: "D1".into(),
            section: "1.5".into(),
            scope: "system".into(),
        },
        role: "section-writer@1".into(),
        claims: vec![DraftClaim {
            statement: "Refunds over the limit need a supervisor.".into(),
            kind: "implemented".into(),
            citations: vec![id("rule:limit"), ATTESTED.into()],
        }],
        cited_hashes,
        replaces: None,
        decision: Some(DraftReview::Accepted),
    }
}

fn projection(standing: AttestedStanding) -> CuratedProjection {
    let facts = BTreeSet::from([id("rule:limit")]);
    project(&ProjectionInput {
        nodes: &nodes(),
        edges: &[],
        graph_content_hash: "graph-hash-1",
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[draft()],
        cited_fact_hashes: &cited_fact_hashes(&nodes(), &[], &facts),
        findings: &[],
        cited_fact_digests: &BTreeMap::new(),
        attested: &[record(standing)],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap()
}

fn compile(mode: ExportMode, projection: &CuratedProjection) -> SpecBundle {
    try_compile_spec_supplied(
        &nodes(),
        &[],
        &flows(),
        mode,
        &BTreeSet::new(),
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

fn artifact<'a>(bundle: &'a SpecBundle, artifact_id: &str) -> &'a str {
    &bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == artifact_id)
        .unwrap()
        .content
}

fn index(bundle: &SpecBundle) -> FactIndex {
    serde_json::from_str(artifact(bundle, "fact-index-json")).unwrap()
}

#[test]
fn best_effort_prints_an_attested_claim_with_its_label_and_contributors() {
    let projection = projection(AttestedStanding::EngineerBacked);
    assert_eq!(projection.draft_counts.eligible, 1);
    let bundle = compile(ExportMode::BestEffort, &projection);
    let content = &d1(&bundle).content;
    let rules = section(content, "## 1.5 ");
    assert!(rules.contains(&format!(
        "1. Refunds over the limit need a supervisor. (implemented; T3 / InferredWeak; role \
         `section-writer@1`; human-reviewed: accepted; cites {}, {ATTESTED}; {ATTESTED} is \
         attested: stated by gap-filler@1 (run run-7), confirmed by Placeholder Engineer)\n",
        id("rule:limit")
    )));
    assert!(content.contains("Of the drafted claims, 1 cite an attested record"));

    // The record has its own row in the index, apart from the facts; every
    // printed id resolves there.
    let index = index(&bundle);
    let [row] = index.attested.as_slice() else {
        panic!("one attested row");
    };
    assert_eq!(
        (
            row.citation_id.as_str(),
            row.label.as_str(),
            row.statement.as_str()
        ),
        (ATTESTED, "attested", STATEMENT)
    );
    assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));
    assert!(index.rows.iter().all(|row| row.citation_id != ATTESTED));
    check_citations(
        &index,
        printed_citations(content).into_iter().chain([ATTESTED]),
    )
    .unwrap();
    let markdown = artifact(&bundle, "fact-index");
    assert!(markdown.contains("## Attested records"));
    assert!(markdown.contains(&format!("| {ATTESTED} | attested | {STATEMENT} |")));
}

#[test]
fn an_agent_only_record_is_printed_with_its_own_label_never_as_engineer_backed() {
    let projection = projection(AttestedStanding::AgentAttestedUnconfirmed);
    let bundle = compile(ExportMode::BestEffort, &projection);
    let content = &d1(&bundle).content;
    assert!(content.contains(&format!(
        "{ATTESTED} is agent-attested, unconfirmed: stated by gap-filler@1 (run run-7))\n"
    )));
    assert!(!content.contains("Placeholder Engineer"));
    assert_eq!(
        index(&bundle).attested[0].label,
        "agent-attested, unconfirmed"
    );
}

#[test]
fn verified_only_prints_no_attested_claim_and_counts_it_in_its_section() {
    for standing in [
        AttestedStanding::EngineerBacked,
        AttestedStanding::AgentAttestedUnconfirmed,
    ] {
        let bundle = compile(ExportMode::VerifiedOnly, &projection(standing));
        let content = &d1(&bundle).content;
        assert!(!content.contains(ATTESTED));
        assert!(!content.contains("Refunds over the limit"));
        assert!(section(content, "## 1.5 ").contains(
            "Drafted claims left out of this `verified-only` export: 1. Of these, 1 cite an \
             attested record.\n"
        ));
        let index = index(&bundle);
        assert!(index.attested.is_empty());
        assert!(!artifact(&bundle, "fact-index").contains("Attested records"));
    }
}

#[test]
fn a_draft_citing_a_withdrawn_record_is_not_printed_and_its_id_is_not_indexed() {
    let projection = projection(AttestedStanding::Withdrawn);
    assert_eq!(projection.draft_counts.unresolvable, 1);
    let bundle = compile(ExportMode::BestEffort, &projection);
    let content = &d1(&bundle).content;
    assert!(!content.contains("Refunds over the limit"));
    assert!(!content.contains(ATTESTED));
    assert!(index(&bundle).attested.is_empty());
}

#[test]
fn the_index_refuses_an_attested_citation_it_cannot_print() {
    let row = |label: &str| crate::AttestedIndexRow {
        citation_id: ATTESTED.into(),
        label: label.into(),
        statement: STATEMENT.into(),
        contributors: "stated by gap-filler@1 (run run-7)".into(),
        cited_by: Vec::new(),
    };
    // verified-only never prints one, so a citation there fails the export.
    let mut verified =
        super::super::fact_index::Collector::new(ExportMode::VerifiedOnly, citation_id);
    assert_eq!(
        verified.cite_attested(D1_ARTIFACT_ID, &[row("attested")]),
        Err(FactIndexError::Withheld {
            citation_id: ATTESTED.into()
        })
    );
    let mut best = super::super::fact_index::Collector::new(ExportMode::BestEffort, citation_id);
    assert_eq!(
        best.cite_attested(D1_ARTIFACT_ID, &[row("withdrawn")]),
        Err(FactIndexError::Unresolved {
            citation_id: ATTESTED.into()
        })
    );
    best.cite_attested(D1_ARTIFACT_ID, &[row("attested")])
        .unwrap();
    // One id cited with two different records is refused, not merged.
    let mut other = row("attested");
    other.statement = "Another statement.".into();
    assert!(best.cite_attested("curated", &[other]).is_err());
    let index = best.finish().unwrap();
    assert!(check_citations(&index, [ATTESTED]).is_ok());
    assert!(check_citations(&index, ["A-00000000000000b2"]).is_err());
}
