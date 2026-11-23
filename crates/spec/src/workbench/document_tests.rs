//! AC-0663 (T-0663): `product-specification.md`. Every catalogue section in
//! order, frames from the admitted facts with citations that resolve, the
//! "not drafted" and blank markers, the header, both export modes, a
//! bounded selected compile, and no change to any other artifact.

use super::*;
use crate::{
    ExportMode, FactIndex, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits,
    SpecArtifact, SpecBundle, Supplied, check_citations, compile_spec, compile_spec_selected,
    try_compile_spec_supplied,
};
use context_hub::curated::{CURATED_POLICY_VERSION, CuratedProjection, ProjectionInput, project};
use core_prov::{EvidenceRef, Provenance, Tier};
use flowtracer::FlowStatus;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const GRAPH_HASH: &str = "graph-hash-1";
const MODES: [ExportMode; 2] = [ExportMode::VerifiedOnly, ExportMode::BestEffort];

pub(super) fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
    let prov = Provenance::new(
        tier,
        confidence,
        vec![EvidenceRef {
            repo: "local/shop".into(),
            path: "src/app.ts".into(),
            byte_start: 1,
            byte_end: 9,
            commit_sha: "abc123".into(),
        }],
        "spec.document.test",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({ "name": id, "prov": prov }),
    }
}

/// Two capabilities (one a T2 inference), one actor, one rule, one Gap and
/// a Gap-tier capability that no frame may list.
pub(super) fn nodes() -> Vec<Node> {
    use ConfidenceTier::{Confirmed, Gap, InferredStrong};
    vec![
        node("cap:checkout", "Capability", Tier::Deterministic, Confirmed),
        node(
            "cap:<b>refund</b>",
            "Capability",
            Tier::Semantic,
            InferredStrong,
        ),
        node("cap:unknown", "Capability", Tier::Deterministic, Gap),
        node("actor:admin", "Actor", Tier::Deterministic, Confirmed),
        node("rule:limit", "BusinessRule", Tier::Deterministic, Confirmed),
        node("gap:1", "Gap", Tier::Deterministic, Gap),
    ]
}

pub(super) fn flows() -> Vec<Flow> {
    vec![Flow {
        trigger: "cap:checkout".into(),
        trigger_kind: "Endpoint".into(),
        trigger_name: "POST /checkout".into(),
        hops: Vec::new(),
        status: FlowStatus::Verified,
        score: 1.0,
        depth_limited: false,
    }]
}

fn projection() -> CuratedProjection {
    project(&ProjectionInput {
        nodes: &nodes(),
        edges: &[],
        graph_content_hash: GRAPH_HASH,
        proposals: &[],
        assessments: &BTreeMap::new(),
        assertion_decisions: &[],
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[],
        cited_fact_hashes: &std::collections::BTreeMap::new(),
        findings: &[],
        cited_fact_digests: &std::collections::BTreeMap::new(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap()
}

fn compile(mode: ExportMode, curated: Option<&CuratedProjection>) -> SpecBundle {
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
            curated: curated.map_or(Curated::NotSupplied, Curated::Computed),
            graph_content_hash: None,
            source_revisions: None,
        },
    )
    .unwrap()
}

pub(super) fn d1(bundle: &SpecBundle) -> &SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == D1_ARTIFACT_ID)
        .unwrap()
}

/// The part of the document under one `## ` heading.
pub(super) fn section<'a>(content: &'a str, heading: &str) -> &'a str {
    let start = content.find(heading).unwrap() + heading.len();
    let rest = &content[start..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

pub(super) fn printed_citations(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| word.len() == 18 && word.starts_with("F-"))
        .collect()
}

#[test]
fn every_catalogue_section_is_printed_in_order_with_its_kind() {
    for mode in MODES {
        let bundle = compile(mode, None);
        let document = d1(&bundle);
        assert_eq!(document.file_name, "product-specification.md");
        let headings: Vec<&str> = document
            .content
            .lines()
            .filter(|line| line.starts_with("## 1."))
            .collect();
        assert_eq!(
            headings,
            [
                "## 1.1 Purpose and scope of the system (A)",
                "## 1.2 Actors and roles (P+A)",
                "## 1.3 Capabilities (P+A)",
                "## 1.4 Acceptance criteria per capability (P+A)",
                "## 1.5 Business rules (P+A)",
                "## 1.6 Business flows (P)",
                "## 1.7 Out of scope and non-goals (H)",
                "## 1.8 Known gaps (P)",
            ],
            "{mode:?}"
        );
    }
}

