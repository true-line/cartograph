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
        findings: &[],
        cited_fact_digests: &std::collections::BTreeMap::new(),
        attested: &[],
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
        // AC-0672: a stale draft is the only one of these a best-effort
        // export prints; verified-only prints no claim.
        for statement in ["Gone.", "One.", "Two."] {
            assert!(!content.contains(statement), "{statement}");
        }
        assert_eq!(
            content.contains("Stale."),
            mode == ExportMode::BestEffort,
            "{mode:?}"
        );
        // The header names the stale and the unresolvable address.
        assert!(content.contains(
            "- Stale sections: 1.1 (scope `system`, `stale`), 1.2 (scope `system`, \
             `unresolvable`)\n"
        ));
        // An unresolvable draft is not printed and its ids are not: every
        // id this document prints resolves in the fact index.
        let actors = section(content, "## 1.2 ");
        assert!(actors.contains("_Not drafted._ No eligible draft is recorded"));
        assert!(actors.contains(
            "- Draft `draft-u` for scope `system` is listed as `unresolvable` \
             (`cited_fact_missing`) and is not printed: 1 of the facts it cites is no longer \
             in the graph. `curated.md` names them.\n"
        ));
        assert!(!content.contains("F-0000000000000000"));
        let capabilities = section(content, "## 1.3 ");
        for draft in ["draft-c1", "draft-c2"] {
            assert!(capabilities.contains(&format!(
                "- Draft `{draft}` for scope `system` is listed as `conflicted` \
                 (`another_draft_accepted`).\n"
            )));
        }
        check_citations(&index(&bundle), printed_citations(content)).unwrap();
    }
}

/// AC-0672 (T-0672): a stale accepted draft stays in place in a
/// `best-effort` export, marked, with the changed ids; `verified-only`
/// prints no claim and counts it.
#[test]
fn a_stale_draft_is_printed_in_place_and_marked() {
    let checkout = id("cap:checkout");
    let admin = id("actor:admin");
    let mut stale = draft(
        "draft-s",
        "1.1",
        "Checkout exists.",
        std::slice::from_ref(&checkout),
    );
    stale.claims.push(DraftClaim {
        statement: "An administrator exists.".into(),
        kind: "implemented".into(),
        citations: vec![admin.clone()],
    });
    stale.cited_hashes = cited_fact_hashes(
        &nodes(),
        &[edge()],
        &BTreeSet::from([checkout.clone(), admin.clone()]),
    );
    stale
        .cited_hashes
        .insert(checkout.clone(), "another-hash".into());
    let projection = projection(&[stale], &nodes());
    assert_eq!(projection.draft_counts.eligible, 0);

    let bundle = compile(ExportMode::BestEffort, &projection, &BTreeSet::new());
    let document = d1(&bundle);
    let content = &document.content;
    assert!(content.contains("- Stale sections: 1.1 (scope `system`, `stale`)\n"));
    let purpose = section(content, "## 1.1 ");
    assert!(!purpose.contains("_Not drafted._"));
    assert!(purpose.contains(&format!(
        "Drafted for scope `system` (draft `draft-s`):\n\n\
         Stale: cited facts changed since this text was accepted: {checkout}.\n\n\
         1. Checkout exists. (implemented; T3 / InferredWeak; role `section-writer@1`; \
         human-reviewed: accepted; cites {checkout}; cites a changed fact)\n\
         2. An administrator exists. (implemented; T3 / InferredWeak; role \
         `section-writer@1`; human-reviewed: accepted; cites {admin})\n"
    )));
    assert!(content.contains("Drafted claims printed: 2."));
    check_citations(&index(&bundle), printed_citations(content)).unwrap();

    let bundle = compile(ExportMode::VerifiedOnly, &projection, &BTreeSet::new());
    let content = &d1(&bundle).content;
    assert!(content.contains("- Stale sections: 1.1 (scope `system`, `stale`)\n"));
    let purpose = section(content, "## 1.1 ");
    assert!(!purpose.contains("Checkout exists."));
    assert!(!purpose.contains("Stale: cited facts changed"));
    assert!(purpose.contains(
        "- Draft `draft-s` for scope `system` is listed as `stale` (`cited_fact_changed`).\n"
    ));
    assert!(purpose.contains("Drafted claims left out of this `verified-only` export: 2."));
    check_citations(&index(&bundle), printed_citations(content)).unwrap();
}

