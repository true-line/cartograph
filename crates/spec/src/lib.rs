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
mod documents;
mod front_doors;
pub use documents::{
    DOCUMENT_EXTRACTOR_ID, DOCUMENTATION_DRIFT_EXTRACTOR_ID, DocumentCounts, DocumentFacts,
    DocumentReference, DocumentSkip, Documentation, DocumentationCases, DocumentationDrift,
    MAX_DOCUMENT_BYTES, MAX_DOCUMENTS_PER_REPO, MAX_SECTIONS_PER_DOCUMENT, ROLE_API_REFERENCE,
    ROLE_DECISION_RECORD, ReferenceIndex, ReferenceRule, document_id, documentation_drift,
    extract_documents, link_references, read_documentation, section_content_hash, section_id,
};
mod graph_basis;
pub use graph_basis::{
    FactLines, GraphBasis, SourceRevision, edge_fact_line, graph_content_hash, hash_fact_lines,
    node_fact_line,
};
mod knowledge;
pub use knowledge::{
    ACTOR_ALLOW_ASSUMPTION, ACTOR_ALLOW_RULE, ACTOR_ALLOW_RULE_VERSION, ActorReport,
    BUSINESS_FLOW_RULE, BUSINESS_FLOW_RULE_VERSION, BusinessFlowOmission, BusinessFlowReport,
    CAPABILITY_CONTROLLER_RULE, CAPABILITY_CONTROLLER_RULE_VERSION, CAPABILITY_ENTRY_POINT_RULE,
    CAPABILITY_ENTRY_POINT_RULE_VERSION, CAPABILITY_GRAPHQL_RULE, CAPABILITY_GRAPHQL_RULE_VERSION,
    CapabilityReport, DOMAIN_ENTITY_ORM_RULE, DOMAIN_ENTITY_ORM_RULE_VERSION, DomainEntityReport,
    MAX_SUBSYSTEM_FILES, SUBSYSTEM_FILE_CAP, SUBSYSTEM_RULE, SUBSYSTEM_RULE_VERSION,
    SubsystemReport, derive_business_flows, derive_controller_capabilities,
    derive_entry_point_capabilities, derive_graphql_resolver_capabilities,
    derive_orm_domain_entities, derive_permission_actors, derive_subsystems,
};

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
mod test_drift;
pub use rules::journey::{
    self, AssumedGoal, AssumptionKind, ENTRY_CALLBACK_LIMITATION, EntrySearchSummary, ForwardHop,
    GoalBasis, JOURNEY_RULE, JOURNEY_RULE_VERSION, JourneyAssumption, JourneyBudget,
    JourneyCandidate, JourneyGoal, JourneyResult, JourneySearch, JourneyStep, NavigationSite,
    STARTUP_LIMITATION, StepCall, StepNavigation, TruncationReason, UnreadCallback, UnresolvedKind,
    UnresolvedStepItem, WriteFact, candidates as journey_candidates, search as journey_search,
};
pub use rules::use_case::{
    AcceptanceReport, CaseResult, DecisionRow, DocumentedIntent, EXPECTED_RULES_SCHEMA,
    ExpectationLabel, ExpectationResult, ExpectedCase, ExpectedRule, ExpectedSet, ExpectedSetError,
    ExpectedSpan, OwnerCoverage, RowCondition, RowOutcome, RowRole, USE_CASE_ARTIFACT_ID,
    USE_CASE_CANDIDATE_RULE, USE_CASE_ROW_RULE, UseCaseProjection, UseCaseScope,
    check_use_case_acceptance, project_use_case,
};
pub use test_drift::{
    FOLLOWS_REDIRECTS_REASON, PARTIAL_RESPONSES_REASON, TEST_DRIFT_EXTRACTOR_ID, UNASSERTED_REASON,
    derive_test_drift,
};
pub use workbench::{compact_base, compact_edge, compact_node};
mod security;
pub use security::{
    SecurityLimit, SecurityLimitExceeded, SecurityLimits, SecurityProjectionSize,
    derive_security_findings_bounded, security_projection_bound, security_projection_size,
    security_projection_value_bound,
};
mod views;
mod workbench;
pub use adr::{
    AdrFacts, AdrLimit, AdrLimitExceeded, AdrLimits, FOUND_ADR_EXTRACTOR_ID, FoundAdrRead,
    MAX_FOUND_ADR_BYTES, UnreadAdr, UnreadAdrReason, derive_adr_facts, derive_adr_facts_bounded,
    extract_found_adrs, extract_found_adrs_among, read_found_adrs_among,
};
pub use contract::{
    CONTRACT_DRIFT_EXTRACTOR_ID, CONTRACT_EXTRACTOR_ID, CONTRACT_METHODS, ContractFacts,
    ContractValidationGap, DOCUMENTED_NOT_SERVED_REASON, DeclaredApiDoc, EMPTY_DOC_REASON,
    UNSUPPORTED_YAML_REASON, canonical_endpoint, documented_endpoints_markdown,
    extract_contract_facts, extract_contract_facts_among,
};
pub use views::{
    ArchitectureComponents, ArchitectureElement, ArchitectureFact, ArchitectureRelation,
    ArchitectureView, DeploymentRole, SPEC_VIEW_MAX_EDGES, SPEC_VIEW_MAX_FLOWS,
    SPEC_VIEW_MAX_NODES, SpecDiagramView, SpecViewBundle, SpecViewCaps, compile_selected_flow_view,
    compile_selected_flow_view_capped, compile_spec_views, compile_spec_with_views,
    compile_spec_with_views_and_coverage, try_compile_spec_with_views_and_coverage,
    try_compile_spec_with_views_supplied, try_compile_spec_with_views_supplied_kept,
    try_compile_spec_with_views_supplied_kept_capped,
};
pub use workbench::{
    AREA_BRIEFING_BYTES, AREA_SECTIONS, AreaBriefing, AreaNeighbours, area_blocks, area_briefing,
    area_neighbours, briefing_citation_ids,
};
pub use workbench::{
    ActorStoryWorkspaceBound, ArtifactDescriptor, ArtifactSelectionError, COMPILE_WORKSPACE_MODEL,
    CONFIG_READ_COVERAGE, COVERAGE_DETAIL_REPO_LIMIT, CapabilityCompileWorkspaceBound,
    CapabilityStory, CapabilityStoryInputError, CompileWorkspaceBound, Curated, CuratedBasis,
    CuratedFailure, DATA_PARTITION_EDGE_LABELS, ENDPOINT_PATH_MAX_DEPTH,
    ENDPOINT_PATH_MAX_RELATIONS, ENDPOINT_PATH_MAX_SYMBOLS, ExportMode, FACT_INDEX_SCHEMA,
    FactIndex, FactIndexError, FactIndexFailure, FactIndexRow, FactRecord, FactSpan,
    KnowledgeStories, KnowledgeStoryCompileWorkspaceBound, PartitionAcquisition, PartitionArtifact,
    PartitionArtifactKind, PartitionError, PartitionWorkspaceBound, SelectedArtifactLimit,
    SelectedArtifactOutcome, SelectedCompileLimits, SharedText, SpecArtifact, SpecAssertion,
    SpecBundle, StaleCoverage, Supplied, artifact_catalog, check_citations, citation_id,
    cited_fact_digests, cited_fact_hashes, cited_node_hashes, compile_capability_user_stories,
    compile_capability_user_stories_with_flows_and_security,
    compile_capability_user_stories_with_security, compile_knowledge_user_stories,
    compile_knowledge_user_stories_with_flows_and_security, compile_partition_selected,
    compile_spec, compile_spec_selected, compile_spec_selected_supplied,
    compile_spec_selected_with_coverage, compile_spec_selected_with_security,
    compile_spec_selected_with_security_supplied, compile_spec_with_coverage, count_gap_findings,
    count_interpretation_backlog, curated_basis_summary, curated_finding_reason_sentence,
    curated_finding_shown, curated_reason_sentence, is_drift_edge, is_drift_node, is_gap_edge,
    is_gap_node, logical_actor_story_workspace_bound, logical_capability_compile_bound,
    logical_compile_bound, logical_coverage_render_bound, logical_knowledge_story_compile_bound,
    logical_partition_bound, project_recovered_facts, provenance, stored_prov,
    try_compile_spec_supplied, try_compile_spec_supplied_kept, try_compile_spec_with_coverage,
    validate_knowledge_stories,
};
pub use workbench::{
    CATALOGUE_VERSION, CODE_LABELS, CatalogueSection, CompositionFact, CompositionFacts,
    DerivedGroup, SYSTEM_SCOPE, SectionFrame, SectionKind, SectionScopeError, SourceIdentifiers,
    catalogue_section, catalogue_sections, composition_facts, composition_source, composition_text,
    drafted_section, rule_excerpt, rule_excerpt_budget, rule_excerpt_within, section_citation_set,
    section_text,
};

