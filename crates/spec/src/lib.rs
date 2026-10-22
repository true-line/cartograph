//! Spec compiler: graph → official artifacts (SPEC-00 §7).
//!
//! M2 brought the first artifact (resource/topology map); M3 adds flow
//! dossiers (Markdown + Mermaid sequence + provenance table per flow).
//! Both are portable, renderable anywhere, diffable in a PR. M9 adds the full
//! artifact set (US/AC, data model, ADRs, and registers).

use core_graph::{Edge, Node};
use flowtracer::Flow;

mod adr;
mod contract;

/// Version of the spec compiler's projection rules, recorded with every
/// rule-derived statement's producing method (AC-0244). Bump when a rule's
/// output changes for the same input.
pub const SPEC_RULE_VERSION: &str = "1";

/// The producing method of a statement derived by the named spec rule.
pub(crate) fn spec_rule(rule: &str) -> core_prov::ProducingMethod {
    core_prov::ProducingMethod::Rule {
        rule: rule.into(),
        version: SPEC_RULE_VERSION.into(),
    }
}
mod rules;
pub use rules::use_case::{
    AcceptanceReport, CaseResult, DecisionRow, DocumentedIntent, EXPECTED_RULES_SCHEMA,
    ExpectationLabel, ExpectationResult, ExpectedCase, ExpectedRule, ExpectedSet, ExpectedSetError,
    ExpectedSpan, OwnerCoverage, RowCondition, RowOutcome, RowRole, USE_CASE_ARTIFACT_ID,
    USE_CASE_CANDIDATE_RULE, USE_CASE_ROW_RULE, UseCaseProjection, UseCaseScope,
    check_use_case_acceptance, project_use_case,
};
mod security;
pub use security::{
    SecurityLimit, SecurityLimitExceeded, SecurityLimits, SecurityProjectionSize,
    derive_security_findings_bounded, security_projection_bound, security_projection_size,
};
mod views;
mod workbench;
pub use adr::{
    AdrFacts, AdrLimit, AdrLimitExceeded, AdrLimits, derive_adr_facts, derive_adr_facts_bounded,
    extract_found_adrs,
};
pub use contract::{
    CONTRACT_DRIFT_EXTRACTOR_ID, CONTRACT_EXTRACTOR_ID, CONTRACT_METHODS, ContractFacts,
    ContractValidationGap, DOCUMENTED_NOT_SERVED_REASON, DeclaredApiDoc, EMPTY_DOC_REASON,
    UNSUPPORTED_YAML_REASON, canonical_endpoint, documented_endpoints_markdown,
    extract_contract_facts,
};
pub use views::{
    DeploymentRole, SPEC_VIEW_MAX_EDGES, SPEC_VIEW_MAX_FLOWS, SPEC_VIEW_MAX_NODES, SpecDiagramView,
    SpecViewBundle, compile_spec_views, compile_spec_with_views,
};
pub use workbench::{
    ArtifactDescriptor, ArtifactSelectionError, COMPILE_WORKSPACE_MODEL, CompileWorkspaceBound,
    ExportMode, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits,
    SpecArtifact, SpecAssertion, SpecBundle, artifact_catalog, compile_spec, compile_spec_selected,
    compile_spec_selected_with_security, count_gap_findings, is_drift_edge, is_drift_node,
    is_gap_edge, is_gap_node, logical_compile_bound, project_recovered_facts, provenance,
};

/// Edge labels that appear on the topology map — callers use this to query
/// exactly the edges the artifact renders.
pub const TOPOLOGY_EDGE_LABELS: &[&str] = &[
    "TRIGGERS",
    "ROUTES",
    "SUBSCRIBES",
    "GRANTS",
    "BACKS",
    "DEPENDS_ON",
    "REFERENCES",
];

/// Edge labels that appear on the topology map, with their arrow style.
/// Capability edges are solid and labeled; the raw reference DAG is dotted.
const TOPOLOGY_EDGES: &[(&str, &str)] = &[
    ("TRIGGERS", "-->"),
    ("ROUTES", "-->"),
    ("SUBSCRIBES", "-->"),
    ("GRANTS", "-->"),
    ("BACKS", "-->"),
    ("DEPENDS_ON", "-.->"),
    ("REFERENCES", "-.->"),
];

/// Mermaid identifier for a graph node id: sanitized for readability plus
/// a short content hash for uniqueness — sanitization alone collapses ids
/// differing only in punctuation (`acme/foo-bar` vs `acme/foo_bar`), which
/// would visually re-merge nodes the graph keeps distinct.
fn mermaid_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let hash = blake3::hash(id.as_bytes()).to_hex();
    format!("{sanitized}_{}", &hash.as_str()[..8])
}

