//! Unit tests for `journey-candidate@2` and the `journeys` spec artifact
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
            json!({"method": "POST", "straight_line": true}),
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
        "Interpretation: rule journey-candidate@2, InferredWeak, never a confirmed business journey"
    ));
    assert!(artifact.content.contains(&candidate.id));
    assert!(artifact.content.contains(&candidate.support_fingerprint));

    // AC-0857: a reader follows the journey by page, handler, operation and
    // table names; graph ids stay on the `Id` line and in the fact index.
    let content = &artifact.content;
    for line in [
        "**Path:** `/` → `/main` (via `SplashScreen`) → `/new-alias-result/` (via `Main`), then \
         POST /api/v2/alias/custom/new, which writes `Alias`.",
        "| 1 | `/main` | from `SplashScreen` at src/popup/components/SplashScreen.vue (byte 80) | \
         POST /api/v2/aliases, POST /api/v2/alias/custom/new | — |",
        "| 2 | `/new-alias-result/` | from `Main` at src/popup/components/Main.vue (byte 340) | — | — |",
        "- Entry: `/`",
        "- Writes: `new_custom_alias` → `Alias`",
        "- Result: ordered (`/new-alias-result/`)",
        "- Flows: POST /api/v2/alias/custom/new",
        "- Tests: `test_create_custom_alias`",
        "- Earlier assumed goals: POST /api/v2/aliases (unknown)",
    ] {
        assert!(content.contains(line), "{line}\n{content}");
    }
    let readable = &content[..content.find("## Assertions").unwrap()];
    for line in readable.lines().filter(|l| !l.starts_with("- Id: ")) {
        for id in ["screen:", "ep:", "sym:", "data:", "test:", "file:"] {
            assert!(!line.contains(id), "{line}");
        }
    }
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

    // 5b'. (AC-1309, T-1309) The same conditional site under a branch on the fetch's
    // own bound result (`const res = await post(…); if (res.status === 201)`)
    // is ordered, even when the fetch sits in a `try` and is not
    // straight-line, also when the site lists another call too. A `result_of`
    // naming only another call proves nothing.
    for (result_of, expected) in [
        (
            json!([{"byte_start": 100, "byte_end": 170}, {"byte_start": 240, "byte_end": 310}]),
            JourneyResult::Ordered,
        ),
        (
            json!([{"byte_start": 100, "byte_end": 170}]),
            JourneyResult::Unordered,
        ),
    ] {
        let mut edges_branch = edges_conditional.clone();
        for e in &mut edges_branch {
            if e.label == "NAVIGATES_TO"
                && e.dst == "screen:simplelogin/extension@/new-alias-result/"
            {
                e.props["sites"][0]["result_of"] = result_of.clone();
            }
            if e.label == "FETCHES" && e.dst == "ep:simplelogin/app@POST:/api/v2/alias/custom/new" {
                e.props["straight_line"] = json!(false);
            }
        }
        let (_, branch_refs) = refs(&nodes, &edges_branch);
        let branch = &candidates(&node_refs, &branch_refs, &flows, JourneyBudget::default())[0];
        assert_eq!(branch.result, expected, "{result_of}");
        if expected == JourneyResult::Ordered {
            assert_eq!(branch.label, baseline.label);
            let step = branch.result_step.as_ref().unwrap();
            assert_eq!(
                step.screen,
                "screen:simplelogin/extension@/new-alias-result/"
            );
            let site = &step.navigation.as_ref().unwrap().sites_taken[0];
            assert_eq!(
                (site.conditional, site.result_of.clone()),
                (true, vec![(100, 170), (240, 310)])
            );
            assert_eq!(branch.id, baseline.id);
        } else {
            assert!(branch.result_step.is_none());
            assert_eq!(branch.label, "Main → POST /api/v2/alias/custom/new");
        }
    }

    // 5c. (#1012 review) The same unconditional navigation after a call that
    // is not straight-line in the Symbol (`if (flag) await post(…)` and then
    // `router.push(…)`): the navigation can run without the call, so the
    // result is "unordered". A relation that does not record whether the
    // call is straight-line proves no more.
    for straight_line in [Some(false), None] {
        let mut edges_optional = edges.clone();
        for e in &mut edges_optional {
            if e.label == "FETCHES" && e.dst == "ep:simplelogin/app@POST:/api/v2/alias/custom/new" {
                match straight_line {
                    Some(value) => e.props["straight_line"] = json!(value),
                    None => {
                        e.props.as_object_mut().unwrap().remove("straight_line");
                    }
                }
            }
        }
        let (_, optional_refs) = refs(&nodes, &edges_optional);
        let optional = &candidates(&node_refs, &optional_refs, &flows, JourneyBudget::default())[0];
        assert_eq!(
            optional.result,
            JourneyResult::Unordered,
            "{straight_line:?}"
        );
        assert!(optional.result_step.is_none());
        assert_eq!(optional.label, "Main → POST /api/v2/alias/custom/new");
        // The path is the same journey; only its support differs.
        assert_eq!(optional.goal, baseline.goal);
        assert_ne!(optional.support_fingerprint, baseline.support_fingerprint);
    }
    assert_eq!(baseline.result, JourneyResult::Ordered);

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
        max_alternatives: 64,
    };
    assert!(baseline.alternatives_complete);
    assert_eq!(baseline.truncated, None);
    // Under @2 a goal step still expands (ADR-0084), so one expansion no
    // longer covers the full graph; three do: `/`, `/main` and its result.
    let cut = &candidates(&node_refs, &edge_refs, &flows, tiny_state_budget)[0];
    assert!(!cut.alternatives_complete);
    assert_eq!(cut.truncated, Some(TruncationReason::ExpandedStates));
    let exact = &candidates(
        &node_refs,
        &edge_refs,
        &flows,
        JourneyBudget {
            max_expanded_states: 3,
            ..tiny_state_budget
        },
    )[0];
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

