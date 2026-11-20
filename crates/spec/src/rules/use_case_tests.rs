//! AC-0243 (T-0243): use-case projection and content acceptance.

use super::*;
use core_prov::ProducingMethod;
use serde_json::json;

const ORACLE: &str = include_str!("../../../../docs/evals/vendure-cart-readiness.expected.json");
const OBSERVATIONS: &str =
    include_str!("../../tests/fixtures/vendure-cart-readiness.observations.json");
const ENTRY: &str = "sym:vendurehq/vendure@packages/core/src/config/order/default-order-process.ts#configureDefaultOrderProcess/orderProcess@6228.onTransitionStart@9799";
const SALEABLE: &str = "sym:vendurehq/vendure@packages/core/src/service/services/product-variant.service.ts#ProductVariantService.getSaleableStockLevel";
const CHANNEL: &str = "sym:vendurehq/vendure@packages/core/src/config/catalog/multi-channel-stock-location-strategy.ts#MultiChannelStockLocationStrategy.getAvailableStock";

fn scope() -> UseCaseScope {
    UseCaseScope {
        id: "cart-readiness".into(),
        title: "Cart readiness to ArrangingPayment".into(),
        goal: "an AddingItems order enters ArrangingPayment".into(),
        entry_owners: vec![ENTRY.into()],
        supporting_owners: vec![SALEABLE.into(), CHANNEL.into()],
    }
}

fn observation_node(
    id: &str,
    rule: serde_json::Value,
    tier: Tier,
    confidence: ConfidenceTier,
) -> Node {
    let exit: EvidenceRef = serde_json::from_value(rule["exit_source"].clone()).unwrap();
    let prov =
        Provenance::new(tier, confidence, vec![exit], "t0.adapter-ts", id.as_bytes()).unwrap();
    Node {
        id: id.into(),
        label: "BusinessRule".into(),
        props: json!({"rule": rule, "prov": prov}),
    }
}

fn vendure_nodes() -> Vec<Node> {
    let fixture: serde_json::Value = serde_json::from_str(OBSERVATIONS).unwrap();
    fixture["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            observation_node(
                o["node_id"].as_str().unwrap(),
                o["rule"].clone(),
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            )
        })
        .collect()
}

fn oracle() -> ExpectedSet {
    ExpectedSet::from_manifest(&serde_json::from_str(ORACLE).unwrap()).unwrap()
}

fn label(report: &AcceptanceReport, id: &str) -> ExpectationLabel {
    report
        .expectations
        .iter()
        .find(|r| r.id == id)
        .unwrap()
        .label
}