/// Compile the resource/topology map (SPEC-00 §7 artifact table) from
/// `Resource` nodes and their infra edges. Channels join the map only via
/// an observed `BACKS` edge (M6: deployed resource → the code-layer
/// channel it backs) and render as cylinders. Deterministic output: same
/// graph, same text (US-0014).
pub fn topology_mermaid(nodes: &[Node], edges: &[Edge]) -> String {
    let mut out = String::new();
    workbench::diagrams::topology(&mut out, nodes, edges).expect("write to string");
    out
}

/// Compile the flow-dossier artifact (SPEC-00 §7: Markdown + Mermaid
/// sequence + provenance table per flow). Deterministic for a given trace
/// (US-0014); statuses and tiers are always visible (R-INT-2), Gap hops
/// render with a distinct broken arrow (R-INT-4).
pub fn flow_dossier(flows: &[Flow]) -> String {
    let mut out = String::new();
    workbench::diagrams::flows(&mut out, flows).expect("write to string");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(id: &str, logical: &str) -> Node {
        Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({ "logical_id": logical }),
        }
    }

    fn edge(src: &str, dst: &str, label: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({}),
        }
    }

    #[test]
    fn topology_map_renders_capability_and_reference_edges() {
        let nodes = vec![
            resource("res:aws_sqs_queue.orders", "aws_sqs_queue.orders"),
            resource(
                "res:aws_lambda_function.fulfill",
                "aws_lambda_function.fulfill",
            ),
        ];
        let edges = vec![
            edge(
                "res:aws_sqs_queue.orders",
                "res:aws_lambda_function.fulfill",
                "TRIGGERS",
            ),
            edge(
                "res:aws_lambda_function.fulfill",
                "res:aws_sqs_queue.orders",
                "REFERENCES",
            ),
            // Non-topology edges never leak onto the infra map.
            edge(
                "res:aws_sqs_queue.orders",
                "res:aws_lambda_function.fulfill",
                "CALLS",
            ),
        ];
        let mmd = topology_mermaid(&nodes, &edges);
        assert!(mmd.starts_with("flowchart LR\n"));
        let q = mermaid_id("res:aws_sqs_queue.orders");
        let f = mermaid_id("res:aws_lambda_function.fulfill");
        assert!(mmd.contains(&format!(r#"{q}["aws_sqs_queue.orders"]"#)));
        assert!(mmd.contains(&format!("{q} -->|TRIGGERS| {f}")));
        assert!(mmd.contains("-.->|REFERENCES|"));
        assert!(!mmd.contains("CALLS"));
    }

    #[test]
    fn placeholders_render_distinctly() {
        // R-INT-2 at the artifact level: unresolved is visibly unresolved.
        let mut unresolved = resource("res:module.vpc", "module.vpc");
        unresolved.props = serde_json::json!({ "placeholder": true });
        let mmd = topology_mermaid(&[unresolved], &[]);
        assert!(mmd.contains(&format!(
            r#"{}(["module.vpc ?"])"#,
            mermaid_id("res:module.vpc")
        )));
    }

    fn hop(label: &str, src: &str, dst: &str, confidence: &str) -> flowtracer::Hop {
        flowtracer::Hop {
            label: label.into(),
            src: src.into(),
            dst: dst.into(),
            src_name: src.into(),
            dst_name: dst.into(),
            tier: "Deterministic".into(),
            confidence: confidence.into(),
            evidence: Some("src/app.ts bytes 1..9".into()),
            provenance: core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                match confidence {
                    "Confirmed" => core_prov::ConfidenceTier::Confirmed,
                    "InferredStrong" => core_prov::ConfidenceTier::InferredStrong,
                    "InferredWeak" => core_prov::ConfidenceTier::InferredWeak,
                    _ => core_prov::ConfidenceTier::Gap,
                },
                vec![core_prov::EvidenceRef {
                    repo: "local/test".into(),
                    path: "src/app.ts".into(),
                    byte_start: 1,
                    byte_end: 9,
                    commit_sha: "abc123".into(),
                }],
                "spec.test",
                format!("{src}:{label}:{dst}").as_bytes(),
            )
            .unwrap(),
            gap_reason: None,
            attempted_tiers: Vec::new(),
        }
    }

    #[test]
    fn flow_dossier_renders_sequence_and_provenance_table() {
        let flows = vec![Flow {
            trigger: "ep:GET:/users".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "GET /users".into(),
            hops: vec![
                hop("HANDLES", "ep:GET:/users", "sym:app.ts#list", "Confirmed"),
                hop("PUBLISHES", "sym:app.ts#list", "gap:chan:app.ts@5", "Gap"),
            ],
            status: flowtracer::FlowStatus::Partial,
            score: 0.5,
            depth_limited: false,
        }];
        let dossier = flow_dossier(&flows);
        assert!(dossier.starts_with("# Flow dossier\n"));
        assert!(dossier.contains("## GET /users — Partial (score 0.50)"));
        assert!(dossier.contains("sequenceDiagram"));
        // Confirmed hops are solid, Gap hops visibly broken (R-INT-4).
        assert!(dossier.contains("p0->>p1: HANDLES [Confirmed]"));
        assert!(dossier.contains("p1--xp2: PUBLISHES [Gap]"));
        // Provenance table carries tier + confidence + method + evidence
        // (R-INT-2, AC-0244: a fact without a method says so).
        assert!(dossier.contains("| 1 | HANDLES `ep:GET:/users` → `sym:app.ts#list` | Deterministic | Confirmed | not recorded | local/test:src/app.ts bytes 1..9 @ abc123 | `spec.test` |"));
        // Deterministic (US-0014).
        assert_eq!(dossier, flow_dossier(&flows));
    }

    #[test]
    fn flow_dossier_escapes_the_producing_method_cell() {
        // AC-0244 (#601 review): a method label cannot break the table.
        let mut handles = hop("HANDLES", "ep:GET:/users", "sym:app.ts#list", "Confirmed");
        handles.provenance.method = core_prov::ProducingMethod::Rule {
            rule: "split|rule\nnext".into(),
            version: "1".into(),
        };
        let flows = vec![Flow {
            trigger: "ep:GET:/users".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "GET /users".into(),
            hops: vec![handles],
            status: flowtracer::FlowStatus::Partial,
            score: 0.5,
            depth_limited: false,
        }];
        let dossier = flow_dossier(&flows);
        let row = dossier
            .lines()
            .find(|line| line.starts_with("| 1 | HANDLES"))
            .expect("hop row");
        assert!(row.contains("| rule split\\|rule next@1 |"), "{row}");
        assert!(!dossier.contains("rule\nnext"));
    }

    #[test]
    fn backs_edge_renders_the_channel_as_a_cylinder() {
        // M6: the infra↔code join is visible on the topology map — but a
        // channel appears only through an observed BACKS edge.
        let queue = resource(
            "res:local/infra@aws_sqs_queue.orders",
            "aws_sqs_queue.orders",
        );
        let chan = Node {
            id: "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({}),
        };
        let orphan_chan = Node {
            id: "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/other".into(),
            label: "Channel".into(),
            props: serde_json::json!({}),
        };
        let backs = edge(
            "res:local/infra@aws_sqs_queue.orders",
            "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders",
            "BACKS",
        );
        let mmd = topology_mermaid(&[queue, chan, orphan_chan], &[backs]);
        assert!(mmd.contains(r#"[("sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders")]"#));
        assert!(mmd.contains("-->|BACKS|"));
        // No BACKS edge, no cylinder: channels are not infra topology on
        // their own.
        assert!(!mmd.contains("1/other"));
    }

    #[test]
    fn punctuation_variant_repos_render_distinctly() {
        // Sanitization alone maps acme/foo-bar and acme/foo_bar to the same
        // Mermaid identifier; the hash suffix keeps them apart.
        let a = mermaid_id("res:acme/foo-bar@aws_sqs_queue.q");
        let b = mermaid_id("res:acme/foo_bar@aws_sqs_queue.q");
        assert_ne!(a, b);
        // Deterministic: same id, same key.
        assert_eq!(a, mermaid_id("res:acme/foo-bar@aws_sqs_queue.q"));
    }

    #[test]
    fn output_is_deterministic() {
        let nodes = vec![resource("res:b.b", "b.b"), resource("res:a.a", "a.a")];
        let edges = vec![
            edge("res:b.b", "res:a.a", "REFERENCES"),
            edge("res:a.a", "res:b.b", "TRIGGERS"),
        ];
        assert_eq!(
            topology_mermaid(&nodes, &edges),
            topology_mermaid(&nodes, &edges)
        );
        // Nodes are ordered by id regardless of input order.
        let mmd = topology_mermaid(&nodes, &edges);
        let a_pos = mmd.find(&format!("{}[", mermaid_id("res:a.a"))).unwrap();
        let b_pos = mmd.find(&format!("{}[", mermaid_id("res:b.b"))).unwrap();
        assert!(a_pos < b_pos);
    }
}