// AC-0352 (T-0352, #1022 review): every observed goal on the terminal step is
// a candidate; a `max_screens` cutoff is reported; export mode and Workbench
// rejection apply to the derived candidates.
#[test]
fn journey_candidates_keep_every_terminal_goal_report_depth_cuts_and_honor_curation() {
    let (mut nodes, mut edges, mut flows) = simplelogin_fixture(0);
    let srv = "simplelogin/app";
    let delete = "ep:simplelogin/app@DELETE:/api/aliases/:id";
    let handler = "sym:simplelogin/app@app/api/views/alias.py#delete_alias";
    nodes.push(node(
        delete,
        "Endpoint",
        json!({"method": "DELETE", "path": "/api/aliases/:id"}),
        vec![ev(srv, "app/api/views/alias.py", 400, 480)],
    ));
    nodes.push(node(
        handler,
        "Symbol",
        json!({"name": "delete_alias"}),
        vec![ev(srv, "app/api/views/alias.py", 400, 480)],
    ));
    edges.push(edge(
        "sym:simplelogin/extension@src/popup/components/Main.vue#Main",
        delete,
        "FETCHES",
        json!({"method": "DELETE"}),
        vec![ev(
            "simplelogin/extension",
            "src/popup/components/Main.vue",
            420,
            470,
        )],
    ));
    edges.push(edge(
        handler,
        "data:simplelogin/app@alias",
        "WRITES",
        json!({}),
        vec![ev(srv, "app/api/views/alias.py", 430, 470)],
    ));
    let mut second = flows[0].clone();
    second.trigger = delete.into();
    second.hops[0].src = delete.into();
    second.hops[0].dst = handler.into();
    flows.push(second);
    let (node_refs, edge_refs) = refs(&nodes, &edges);

    let found = candidates(&node_refs, &edge_refs, &flows, JourneyBudget::default());
    let goals: Vec<&str> = found.iter().map(|c| c.goal.endpoint.as_str()).collect();
    assert_eq!(goals.len(), 2);
    assert!(goals.contains(&delete));
    assert!(goals.contains(&"ep:simplelogin/app@POST:/api/v2/alias/custom/new"));
    for c in &found {
        assert_eq!(c.goal.goal_basis, GoalBasis::ObservedWrite);
        // The list call precedes both writes in `Main`.
        assert_eq!(c.earlier_assumed_goals.len(), 1);
    }

    // `/main` at `max_screens: 2` still navigates to `/new-alias-result/`.
    let (base_nodes, base_edges, base_flows) = simplelogin_fixture(0);
    let list_only: Vec<Edge> = base_edges
        .iter()
        .filter(|e| {
            !(e.label == "FETCHES" && e.dst == "ep:simplelogin/app@POST:/api/v2/alias/custom/new")
        })
        .cloned()
        .collect();
    let (base_refs, list_refs) = refs(&base_nodes, &list_only);
    let shallow = JourneyBudget {
        max_screens: 2,
        ..JourneyBudget::default()
    };
    let cut = &candidates(&base_refs, &list_refs, &base_flows, shallow)[0];
    assert_eq!(cut.truncated, Some(TruncationReason::MaxScreens));
    assert!(!cut.alternatives_complete);

    // Verified-only withholds the InferredWeak candidates; a rejected
    // candidate's content hash removes it from a best-effort export.
    let (_, base_edge_refs) = refs(&base_nodes, &base_edges);
    let baseline = &candidates(
        &base_refs,
        &base_edge_refs,
        &base_flows,
        JourneyBudget::default(),
    )[0];
    let journeys = |mode, rejected: &BTreeSet<String>| {
        compile_spec(&base_nodes, &base_edges, &base_flows, mode, rejected)
            .artifacts
            .into_iter()
            .find(|a| a.id == "journeys")
            .unwrap()
    };
    let verified = journeys(ExportMode::VerifiedOnly, &BTreeSet::new());
    assert!(!verified.content.contains(&baseline.label));
    assert!(
        verified
            .content
            .contains("verified-only export withholds them")
    );
    assert!(verified.assertions.is_empty());
    let rejected = BTreeSet::from([baseline.provenance.content_hash.clone()]);
    let curated = journeys(ExportMode::BestEffort, &rejected);
    assert!(!curated.content.contains(&baseline.label));
    assert!(curated.assertions.is_empty());
}