#[test]
fn drafted_sections_are_marked_not_drafted_and_the_human_section_is_blank() {
    for mode in MODES {
        let bundle = compile(mode, None);
        let content = &d1(&bundle).content;
        for heading in ["## 1.1 ", "## 1.2 ", "## 1.3 ", "## 1.4 ", "## 1.5 "] {
            let text = section(content, heading);
            assert!(text.contains(NOT_DRAFTED), "{heading} in {mode:?}");
            // A verified-only export states how many claims it left out.
            assert_eq!(
                text.contains("Drafted claims left out of this `verified-only` export: 0."),
                mode == ExportMode::VerifiedOnly,
                "{heading} in {mode:?}"
            );
        }
        for heading in ["## 1.6 ", "## 1.7 ", "## 1.8 "] {
            let text = section(content, heading);
            assert!(!text.contains(NOT_DRAFTED), "{heading}");
            assert!(!text.contains("left out"), "{heading}");
        }
        assert!(section(content, "## 1.7 ").contains(HUMAN_BLANK));
        assert!(printed_citations(section(content, "## 1.1 ")).is_empty());
        assert!(printed_citations(section(content, "## 1.7 ")).is_empty());
    }
}

#[test]
fn frames_list_the_admitted_facts_and_their_citations_resolve() {
    for mode in MODES {
        let bundle = compile(mode, None);
        let document = d1(&bundle);
        let content = &document.content;
        let id = |node: &str| citation_id(&FactKey::Node { id: node.into() });

        let actors = section(content, "## 1.2 ");
        assert!(actors.contains(&format!(
            "| actor:admin | Deterministic | Confirmed | {} |",
            id("actor:admin")
        )));
        let capabilities = section(content, "## 1.3 ");
        assert!(capabilities.contains(&id("cap:checkout")));
        // A stored name is literal text, never markup.
        assert!(!capabilities.contains("<b>"));
        assert!(capabilities.contains(&format!(
            "| Semantic | InferredStrong | {} |",
            id("cap:<b>refund</b>")
        )));
        // A Gap-tier node is not a capability statement.
        assert!(!content.contains(&id("cap:unknown")));
        // A rule is rendered only by the rule inventory.
        let rules = section(content, "## 1.5 ");
        assert!(rules.contains("`rule-evidence.md`"));
        assert!(!content.contains(&id("rule:limit")));
        assert!(
            section(content, "## 1.4 ").contains("`rule-evidence.md` and `test_arrangements.md`")
        );
        let flows = section(content, "## 1.6 ");
        assert!(flows.contains("| POST /checkout | Endpoint | "));
        assert!(flows.contains("`flow_dossiers.md`"));
        assert!(section(content, "## 1.8 ").contains("Gap findings in this export: 2."));
        assert!(content.contains("Cited facts printed in the frames above: 3."));
        assert!(content.contains("- Deterministic: 2\n- Semantic: 1\n"));

        // Each printed id is an assertion of this artifact and a row of the index.
        let printed = printed_citations(content);
        assert_eq!(printed.len(), 3);
        assert_eq!(document.assertions.len(), 3);
        let index: FactIndex = serde_json::from_str(
            &bundle
                .artifacts
                .iter()
                .find(|a| a.id == "fact-index-json")
                .unwrap()
                .content,
        )
        .unwrap();
        check_citations(&index, printed.iter().copied()).unwrap();
        // The index names this document among the artifacts citing each
        // fact, which is the one change to an existing file (AC-0663).
        for citation in &printed {
            let row = index
                .rows
                .iter()
                .find(|row| row.citation_id == *citation)
                .unwrap();
            assert!(row.cited_by.iter().any(|by| by == D1_ARTIFACT_ID));
        }
    }
}

#[test]
fn an_empty_graph_states_that_each_frame_is_empty() {
    let bundle = compile_spec(&[], &[], &[], ExportMode::BestEffort, &BTreeSet::new());
    let content = &d1(&bundle).content;
    assert!(section(content, "## 1.2 ").contains("No `Actor` fact is in this export."));
    assert!(section(content, "## 1.3 ").contains("No `Capability` fact is in this export."));
    assert!(section(content, "## 1.6 ").contains("No flow is traced in this export."));
    assert!(section(content, "## 1.8 ").contains("Gap findings in this export: 0."));
    assert!(d1(&bundle).assertions.is_empty());
}