/// AC-0672 (T-0672): one changed fact marks exactly the accepted drafts
/// that cite it, and a second run over the same inputs is byte-identical.
#[test]
fn one_changed_fact_marks_exactly_the_citing_drafts_twice_the_same() {
    let checkout = id("cap:checkout");
    let drafts = [
        draft(
            "draft-a",
            "1.1",
            "Purpose.",
            std::slice::from_ref(&checkout),
        ),
        draft("draft-b", "1.2", "Actors.", &[id("actor:admin")]),
        draft(
            "draft-c",
            "1.3",
            "Capabilities.",
            &[checkout.clone(), id("actor:admin")],
        ),
        draft("draft-d", "1.5", "Rules.", &[id("rule:limit")]),
    ];
    // Before the change nothing is stale.
    let before = projection(&drafts, &nodes());
    assert_eq!(before.draft_counts.eligible, 4);
    assert!(
        d1(&compile(ExportMode::BestEffort, &before, &BTreeSet::new()))
            .content
            .contains("- Stale sections: none\n")
    );

    // The handler behind the capability changes: its content hash moves.
    let mut changed = nodes();
    let node = changed
        .iter_mut()
        .find(|node| node.id == "cap:checkout")
        .unwrap();
    let recorded = node.props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let moved: String = recorded.chars().rev().collect();
    assert_ne!(moved, recorded);
    node.props["prov"]["content_hash"] = serde_json::json!(moved);
    let run = || {
        let projection = projection(&drafts, &changed);
        let stale: Vec<(String, Vec<String>)> = projection
            .drafts
            .iter()
            .filter(|draft| draft.state == DraftState::Stale)
            .map(|draft| (draft.draft_id.clone(), draft.changed.clone()))
            .collect();
        let documents: Vec<String> = MODES
            .iter()
            .map(|mode| {
                let bundle = try_compile_spec_supplied(
                    &changed,
                    &[edge()],
                    &flows(),
                    *mode,
                    &BTreeSet::new(),
                    Supplied {
                        repo_names: &[],
                        knowledge: None,
                        coverage: &[],
                        stale_coverage: &[],
                        curated: Curated::Computed(&projection),
                        graph_content_hash: None,
                        source_revisions: None,
                    },
                )
                .unwrap();
                d1(&bundle).content.clone()
            })
            .collect();
        (stale, documents)
    };
    let (stale, documents) = run();
    assert_eq!(
        stale,
        vec![
            ("draft-a".to_owned(), vec![checkout.clone()]),
            ("draft-c".to_owned(), vec![checkout.clone()]),
        ]
    );
    for document in &documents {
        assert!(document.contains(
            "- Stale sections: 1.1 (scope `system`, `stale`), 1.3 (scope `system`, `stale`)\n"
        ));
    }
    // The untouched drafts print as before, unmarked.
    let best_effort = &documents[1];
    assert!(!section(best_effort, "## 1.2 ").contains("Stale:"));
    assert!(!section(best_effort, "## 1.5 ").contains("Stale:"));
    assert!(section(best_effort, "## 1.3 ").contains("cites a changed fact"));
    assert_eq!(run(), (stale, documents));
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

// AC-0844: the digest a finding's citations are compared with.
#[test]
fn cited_fact_digests_reads_only_the_cited_facts_and_follows_every_field() {
    let edge_id = citation_id(&edge_key(&edge()));
    let cited = BTreeSet::from([
        id("cap:checkout"),
        edge_id.clone(),
        "F-0000000000000000".to_owned(),
    ]);
    let digests = crate::cited_fact_digests(&nodes(), &[edge()], &cited);
    assert_eq!(digests.len(), 2);
    // The digest an investigation records for a fact it reads.
    assert_eq!(
        digests[&id("cap:checkout")],
        core_graph::source::node_digest(&nodes()[0]).unwrap()
    );
    assert_eq!(
        digests[&edge_id],
        core_graph::source::edge_digest(&edge()).unwrap()
    );
    assert!(crate::cited_fact_digests(&nodes(), &[edge()], &BTreeSet::new()).is_empty());

    // Any field of the fact moves it, not only its provenance.
    let mut changed = nodes();
    changed[0].props["note"] = serde_json::json!("changed");
    let moved = crate::cited_fact_digests(&changed, &[edge()], &cited);
    assert_ne!(moved[&id("cap:checkout")], digests[&id("cap:checkout")]);
    assert_eq!(moved[&edge_id], digests[&edge_id]);
}

// AC-0668 (T-0668): only a drafted section of this catalogue version can be
// the target of a drafting run.
#[test]
fn only_a_drafted_section_of_this_catalogue_version_is_addressable() {
    for number in ["1.1", "1.2", "1.3", "1.4", "1.5"] {
        assert_eq!(
            drafted_section(CATALOGUE_VERSION, "D1", number).map(|section| section.number),
            Ok(number)
        );
    }
    for (version, document, number) in [
        (CATALOGUE_VERSION, "D1", "1.6"),
        (CATALOGUE_VERSION, "D1", "1.7"),
        (CATALOGUE_VERSION, "D1", "1.8"),
        (CATALOGUE_VERSION, "D1", "9.9"),
        (CATALOGUE_VERSION, "D2", "2.1"),
        ("0.9", "D1", "1.3"),
    ] {
        let refused = drafted_section(version, document, number).unwrap_err();
        assert_eq!(refused, SectionScopeError::NotDrafted);
        assert_eq!(refused.code(), "section_not_drafted");
    }
}

// AC-0668 (T-0668): the citation set is the facts of the declared scope, by
// citation id with the current content hash.
#[test]
fn the_citation_set_is_the_scope_by_citation_id_with_current_hashes() {
    let nodes = nodes();
    let edges = [edge()];
    let edge_id = citation_id(&edge_key(&edges[0]));
    let all: BTreeSet<String> = [
        id("cap:checkout"),
        id("actor:admin"),
        id("rule:limit"),
        edge_id.clone(),
        id("not:there"),
    ]
    .into();
    let capabilities = drafted_section(CATALOGUE_VERSION, "D1", "1.3").unwrap();

    // The system scope admits every fact of the graph and nothing else.
    let system = section_citation_set(capabilities, SYSTEM_SCOPE, &nodes, &edges, &all).unwrap();
    assert_eq!(system, cited_fact_hashes(&nodes, &edges, &all));
    assert_eq!(system.len(), 4);
    assert!(!system.contains_key(&id("not:there")));

    // A subject scope admits the subject, the edges that touch it and the
    // nodes at their other ends; a fact elsewhere is not admitted.
    let subject =
        section_citation_set(capabilities, &id("cap:checkout"), &nodes, &edges, &all).unwrap();
    let admitted: BTreeSet<&str> = subject.keys().map(String::as_str).collect();
    let expected = [id("cap:checkout"), id("actor:admin"), edge_id];
    assert_eq!(
        admitted,
        expected.iter().map(String::as_str).collect::<BTreeSet<_>>()
    );
    for (fact, hash) in &subject {
        assert_eq!(system.get(fact), Some(hash));
    }

    // Only the ids asked for are hashed.
    let one: BTreeSet<String> = [id("actor:admin")].into();
    let only = section_citation_set(capabilities, SYSTEM_SCOPE, &nodes, &edges, &one).unwrap();
    assert_eq!(only.keys().collect::<Vec<_>>(), [&id("actor:admin")]);

    // The subject must be a node of the section's subject label.
    for scope in [
        id("actor:admin"),
        id("not:there"),
        "F-0000000000000000".into(),
    ] {
        let refused = section_citation_set(capabilities, &scope, &nodes, &edges, &all).unwrap_err();
        assert_eq!(refused, SectionScopeError::SubjectNotFound);
        assert_eq!(refused.code(), "subject_not_found");
    }
    // A section written once has the system scope only.
    let purpose = drafted_section(CATALOGUE_VERSION, "D1", "1.1").unwrap();
    let refused =
        section_citation_set(purpose, &id("cap:checkout"), &nodes, &edges, &all).unwrap_err();
    assert_eq!(refused, SectionScopeError::NotPerSubject);
    assert_eq!(refused.code(), "section_not_per_subject");
}

// AC-0671 (ADR-0045 decisions 2 and 3): `curated.md` names, for each
// accepted draft, the cited facts that changed and the ids that are gone,
// in both modes, and repeats no claim. With no accepted draft the artifact
// keeps its bytes.
#[test]
fn curated_names_the_changed_and_missing_facts_of_each_accepted_draft() {
    fn curated(bundle: &SpecBundle) -> &str {
        &bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "curated")
            .unwrap()
            .content
    }
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
        draft("draft-e", "1.3", "Current.", &[id("rule:limit")]),
    ];
    let empty = projection(&[], &nodes());
    let projection = projection(&drafts, &nodes());
    for mode in MODES {
        let bundle = compile(mode, &projection, &BTreeSet::new());
        let content = curated(&bundle);
        let rows = section(content, "## Accepted section drafts");
        assert!(rows.contains(&format!(
            "| draft-s | D1 | 1.1 | system | stale | `cited_fact_changed` | {} | — |\n",
            id("cap:checkout")
        )));
        assert!(rows.contains(
            "| draft-u | D1 | 1.2 | system | unresolvable | `cited_fact_missing` | — | \
             F-0000000000000000 |\n"
        ));
        assert!(rows.contains(
            "| draft-e | D1 | 1.3 | system | eligible | `all_conditions_hold` | — | — |\n"
        ));
        for statement in ["Stale.", "Gone.", "Current."] {
            assert!(!content.contains(statement), "{statement}");
        }
        // The same inputs give the same bytes.
        assert_eq!(
            content,
            curated(&compile(mode, &projection, &BTreeSet::new()))
        );
    }
    let none = compile(ExportMode::BestEffort, &empty, &BTreeSet::new());
    assert!(!curated(&none).contains("Accepted section drafts"));
}