pub use workbench::{CitedText, Support, route_literals};

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
    "CONFIGURED_IN",
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
    ("CONFIGURED_IN", "-.->"),
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

/// The topology map under one export mode (R-INT-5, #1755): facts the mode
/// withholds, and inferences the Workbench rejected, are left out before the
/// diagram is drawn, exactly as in the bundle's `topology.md`.
pub fn topology_mermaid_in(
    nodes: &[Node],
    edges: &[Edge],
    mode: ExportMode,
    rejected_hashes: &std::collections::BTreeSet<String>,
) -> String {
    let nodes: Vec<Node> = workbench::filter_nodes(nodes, mode, rejected_hashes)
        .into_iter()
        .cloned()
        .collect();
    let edges: Vec<Edge> = workbench::filter_edges(edges, mode, rejected_hashes)
        .into_iter()
        .cloned()
        .collect();
    topology_mermaid(&nodes, &edges)
}

/// The flow dossier under one export mode (R-INT-5, #1755): a withheld hop
/// becomes an explicit Gap and the flow turns partial, exactly as in the
/// bundle's `flow_dossiers.md`.
pub fn flow_dossier_in(
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &std::collections::BTreeSet<String>,
) -> String {
    flow_dossier(&workbench::project_flows(flows, mode, rejected_hashes))
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
        // Facts with no valid provenance print as the explicit Gap fallback.
        assert!(mmd.contains(&format!(r#"{q}["aws_sqs_queue.orders · T0 Gap"]"#)));
        assert!(mmd.contains(&format!("{q} -->|TRIGGERS · T0 Gap| {f}")));
        assert!(mmd.contains("-.->|REFERENCES · T0 Gap|"));
        assert!(!mmd.contains("CALLS"));
    }

    #[test]
    fn topology_labels_name_the_root_module_of_a_scoped_resource() {
        // AC-1500 (ADR-0077): one address in two root modules draws two
        // boxes whose labels say which module directory each belongs to.
        let mut prod = resource(
            "res:r@envs/prod/aws_sqs_queue.orders",
            "aws_sqs_queue.orders",
        );
        prod.props["root_module"] = "envs/prod".into();
        let mut staging = resource(
            "res:r@envs/staging/aws_sqs_queue.orders",
            "aws_sqs_queue.orders",
        );
        staging.props["root_module"] = "envs/staging".into();
        let root = resource("res:r@aws_sqs_queue.orders", "aws_sqs_queue.orders");
        let mmd = topology_mermaid(&[prod, staging, root], &[]);
        assert!(mmd.contains(&format!(
            r#"{}["envs/prod: aws_sqs_queue.orders · T0 Gap"]"#,
            mermaid_id("res:r@envs/prod/aws_sqs_queue.orders")
        )));
        assert!(mmd.contains(&format!(
            r#"{}["envs/staging: aws_sqs_queue.orders · T0 Gap"]"#,
            mermaid_id("res:r@envs/staging/aws_sqs_queue.orders")
        )));
        assert!(mmd.contains(&format!(
            r#"{}["aws_sqs_queue.orders · T0 Gap"]"#,
            mermaid_id("res:r@aws_sqs_queue.orders")
        )));
        let mut odd = resource("res:r@e\"v\n/aws_sqs_queue.q", "aws_sqs_queue.q");
        odd.props["root_module"] = "e\"v\n".into();
        let mmd = topology_mermaid(&[odd], &[]);
        assert!(
            mmd.contains(r#"["e#quot;v : aws_sqs_queue.q · T0 Gap"]"#),
            "{mmd}"
        );
    }

    #[test]
    fn placeholders_render_distinctly() {
        // R-INT-2 at the artifact level: unresolved is visibly unresolved.
        let mut unresolved = resource("res:module.vpc", "module.vpc");
        unresolved.props = serde_json::json!({ "placeholder": true });
        let mmd = topology_mermaid(&[unresolved], &[]);
        assert!(mmd.contains(&format!(
            r#"{}(["module.vpc ? · T0 Gap"])"#,
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
        assert!(
            mmd.contains(
                r#"[("sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders · T0 Gap")]"#
            )
        );
        assert!(mmd.contains("-->|BACKS · T0 Gap|"));
        // No BACKS edge, no cylinder: channels are not infra topology on
        // their own.
        assert!(!mmd.contains("1/other"));
    }

    #[test]
    fn configured_in_edge_renders_the_env_file_by_path() {
        // #734: an env file joins the map only through a resource's
        // CONFIGURED_IN edge, drawn dotted and named by its path.
        let db = resource(
            "res:local/app@observed:sql-database:env:DB_URI",
            "observed:sql-database:env:DB_URI",
        );
        let file = |path: &str| Node {
            id: format!("file:local/app@{path}"),
            label: "File".into(),
            props: serde_json::json!({ "path": path, "redacted": true }),
        };
        let configured = edge(
            "res:local/app@observed:sql-database:env:DB_URI",
            "file:local/app@tests/test.env",
            "CONFIGURED_IN",
        );
        let mmd = topology_mermaid(
            &[db, file("tests/test.env"), file("example.env")],
            &[configured],
        );
        assert!(mmd.contains(r#"[/"tests/test.env · T0 Gap"/]"#));
        assert!(mmd.contains("-.->|CONFIGURED_IN · T0 Gap|"));
        assert!(!mmd.contains("example.env"));
        // A quote or line break in a legal filename cannot close the label
        // or start a new diagram line.
        let odd = "cfg/a\"]\nX-->Y.env";
        let mmd_odd = topology_mermaid(
            &[
                resource(
                    "res:local/app@observed:sql-database:env:DB_URI",
                    "observed:sql-database:env:DB_URI",
                ),
                file(odd),
            ],
            &[edge(
                "res:local/app@observed:sql-database:env:DB_URI",
                &format!("file:local/app@{odd}"),
                "CONFIGURED_IN",
            )],
        );
        assert!(mmd_odd.contains(r#"[/"cfg/a#quot;] X-->Y.env · T0 Gap"/]"#));
        assert!(!mmd_odd.lines().any(|line| line.starts_with("X-->")));
        assert_eq!(
            mmd,
            topology_mermaid(
                &[
                    resource(
                        "res:local/app@observed:sql-database:env:DB_URI",
                        "observed:sql-database:env:DB_URI",
                    ),
                    file("example.env"),
                    file("tests/test.env"),
                ],
                &[edge(
                    "res:local/app@observed:sql-database:env:DB_URI",
                    "file:local/app@tests/test.env",
                    "CONFIGURED_IN",
                )],
            )
        );
    }

    fn prov(
        tier: core_prov::Tier,
        confidence: core_prov::ConfidenceTier,
        identity: &str,
    ) -> serde_json::Value {
        serde_json::to_value(
            core_prov::Provenance::new(
                tier,
                confidence,
                vec![core_prov::EvidenceRef {
                    repo: "local/infra".into(),
                    path: "main.tf".into(),
                    byte_start: 1,
                    byte_end: 9,
                    commit_sha: "abc123".into(),
                }],
                "spec.test",
                identity.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    // AC-0587 (#1755): R-INT-2 and R-INT-5 on the topology map.
    #[test]
    fn topology_shows_tier_and_confidence_and_honours_the_export_mode() {
        use core_prov::{ConfidenceTier as C, Tier as T};
        let mut queue = resource("res:q", "aws_sqs_queue.orders");
        queue.props["prov"] = prov(T::Deterministic, C::Confirmed, "res:q");
        let mut lambda = resource("res:f", "aws_lambda_function.fulfill");
        lambda.props["prov"] = prov(T::Deterministic, C::Confirmed, "res:f");
        let mut guessed = resource("res:g", "guessed.bucket");
        guessed.props["prov"] = prov(T::Agentic, C::InferredWeak, "res:g");
        let chan = Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({ "prov": prov(T::Deterministic, C::Confirmed, "chan:orders") }),
        };
        let with = |mut e: Edge, tier, confidence| {
            let identity = format!("{} {} {}", e.src, e.label, e.dst);
            e.props["prov"] = prov(tier, confidence, &identity);
            e
        };
        let nodes = [queue, lambda, guessed, chan];
        let edges = [
            with(
                edge("res:q", "res:f", "TRIGGERS"),
                T::Deterministic,
                C::Confirmed,
            ),
            with(
                edge("res:q", "chan:orders", "BACKS"),
                T::Dynamic,
                C::Confirmed,
            ),
            with(
                edge("res:f", "res:q", "DEPENDS_ON"),
                T::Semantic,
                C::InferredStrong,
            ),
            with(
                edge("res:f", "res:g", "REFERENCES"),
                T::Agentic,
                C::InferredWeak,
            ),
        ];
        let none = std::collections::BTreeSet::new();

        let best = topology_mermaid_in(&nodes, &edges, ExportMode::BestEffort, &none);
        // A static edge, an observed edge and two inferred edges read differently.
        assert!(best.contains(r#"["aws_sqs_queue.orders · T0 Confirmed"]"#));
        assert!(best.contains(r#"[("orders · T0 Confirmed")]"#));
        assert!(best.contains("-->|TRIGGERS · T0 Confirmed|"));
        assert!(best.contains("-->|BACKS · T1 Confirmed|"));
        assert!(best.contains("-.->|DEPENDS_ON · T2 InferredStrong|"));
        assert!(best.contains(r#"["guessed.bucket · T3 InferredWeak"]"#));
        assert!(best.contains("-.->|REFERENCES · T3 InferredWeak|"));

        // Verified-only withholds the weak node and the weak edge, and only those.
        let verified = topology_mermaid_in(&nodes, &edges, ExportMode::VerifiedOnly, &none);
        assert!(!verified.contains("InferredWeak"));
        assert!(!verified.contains("guessed.bucket"));
        assert!(verified.contains("-->|BACKS · T1 Confirmed|"));
        assert!(verified.contains("-.->|DEPENDS_ON · T2 InferredStrong|"));

        // A rejected inference is withheld in either mode.
        let strong = crate::provenance(&edges[2].props, "res:f DEPENDS_ON res:q").content_hash;
        let rejected = std::collections::BTreeSet::from([strong]);
        let curated = topology_mermaid_in(&nodes, &edges, ExportMode::BestEffort, &rejected);
        assert!(!curated.contains("DEPENDS_ON"));
    }

    // AC-0587 (#1755): the standalone flow dossier honours the export mode.
    #[test]
    fn flow_dossier_withholds_weak_hops_in_verified_only() {
        let flows = [Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![
                hop("HANDLES", "ep:orders", "sym:handler", "Confirmed"),
                hop("CALLS", "sym:handler", "sym:guess", "InferredWeak"),
            ],
            status: flowtracer::FlowStatus::Inferred,
            score: 0.5,
            depth_limited: false,
        }];
        let none = std::collections::BTreeSet::new();
        let best = flow_dossier_in(&flows, ExportMode::BestEffort, &none);
        assert!(best.contains("InferredWeak"));
        let verified = flow_dossier_in(&flows, ExportMode::VerifiedOnly, &none);
        assert!(!verified.contains("InferredWeak"));
        assert!(verified.contains("weak inference excluded by verified-only export"));
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