/// Synthetic Android Navigation 3 graph shaped like SimpleLogin Android:
/// - `InitializationDestination` (`initial: true`, `back_stack_owner: AppRootViewModel`, no `RENDERS`).
/// - `AppRootViewModel` has init-phase `NAVIGATES_TO` edges to `HomeDestination` and `LogInDestination` (conditional).
/// - `HomeDestination` `RENDERS` `HomeScreen` and carries `entry_callbacks`:
///   - `onCreateAlias` -> `AppRootViewModel.showCreateAliasScreen` (`read: true`, one forwarded hop)
///   - `onUnread` -> `AppRootViewModel.x` (`read: false`)
/// - `showCreateAliasScreen` `NAVIGATES_TO` `CreateAliasDestination`.
/// - `AppRootViewModel.x` also `NAVIGATES_TO` `CreateAliasDestination` (must never be crossed because `read == false`).
/// - `CreateAliasDestination` `RENDERS` `CreateAliasScreen`, which `CALLS` `CreateAliasViewModel.create`,
///   which `FETCHES` `POST /api/v5/alias/custom/new` (observed write via server flow).
fn simplelogin_android_nav3_fixture() -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
    let repo = "simplelogin/android";
    let srv = "simplelogin/app";
    let root_path = "app/src/main/java/io/simplelogin/android/root/AppRoot.kt";
    let vm_path = "app/src/main/java/io/simplelogin/android/root/AppRootViewModel.kt";
    let home_path = "app/src/main/java/io/simplelogin/android/home/HomeScreen.kt";
    let create_path = "app/src/main/java/io/simplelogin/android/create/CreateAliasScreen.kt";

    let init_screen = "screen:simplelogin/android@navigation3:io.simplelogin.android.root.InitializationDestination";
    let home_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.HomeDestination";
    let login_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.LogInDestination";
    let create_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.CreateAliasDestination";

    let owner_sym = format!("sym:{repo}@{vm_path}#AppRootViewModel");
    let show_create_sym = format!("sym:{repo}@{vm_path}#AppRootViewModel.showCreateAliasScreen");
    let unread_sym = format!("sym:{repo}@{vm_path}#AppRootViewModel.x");
    let home_comp_sym = format!("sym:{repo}@{home_path}#HomeScreen");
    let create_comp_sym = format!("sym:{repo}@{create_path}#CreateAliasScreen");
    let create_call_sym = format!("sym:{repo}@{create_path}#CreateAliasViewModel.create");
    let endpoint_id = "ep:simplelogin/app@POST:/api/v5/alias/custom/new";
    let handler_sym = "sym:simplelogin/app@app/api/views/v5_alias.py#create_custom_alias";
    let data_id = "data:simplelogin/app@alias";

    let nodes = vec![
        node(
            init_screen,
            "Screen",
            json!({
                "route": "navigation3:io.simplelogin.android.root.InitializationDestination",
                "router": "navigation3",
                "initial": true,
                "back_stack_owner": owner_sym,
            }),
            vec![ev(repo, root_path, 100, 160)],
        ),
        node(
            home_screen,
            "Screen",
            json!({
                "route": "navigation3:io.simplelogin.android.root.HomeDestination",
                "router": "navigation3",
                "entry_callbacks": [
                    {
                        "param": "onCreateAlias",
                        "symbol": show_create_sym,
                        "composable": home_comp_sym,
                        "evidence": {
                            "path": root_path,
                            "byte_start": 210,
                            "byte_end": 260
                        },
                        "read": true,
                        "forwarded": [
                            {
                                "composable": "HomeContent",
                                "param": "onAddAlias"
                            }
                        ]
                    },
                    {
                        "param": "onUnread",
                        "symbol": unread_sym,
                        "composable": home_comp_sym,
                        "evidence": {
                            "path": root_path,
                            "byte_start": 265,
                            "byte_end": 295
                        },
                        "read": false,
                        "forwarded": []
                    }
                ]
            }),
            vec![ev(repo, root_path, 180, 310)],
        ),
        node(
            login_screen,
            "Screen",
            json!({
                "route": "navigation3:io.simplelogin.android.root.LogInDestination",
                "router": "navigation3",
            }),
            vec![ev(repo, root_path, 320, 380)],
        ),
        node(
            create_screen,
            "Screen",
            json!({
                "route": "navigation3:io.simplelogin.android.root.CreateAliasDestination",
                "router": "navigation3",
            }),
            vec![ev(repo, root_path, 400, 470)],
        ),
        node(
            &owner_sym,
            "Symbol",
            json!({"name": "AppRootViewModel"}),
            vec![ev(repo, vm_path, 10, 500)],
        ),
        node(
            &show_create_sym,
            "Symbol",
            json!({"name": "showCreateAliasScreen"}),
            vec![ev(repo, vm_path, 220, 310)],
        ),
        node(
            &unread_sym,
            "Symbol",
            json!({"name": "x"}),
            vec![ev(repo, vm_path, 320, 380)],
        ),
        node(
            &home_comp_sym,
            "Symbol",
            json!({"name": "HomeScreen"}),
            vec![ev(repo, home_path, 10, 200)],
        ),
        node(
            &create_comp_sym,
            "Symbol",
            json!({"name": "CreateAliasScreen"}),
            vec![ev(repo, create_path, 10, 200)],
        ),
        node(
            &create_call_sym,
            "Symbol",
            json!({"name": "create"}),
            vec![ev(repo, create_path, 210, 360)],
        ),
        node(
            endpoint_id,
            "Endpoint",
            json!({
                "method": "POST",
                "path": "/api/v5/alias/custom/new",
                "name": "POST /api/v5/alias/custom/new"
            }),
            vec![ev(srv, "app/api/views/v5_alias.py", 100, 220)],
        ),
        node(
            handler_sym,
            "Symbol",
            json!({"name": "create_custom_alias"}),
            vec![ev(srv, "app/api/views/v5_alias.py", 100, 220)],
        ),
        node(
            data_id,
            "DataEntity",
            json!({"name": "Alias"}),
            vec![ev(srv, "app/models.py", 50, 120)],
        ),
    ];

    let edges = vec![
        edge(
            &owner_sym,
            home_screen,
            "NAVIGATES_TO",
            json!({
                "conditional": true,
                "sites": [{
                    "path": vm_path,
                    "byte_start": 80,
                    "byte_end": 120,
                    "conditional": true,
                    "via": "back_stack_add",
                    "phase": "init"
                }]
            }),
            vec![ev(repo, vm_path, 80, 120)],
        ),
        edge(
            &owner_sym,
            login_screen,
            "NAVIGATES_TO",
            json!({
                "conditional": true,
                "sites": [{
                    "path": vm_path,
                    "byte_start": 130,
                    "byte_end": 170,
                    "conditional": true,
                    "via": "back_stack_add",
                    "phase": "init"
                }]
            }),
            vec![ev(repo, vm_path, 130, 170)],
        ),
        edge(
            home_screen,
            &home_comp_sym,
            "RENDERS",
            json!({}),
            vec![ev(repo, root_path, 195, 295)],
        ),
        edge(
            &show_create_sym,
            create_screen,
            "NAVIGATES_TO",
            json!({
                "conditional": false,
                "sites": [{
                    "path": vm_path,
                    "byte_start": 250,
                    "byte_end": 295,
                    "conditional": false,
                    "via": "back_stack_builder_helper"
                }]
            }),
            vec![ev(repo, vm_path, 250, 295)],
        ),
        edge(
            &unread_sym,
            create_screen,
            "NAVIGATES_TO",
            json!({
                "conditional": false,
                "sites": [{
                    "path": vm_path,
                    "byte_start": 340,
                    "byte_end": 370,
                    "conditional": false,
                    "via": "back_stack_add"
                }]
            }),
            vec![ev(repo, vm_path, 340, 370)],
        ),
        edge(
            create_screen,
            &create_comp_sym,
            "RENDERS",
            json!({}),
            vec![ev(repo, root_path, 415, 455)],
        ),
        edge(
            &create_comp_sym,
            &create_call_sym,
            "CALLS",
            json!({}),
            vec![ev(repo, create_path, 90, 140)],
        ),
        edge(
            &create_call_sym,
            endpoint_id,
            "FETCHES",
            json!({"method": "POST"}),
            vec![ev(repo, create_path, 260, 330)],
        ),
        edge(
            handler_sym,
            data_id,
            "WRITES",
            json!({}),
            vec![ev(srv, "app/api/views/v5_alias.py", 160, 205)],
        ),
    ];

    let hop_prov = prov(
        vec![ev(srv, "app/api/views/v5_alias.py", 100, 220)],
        "nav3-flow-hop",
    );
    let flows = vec![Flow {
        trigger: endpoint_id.into(),
        trigger_kind: "Endpoint".into(),
        trigger_name: "POST /api/v5/alias/custom/new".into(),
        hops: vec![Hop {
            label: "HANDLES".into(),
            src: endpoint_id.into(),
            dst: handler_sym.into(),
            src_name: "POST /api/v5/alias/custom/new".into(),
            dst_name: "create_custom_alias".into(),
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

// AC-0352 (T-0352): Navigation 3 initial destinations, startup navigations,
// entry callbacks, unread callback records, search summaries, and artifact tables.
#[test]
fn journey_candidates_cross_navigation3_startup_and_entry_callbacks_as_assumptions() {
    let (nodes, edges, flows) = simplelogin_android_nav3_fixture();
    let (node_refs, edge_refs) = refs(&nodes, &edges);

    let init_screen = "screen:simplelogin/android@navigation3:io.simplelogin.android.root.InitializationDestination";
    let home_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.HomeDestination";
    let create_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.CreateAliasDestination";
    let root_path = "app/src/main/java/io/simplelogin/android/root/AppRoot.kt";
    let vm_path = "app/src/main/java/io/simplelogin/android/root/AppRootViewModel.kt";
    let home_path = "app/src/main/java/io/simplelogin/android/home/HomeScreen.kt";

    let owner_sym = format!("sym:simplelogin/android@{vm_path}#AppRootViewModel");
    let show_create_sym =
        format!("sym:simplelogin/android@{vm_path}#AppRootViewModel.showCreateAliasScreen");
    let unread_sym = format!("sym:simplelogin/android@{vm_path}#AppRootViewModel.x");
    let home_comp_sym = format!("sym:simplelogin/android@{home_path}#HomeScreen");

    // 1. One candidate goes Initialization -> Home -> CreateAlias with an observed-write goal,
    // startup assumption at step 1, entry_callback assumption at step 2, and InferredWeak confidence.
    let result = search(&node_refs, &edge_refs, &flows, JourneyBudget::default());
    assert_eq!(result.candidates.len(), 1);
    let candidate = &result.candidates[0];
    assert_eq!(candidate.entry, init_screen);
    assert_eq!(candidate.goal.goal_basis, GoalBasis::ObservedWrite);
    assert_eq!(
        candidate.goal.endpoint,
        "ep:simplelogin/app@POST:/api/v5/alias/custom/new"
    );
    let step_screens: Vec<&str> = candidate.steps.iter().map(|s| s.screen.as_str()).collect();
    assert_eq!(step_screens, vec![init_screen, home_screen, create_screen]);
    assert_eq!(
        candidate.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        candidate.assumptions,
        vec![
            JourneyAssumption {
                kind: AssumptionKind::Startup,
                step: 1,
                screen: init_screen.into(),
                symbol: owner_sym.clone(),
                interface: None,
                param: None,
                composable: None,
                registration: None,
                forwarded: Vec::new(),
                limitation: STARTUP_LIMITATION.into(),
            },
            JourneyAssumption {
                kind: AssumptionKind::EntryCallback,
                step: 2,
                screen: home_screen.into(),
                symbol: show_create_sym.clone(),
                interface: None,
                param: Some("onCreateAlias".into()),
                composable: Some(home_comp_sym.clone()),
                registration: Some(ev("simplelogin/android", root_path, 210, 260)),
                forwarded: vec![ForwardHop {
                    composable: "HomeContent".into(),
                    param: "onAddAlias".into(),
                }],
                limitation: ENTRY_CALLBACK_LIMITATION.into(),
            },
        ]
    );

    // The registration is the evidence tying Home to its callback, so the
    // candidate cites it.
    assert!(candidate.provenance.evidence.contains(&ev(
        "simplelogin/android",
        root_path,
        210,
        260
    )));

    // 2. The unread record is listed on step 1 (Home) and never crossed.
    for c in &result.candidates {
        assert!(
            c.steps
                .iter()
                .all(|s| s.navigation.as_ref().is_none_or(|n| n.source != unread_sym)),
            "an unread callback must never be crossed"
        );
        assert!(c.assumptions.iter().all(|a| a.symbol != unread_sym));
    }
    assert!(candidate.steps[0].unread_callbacks.is_empty());
    assert_eq!(
        candidate.steps[1].unread_callbacks,
        vec![UnreadCallback {
            param: "onUnread".into(),
            composable: home_comp_sym.clone(),
            symbol: unread_sym.clone(),
            registration: ev("simplelogin/android", root_path, 265, 295),
        }]
    );
    assert!(candidate.steps[2].unread_callbacks.is_empty());
    assert_eq!(candidate.alternatives, 0);
    assert_eq!(
        candidate.steps[2].navigation.as_ref().unwrap().source,
        show_create_sym
    );

    // 3. Proven beats assumed: if HomeScreen itself CALLS showCreateAliasScreen,
    // the step 2 assumption disappears and the candidate id is unchanged.
    let mut proven_edges = edges.clone();
    proven_edges.push(edge(
        &home_comp_sym,
        &show_create_sym,
        "CALLS",
        json!({}),
        vec![ev("simplelogin/android", home_path, 50, 90)],
    ));
    let (_, proven_edge_refs) = refs(&nodes, &proven_edges);
    let proven_cands = candidates(
        &node_refs,
        &proven_edge_refs,
        &flows,
        JourneyBudget::default(),
    );
    assert_eq!(proven_cands.len(), 1);
    assert_eq!(proven_cands[0].id, candidate.id);
    assert_eq!(
        proven_cands[0].assumptions,
        vec![JourneyAssumption {
            kind: AssumptionKind::Startup,
            step: 1,
            screen: init_screen.into(),
            symbol: owner_sym,
            interface: None,
            param: None,
            composable: None,
            registration: None,
            forwarded: Vec::new(),
            limitation: STARTUP_LIMITATION.into(),
        }]
    );

    // 4. Fingerprint: changing the assumption data (e.g. forwarded hops) changes
    // support_fingerprint without changing id.
    let mut hop_nodes = nodes.clone();
    for n in &mut hop_nodes {
        if n.id == home_screen {
            n.props["entry_callbacks"][0]["forwarded"] = json!([
                {"composable": "HomeContent", "param": "onAddAlias"},
                {"composable": "AliasFab", "param": "onClick"}
            ]);
        }
    }
    let (hop_node_refs, _) = refs(&hop_nodes, &edges);
    let hop_cand = &candidates(&hop_node_refs, &edge_refs, &flows, JourneyBudget::default())[0];
    assert_eq!(hop_cand.id, candidate.id);
    assert_ne!(hop_cand.support_fingerprint, candidate.support_fingerprint);

    // 4b. A second read registration of the same callback (another parameter
    // passing showCreateAliasScreen) supports the same step: both are listed
    // and cited, the path and id are unchanged.
    let mut twin_nodes = nodes.clone();
    for n in &mut twin_nodes {
        if n.id == home_screen {
            n.props["entry_callbacks"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "param": "onFab",
                    "symbol": show_create_sym,
                    "composable": home_comp_sym,
                    "evidence": {"path": root_path, "byte_start": 300, "byte_end": 330},
                    "read": true,
                    "forwarded": []
                }));
        }
    }
    let (twin_node_refs, _) = refs(&twin_nodes, &edges);
    let twin = &candidates(
        &twin_node_refs,
        &edge_refs,
        &flows,
        JourneyBudget::default(),
    )[0];
    assert_eq!(twin.id, candidate.id);
    let twin_params: Vec<(usize, Option<&str>)> = twin
        .assumptions
        .iter()
        .filter(|a| a.kind == AssumptionKind::EntryCallback)
        .map(|a| (a.step, a.param.as_deref()))
        .collect();
    assert_eq!(
        twin_params,
        vec![(2, Some("onCreateAlias")), (2, Some("onFab"))]
    );
    for span in [(210, 260), (300, 330)] {
        assert!(twin.provenance.evidence.contains(&ev(
            "simplelogin/android",
            root_path,
            span.0,
            span.1
        )));
    }
    assert_ne!(twin.support_fingerprint, candidate.support_fingerprint);

    // 5. Path routers unchanged: an existing path-router fixture yields identical
    // candidates, ids, and fingerprints through `candidates` and `search`.
    let (pr_nodes, pr_edges, pr_flows) = simplelogin_fixture(0);
    let (pr_node_refs, pr_edge_refs) = refs(&pr_nodes, &pr_edges);
    let pr_from_candidates = candidates(
        &pr_node_refs,
        &pr_edge_refs,
        &pr_flows,
        JourneyBudget::default(),
    );
    let pr_from_search = search(
        &pr_node_refs,
        &pr_edge_refs,
        &pr_flows,
        JourneyBudget::default(),
    );
    assert_eq!(pr_from_candidates, pr_from_search.candidates);
    assert_eq!(pr_from_candidates.len(), 1);
    assert!(pr_from_candidates[0].assumptions.is_empty());
    assert!(
        pr_from_candidates[0]
            .steps
            .iter()
            .all(|s| s.unread_callbacks.is_empty())
    );
    assert_eq!(pr_from_search.entries.len(), 1);
    assert_eq!(pr_from_search.entries[0].assumed_navigations, 0);
    assert_eq!(pr_from_search.entries[0].unread_callbacks, 0);

    // 6. Search summary for the initial entry has assumed_navigations >= 2,
    // unread_callbacks == 1, and candidates == 1.
    assert_eq!(result.entries.len(), 1);
    let summary = &result.entries[0];
    assert_eq!(summary.entry, init_screen);
    assert!(
        summary.assumed_navigations >= 2,
        "expected >= 2 assumed navigations, got {}",
        summary.assumed_navigations
    );
    assert_eq!(summary.assumed_navigations, 3);
    assert_eq!(summary.unread_callbacks, 1);
    assert_eq!(summary.candidates, 1);
    assert_eq!(summary.truncated, None);

    // 7. Artifact: best-effort contains `## Search`, `### Assumptions`, and the unread callback;
    // verified-only output is unchanged.
    let best_effort = compile_spec(
        &nodes,
        &edges,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    )
    .artifacts
    .into_iter()
    .find(|a| a.id == "journeys")
    .expect("journeys artifact");
    assert!(best_effort.content.contains("## Search\n"));
    assert!(best_effort.content.contains(
        "| Entry | Expanded | Candidates | Truncated | Assumed navigations | Unread callbacks |"
    ));
    assert!(best_effort.content.contains("### Assumptions\n"));
    assert!(best_effort.content.contains(
        "| Step | Kind | Screen | Symbol | Param | Composable | Registration | Forwarded | Limitation |"
    ));
    assert!(best_effort.content.contains("HomeContent.onAddAlias"));
    assert!(
        best_effort
            .content
            .contains(&format!("| {root_path}:210-260 |"))
    );
    assert!(
        best_effort
            .content
            .contains("unread callback: onUnread (`HomeScreen`)")
    );
    // AC-0857: assumptions, unresolved items and unread callbacks read by
    // name too; ids stay on the `Id` line.
    let readable = &best_effort.content[..best_effort.content.find("## Assertions").unwrap()];
    for line in readable.lines().filter(|l| !l.starts_with("- Id: ")) {
        for id in ["screen:", "ep:", "sym:", "data:", "test:", "file:", "gap:"] {
            assert!(!line.contains(id), "{line}\n{readable}");
        }
    }

    let verified_only = compile_spec(
        &nodes,
        &edges,
        &flows,
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
    )
    .artifacts
    .into_iter()
    .find(|a| a.id == "journeys")
    .expect("journeys artifact");
    assert!(!verified_only.content.contains("## Search"));
    assert!(!verified_only.content.contains("### Assumptions"));
    assert!(
        verified_only
            .content
            .contains("verified-only export withholds them")
    );
}

// AC-0355 (T-0355): a Hilt `@Binds` hop from an interface member to its bound
// implementation is crossed only as a `hilt_dispatch` assumption; proven code
// dominates and nothing is reached without the binding.
#[test]
fn journey_candidates_cross_hilt_dispatch_as_an_assumption() {
    let repo = "simplelogin/android";
    let create_path = "app/src/main/java/io/simplelogin/android/create/CreateAliasScreen.kt";
    let ds_path = "core/network/src/main/java/io/simplelogin/core/network/datasource/Creation.kt";
    let module_path = "core/network/src/main/java/io/simplelogin/core/network/Module.kt";
    let create_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.CreateAliasDestination";
    let create_call_sym = format!("sym:{repo}@{create_path}#CreateAliasViewModel.create");
    let iface_sym = format!("sym:{repo}@{ds_path}#CreationRemoteDatasource.create");
    let impl_sym = format!("sym:{repo}@{ds_path}#CreationRemoteDatasourceImpl.create");
    let endpoint_id = "ep:simplelogin/app@POST:/api/v5/alias/custom/new";
    let binding = ev(repo, module_path, 400, 480);

    // The view model calls the interface; only the implementation fetches.
    let (mut nodes, base_edges, flows) = simplelogin_android_nav3_fixture();
    for (id, name, start) in [(&iface_sym, "create", 10), (&impl_sym, "create", 100)] {
        nodes.push(node(
            id,
            "Symbol",
            json!({ "name": name }),
            vec![ev(repo, ds_path, start, start + 60)],
        ));
    }
    let mut edges: Vec<Edge> = base_edges
        .into_iter()
        .filter(|e| !(e.src == create_call_sym && e.label == "FETCHES"))
        .collect();
    edges.push(edge(
        &create_call_sym,
        &iface_sym,
        "CALLS",
        json!({"resolution": "declared-type"}),
        vec![ev(repo, create_path, 230, 260)],
    ));
    edges.push(edge(
        &impl_sym,
        endpoint_id,
        "FETCHES",
        json!({"method": "POST"}),
        vec![ev(repo, ds_path, 120, 150)],
    ));
    let without_binding = edges.clone();
    let dispatch_prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::InferredStrong,
        vec![binding.clone()],
        "test.fixture",
        b"hilt",
    )
    .unwrap();
    edges.push(Edge {
        src: iface_sym.clone(),
        dst: impl_sym.clone(),
        label: "CALLS".into(),
        props: json!({
            "resolution": "hilt-binds",
            "assumption": "hilt_dispatch",
            "prov": dispatch_prov,
        }),
    });

    // 1. The goal is reached through the binding, recorded on the step whose
    // call relies on it, and cited.
    let (node_refs, edge_refs) = refs(&nodes, &edges);
    let result = search(&node_refs, &edge_refs, &flows, JourneyBudget::default());
    assert_eq!(result.candidates.len(), 1);
    let candidate = &result.candidates[0];
    assert_eq!(candidate.goal.endpoint, endpoint_id);
    assert_eq!(candidate.steps[2].screen, create_screen);
    assert_eq!(candidate.steps[2].calls[0].source, impl_sym);
    let hilt: Vec<&JourneyAssumption> = candidate
        .assumptions
        .iter()
        .filter(|a| a.kind == AssumptionKind::HiltDispatch)
        .collect();
    assert_eq!(
        hilt,
        vec![&JourneyAssumption {
            kind: AssumptionKind::HiltDispatch,
            step: 2,
            screen: create_screen.into(),
            symbol: impl_sym.clone(),
            interface: Some(iface_sym.clone()),
            param: None,
            composable: None,
            registration: Some(binding.clone()),
            forwarded: Vec::new(),
            limitation: HILT_DISPATCH_LIMITATION.into(),
        }]
    );
    assert!(candidate.provenance.evidence.contains(&binding));
    assert_eq!(
        candidate.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );

    // 2. Without the binding the implementation is unreachable: no goal.
    let (_, unbound_refs) = refs(&nodes, &without_binding);
    assert!(candidates(&node_refs, &unbound_refs, &flows, JourneyBudget::default()).is_empty());

    // 3. Proven beats assumed: a direct call to the implementation removes
    // the hilt_dispatch assumption without changing the candidate id.
    let mut proven = edges.clone();
    proven.push(edge(
        &create_call_sym,
        &impl_sym,
        "CALLS",
        json!({}),
        vec![ev(repo, create_path, 270, 300)],
    ));
    let (_, proven_refs) = refs(&nodes, &proven);
    let proven_cands = candidates(&node_refs, &proven_refs, &flows, JourneyBudget::default());
    assert_eq!(proven_cands.len(), 1);
    assert_eq!(proven_cands[0].id, candidate.id);
    assert!(
        proven_cands[0]
            .assumptions
            .iter()
            .all(|a| a.kind != AssumptionKind::HiltDispatch)
    );

    // 4. The artifact row names the interface it crossed from.
    let artifact = compile_spec(
        &nodes,
        &edges,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    )
    .artifacts
    .into_iter()
    .find(|a| a.id == "journeys")
    .expect("journeys artifact");
    assert!(
        artifact
            .content
            .contains("`CreationRemoteDatasource.create → CreationRemoteDatasourceImpl.create`")
    );
    assert!(artifact.content.contains("`hilt_dispatch`"));

    // 5. A nested dispatch is not required when an ordinary route reaches
    // the same implementation: `impl` calls interface `B.go` (bound to
    // `BImpl.go`) and also `Helper.run`, which calls `BImpl.go` directly.
    // `B.go` sorts first, so its dispatch route is queued before the helper's.
    let b_iface = format!("sym:{repo}@{ds_path}#B.go");
    let b_impl = format!("sym:{repo}@{ds_path}#BImpl.go");
    let helper = format!("sym:{repo}@{ds_path}#Helper.run");
    let mut nested_nodes = nodes.clone();
    for (id, name, start) in [
        (&b_iface, "go", 200),
        (&b_impl, "go", 300),
        (&helper, "run", 400),
    ] {
        nested_nodes.push(node(
            id,
            "Symbol",
            json!({ "name": name }),
            vec![ev(repo, ds_path, start, start + 60)],
        ));
    }
    let mut nested: Vec<Edge> = edges
        .iter()
        .filter(|e| !(e.src == impl_sym && e.label == "FETCHES"))
        .cloned()
        .collect();
    for (src, dst) in [
        (&impl_sym, &b_iface),
        (&impl_sym, &helper),
        (&helper, &b_impl),
    ] {
        nested.push(edge(
            src,
            dst,
            "CALLS",
            json!({}),
            vec![ev(repo, ds_path, 500, 520)],
        ));
    }
    nested.push(Edge {
        src: b_iface.clone(),
        dst: b_impl.clone(),
        label: "CALLS".into(),
        props: json!({
            "resolution": "hilt-binds",
            "assumption": "hilt_dispatch",
            "prov": Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::InferredStrong,
                vec![ev(repo, module_path, 600, 680)],
                "test.fixture",
                b"hilt-b",
            )
            .unwrap(),
        }),
    });
    nested.push(edge(
        &b_impl,
        endpoint_id,
        "FETCHES",
        json!({"method": "POST"}),
        vec![ev(repo, ds_path, 320, 350)],
    ));
    let (nested_node_refs, nested_refs) = refs(&nested_nodes, &nested);
    let nested_cands = candidates(
        &nested_node_refs,
        &nested_refs,
        &flows,
        JourneyBudget::default(),
    );
    assert_eq!(nested_cands.len(), 1);
    let dispatches: Vec<&str> = nested_cands[0]
        .assumptions
        .iter()
        .filter(|a| a.kind == AssumptionKind::HiltDispatch)
        .map(|a| a.symbol.as_str())
        .collect();
    assert_eq!(dispatches, vec![impl_sym.as_str()]);
}

