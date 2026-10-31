use super::super::{
    derive_security_findings, security_projection_bound, security_projection_value_bound,
};
use super::*;
use core_prov::{EvidenceRef, ProducingMethod, Provenance};
use serde_json::{Value, json};

fn support(tier: Tier, confidence: ConfidenceTier, text: &str) -> Value {
    serde_json::to_value(
        Provenance::new(
            tier,
            confidence,
            vec![EvidenceRef {
                repo: text.into(),
                path: text.into(),
                byte_start: 0,
                byte_end: 42,
                commit_sha: text.into(),
            }],
            text,
            b"support",
        )
        .unwrap()
        .with_method(ProducingMethod::Human { actor: text.into() }),
    )
    .unwrap()
}
fn fixture(text: &str) -> (Vec<Node>, Vec<Edge>) {
    let prov = support(Tier::Dynamic, ConfidenceTier::Confirmed, text);
    (
        vec![Node {
            id: format!("endpoint:{text}"),
            label: "Endpoint".into(),
            props: json!({"method":text,"path":text,"auth":"PUBLIC","prov":prov}),
        }],
        vec![Edge {
            src: format!("policy:{text}"),
            dst: format!("bucket:{text}"),
            label: "GRANTS".into(),
            props: json!({"actions":[format!("{text}*"),"duplicate","duplicate",true],"resource_scopes":[text,"*",text],"prov":prov}),
        }],
    )
}
fn values(value: &Value) -> usize {
    1 + match value {
        Value::Array(items) => items.iter().map(values).sum(),
        Value::Object(items) => items.values().map(values).sum(),
        _ => 0,
    }
}