#[test]
fn header_carries_the_catalogue_version_mode_graph_hash_and_curated_revision() {
    let projection = projection();
    for (mode, name) in MODES.into_iter().zip(["verified-only", "best-effort"]) {
        let bare = compile(mode, None);
        let header = section(&d1(&bare).content, "# Product specification");
        assert!(header.contains("- Document: D1 of document catalogue version 1.0\n"));
        assert!(header.contains(&format!("- Export mode: `{name}`\n")));
        assert!(header.contains("- Graph content hash: not supplied to this compilation\n"));
        assert!(header.contains("- Curated revision: not supplied to this compilation\n"));

        let supplied = compile(mode, Some(&projection));
        let header = section(&d1(&supplied).content, "# Product specification");
        assert!(header.contains(&format!("- Graph content hash: {GRAPH_HASH}\n")));
        assert!(header.contains(&format!("- Curated revision: {}\n", projection.revision)));

        // Only this document and the bundle manifest (AC-0682) read the
        // projection's revision; with no accepted record nothing else
        // differs, `curated.md` apart.
        for (before, after) in bare.artifacts.iter().zip(&supplied.artifacts) {
            assert_eq!(
                before == after,
                !matches!(
                    before.id.as_str(),
                    "curated" | D1_ARTIFACT_ID | "bundle-manifest"
                ),
                "{} in {mode:?}",
                before.id
            );
        }
    }
}

/// The host knows the graph hash even when it could not compute the curated
/// projection; the header states the hash and why there is no revision.
#[test]
fn header_states_the_graph_hash_when_the_projection_was_not_computed() {
    for mode in MODES {
        let bundle = try_compile_spec_supplied(
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
                curated: Curated::NotComputed(crate::CuratedFailure::ReviewsUnavailable),
                graph_content_hash: Some(GRAPH_HASH),
                source_revisions: None,
            },
        )
        .unwrap();
        let header = section(&d1(&bundle).content, "# Product specification");
        assert!(header.contains(&format!("- Graph content hash: {GRAPH_HASH}\n")));
        assert!(header.contains(
            "- Curated revision: not computed (`reviews_unavailable`); see `curated.md`\n"
        ));
        assert!(!header.contains("not supplied"));

        // The host's hash is the one printed when both are supplied.
        let projection = projection();
        let both = try_compile_spec_supplied(
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
                curated: Curated::Computed(&projection),
                graph_content_hash: Some("host-hash"),
                source_revisions: None,
            },
        )
        .unwrap();
        let header = section(&d1(&both).content, "# Product specification");
        assert!(header.contains("- Graph content hash: host-hash\n"));
        assert!(header.contains(&format!("- Curated revision: {}\n", projection.revision)));
    }
}

#[test]
fn document_is_deterministic_selected_alone_and_bounded() {
    let limits = |max_bytes| SelectedCompileLimits {
        max_bytes,
        adr: crate::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    };
    for mode in MODES {
        let bundle = compile(mode, None);
        assert_eq!(bundle, compile(mode, None));
        let expected = d1(&bundle);
        let select = |max_bytes| {
            compile_spec_selected(
                &nodes(),
                &[],
                &flows(),
                mode,
                &BTreeSet::new(),
                &[D1_ARTIFACT_ID],
                limits(max_bytes),
            )
            .unwrap()
        };
        assert_eq!(
            select(usize::MAX),
            [SelectedArtifactOutcome::Produced(expected.clone())]
        );
        assert!(matches!(
            select(64).as_slice(),
            [SelectedArtifactOutcome::Limited {
                limit: SelectedArtifactLimit::OutputBytes { .. },
                ..
            }]
        ));
    }
}

