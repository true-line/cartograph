//! AC-0667 (T-0667): accepted section drafts in `product-specification.md`.
//! An eligible draft is printed in `best-effort` with its annotations and
//! citations that resolve in the fact index; `verified-only` prints none and
//! counts them; any other state is named with its reason.

use super::tests::{d1, flows, node, nodes, printed_citations, section};
use super::*;
use crate::{
    FactIndex, SpecBundle, Supplied, check_citations, cited_fact_hashes, try_compile_spec_supplied,
};
use context_hub::curated::{
    CURATED_POLICY_VERSION, CuratedProjection, DraftAddress, DraftClaim, DraftReview,
    ProjectionInput, ReviewedDraft, project,
};
use core_prov::Tier;
use std::collections::{BTreeMap, BTreeSet};

const MODES: [ExportMode; 2] = [ExportMode::VerifiedOnly, ExportMode::BestEffort];

fn id(node: &str) -> String {
    citation_id(&FactKey::Node { id: node.into() })
}

fn edge() -> Edge {
    let prov = node(
        "edge",
        "Edge",
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    )
    .props["prov"]
        .clone();
    Edge {
        src: "actor:admin".into(),
        dst: "cap:checkout".into(),
        label: "PERFORMS".into(),
        props: serde_json::json!({ "prov": prov }),
    }
}

/// An accepted draft whose one claim cites `cited` as the graph holds them.
fn draft(draft_id: &str, number: &str, statement: &str, cited: &[String]) -> ReviewedDraft {
    let ids: BTreeSet<String> = cited.iter().cloned().collect();
    ReviewedDraft {
        draft_id: draft_id.into(),
        address: DraftAddress {
            catalogue_version: CATALOGUE_VERSION.into(),
            document: "D1".into(),
            section: number.into(),
            scope: "system".into(),
        },
        role: "section-writer@1".into(),
        claims: vec![DraftClaim {
            statement: statement.into(),
            kind: "implemented".into(),
            citations: cited.to_vec(),
        }],
        cited_hashes: cited_fact_hashes(&nodes(), &[edge()], &ids),
        replaces: None,
        decision: Some(DraftReview::Accepted),
    }
}

fn projection(drafts: &[ReviewedDraft], nodes: &[Node]) -> CuratedProjection {
    let cited: BTreeSet<String> = drafts
        .iter()
        .flat_map(|draft| draft.cited_hashes.keys().cloned())
        .collect();
    project(&ProjectionInput {
        nodes,
        edges: &[],
        graph_content_hash: "graph-hash-1",
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts,
        cited_fact_hashes: &cited_fact_hashes(nodes, &[edge()], &cited),
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap()
}

fn compile(
    mode: ExportMode,
    projection: &CuratedProjection,
    rejected: &BTreeSet<String>,
) -> SpecBundle {
    try_compile_spec_supplied(
        &nodes(),
        &[edge()],
        &flows(),
        mode,
        rejected,
        Supplied {
            coverage: &[],
            stale_coverage: &[],
            curated: Curated::Computed(projection),
            graph_content_hash: None,
        },
    )
    .unwrap()
}

fn index(bundle: &SpecBundle) -> FactIndex {
    serde_json::from_str(
        &bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "fact-index-json")
            .unwrap()
            .content,
    )
    .unwrap()
}

#[test]
fn an_eligible_draft_is_printed_in_best_effort_with_its_annotations() {
    let drafts = [
        draft(
            "draft-a",
            "1.1",
            "Admins <b>check out</b> orders.",
            &[id("cap:checkout"), id("rule:limit")],
        ),
        draft(
            "draft-b",
            "1.3",
            "Checkout is limited.",
            &[id("rule:limit")],
        ),
    ];
    let projection = projection(&drafts, &nodes());
    assert_eq!(projection.draft_counts.eligible, 2);
    let bundle = compile(ExportMode::BestEffort, &projection, &BTreeSet::new());
    let document = d1(&bundle);
    let content = &document.content;

    let purpose = section(content, "## 1.1 ");
    assert!(purpose.contains("Drafted for scope `system` (draft `draft-a`):"));
    assert!(purpose.contains(&format!(
        "1. Admins &lt;b&gt;check out&lt;/b&gt; orders. (implemented; T3 / InferredWeak; role \
         `section-writer@1`; human-reviewed: accepted; cites {}, {})\n",
        id("cap:checkout"),
        id("rule:limit")
    )));
    assert!(!purpose.contains("Not drafted"));
    assert!(!content.contains("<b>"));
    // The frame of a P+A section stays, and the draft follows it.
    let capabilities = section(content, "## 1.3 ");
    let frame = capabilities.find("| Name | Tier |").unwrap();
    assert!(capabilities.find("Drafted for scope").unwrap() > frame);
    assert!(capabilities.contains("1. Checkout is limited. (implemented;"));
    // A section with no draft says so, as before.
    assert!(section(content, "## 1.2 ").contains("_Not drafted._ No accepted draft"));
    assert!(
        content.contains("Cited facts printed in the frames above: 3. Drafted claims printed: 2.")
    );

    // Every printed id resolves, and each cited fact is asserted once: the
    // three framed facts and the rule the drafts cite.
    let index = index(&bundle);
    check_citations(&index, printed_citations(content)).unwrap();
    assert_eq!(document.assertions.len(), 4);
    let row = index
        .rows
        .iter()
        .find(|row| row.citation_id == id("rule:limit"))
        .unwrap();
    assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));
}