#[test]
fn vendure_cart_readiness_use_case_is_partial_and_labels_each_expectation() {
    let projection = project_use_case(
        &vendure_nodes(),
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    assert_eq!(projection.rows.len(), 6);
    assert!(projection.partial);
    assert!(projection.artifact.verify_identity());
    assert_eq!(projection.artifact.schema, "cartograph.spec/use-case@1");
    let content = &projection.artifact.content;
    assert!(content.contains("**Status: PARTIAL.**"));
    assert!(content.contains(&format!(
        "no recovered rule observation for declared owner {}",
        text(CHANNEL)
    )));
    assert!(content.contains("execution predicate not established"));
    assert!(content.contains("## Decision table"));
    // Entry rows come first, in lexical order within the owner.
    let orders: Vec<(RowRole, u32)> = projection
        .rows
        .iter()
        .map(|r| (r.role, r.source_order))
        .collect();
    assert_eq!(
        orders,
        [
            (RowRole::Entry, 4),
            (RowRole::Entry, 5),
            (RowRole::Entry, 6),
            (RowRole::Entry, 7),
            (RowRole::Entry, 8),
            (RowRole::Supporting, 0),
        ]
    );

    let report = check_use_case_acceptance(&projection, &oracle());
    assert!(!report.complete);
    assert_eq!(report.artifact_content_id, projection.artifact.content_id);
    for id in [
        "VEN-READY-01",
        "VEN-READY-02",
        "VEN-READY-03",
        "VEN-READY-04",
        "VEN-READY-05",
        "VEN-READY-06",
    ] {
        assert_eq!(label(&report, id), ExpectationLabel::Inferred, "{id}");
        let result = report.expectations.iter().find(|r| r.id == id).unwrap();
        assert_eq!(result.row_ids.len(), 1, "{id} gets exactly its own exit");
        assert!(
            result
                .missing
                .iter()
                .any(|m| m.contains("consumer effect not established"))
        );
    }
    for id in ["VEN-READY-07", "VEN-READY-08"] {
        assert_eq!(label(&report, id), ExpectationLabel::Unknown, "{id}");
    }
    // The narrowest containing span decides: the contents exit is not also
    // credited to the customer or shipping rule whose spans enclose it.
    let customer = report
        .expectations
        .iter()
        .find(|r| r.id == "VEN-READY-03")
        .unwrap();
    assert_eq!(
        customer.row_ids,
        [
            "row:rule:vendurehq/vendure@packages/core/src/config/order/default-order-process.ts#exit@12930"
        ]
    );
    assert_eq!(
        report
            .expectation_labels
            .get(&ExpectationLabel::Implemented),
        None
    );
    assert_eq!(report.expectation_labels[&ExpectationLabel::Inferred], 6);
    assert_eq!(report.expectation_labels[&ExpectationLabel::Unknown], 2);
    assert_eq!(report.cases.len(), 41);
    assert_eq!(report.case_labels[&ExpectationLabel::Inferred], 27);
    assert_eq!(report.case_labels[&ExpectationLabel::Unknown], 14);
    assert!(
        report
            .cases
            .iter()
            .all(|c| c.execution_status == "not_executed")
    );
    assert!(report.extra_row_ids.is_empty());
    assert!(report.unchecked.iter().any(|u| u.contains("VEN-NEG-10")));
    let markdown = report.to_markdown();
    assert!(markdown.contains("**Status: PARTIAL — not complete.**"));
    assert!(
        markdown.contains(
            "Expectations (8): 0 implemented, 0 documented intent, 6 inferred, 2 unknown"
        )
    );
    assert!(markdown.contains(
        "Decision cases (41): 0 implemented, 0 documented intent, 27 inferred, 14 unknown"
    ));
}

#[test]
fn use_case_projection_is_deterministic_across_input_order() {
    let nodes = vendure_nodes();
    let mut reversed = nodes.clone();
    reversed.reverse();
    let first = project_use_case(
        &nodes,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    let second = project_use_case(
        &reversed,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    assert_eq!(first, second);
    assert_eq!(
        check_use_case_acceptance(&first, &oracle()),
        check_use_case_acceptance(&second, &oracle())
    );
}

#[test]
fn guard_interpretation_stays_t2_inferred_weak_and_never_promotes_the_row() {
    let mut nodes = vendure_nodes();
    // A T3 observation in scope keeps its own tier on the restated row.
    let mut weak_rule: serde_json::Value = nodes[0].props["rule"].clone();
    weak_rule["source_order"] = json!(99);
    nodes.push(observation_node(
        "rule:t3",
        weak_rule,
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
    ));
    let best = project_use_case(
        &nodes,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    for row in &best.rows {
        let candidate = row.candidate.as_ref().unwrap();
        assert_eq!(
            (candidate.tier, candidate.confidence_tier),
            (Tier::Semantic, ConfidenceTier::InferredWeak)
        );
        assert_eq!(
            candidate.method,
            ProducingMethod::Rule {
                rule: USE_CASE_CANDIDATE_RULE.into(),
                version: crate::SPEC_RULE_VERSION.into()
            }
        );
        assert!(
            matches!(&row.row.method, ProducingMethod::Rule { rule, .. } if rule == USE_CASE_ROW_RULE)
        );
        assert!(!row.is_complete());
    }
    let t3 = best.rows.iter().find(|r| r.rule_id == "rule:t3").unwrap();
    assert_eq!(
        (t3.row.tier, t3.row.confidence_tier),
        (Tier::Agentic, ConfidenceTier::InferredWeak)
    );
    assert!(t3.missing[0].contains("not a deterministic Confirmed observation"));

    // Verified-only drops the T3 observation and every candidate, but keeps
    // the Confirmed local rows.
    let verified = project_use_case(
        &nodes,
        &[],
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        &scope(),
    );
    assert_eq!(verified.rows.len(), 6);
    assert!(verified.rows.iter().all(|r| r.candidate.is_none()));
    assert!(
        verified
            .artifact
            .content
            .contains("excluded by export mode or curation")
    );

    // Curation rejects one candidate by hash without touching its row.
    let rejected = best.rows[0]
        .candidate
        .as_ref()
        .unwrap()
        .content_hash
        .clone();
    let curated = project_use_case(
        &nodes,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::from([rejected]),
        &scope(),
    );
    assert!(curated.rows[0].candidate.is_none());
    assert_eq!(curated.rows[0].row, best.rows[0].row);
}

#[test]
fn documented_intent_and_unmatched_rows_are_labeled_separately() {
    let evidence = |path: &str, start: u64, end: u64| EvidenceRef {
        repo: "local/shop".into(),
        path: path.into(),
        byte_start: start,
        byte_end: end,
        commit_sha: "abc".into(),
    };
    let prov = |id: &str, ev: EvidenceRef| {
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![ev],
            "test",
            id.as_bytes(),
        )
        .unwrap()
    };
    let rule = |owner: &str, start: u64| {
        json!({
            "schema_version": 1, "kind": "guarded_exit", "owner_id": owner,
            "exit_source": evidence("src/checkout.ts", start, start + 10),
            "source_order": 0, "conditions": [],
            "effect": {"kind": "return", "value": null},
            "dependencies": [],
            "interpretation": {"execution_predicate": "not_established", "consumer_effect": "not_established", "gap_ids": []},
            "redactions": []
        })
    };
    let nodes = vec![
        observation_node(
            "rule:in-scope",
            rule("sym:check", 100),
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        observation_node(
            "rule:elsewhere",
            rule("sym:other", 150),
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        Node {
            id: "sym:pay".into(),
            label: "Symbol".into(),
            props: json!({"prov": prov("sym:pay", evidence("src/pay.ts", 0, 500))}),
        },
        Node {
            id: "adr:found:pay".into(),
            label: "ADR".into(),
            props: json!({"origin": "found", "title": "Payments need a customer", "prov": prov("adr", evidence("docs/adr/1.md", 0, 40))}),
        },
    ];
    let edges = vec![Edge {
        src: "adr:found:pay".into(),
        dst: "sym:pay".into(),
        label: "DECIDES".into(),
        props: json!({"prov": prov("adr-edge", evidence("docs/adr/1.md", 0, 40))}),
    }];
    let scope = UseCaseScope {
        id: "checkout".into(),
        title: "Checkout".into(),
        goal: "checkout".into(),
        entry_owners: vec!["sym:check".into()],
        supporting_owners: vec!["sym:pay".into()],
    };
    let projection = project_use_case(
        &nodes,
        &edges,
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope,
    );
    // Only the declared owner's observation becomes a row.
    assert_eq!(projection.rows.len(), 1);
    assert_eq!(projection.documented.len(), 1);
    let manifest = json!({
        "schema": EXPECTED_RULES_SCHEMA,
        "benchmark_id": "synthetic",
        "source_refs": {
            "S-far": {"path": "src/checkout.ts", "commit_sha": "abc", "byte_start": 300, "byte_end": 400},
            "S-pay": {"path": "src/pay.ts", "commit_sha": "abc", "byte_start": 10, "byte_end": 20},
            "S-none": {"path": "src/none.ts", "commit_sha": "abc", "byte_start": 0, "byte_end": 5}
        },
        "rules": [
            {"id": "R-far", "category": "core", "title": "far", "source_refs": ["S-far"], "dependencies": []},
            {"id": "R-pay", "category": "core", "title": "pay", "source_refs": ["S-pay"], "dependencies": []},
            {"id": "R-none", "category": "core", "title": "none", "source_refs": ["S-none"], "dependencies": ["D-x"]}
        ],
        "decision_case_matrix": {"cases": [
            {"id": "C-1", "name": "mixed", "rule_ids": ["R-pay", "R-none"]},
            {"id": "C-2", "name": "pay only", "rule_ids": ["R-pay"]}
        ]}
    });
    let report =
        check_use_case_acceptance(&projection, &ExpectedSet::from_manifest(&manifest).unwrap());
    assert_eq!(label(&report, "R-far"), ExpectationLabel::Unknown);
    assert_eq!(label(&report, "R-pay"), ExpectationLabel::DocumentedIntent);
    assert_eq!(label(&report, "R-none"), ExpectationLabel::Unknown);
    let none = report
        .expectations
        .iter()
        .find(|r| r.id == "R-none")
        .unwrap();
    assert!(
        none.missing
            .iter()
            .any(|m| m == "dependency claims not checked: D-x")
    );
    assert_eq!(report.cases[0].label, ExpectationLabel::Unknown);
    assert_eq!(report.cases[1].label, ExpectationLabel::DocumentedIntent);
    assert_eq!(report.extra_row_ids, ["row:rule:in-scope"]);
    assert!(!report.complete);
}

#[test]
fn expected_set_rejects_unknown_schema_and_dangling_references() {
    assert_eq!(
        ExpectedSet::from_manifest(&json!({"schema": "other"})),
        Err(ExpectedSetError::UnsupportedSchema)
    );
    let base = |rules: serde_json::Value, cases: serde_json::Value| {
        json!({
            "schema": EXPECTED_RULES_SCHEMA, "benchmark_id": "b",
            "source_refs": {"S": {"path": "a.ts", "commit_sha": "c", "byte_start": 0, "byte_end": 1}},
            "rules": rules, "decision_case_matrix": {"cases": cases}
        })
    };
    let rule = |source: &str| json!([{"id": "R", "category": "c", "title": "t", "source_refs": [source], "dependencies": []}]);
    assert_eq!(
        ExpectedSet::from_manifest(&base(rule("S-missing"), json!([]))),
        Err(ExpectedSetError::UnknownSourceRef("S-missing".into()))
    );
    assert_eq!(
        ExpectedSet::from_manifest(&base(
            rule("S"),
            json!([{"id": "C", "name": "n", "rule_ids": ["R-missing"]}])
        )),
        Err(ExpectedSetError::UnknownRule("R-missing".into()))
    );
    let frozen = oracle();
    assert_eq!(
        (
            frozen.rules.len(),
            frozen.cases.len(),
            frozen.negative_claim_ids.len()
        ),
        (8, 41, 10)
    );
}

#[test]
fn duplicate_owners_collapse_and_unknown_case_rule_labels_unknown() {
    // A duplicated declared owner yields exactly one OwnerCoverage row:
    // entry owners first, then supporting owners not already listed.
    let mut duplicated = scope();
    duplicated.entry_owners = vec![ENTRY.into(), ENTRY.into()];
    duplicated.supporting_owners = vec![
        SALEABLE.into(),
        CHANNEL.into(),
        SALEABLE.into(),
        ENTRY.into(),
    ];
    let projection = project_use_case(
        &vendure_nodes(),
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &duplicated,
    );
    assert_eq!(projection.owners.len(), 3);
    assert_eq!(
        projection
            .owners
            .iter()
            .map(|owner| (owner.owner_id.as_str(), owner.role))
            .collect::<Vec<_>>(),
        [
            (ENTRY, RowRole::Entry),
            (SALEABLE, RowRole::Supporting),
            (CHANNEL, RowRole::Supporting),
        ]
    );

    // A hand-built ExpectedSet case naming an unknown rule labels the case
    // Unknown instead of panicking on the missing lookup.
    let expected = ExpectedSet {
        benchmark_id: "synthetic".into(),
        rules: vec![],
        cases: vec![ExpectedCase {
            id: "C-ghost".into(),
            name: "ghost rule".into(),
            rule_ids: vec!["R-ghost".into()],
        }],
        negative_claim_ids: vec![],
    };
    let report = check_use_case_acceptance(&projection, &expected);
    assert_eq!(report.cases.len(), 1);
    assert_eq!(report.cases[0].id, "C-ghost");
    assert_eq!(report.cases[0].label, ExpectationLabel::Unknown);
    assert!(
        report.cases[0]
            .missing
            .iter()
            .any(|m| m == "R-ghost has no recovered row")
    );
}

#[test]
fn a_guarded_effect_is_no_decision_row() {
    // AC-0910: an effect in a declared owner changes state; it is not an
    // exit that may block the goal, so it adds no row or guard candidate.
    let nodes = vendure_nodes();
    let before = project_use_case(
        &nodes,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    let mut with_effect = nodes.clone();
    let mut rule = nodes[0].props["rule"].clone();
    let value = rule["effect"]["value"].clone();
    rule["kind"] = json!("guarded_effect");
    rule["effect"] = json!({"kind": "call", "value": value});
    assert!(GuardedExitEvidence::from_value(rule.clone()).is_ok());
    with_effect.push(observation_node(
        "rule:effect-in-entry",
        rule,
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
    ));
    let after = project_use_case(
        &with_effect,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
        &scope(),
    );
    assert_eq!(before.rows.len(), after.rows.len());
    assert!(after.rows.iter().all(|r| !r.id.contains("effect-in-entry")));
}