// AC-0356 (T-0356): an endpoint an anchor's flow fetches has no flow of its
// own; its observed writes are those the flow reaches downstream of it.
#[test]
fn journey_goals_observe_writes_downstream_of_an_endpoint_inside_an_anchor_flow() {
    let (mut nodes, mut edges, flows) = simplelogin_android_nav3_fixture();
    let repo = "simplelogin/android";
    let create_path = "app/src/main/java/io/simplelogin/android/create/CreateAliasScreen.kt";
    let create_screen =
        "screen:simplelogin/android@navigation3:io.simplelogin.android.root.CreateAliasDestination";
    let create_call_sym = format!("sym:{repo}@{create_path}#CreateAliasViewModel.create");
    let endpoint_id = "ep:simplelogin/app@POST:/api/v5/alias/custom/new";
    let handler_sym = "sym:simplelogin/app@app/api/views/v5_alias.py#create_custom_alias";
    let hop = |label: &str, src: &str, dst: &str| Hop {
        label: label.into(),
        src: src.into(),
        dst: dst.into(),
        src_name: src.into(),
        dst_name: dst.into(),
        tier: "Deterministic".into(),
        confidence: "Confirmed".into(),
        evidence: None,
        provenance: prov(vec![ev(repo, create_path, 1, 2)], label),
        gap_reason: None,
        attempted_tiers: vec![],
    };
    // flowtracer shape: the screen's flow reaches the endpoint, so the
    // endpoint gets no flow of its own.
    let mut anchor = flows[0].clone();
    anchor.trigger = create_screen.into();
    anchor.trigger_kind = "Screen".into();
    anchor.hops = vec![
        hop("CALLS", create_screen, &create_call_sym),
        hop("FETCHES", &create_call_sym, endpoint_id),
        hop("HANDLES", endpoint_id, handler_sym),
    ];
    let anchored = vec![anchor];

    let (node_refs, edge_refs) = refs(&nodes, &edges);
    let found = candidates(&node_refs, &edge_refs, &anchored, JourneyBudget::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].goal.goal_basis, GoalBasis::ObservedWrite);
    assert_eq!(
        found[0].goal.writes,
        vec![WriteFact {
            symbol: handler_sym.into(),
            data_entity: "data:simplelogin/app@alias".into(),
        }]
    );

    // A write upstream of the endpoint (client side) is not the endpoint's.
    edges.retain(|e| e.label != "WRITES");
    nodes.push(node(
        "data:simplelogin/android@room:draft",
        "DataEntity",
        json!({"name": "Draft"}),
        vec![ev(repo, create_path, 5, 9)],
    ));
    edges.push(edge(
        &create_call_sym,
        "data:simplelogin/android@room:draft",
        "WRITES",
        json!({}),
        vec![ev(repo, create_path, 5, 9)],
    ));
    let (node_refs, edge_refs) = refs(&nodes, &edges);
    let upstream = candidates(&node_refs, &edge_refs, &anchored, JourneyBudget::default());
    assert_eq!(upstream.len(), 1);
    assert_eq!(upstream[0].goal.goal_basis, GoalBasis::MethodAssumed);
}