#[test]
fn verified_only_prints_no_claim_and_counts_what_it_left_out() {
    let drafts = [draft(
        "draft-a",
        "1.1",
        "Admins check out orders.",
        &[id("cap:checkout"), id("rule:limit")],
    )];
    let projection = projection(&drafts, &nodes());
    let bundle = compile(ExportMode::VerifiedOnly, &projection, &BTreeSet::new());
    let document = d1(&bundle);
    let content = &document.content;
    assert!(!content.contains("Admins check out orders."));
    assert!(!content.contains("draft-a"));
    assert!(!content.contains(&id("rule:limit")));
    assert!(
        section(content, "## 1.1 ")
            .contains("Drafted claims left out of this `verified-only` export: 1.\n")
    );
    assert!(
        section(content, "## 1.2 ")
            .contains("Drafted claims left out of this `verified-only` export: 0.\n")
    );
    assert!(content.contains("Drafted claims printed: 0."));
    // Only the frames are cited.
    assert_eq!(document.assertions.len(), 3);
}

#[test]
fn a_draft_in_any_other_state_is_named_with_its_reason_in_both_modes() {
    let mut stale = draft("draft-s", "1.1", "Stale.", &[id("cap:checkout")]);
    stale
        .cited_hashes
        .insert(id("cap:checkout"), "another-hash".into());
    let mut gone = draft("draft-u", "1.2", "Gone.", &[id("actor:admin")]);
    gone.claims[0].citations = vec!["F-0000000000000000".into()];
    gone.cited_hashes = BTreeMap::from([("F-0000000000000000".to_owned(), "hash".to_owned())]);
    let drafts = [
        stale,
        gone,
        draft("draft-c1", "1.3", "One.", &[id("cap:checkout")]),
        draft("draft-c2", "1.3", "Two.", &[id("cap:checkout")]),
    ];
    let projection = projection(&drafts, &nodes());
    assert_eq!(projection.draft_counts.eligible, 0);
    for mode in MODES {
        let bundle = compile(mode, &projection, &BTreeSet::new());
        let content = &d1(&bundle).content;
        for statement in ["Stale.", "Gone.", "One.", "Two."] {
            assert!(!content.contains(statement), "{statement}");
        }
        let purpose = section(content, "## 1.1 ");
        assert!(purpose.contains("_Not drafted._ No eligible draft is recorded"));
        assert!(purpose.contains(
            "- Draft `draft-s` for scope `system` is listed as `stale` (`cited_fact_changed`).\n"
        ));
        assert!(section(content, "## 1.2 ").contains(
            "- Draft `draft-u` for scope `system` is listed as `unresolvable` \
             (`cited_fact_missing`).\n"
        ));
        let capabilities = section(content, "## 1.3 ");
        for draft in ["draft-c1", "draft-c2"] {
            assert!(capabilities.contains(&format!(
                "- Draft `{draft}` for scope `system` is listed as `conflicted` \
                 (`another_draft_accepted`).\n"
            )));
        }
        assert!(content.contains("Drafted claims printed: 0."));
        check_citations(&index(&bundle), printed_citations(content)).unwrap();
    }
}