// AC-1011 (T-1011): a draft citing a derived Capability the export prints
// is printed, and the fact index lists that Capability. Without the
// knowledge layer the same draft is named as citing a fact the export does
// not list.
#[test]
fn a_draft_citing_a_derived_capability_resolves_in_the_fact_index() {
    use crate::workbench::capabilities::{KnowledgeStories, Stories, Story};
    let mut refunds = node(
        "capability:refunds",
        "Capability",
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
    );
    refunds.props["title"] = "Refunds".into();
    let cited = vec![id("capability:refunds"), id("rule:limit")];
    let ids: BTreeSet<String> = cited.iter().cloned().collect();
    let mut hashes = cited_fact_hashes(&nodes(), &[edge()], &ids);
    hashes.extend(crate::cited_node_hashes([&refunds], &ids));
    let mut accepted = draft("draft-k", "1.3", "Refunds are limited per order.", &cited);
    accepted.cited_hashes = hashes.clone();
    let projection = project(&ProjectionInput {
        nodes: &nodes(),
        edges: &[],
        graph_content_hash: "graph-hash-1",
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: std::slice::from_ref(&accepted),
        cited_fact_hashes: &hashes,
        findings: &[],
        cited_fact_digests: &BTreeMap::new(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap();
    assert_eq!(projection.draft_counts.eligible, 1);
    let knowledge = KnowledgeStories(Stories {
        included: vec![Story {
            node: &refunds,
            support: Vec::new(),
        }],
        ..Stories::default()
    });
    let compiled = |knowledge| {
        try_compile_spec_supplied(
            &nodes(),
            &[edge()],
            &flows(),
            ExportMode::BestEffort,
            &BTreeSet::new(),
            Supplied {
                repo_names: &[],
                knowledge,
                coverage: &[],
                stale_coverage: &[],
                curated: Curated::Computed(&projection),
                graph_content_hash: None,
                source_revisions: None,
            },
        )
        .unwrap()
    };

    let bundle = compiled(Some(&knowledge));
    let content = &d1(&bundle).content;
    assert!(content.contains("1. Refunds are limited per order. (implemented;"));
    let listed = index(&bundle);
    check_citations(&listed, printed_citations(content)).unwrap();
    let row = listed
        .rows
        .iter()
        .find(|row| row.citation_id == id("capability:refunds"))
        .unwrap();
    assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));

    let bundle = compiled(None);
    let content = &d1(&bundle).content;
    assert!(!content.contains("Refunds are limited per order."));
    assert!(content.contains(
        "- Draft `draft-k` for scope `system` is eligible and is not printed: it cites a fact \
         this export does not list.\n"
    ));
    check_citations(&index(&bundle), printed_citations(content)).unwrap();
}

// AC-1602 (T-1602): a draft citing an area this export prints, a concept
// Subsystem the arrangement kept, is printed in the best-effort document
// too, and the fact index lists that area. Without the area the same draft
// is named as citing a fact the export does not list (#2511, #1748).
#[test]
fn a_draft_citing_a_printed_area_is_printed() {
    use crate::workbench::capabilities::{KnowledgeStories, Stories, Story};
    let mut refunds = node(
        "capability:refunds",
        "Capability",
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
    );
    refunds.props["title"] = "Refunds".into();
    let mut billing = node(
        "subsystem:billing",
        "Subsystem",
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
    );
    billing.props["name"] = "Billing".into();
    billing.props["kind"] = "concept".into();
    let cited = vec![id("subsystem:billing"), id("rule:limit")];
    let ids: BTreeSet<String> = cited.iter().cloned().collect();
    let mut hashes = cited_fact_hashes(&nodes(), &[edge()], &ids);
    hashes.extend(crate::cited_node_hashes([&billing], &ids));
    let mut accepted = draft(
        "draft-a",
        "1.3",
        "Billing limits refunds per order.",
        &cited,
    );
    accepted.cited_hashes = hashes.clone();
    let projection = project(&ProjectionInput {
        nodes: &nodes(),
        edges: &[],
        graph_content_hash: "graph-hash-1",
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: std::slice::from_ref(&accepted),
        cited_fact_hashes: &hashes,
        findings: &[],
        cited_fact_digests: &BTreeMap::new(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap();
    assert_eq!(projection.draft_counts.eligible, 1);
    let story = || Story {
        node: &refunds,
        support: Vec::new(),
    };
    let arranged = KnowledgeStories(Stories {
        included: vec![story()],
        areas: BTreeMap::from([(refunds.id.as_str(), &billing)]),
        ..Stories::default()
    });
    let unarranged = KnowledgeStories(Stories {
        included: vec![story()],
        ..Stories::default()
    });
    let compiled = |knowledge| {
        try_compile_spec_supplied(
            &nodes(),
            &[edge()],
            &flows(),
            ExportMode::BestEffort,
            &BTreeSet::new(),
            Supplied {
                repo_names: &[],
                knowledge,
                coverage: &[],
                stale_coverage: &[],
                curated: Curated::Computed(&projection),
                graph_content_hash: None,
                source_revisions: None,
            },
        )
        .unwrap()
    };

    let bundle = compiled(Some(&arranged));
    let content = &d1(&bundle).content;
    assert!(content.contains("1. Billing limits refunds per order. (implemented;"));
    let listed = index(&bundle);
    check_citations(&listed, printed_citations(content)).unwrap();
    let row = listed
        .rows
        .iter()
        .find(|row| row.citation_id == id("subsystem:billing"))
        .unwrap();
    assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));

    let bundle = compiled(Some(&unarranged));
    let content = &d1(&bundle).content;
    assert!(!content.contains("Billing limits refunds per order."));
    assert!(content.contains(
        "- Draft `draft-a` for scope `system` is eligible and is not printed: it cites a fact \
         this export does not list.\n"
    ));
    check_citations(&index(&bundle), printed_citations(content)).unwrap();
}