// AC-0855: server-rendered Flask pages from the real Python adapter are
// walked like client screens: a page's handler, its templates and what they
// include form its closure, a redirect or template link is a navigation, and
// a form submission is a goal call.
#[test]
fn journey_candidates_walk_server_rendered_flask_pages() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "app/main.py",
        r#"
from flask import Blueprint, Flask, jsonify, redirect, render_template, url_for

auth_bp = Blueprint("auth", __name__)
dashboard_bp = Blueprint("dashboard", __name__)

@auth_bp.route("/login", methods=["GET", "POST"])
def login():
    if form.validate_on_submit():
        return redirect(url_for("dashboard.index"))
    return render_template("auth/login.html")

@dashboard_bp.route("/")
def index():
    return render_template("dashboard/index.html")

@dashboard_bp.route("/aliases", methods=["POST"])
def create_alias():
    return jsonify(ok=True)

app = Flask(__name__)
app.register_blueprint(auth_bp, url_prefix="/auth")
app.register_blueprint(dashboard_bp, url_prefix="/dashboard")

@app.route("/")
def home():
    if current_user.is_authenticated:
        return redirect(url_for("dashboard.index"))
    return redirect(url_for("auth.login"))
"#,
    );
    write(
        "templates/base.html",
        "<html>{% block content %}{% endblock %}</html>\n",
    );
    write(
        "templates/auth/login.html",
        "{% extends \"base.html\" %}{% block content %}<form method=\"post\"></form>{% endblock %}\n",
    );
    write(
        "templates/dashboard/index.html",
        "{% extends \"base.html\" %}{% block content %}\
         <form action=\"{{ url_for('dashboard.create_alias') }}\" method=\"post\"></form>\
         {% endblock %}\n",
    );
    let out = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: "local/pages",
            commit: "abc123",
        },
    )
    .unwrap();
    let flows = flowtracer::trace(&out.nodes, &out.edges);
    // A page is not a flow anchor: its GET route already traces its handler.
    assert!(flows.iter().all(|f| !f.trigger.starts_with("screen:")));
    let (nodes, edges) = refs(&out.nodes, &out.edges);
    let search = search(&nodes, &edges, &flows, JourneyBudget::default());
    let mut labels: Vec<(&str, Vec<&str>)> = search
        .candidates
        .iter()
        .map(|c| {
            (
                c.label.as_str(),
                c.steps.iter().map(|s| s.screen.as_str()).collect(),
            )
        })
        .collect();
    labels.sort();
    assert_eq!(
        labels,
        [
            (
                "auth.login → POST /auth/login",
                vec![
                    "screen:local/pages@flask:/",
                    "screen:local/pages@flask:/auth/login"
                ],
            ),
            (
                "dashboard.index → POST /dashboard/aliases",
                vec![
                    "screen:local/pages@flask:/",
                    "screen:local/pages@flask:/dashboard/"
                ],
            ),
        ]
    );
    // The submission cites the form in the template that renders it.
    let login = &search.candidates[0];
    let call = &login.steps[1].calls[0];
    assert_eq!(call.method, "POST");
    assert_eq!(
        call.evidence.as_ref().unwrap().path,
        "templates/auth/login.html"
    );
}