#[test]
fn bounded_security_matches_original_bytes_ids_order_and_exact_admission() {
    // AC-0288: output construction follows complete exact admission.
    for text in ["ordinary", "\0\n\t\r\\\"🧭 café", ""] {
        let (mut nodes, edges) = fixture(text);
        nodes.push(nodes[0].clone()); // stable deduplication must precede counting
        let expected = derive_security_findings(&nodes, &edges);
        let expected_bytes = serde_json::to_vec(&expected).unwrap();
        let size = security_projection_size(&nodes, &edges).unwrap();
        assert_eq!(
            size,
            SecurityProjectionSize {
                rows: expected.len(),
                bytes: expected_bytes.len()
            }
        );
        MATERIALIZED.with(|count| count.set(0));
        assert!(
            derive_security_findings_bounded(
                &nodes,
                &edges,
                SecurityLimits {
                    max_rows: size.rows,
                    max_output_bytes: size.bytes - 1
                }
            )
            .is_err()
        );
        MATERIALIZED.with(|count| assert_eq!(count.get(), 0));
        assert!(matches!(
            derive_security_findings_bounded(
                &nodes,
                &edges,
                SecurityLimits {
                    max_rows: size.rows - 1,
                    max_output_bytes: size.bytes
                }
            ),
            Err(SecurityLimitExceeded {
                limit: SecurityLimit::Rows,
                ..
            })
        ));
        MATERIALIZED.with(|count| assert_eq!(count.get(), 0));
        let actual = derive_security_findings_bounded(
            &nodes,
            &edges,
            SecurityLimits {
                max_rows: size.rows,
                max_output_bytes: size.bytes,
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_vec(&actual).unwrap(), expected_bytes);
        MATERIALIZED.with(|count| assert_eq!(count.get(), size.rows));
    }
}

#[test]
fn security_source_bound_covers_control_character_scope_and_evidence_expansion() {
    let text = "\0\n\"\\🧭".repeat(2048);
    let (mut nodes, mut edges) = fixture(&text);
    let mut refs = vec![];
    for index in 0..64 {
        refs.push(json!({"repo":format!("{index}{text}"),"path":text,"byte_start":0,"byte_end":u64::MAX,"commit_sha":text}));
    }
    nodes[0].props["prov"]["evidence"] = json!(refs);
    edges[0].props["prov"]["evidence"] = nodes[0].props["prov"]["evidence"].clone();
    let graph = serde_json::to_value((&nodes, &edges)).unwrap();
    let bytes = serde_json::to_vec(&graph).unwrap().len();
    let bound =
        security_projection_bound(bytes, nodes.len() + edges.len(), values(&graph)).unwrap();
    let actual = security_projection_size(&nodes, &edges).unwrap();
    assert!(bound.bytes >= actual.bytes);
    assert!(bound.rows >= actual.rows);
    MATERIALIZED.with(|count| count.set(0));
    assert!(
        derive_security_findings_bounded(
            &nodes,
            &edges,
            SecurityLimits {
                max_rows: bound.rows,
                max_output_bytes: 1
            }
        )
        .is_err()
    );
    MATERIALIZED.with(|count| assert_eq!(count.get(), 0));
    assert_eq!(
        derive_security_findings_bounded(
            &nodes,
            &edges,
            SecurityLimits {
                max_rows: bound.rows,
                max_output_bytes: bound.bytes
            }
        )
        .unwrap(),
        derive_security_findings(&nodes, &edges)
    );
}

#[test]
fn borrowed_security_validation_preserves_tier_method_and_source_wire_shapes() {
    for tier in [
        Tier::Deterministic,
        Tier::Dynamic,
        Tier::Semantic,
        Tier::Agentic,
    ] {
        for confidence in [
            ConfidenceTier::Confirmed,
            ConfidenceTier::InferredStrong,
            ConfidenceTier::InferredWeak,
            ConfidenceTier::Gap,
        ] {
            let (mut nodes, edges) = fixture("fixture");
            nodes[0].props["prov"]["tier"] = json!(tier);
            nodes[0].props["prov"]["confidence_tier"] = json!(confidence);
            let actual = derive_security_findings_bounded(
                &nodes,
                &edges,
                SecurityLimits {
                    max_rows: 10,
                    max_output_bytes: 100_000,
                },
            )
            .unwrap();
            assert_eq!(actual, derive_security_findings(&nodes, &edges));
        }
    }
    for method in [
        json!(null),
        json!({"kind":"unknown"}),
        json!({"kind":"rule","rule":null,"version":"1"}),
        json!({"kind":"model","model":"valid"}),
        json!({"kind":"human","actor":"valid"}),
        json!({"kind":"unrecorded"}),
    ] {
        let (mut nodes, edges) = fixture("fixture");
        nodes[0].props["prov"]["method"] = method;
        assert_eq!(
            derive_security_findings_bounded(
                &nodes,
                &edges,
                SecurityLimits {
                    max_rows: 10,
                    max_output_bytes: 100_000
                }
            )
            .unwrap(),
            derive_security_findings(&nodes, &edges)
        );
    }
    // serde permits positional struct representations. The source-bound helper
    // includes fixed object key overhead when evidence is encoded positionally.
    let (mut nodes, edges) = fixture("fixture");
    nodes[0].props["prov"]["evidence"] = json!([["repo", "path", 0, 1, "commit"]]);
    let expected = derive_security_findings(&nodes, &edges);
    let actual = derive_security_findings_bounded(
        &nodes,
        &edges,
        SecurityLimits {
            max_rows: 10,
            max_output_bytes: 100_000,
        },
    )
    .unwrap();
    assert_eq!(actual, expected);
    let graph = serde_json::to_value((&nodes, &edges)).unwrap();
    assert!(
        security_projection_bound(
            serde_json::to_vec(&graph).unwrap().len(),
            nodes.len() + edges.len(),
            values(&graph)
        )
        .unwrap()
        .bytes
            >= security_projection_size(&nodes, &edges).unwrap().bytes
    );
}

#[test]
fn security_source_bound_is_checked_monotone_and_empty_output_is_exact() {
    assert_eq!(
        security_projection_size(&[], &[]),
        Some(SecurityProjectionSize { rows: 0, bytes: 2 })
    );
    let baseline = security_projection_bound(100, 10, 30).unwrap();
    assert!(security_projection_bound(101, 10, 30).unwrap().bytes >= baseline.bytes);
    assert!(security_projection_bound(100, 11, 30).unwrap().bytes >= baseline.bytes);
    assert!(security_projection_bound(100, 10, 31).unwrap().bytes >= baseline.bytes);
    assert!(security_projection_bound(usize::MAX, 1, 1).is_none());
    assert!(security_projection_bound(1, usize::MAX, 1).is_none());
    assert!(security_projection_bound(1, 1, usize::MAX).is_none());
    assert!(
        derive_security_findings_bounded(
            &[],
            &[],
            SecurityLimits {
                max_rows: 0,
                max_output_bytes: 1
            }
        )
        .is_err()
    );
    assert!(
        derive_security_findings_bounded(
            &[],
            &[],
            SecurityLimits {
                max_rows: 0,
                max_output_bytes: 2
            }
        )
        .unwrap()
        .is_empty()
    );
}

fn row_values(nodes: &[Node], edges: &[Edge]) -> usize {
    nodes.iter().map(|n| values(&n.props)).sum::<usize>()
        + edges.iter().map(|e| values(&e.props)).sum::<usize>()
}
fn output_values(nodes: &[Node], edges: &[Edge]) -> usize {
    derive_security_findings(nodes, edges)
        .iter()
        .map(|finding| values(&serde_json::to_value(finding).unwrap()))
        .sum()
}
fn grant(actions: Value, scopes: Value, evidence: Value) -> Edge {
    let mut prov = support(Tier::Deterministic, ConfidenceTier::Confirmed, "g");
    prov["evidence"] = evidence;
    Edge {
        src: "policy:g".into(),
        dst: "bucket:g".into(),
        label: "GRANTS".into(),
        props: json!({"actions":actions,"resource_scopes":scopes,"prov":prov}),
    }
}

// AC-0521: finding value counts stay within the typed-row bound for escaped,
// concentrated and many-minimal inputs, counting only property values.
#[test]
fn security_value_bound_covers_concentrated_and_minimal_rows() {
    let mut cases = vec![];
    for text in ["ordinary", "\0\n\t\r\\\"🧭 café", ""] {
        cases.push(fixture(text));
    }
    // One row holding the whole allowance: many actions, scopes and evidence.
    let actions: Vec<_> = (0..200).map(|i| json!(format!("a{i}*"))).collect();
    let scopes: Vec<_> = (0..200).map(|i| json!(format!("s{i}"))).collect();
    let evidence: Vec<_> = (0..64)
        .map(|i| json!([format!("r{i}"), "p", 0, 1, "c"]))
        .collect();
    cases.push((
        vec![],
        vec![grant(json!(actions), json!(scopes), json!(evidence))],
    ));
    // Many minimal rows: positional evidence, empty scopes (fallback to dst).
    let minimal: Vec<_> = (0..50)
        .map(|i| {
            let mut edge = grant(json!(["*"]), json!([]), json!([["r", "p", 0, 1, "c"]]));
            edge.dst = format!("bucket:{i}");
            edge
        })
        .collect();
    cases.push((vec![], minimal));
    for (nodes, edges) in cases {
        let rows = nodes.len() + edges.len();
        let bound = security_projection_value_bound(rows, row_values(&nodes, &edges)).unwrap();
        assert!(output_values(&nodes, &edges) <= bound);
    }
}

// AC-0521: each variable item costs exactly as many output values as its
// smallest source form, so the per-value coefficient cannot be lowered.
#[test]
fn security_value_bound_slopes_match_their_source_values() {
    let base = |actions: usize, scopes: usize, evidence: usize| {
        let edge = grant(
            json!((0..actions).map(|i| format!("a{i}*")).collect::<Vec<_>>()),
            json!((0..scopes).map(|i| format!("s{i}")).collect::<Vec<_>>()),
            json!(
                (0..evidence)
                    .map(|i| json!([format!("r{i}"), "p", 0, 1, "c"]))
                    .collect::<Vec<_>>()
            ),
        );
        let (nodes, edges) = (vec![], vec![edge]);
        (output_values(&nodes, &edges), row_values(&nodes, &edges))
    };
    let (output, source) = base(1, 1, 1);
    for (actions, scopes, evidence) in [(2, 1, 1), (1, 2, 1), (1, 1, 2)] {
        let (more_output, more_source) = base(actions, scopes, evidence);
        assert_eq!(more_output - output, more_source - source);
    }
    let fixed = security_projection_value_bound(1, 0).unwrap();
    assert!(output <= fixed + source);
    assert!(security_projection_value_bound(usize::MAX, 1).is_none());
    assert!(security_projection_value_bound(1, usize::MAX).is_none());
    assert!(security_projection_value_bound(2, 0).unwrap() == 2 * fixed);
    assert_eq!(security_projection_value_bound(0, 7), Some(7));
}
