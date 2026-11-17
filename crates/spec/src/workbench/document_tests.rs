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

fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
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
fn nodes() -> Vec<Node> {
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

fn flows() -> Vec<Flow> {
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
            coverage: &[],
            stale_coverage: &[],
            curated: curated.map_or(Curated::NotSupplied, Curated::Computed),
        },
    )
    .unwrap()
}

fn d1(bundle: &SpecBundle) -> &SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == D1_ARTIFACT_ID)
        .unwrap()
}

/// The part of the document under one `## ` heading.
fn section<'a>(content: &'a str, heading: &str) -> &'a str {
    let start = content.find(heading).unwrap() + heading.len();
    let rest = &content[start..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

fn printed_citations(text: &str) -> Vec<&str> {
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

        // Only this document reads the projection's revision; with no
        // accepted record nothing else differs, `curated.md` apart.
        for (before, after) in bare.artifacts.iter().zip(&supplied.artifacts) {
            assert_eq!(
                before == after,
                !matches!(before.id.as_str(), "curated" | D1_ARTIFACT_ID),
                "{} in {mode:?}",
                before.id
            );
        }
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
