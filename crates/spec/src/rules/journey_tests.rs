//! Unit tests for `journey-candidate@1` and the `journeys` spec artifact
//! (US-0045, AC-0352, T-0352).

use super::*;
use crate::{ExportMode, compile_spec};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier};
use flowtracer::{Flow, FlowStatus, Hop};
use serde_json::json;
use std::collections::BTreeSet;

fn ev(repo: &str, path: &str, byte_start: u64, byte_end: u64) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start,
        byte_end,
        commit_sha: "abc1234".into(),
    }
}

fn prov(evidence: Vec<EvidenceRef>, tag: &str) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        "test.fixture",
        tag.as_bytes(),
    )
    .unwrap()
}

fn node(id: &str, label: &str, mut props: serde_json::Value, evidence: Vec<EvidenceRef>) -> Node {
    props["prov"] = serde_json::to_value(prov(evidence, id)).unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(
    src: &str,
    dst: &str,
    label: &str,
    mut props: serde_json::Value,
    evidence: Vec<EvidenceRef>,
) -> Edge {
    let key = format!("{src} {label} {dst}");
    props["prov"] = serde_json::to_value(prov(evidence, &key)).unwrap();
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props,
    }
}

/// Synthetic graph shaped like SimpleLogin's browser extension:
/// - Screens `/`, `/main`, and `/new-alias-result/`, with `RENDERS` to `.vue`
///   `File` nodes and `Symbol`s `DEFINED_IN` those files.
/// - `/` -> `/main` through `NAVIGATES_TO` from `SplashScreen`.
/// - `Main` symbol with:
///   - `FETCHES` to `POST /api/v2/aliases` (list call; no write -> `method_assumed`)
///   - `FETCHES` to `POST /api/v2/alias/custom/new` (flow reaches `WRITES` -> `DataEntity` -> `observed_write`)
///   - a later unconditional `NAVIGATES_TO` site to `/new-alias-result/` in the same `Main` symbol.
fn simplelogin_fixture(shift: u64) -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
    let repo = "simplelogin/extension";
    let srv = "simplelogin/app";

    let nodes = vec![
        node(
            "screen:simplelogin/extension@/",
            "Screen",
            json!({"route": "/", "router": "vue-router"}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                10 + shift,
                40 + shift,
            )],
        ),
        node(
            "screen:simplelogin/extension@/main",
            "Screen",
            json!({"route": "/main", "router": "vue-router"}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                50 + shift,
                85 + shift,
            )],
        ),
        node(
            "screen:simplelogin/extension@/new-alias-result/",
            "Screen",
            json!({"route": "/new-alias-result/", "router": "vue-router"}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                90 + shift,
                140 + shift,
            )],
        ),
        node(
            "file:simplelogin/extension@src/popup/components/SplashScreen.vue",
            "File",
            json!({"path": "src/popup/components/SplashScreen.vue", "name": "SplashScreen.vue"}),
            vec![ev(
                repo,
                "src/popup/components/SplashScreen.vue",
                shift,
                200 + shift,
            )],
        ),
        node(
            "file:simplelogin/extension@src/popup/components/Main.vue",
            "File",
            json!({"path": "src/popup/components/Main.vue", "name": "Main.vue"}),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                shift,
                600 + shift,
            )],
        ),
        node(
            "file:simplelogin/extension@src/popup/components/NewAliasResult.vue",
            "File",
            json!({"path": "src/popup/components/NewAliasResult.vue", "name": "NewAliasResult.vue"}),
            vec![ev(
                repo,
                "src/popup/components/NewAliasResult.vue",
                shift,
                200 + shift,
            )],
        ),
        node(
            "sym:simplelogin/extension@src/popup/components/SplashScreen.vue#SplashScreen",
            "Symbol",
            json!({"name": "SplashScreen"}),
            vec![ev(
                repo,
                "src/popup/components/SplashScreen.vue",
                20 + shift,
                180 + shift,
            )],
        ),
        node(
            "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
            "Symbol",
            json!({"name": "Main"}),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                20 + shift,
                550 + shift,
            )],
        ),
        node(
            "sym:simplelogin/extension@src/popup/components/NewAliasResult.vue#NewAliasResult",
            "Symbol",
            json!({"name": "NewAliasResult"}),
            vec![ev(
                repo,
                "src/popup/components/NewAliasResult.vue",
                20 + shift,
                160 + shift,
            )],
        ),
        node(
            "ep:simplelogin/app@POST:/api/v2/aliases",
            "Endpoint",
            json!({"method": "POST", "path": "/api/v2/aliases", "name": "POST /api/v2/aliases"}),
            vec![ev(srv, "app/api/views/alias.py", 100 + shift, 180 + shift)],
        ),
        node(
            "ep:simplelogin/app@POST:/api/v2/alias/custom/new",
            "Endpoint",
            json!({
                "method": "POST",
                "path": "/api/v2/alias/custom/new",
                "name": "POST /api/v2/alias/custom/new"
            }),
            vec![ev(
                srv,
                "app/api/views/custom_alias.py",
                200 + shift,
                320 + shift,
            )],
        ),
        node(
            "sym:simplelogin/app@app/api/views/custom_alias.py#new_custom_alias",
            "Symbol",
            json!({"name": "new_custom_alias"}),
            vec![ev(
                srv,
                "app/api/views/custom_alias.py",
                200 + shift,
                320 + shift,
            )],
        ),
        node(
            "data:simplelogin/app@alias",
            "DataEntity",
            json!({"name": "Alias"}),
            vec![ev(srv, "app/models.py", 50 + shift, 120 + shift)],
        ),
        node(
            "test:simplelogin/app@tests/api/test_custom_alias.py#test_create_custom_alias",
            "TestCase",
            json!({"name": "test_create_custom_alias"}),
            vec![ev(
                srv,
                "tests/api/test_custom_alias.py",
                10 + shift,
                90 + shift,
            )],
        ),
    ];

    let edges = vec![
        edge(
            "screen:simplelogin/extension@/",
            "file:simplelogin/extension@src/popup/components/SplashScreen.vue",
            "RENDERS",
            json!({}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                10 + shift,
                40 + shift,
            )],
        ),
        edge(
            "screen:simplelogin/extension@/main",
            "file:simplelogin/extension@src/popup/components/Main.vue",
            "RENDERS",
            json!({}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                50 + shift,
                85 + shift,
            )],
        ),
        edge(
            "screen:simplelogin/extension@/new-alias-result/",
            "file:simplelogin/extension@src/popup/components/NewAliasResult.vue",
            "RENDERS",
            json!({}),
            vec![ev(
                repo,
                "src/popup/router/index.js",
                90 + shift,
                140 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/SplashScreen.vue#SplashScreen",
            "file:simplelogin/extension@src/popup/components/SplashScreen.vue",
            "DEFINED_IN",
            json!({}),
            vec![ev(
                repo,
                "src/popup/components/SplashScreen.vue",
                20 + shift,
                180 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
            "file:simplelogin/extension@src/popup/components/Main.vue",
            "DEFINED_IN",
            json!({}),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                20 + shift,
                550 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/NewAliasResult.vue#NewAliasResult",
            "file:simplelogin/extension@src/popup/components/NewAliasResult.vue",
            "DEFINED_IN",
            json!({}),
            vec![ev(
                repo,
                "src/popup/components/NewAliasResult.vue",
                20 + shift,
                160 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/SplashScreen.vue#SplashScreen",
            "screen:simplelogin/extension@/main",
            "NAVIGATES_TO",
            json!({
                "conditional": false,
                "sites": [{
                    "path": "src/popup/components/SplashScreen.vue",
                    "byte_start": 80 + shift,
                    "byte_end": 115 + shift,
                    "conditional": false
                }]
            }),
            vec![ev(
                repo,
                "src/popup/components/SplashScreen.vue",
                80 + shift,
                115 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
            "ep:simplelogin/app@POST:/api/v2/aliases",
            "FETCHES",
            json!({"method": "POST"}),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                120 + shift,
                175 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
            "ep:simplelogin/app@POST:/api/v2/alias/custom/new",
            "FETCHES",
            json!({"method": "POST"}),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                240 + shift,
                310 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
            "screen:simplelogin/extension@/new-alias-result/",
            "NAVIGATES_TO",
            json!({
                "conditional": false,
                "sites": [{
                    "path": "src/popup/components/Main.vue",
                    "byte_start": 340 + shift,
                    "byte_end": 395 + shift,
                    "conditional": false
                }]
            }),
            vec![ev(
                repo,
                "src/popup/components/Main.vue",
                340 + shift,
                395 + shift,
            )],
        ),
        edge(
            "sym:simplelogin/app@app/api/views/custom_alias.py#new_custom_alias",
            "data:simplelogin/app@alias",
            "WRITES",
            json!({}),
            vec![ev(
                srv,
                "app/api/views/custom_alias.py",
                260 + shift,
                305 + shift,
            )],
        ),
        edge(
            "test:simplelogin/app@tests/api/test_custom_alias.py#test_create_custom_alias",
            "ep:simplelogin/app@POST:/api/v2/alias/custom/new",
            "REQUESTS",
            json!({"statuses": [201]}),
            vec![ev(
                srv,
                "tests/api/test_custom_alias.py",
                30 + shift,
                70 + shift,
            )],
        ),
    ];

    let hop_prov = prov(
        vec![ev(
            srv,
            "app/api/views/custom_alias.py",
            200 + shift,
            320 + shift,
        )],
        "flow-hop",
    );
    let flows = vec![Flow {
        trigger: "ep:simplelogin/app@POST:/api/v2/alias/custom/new".into(),
        trigger_kind: "Endpoint".into(),
        trigger_name: "POST /api/v2/alias/custom/new".into(),
        hops: vec![Hop {
            label: "HANDLES".into(),
            src: "ep:simplelogin/app@POST:/api/v2/alias/custom/new".into(),
            dst: "sym:simplelogin/app@app/api/views/custom_alias.py#new_custom_alias".into(),
            src_name: "POST /api/v2/alias/custom/new".into(),
            dst_name: "new_custom_alias".into(),
            tier: "Deterministic".into(),
            confidence: "Confirmed".into(),
            evidence: None,
            provenance: hop_prov,
            gap_reason: None,
            attempted_tiers: vec![],
        }],
        status: FlowStatus::Verified,
        score: 1.0,
        depth_limited: false,
    }];

    (nodes, edges, flows)
}

fn refs<'a>(nodes: &'a [Node], edges: &'a [Edge]) -> (Vec<&'a Node>, Vec<&'a Edge>) {
    (nodes.iter().collect(), edges.iter().collect())
}

// AC-0352 (T-0352): SimpleLogin extension path-router journey recovery and journeys.md artifact.
#[test]
fn journey_candidates_recover_simplelogin_path_and_render_journeys_artifact() {
    let (nodes, edges, flows) = simplelogin_fixture(0);
    let (node_refs, edge_refs) = refs(&nodes, &edges);

    // 1. Expected candidate with goal_basis == ObservedWrite and write_coverage == None.
    let found = candidates(&node_refs, &edge_refs, &flows, JourneyBudget::default());
    assert_eq!(found.len(), 1);
    let candidate = &found[0];
    assert_eq!(candidate.entry, "screen:simplelogin/extension@/");
    assert_eq!(
        candidate.goal.endpoint,
        "ep:simplelogin/app@POST:/api/v2/alias/custom/new"
    );
    assert_eq!(candidate.goal.method, "POST");
    assert_eq!(candidate.goal.goal_basis, GoalBasis::ObservedWrite);
    assert_eq!(candidate.goal.write_coverage, None);
    assert_eq!(
        candidate.goal.writes,
        vec![WriteFact {
            symbol: "sym:simplelogin/app@app/api/views/custom_alias.py#new_custom_alias".into(),
            data_entity: "data:simplelogin/app@alias".into(),
        }]
    );
    assert_eq!(
        candidate.label,
        "Main → POST /api/v2/alias/custom/new → NewAliasResult"
    );
    assert_eq!(
        candidate.flows,
        vec!["ep:simplelogin/app@POST:/api/v2/alias/custom/new".to_owned()]
    );
    assert_eq!(
        candidate.tests,
        vec![
            "test:simplelogin/app@tests/api/test_custom_alias.py#test_create_custom_alias"
                .to_owned()
        ]
    );

    // 2. The list call (POST /api/v2/aliases) is recorded as an earlier assumed goal
    // and does not end the path that reaches the observed write.
    assert_eq!(
        candidate.earlier_assumed_goals,
        vec![AssumedGoal {
            screen: "screen:simplelogin/extension@/main".into(),
            endpoint: "ep:simplelogin/app@POST:/api/v2/aliases".into(),
            method: "POST".into(),
            source: "sym:simplelogin/extension@src/popup/components/Main.vue#Main".into(),
            write_coverage: "unknown".into(),
        }]
    );

    // 3. A path whose only goal is the list call ends there as a method_assumed candidate
    // with write_coverage: "unknown".
    let edges_list_only: Vec<Edge> = edges
        .iter()
        .filter(|e| {
            !(e.label == "FETCHES" && e.dst == "ep:simplelogin/app@POST:/api/v2/alias/custom/new")
        })
        .cloned()
        .collect();
    let (node_refs, list_edge_refs) = refs(&nodes, &edges_list_only);
    let list_candidates = candidates(
        &node_refs,
        &list_edge_refs,
        &flows,
        JourneyBudget::default(),
    );
    assert_eq!(list_candidates.len(), 1);
    let list_cand = &list_candidates[0];
    assert_eq!(
        list_cand.goal.endpoint,
        "ep:simplelogin/app@POST:/api/v2/aliases"
    );
    assert_eq!(list_cand.goal.goal_basis, GoalBasis::MethodAssumed);
    assert_eq!(list_cand.goal.write_coverage.as_deref(), Some("unknown"));
    assert!(list_cand.earlier_assumed_goals.is_empty());

    // 4. result: "ordered" with the result step appended.
    assert_eq!(candidate.result, JourneyResult::Ordered);
    let result_step = candidate
        .result_step
        .as_ref()
        .expect("ordered result step must be present");
    assert_eq!(
        result_step.screen,
        "screen:simplelogin/extension@/new-alias-result/"
    );
    let step_screens: Vec<&str> = candidate.steps.iter().map(|s| s.screen.as_str()).collect();
    assert_eq!(
        step_screens,
        vec![
            "screen:simplelogin/extension@/",
            "screen:simplelogin/extension@/main",
            "screen:simplelogin/extension@/new-alias-result/",
        ]
    );
    assert_eq!(candidate.steps.last(), Some(result_step));

    // 10. Provenance is InferredWeak with the rule method.
    assert_eq!(candidate.provenance.tier, Tier::Semantic);
    assert_eq!(
        candidate.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        candidate.provenance.method,
        ProducingMethod::Rule {
            rule: JOURNEY_RULE.into(),
            version: JOURNEY_RULE_VERSION.into(),
        }
    );
    assert!(!candidate.provenance.evidence.is_empty());

    // 11. The artifact markdown contains the label and the Interpretation line.
    let bundle = compile_spec(
        &nodes,
        &edges,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let artifact = bundle
        .artifacts
        .iter()
        .find(|a| a.id == "journeys")
        .expect("journeys artifact must be registered");
    assert_eq!(artifact.file_name, "journeys.md");
    assert_eq!(artifact.title, "Journey candidates");
    assert_eq!(artifact.format, "markdown");
    assert!(artifact.content.contains(&candidate.label));
    assert!(artifact.content.contains(
        "Interpretation: rule journey-candidate@1, InferredWeak, never a confirmed business journey"
    ));
    assert!(artifact.content.contains(&candidate.id));
    assert!(artifact.content.contains(&candidate.support_fingerprint));
}

// AC-0352 (T-0352): ordering, cycle avoidance, budget truncation, identity, and support fingerprint.
#[test]
fn journey_candidates_enforce_ordering_cycle_budget_and_identity_contracts() {
    let (nodes, edges, flows) = simplelogin_fixture(0);
    let (node_refs, edge_refs) = refs(&nodes, &edges);
    let baseline = &candidates(&node_refs, &edge_refs, &flows, JourneyBudget::default())[0];

    // 5a. Variant with navigation site before the fetch gives "unordered".
    let mut edges_before = edges.clone();
    for e in &mut edges_before {
        if e.label == "NAVIGATES_TO" && e.dst == "screen:simplelogin/extension@/new-alias-result/" {
            e.props["sites"] = json!([{
                "path": "src/popup/components/Main.vue",
                "byte_start": 180,
                "byte_end": 210,
                "conditional": false
            }]);
        }
    }
    let (_, before_refs) = refs(&nodes, &edges_before);
    let before_cand = &candidates(&node_refs, &before_refs, &flows, JourneyBudget::default())[0];
    assert_eq!(before_cand.result, JourneyResult::Unordered);
    assert!(before_cand.result_step.is_none());
    assert_eq!(before_cand.label, "Main → POST /api/v2/alias/custom/new");

    // 5b. Variant with conditional navigation site after the fetch gives "unordered".
    let mut edges_conditional = edges.clone();
    for e in &mut edges_conditional {
        if e.label == "NAVIGATES_TO" && e.dst == "screen:simplelogin/extension@/new-alias-result/" {
            e.props["conditional"] = json!(true);
            e.props["sites"] = json!([{
                "path": "src/popup/components/Main.vue",
                "byte_start": 340,
                "byte_end": 395,
                "conditional": true
            }]);
        }
    }
    let (_, cond_refs) = refs(&nodes, &edges_conditional);
    let cond_cand = &candidates(&node_refs, &cond_refs, &flows, JourneyBudget::default())[0];
    assert_eq!(cond_cand.result, JourneyResult::Unordered);
    assert!(cond_cand.result_step.is_none());

    // 6. A cycle `/main` -> `/` is not revisited.
    let mut edges_with_cycle = edges.clone();
    edges_with_cycle.push(edge(
        "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
        "screen:simplelogin/extension@/",
        "NAVIGATES_TO",
        json!({
            "conditional": true,
            "sites": [{
                "path": "src/popup/components/Main.vue",
                "byte_start": 50,
                "byte_end": 80,
                "conditional": true
            }]
        }),
        vec![ev(
            "simplelogin/extension",
            "src/popup/components/Main.vue",
            50,
            80,
        )],
    ));
    let (_, cycle_refs) = refs(&nodes, &edges_with_cycle);
    let cycle_cands = candidates(&node_refs, &cycle_refs, &flows, JourneyBudget::default());
    assert_eq!(cycle_cands.len(), 1);
    assert_eq!(cycle_cands[0].id, baseline.id);
    let visited_screens: BTreeSet<&str> = cycle_cands[0]
        .steps
        .iter()
        .map(|s| s.screen.as_str())
        .collect();
    assert_eq!(visited_screens.len(), cycle_cands[0].steps.len());

    // 7. A tiny budget sets `alternatives_complete: false` and the truncation reason.
    let tiny_state_budget = JourneyBudget {
        max_screens: 6,
        max_expanded_states: 1,
        max_calls_per_screen: 64,
        max_candidates: 32,
    };
    assert!(baseline.alternatives_complete);
    assert_eq!(baseline.truncated, None);
    // One expansion already covers the full graph: `/main` is a goal, not expanded.
    let exact = &candidates(&node_refs, &edge_refs, &flows, tiny_state_budget)[0];
    assert!(exact.alternatives_complete);
    assert_eq!(exact.truncated, None);
    // Without the observed write, `/main` would expand towards `/new-alias-result/`.
    let list_only: Vec<Edge> = edges
        .iter()
        .filter(|e| {
            !(e.label == "FETCHES" && e.dst == "ep:simplelogin/app@POST:/api/v2/alias/custom/new")
        })
        .cloned()
        .collect();
    let (_, list_refs) = refs(&nodes, &list_only);
    let tiny_cands = candidates(&node_refs, &list_refs, &flows, tiny_state_budget);
    assert_eq!(tiny_cands.len(), 1);
    assert!(!tiny_cands[0].alternatives_complete);
    assert_eq!(
        tiny_cands[0].truncated,
        Some(TruncationReason::ExpandedStates)
    );

    // 8. Shifting every byte offset leaves `id` unchanged but changes `support_fingerprint`.
    let (shifted_nodes, shifted_edges, shifted_flows) = simplelogin_fixture(500);
    let (shifted_n_refs, shifted_e_refs) = refs(&shifted_nodes, &shifted_edges);
    let shifted = &candidates(
        &shifted_n_refs,
        &shifted_e_refs,
        &shifted_flows,
        JourneyBudget::default(),
    )[0];
    assert_eq!(shifted.id, baseline.id);
    assert_ne!(shifted.support_fingerprint, baseline.support_fingerprint);

    // 9. Changing a step's screen changes `id`.
    let mut renamed_nodes = nodes.clone();
    for n in &mut renamed_nodes {
        if n.id == "screen:simplelogin/extension@/main" {
            n.id = "screen:simplelogin/extension@/home".into();
            n.props["route"] = json!("/home");
        }
    }
    let mut renamed_edges = edges.clone();
    for e in &mut renamed_edges {
        if e.src == "screen:simplelogin/extension@/main" {
            e.src = "screen:simplelogin/extension@/home".into();
        }
        if e.dst == "screen:simplelogin/extension@/main" {
            e.dst = "screen:simplelogin/extension@/home".into();
        }
    }
    let (ren_n_refs, ren_e_refs) = refs(&renamed_nodes, &renamed_edges);
    let renamed = &candidates(&ren_n_refs, &ren_e_refs, &flows, JourneyBudget::default())[0];
    assert_ne!(renamed.id, baseline.id);
}
