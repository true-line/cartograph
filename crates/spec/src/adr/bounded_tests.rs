// AC-0259 (T-0259): bounded derivation preserves complete ordinary ADR output.
use super::*;

fn provenance(seed: &str, evidence_bytes: usize) -> serde_json::Value {
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence(
                "repo",
                &"source".repeat(evidence_bytes / 6 + 1),
                "commit",
                0,
                10,
            )],
            "fixture",
            seed.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn fixture(adrs: usize, calls: usize, evidence_bytes: usize) -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
    let mut nodes = vec![
        Node {
            id: "src".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov":provenance("src",0)}),
        },
        Node {
            id: "channel".into(),
            label: "Channel".into(),
            props: serde_json::json!({"identity":"queue|\n\"é","prov":provenance("channel",evidence_bytes)}),
        },
    ];
    let mut edges = vec![Edge {
        src: "src".into(),
        dst: "channel".into(),
        label: "PUBLISHES".into(),
        props: serde_json::json!({"prov":provenance("publishes",evidence_bytes)}),
    }];
    for i in 0..adrs {
        let id = format!("adr:{i}");
        nodes.push(Node{id:id.clone(),label:"ADR".into(),props:serde_json::json!({"origin":"found","title":"No | \"sync\"\n é","forbids":["CALLS"],"prov":provenance(&id,evidence_bytes)})});
        edges.push(Edge {
            src: id,
            dst: "src".into(),
            label: "DECIDES".into(),
            props: serde_json::json!({"prov":provenance("decides",0)}),
        });
    }
    let mut flows = Vec::new();
    for i in 0..calls {
        let id = format!("dst:{i}");
        nodes.push(Node {
            id: id.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        });
        let prov = provenance(&id, evidence_bytes);
        edges.push(Edge {
            src: "src".into(),
            dst: id.clone(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov":prov}),
        });
        let hop = flowtracer::Hop {
            label: "CALLS".into(),
            src: "src".into(),
            dst: id,
            src_name: "src".into(),
            dst_name: "dst".into(),
            tier: "Deterministic".into(),
            confidence: "Confirmed".into(),
            evidence: None,
            provenance: serde_json::from_value(prov).unwrap(),
            gap_reason: None,
            attempted_tiers: vec![],
        };
        for trigger in ["z\"\n", "a|é", "a|é"] {
            flows.push(Flow {
                trigger: trigger.into(),
                trigger_kind: "Endpoint".into(),
                trigger_name: "trigger".into(),
                hops: vec![hop.clone()],
                status: flowtracer::FlowStatus::Verified,
                score: 1.,
                depth_limited: false,
            });
        }
    }
    (nodes, edges, flows)
}
fn limits(rows: usize, bytes: usize) -> AdrLimits {
    AdrLimits {
        max_rows: rows,
        max_output_bytes: bytes,
    }
}

#[test]
fn bounded_adr_matches_frozen_prechange_bytes_methods_ids_and_order_at_boundaries() {
    let (nodes, edges, flows) = fixture(3, 4, 24);
    let ordinary = derive_adr_facts(&nodes, &edges, &flows);
    let bytes = serde_json::to_vec(&ordinary).unwrap();
    // Frozen BLAKE3 of this fixture's complete serialized AdrFacts, produced by
    // the original derivation at HEAD956422a before adding bounded admission.
    assert_eq!(
        core_prov::content_hash(&bytes),
        "014eb7ae1575a6690a86ac8d146763bcceb24808a0d1f6f43a100de18c06ed1c"
    );
    let rows = ordinary.nodes.len() + ordinary.edges.len();
    let bounded =
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(rows, bytes.len())).unwrap();
    assert_eq!(serde_json::to_vec(&bounded).unwrap(), bytes);
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(rows - 1, bytes.len()))
            .unwrap_err()
            .limit,
        AdrLimit::Rows
    );
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(rows, bytes.len() - 1))
            .unwrap_err()
            .limit,
        AdrLimit::OutputBytes
    );
    assert!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(rows + 1, bytes.len() + 1)).is_ok()
    );
}

#[test]
fn bounded_adr_empty_result_and_uncited_candidates_use_exact_limits() {
    let empty = serde_json::to_vec(&AdrFacts::default()).unwrap().len();
    assert_eq!(
        derive_adr_facts_bounded(&[], &[], &[], limits(0, empty)).unwrap(),
        AdrFacts::default()
    );
    assert_eq!(
        derive_adr_facts_bounded(&[], &[], &[], limits(0, empty - 1))
            .unwrap_err()
            .limit,
        AdrLimit::OutputBytes
    );
    let (mut nodes, mut edges, flows) = fixture(2, 2, 0);
    for props in nodes
        .iter_mut()
        .map(|n| &mut n.props)
        .chain(edges.iter_mut().map(|e| &mut e.props))
    {
        if let Some(prov) = props.get_mut("prov") {
            prov["evidence"] = serde_json::json!([]);
        }
    }
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(0, empty)).unwrap(),
        AdrFacts::default()
    );
}

#[test]
fn bounded_adr_rejects_fanout_before_materializing_the_next_occurrence() {
    let (mut nodes, mut edges, flows) = fixture(200, 200, 64);
    // Govern the channel so this fixture isolates the ADR×edge drift crossproduct.
    edges.push(Edge {
        src: "adr:0".into(),
        dst: "channel".into(),
        label: "DECIDES".into(),
        props: serde_json::json!({}),
    });
    bounded::take_materialized_rows();
    let error = derive_adr_facts_bounded(&nodes, &edges, &flows, limits(0, 100_000)).unwrap_err();
    assert_eq!(error.limit, AdrLimit::Rows);
    assert_eq!(bounded::take_materialized_rows(), 0);
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(3, 100_000))
            .unwrap_err()
            .limit,
        AdrLimit::Rows
    );
    assert_eq!(bounded::take_materialized_rows(), 3);
    // Very large source-authored title/evidence and many matching flow triggers
    // must be measured as borrowed strings, before owning any derived JSON row.
    nodes.iter_mut().find(|n| n.id == "adr:0").unwrap().props["title"] =
        serde_json::json!("\u{0}".repeat(100_000));
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(3, 4096))
            .unwrap_err()
            .limit,
        AdrLimit::OutputBytes
    );
    assert_eq!(bounded::take_materialized_rows(), 0);
}

#[test]
fn bounded_adr_charges_large_recovered_evidence_and_flow_trigger_repetition() {
    let (nodes, edges, mut flows) = fixture(1, 1, 100_000);
    bounded::take_materialized_rows();
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(100, 1024))
            .unwrap_err()
            .limit,
        AdrLimit::OutputBytes
    );
    assert_eq!(bounded::take_materialized_rows(), 0);
    let (nodes, edges, _) = fixture(1, 1, 0);
    for flow in &mut flows {
        flow.trigger = "\u{0}".repeat(100_000);
    }
    // Enough room for recovery, then reject the first drift's trigger expansion.
    assert_eq!(
        derive_adr_facts_bounded(&nodes, &edges, &flows, limits(100, 4096))
            .unwrap_err()
            .limit,
        AdrLimit::OutputBytes
    );
    assert_eq!(bounded::take_materialized_rows(), 2);
}