#[test]
fn capability_frame_states_the_endpoints_no_capability_groups_and_why() {
    let tier = (Tier::Deterministic, ConfidenceTier::Confirmed);
    let mut gapped = node("ep:local/shop@GET:/a", "Endpoint", tier.0, tier.1);
    gapped.props["framework"] = json!("vercel");
    let mut handled = node("ep:local/shop@POST:/b", "Endpoint", tier.0, tier.1);
    handled.props["framework"] = json!("express");
    let gap = node("gap:handler", "Gap", tier.0, tier.1);
    let handler = node("sym:local/shop@src/app.ts#b", "Symbol", tier.0, tier.1);
    let handles = |src: &Node, dst: &Node| {
        let mut edge = node(
            &format!("{} HANDLES {}", src.id, dst.id),
            "",
            tier.0,
            tier.1,
        );
        edge.props.as_object_mut().unwrap().remove("name");
        Edge {
            src: src.id.clone(),
            dst: dst.id.clone(),
            label: "HANDLES".into(),
            props: edge.props,
        }
    };
    // A Gap-confidence endpoint is in the gap register, not counted here.
    let unproven = node(
        "ep:local/shop@GET:/c",
        "Endpoint",
        tier.0,
        ConfidenceTier::Gap,
    );
    let edges = [handles(&gapped, &gap), handles(&handled, &handler)];
    let nodes = [gapped, handled, gap, handler, unproven];
    let bundle = compile_spec(
        &nodes,
        &edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let frame = section(&d1(&bundle).content, "## 1.3 ");
    assert!(
        frame.contains(
            "Operations in no Capability: 2 of 2 HTTP endpoints. 1 have no resolved handler \
             (their `HANDLES` target is a Gap or missing), so no rule can group them; 1 have a \
             handler that no capability rule groups (by framework: express 1)."
        ),
        "{frame}"
    );
}

#[test]
fn capability_frame_states_a_vercel_front_door_and_the_routes_behind_it() {
    // AC-0926 (#2010): a Vercel function's endpoints that only pass requests
    // on to mux routes under its paths are stated as such, counted once.
    let tier = (Tier::Deterministic, ConfidenceTier::Confirmed);
    let endpoint = |method: &str, path: &str, framework: &str, handler: &str| {
        let mut n = node(
            &format!("ep:local/bot@{method}:{path}"),
            "Endpoint",
            tier.0,
            tier.1,
        );
        n.props["path"] = json!(path);
        n.props["framework"] = json!(framework);
        n.props["handler_sym"] = json!(handler);
        // Registered beside the function, so in a package it reaches.
        n.props["prov"]["evidence"][0]["path"] = json!("api/web/routes.go");
        n
    };
    let nodes = [
        endpoint(
            "ANY",
            "/api/web",
            "vercel",
            "sym:local/bot@api/web/index.go#Handler",
        ),
        endpoint(
            "ANY",
            "/api/web/{path...}",
            "vercel",
            "sym:local/bot@api/web/index.go#Handler",
        ),
        endpoint("GET", "/api/web/session", "net/http", "gap:a"),
        endpoint("POST", "/api/web/devices", "net/http", "gap:b"),
    ];
    let bundle = compile_spec(&nodes, &[], &[], ExportMode::BestEffort, &BTreeSet::new());
    let frame = section(&d1(&bundle).content, "## 1.3 ");
    assert!(
        frame.contains(
            "Operations in no Capability: 4 of 4 HTTP endpoints. 2 are Vercel functions that \
             pass requests on to the 2 routes registered behind them, which are the operations; \
             2 have no resolved handler"
        ),
        "{frame}"
    );
}

/// AC-0945: §1.1 points to the drawn system in every export, before and
/// whatever the drafted prose.
#[test]
fn purpose_and_scope_points_to_the_architecture_diagrams() {
    for mode in MODES {
        let bundle = compile(mode, None);
        let content = &d1(&bundle).content;
        let text = section(content, "## 1.1 ");
        assert!(
            text.contains("[`architecture.md`](architecture.md)"),
            "{mode:?}"
        );
        assert!(
            text.contains("[`architecture.dsl`](architecture.dsl)"),
            "{mode:?}"
        );
        assert!(text.contains(NOT_DRAFTED), "{mode:?}");
        // The files it names are in the same export.
        for file in ["architecture.md", "architecture.dsl"] {
            assert!(
                bundle.artifacts.iter().any(|a| a.file_name == file),
                "{file} in {mode:?}"
            );
        }
        assert!(!section(content, "## 1.2 ").contains("architecture.md"));
        // The bundle manifest lists both under Documents.
        let manifest = &bundle
            .artifacts
            .iter()
            .find(|a| a.file_name == "bundle-manifest.md")
            .expect("manifest")
            .content;
        let documents = &manifest[manifest.find("1. Documents").expect("part 1")
            ..manifest.find("2. Requirements").expect("part 2")];
        assert!(documents.contains("architecture.md"), "{documents}");
        assert!(documents.contains("architecture.dsl"), "{documents}");
    }
}