#[test]
fn a_draft_citing_a_fact_the_export_does_not_admit_is_not_printed() {
    let refund = id("cap:<b>refund</b>");
    let drafts = [draft(
        "draft-r",
        "1.1",
        "Refunds exist.",
        std::slice::from_ref(&refund),
    )];
    let projection = projection(&drafts, &nodes());
    assert_eq!(projection.draft_counts.eligible, 1);
    // The reviewer rejected the inference the draft cites.
    let rejected: BTreeSet<String> =
        cited_fact_hashes(&nodes(), &[], &BTreeSet::from([refund.clone()]))
            .into_values()
            .collect();
    let bundle = compile(ExportMode::BestEffort, &projection, &rejected);
    let content = &d1(&bundle).content;
    assert!(!content.contains("Refunds exist."));
    assert!(!content.contains(&refund));
    let purpose = section(content, "## 1.1 ");
    assert!(purpose.contains("_Not drafted._ No eligible draft is recorded"));
    assert!(purpose.contains(
        "- Draft `draft-r` for scope `system` is eligible and is not printed: it cites a fact \
         this export does not list.\n"
    ));
    check_citations(&index(&bundle), printed_citations(content)).unwrap();
}

#[test]
fn a_cited_edge_resolves_and_scopes_print_in_order() {
    let edge_id = citation_id(&edge_key(&edge()));
    let mut scoped = draft(
        "draft-1",
        "1.3",
        "Admins perform checkout.",
        std::slice::from_ref(&edge_id),
    );
    scoped.address.scope = id("cap:checkout");
    let drafts = [
        scoped,
        draft("draft-2", "1.3", "The system sells.", &[id("cap:checkout")]),
        // Another document or catalogue version is not this document's.
        ReviewedDraft {
            address: DraftAddress {
                catalogue_version: "0.9".into(),
                ..draft("x", "1.1", "x", &[]).address
            },
            ..draft("draft-old", "1.1", "Old catalogue.", &[id("cap:checkout")])
        },
        ReviewedDraft {
            address: DraftAddress {
                document: "D2".into(),
                ..draft("x", "1.1", "x", &[]).address
            },
            ..draft("draft-d2", "1.1", "Other document.", &[id("cap:checkout")])
        },
    ];
    let projection = projection(&drafts, &nodes());
    let bundle = compile(ExportMode::BestEffort, &projection, &BTreeSet::new());
    let document = d1(&bundle);
    let content = &document.content;
    assert!(!content.contains("Old catalogue.") && !content.contains("Other document."));
    let capabilities = section(content, "## 1.3 ");
    // Scope order: the subject's citation id sorts before `system`.
    let first = capabilities.find("Admins perform checkout.").unwrap();
    let second = capabilities.find("The system sells.").unwrap();
    assert!(first < second);
    assert!(capabilities.contains(&format!("cites {edge_id})")));
    let index = index(&bundle);
    check_citations(&index, printed_citations(content)).unwrap();
    let row = index
        .rows
        .iter()
        .find(|row| row.citation_id == edge_id)
        .unwrap();
    assert!(matches!(row.fact, FactKey::Edge { .. }));
    assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));

    // The same inputs give the same bytes.
    let again = compile(ExportMode::BestEffort, &projection, &BTreeSet::new());
    assert_eq!(d1(&again).content, *content);
}

#[test]
fn cited_fact_hashes_reads_only_the_cited_facts() {
    let edge_id = citation_id(&edge_key(&edge()));
    let cited = BTreeSet::from([
        id("cap:checkout"),
        edge_id.clone(),
        "F-0000000000000000".to_owned(),
    ]);
    let hashes = cited_fact_hashes(&nodes(), &[edge()], &cited);
    assert_eq!(hashes.len(), 2);
    assert_eq!(
        hashes[&id("cap:checkout")],
        provenance(&nodes()[0].props, "node:cap:checkout").content_hash
    );
    assert!(hashes.contains_key(&edge_id));
    assert!(cited_fact_hashes(&nodes(), &[edge()], &BTreeSet::new()).is_empty());
}