/// `journey-candidate@2` (ADR-0084, AC-1310, T-1310): a path that crosses an
/// observed write keeps going, and a path to the same goal through a screen
/// the primary skips is kept as an alternative of the primary.
#[test]
fn journey_candidates_continue_past_writes_and_keep_alternatives_through_skipped_screens() {
    let repo = "simplelogin/extension";
    let srv = "simplelogin/app";
    let (mut nodes, mut edges, mut flows) = simplelogin_fixture(0);
    let login = "screen:simplelogin/extension@/login";
    let login_file = "file:simplelogin/extension@src/popup/components/Login.vue";
    let login_sym = "sym:simplelogin/extension@src/popup/components/Login.vue#Login";
    let login_ep = "ep:simplelogin/app@POST:/api/auth/login";
    let login_handler = "sym:simplelogin/app@app/api/views/auth.py#auth_login";
    let login_path = "src/popup/components/Login.vue";
    nodes.extend([
        node(
            login,
            "Screen",
            json!({"route": "/login", "router": "vue-router"}),
            vec![ev(repo, "src/popup/router/index.js", 150, 180)],
        ),
        node(
            login_file,
            "File",
            json!({"path": login_path, "name": "Login.vue"}),
            vec![ev(repo, login_path, 0, 400)],
        ),
        node(
            login_sym,
            "Symbol",
            json!({"name": "Login"}),
            vec![ev(repo, login_path, 10, 390)],
        ),
        node(
            login_ep,
            "Endpoint",
            json!({"method": "POST", "path": "/api/auth/login", "name": "POST /api/auth/login"}),
            vec![ev(srv, "app/api/views/auth.py", 10, 90)],
        ),
        node(
            login_handler,
            "Symbol",
            json!({"name": "auth_login"}),
            vec![ev(srv, "app/api/views/auth.py", 10, 90)],
        ),
        node(
            "data:simplelogin/app@api_key",
            "DataEntity",
            json!({"name": "ApiKey"}),
            vec![ev(srv, "app/models.py", 200, 260)],
        ),
    ]);
    let nav = |dst: &str, start: u64| {
        let props = json!({"conditional": false, "sites": [{"path": login_path, "byte_start": start, "byte_end": start + 30, "conditional": false}]});
        edge(
            login_sym,
            dst,
            "NAVIGATES_TO",
            props,
            vec![ev(repo, login_path, start, start + 30)],
        )
    };
    edges.extend([
        edge(login, login_file, "RENDERS", json!({}), vec![ev(repo, "src/popup/router/index.js", 150, 180)]),
        edge(login_sym, login_file, "DEFINED_IN", json!({}), vec![ev(repo, login_path, 10, 390)]),
        edge(
            "sym:simplelogin/extension@src/popup/components/SplashScreen.vue#SplashScreen",
            login,
            "NAVIGATES_TO",
            json!({"conditional": true, "sites": [{"path": "src/popup/components/SplashScreen.vue", "byte_start": 120, "byte_end": 150, "conditional": true}]}),
            vec![ev(repo, "src/popup/components/SplashScreen.vue", 120, 150)],
        ),
        edge(login_sym, login_ep, "FETCHES", json!({"method": "POST", "straight_line": true}), vec![ev(repo, login_path, 50, 90)]),
        nav("screen:simplelogin/extension@/main", 200),
        edge(login_handler, "data:simplelogin/app@api_key", "WRITES", json!({}), vec![ev(srv, "app/api/views/auth.py", 40, 70)]),
    ]);
    let mut login_flow = flows[0].clone();
    login_flow.trigger = login_ep.into();
    login_flow.trigger_name = "POST /api/auth/login".into();
    login_flow.hops[0].src = login_ep.into();
    login_flow.hops[0].dst = login_handler.into();
    login_flow.hops[0].src_name = "POST /api/auth/login".into();
    login_flow.hops[0].dst_name = "auth_login".into();
    flows.push(login_flow);
    let (node_refs, edge_refs) = refs(&nodes, &edges);
    let found = candidates(&node_refs, &edge_refs, &flows, JourneyBudget::default());

    let custom = "ep:simplelogin/app@POST:/api/v2/alias/custom/new";
    fn screens(c: &JourneyCandidate) -> Vec<&str> {
        c.steps.iter().map(|s| s.screen.as_str()).collect()
    }
    let primaries: Vec<_> = found
        .iter()
        .filter(|c| c.alternative_of.is_none())
        .collect();
    // The @1 primaries are unchanged: the login write, and the shortest custom-alias path.
    assert_eq!(primaries.len(), 2);
    let primary = primaries
        .iter()
        .find(|c| c.goal.endpoint == custom)
        .unwrap();
    assert_eq!(
        screens(primary)[..2],
        [
            "screen:simplelogin/extension@/",
            "screen:simplelogin/extension@/main"
        ]
    );
    assert_eq!(primary.alternatives, 1);
    assert!(primaries.iter().any(|c| c.goal.endpoint == login_ep));
    // The path through the login write is kept, tied to its primary.
    let alternatives: Vec<_> = found
        .iter()
        .filter(|c| c.alternative_of.is_some())
        .collect();
    assert_eq!(alternatives.len(), 1);
    let alt = alternatives[0];
    assert_eq!(alt.alternative_of.as_deref(), Some(primary.id.as_str()));
    assert_eq!(alt.goal.endpoint, custom);
    assert_eq!(
        screens(alt)[..3],
        [
            "screen:simplelogin/extension@/",
            login,
            "screen:simplelogin/extension@/main"
        ]
    );
    assert_ne!(alt.id, primary.id);

    // Rendered: the alternative names the screen it adds and its primary.
    let rendered = compile_spec(
        &nodes,
        &edges,
        &flows,
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let journeys = rendered
        .artifacts
        .iter()
        .find(|a| a.id == "journeys")
        .unwrap();
    assert!(
        journeys.content.contains("NewAliasResult (via `/login`)\n"),
        "{}",
        journeys.content
    );
    assert!(journeys.content.contains(&format!(
        "- Alternative of: {} (`{}`)",
        primary.label, primary.id
    )));

    // A rejected primary still leaves its alternative named as one.
    let rejected = BTreeSet::from([primary.provenance.content_hash.clone()]);
    let curated = compile_spec(&nodes, &edges, &flows, ExportMode::BestEffort, &rejected);
    let curated = curated
        .artifacts
        .iter()
        .find(|a| a.id == "journeys")
        .unwrap();
    assert!(!curated.content.contains(&format!("- Id: `{}`", primary.id)));
    assert!(curated.content.contains("NewAliasResult (via `/login`)\n"));
    assert!(curated.content.contains(&format!(
        "- Alternative of: {} (`{}`)",
        primary.label, primary.id
    )));

    // An alternatives cut marks every candidate the entry keeps, primaries too.
    let no_alternatives = JourneyBudget {
        max_alternatives: 0,
        ..JourneyBudget::default()
    };
    let cut = candidates(&node_refs, &edge_refs, &flows, no_alternatives);
    assert!(cut.iter().all(|c| c.alternative_of.is_none()));
    assert_eq!(cut.len(), 2);
    for c in &cut {
        assert_eq!(
            c.truncated,
            Some(TruncationReason::Alternatives),
            "{}",
            c.label
        );
        assert!(!c.alternatives_complete);
    }
}
