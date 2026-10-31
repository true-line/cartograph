// AC-0516: bounded node-only BusinessFlows retain exact support and partial state.
use super::*;
use core_graph::{Edge, StoreRevision};
use serde_json::json;

fn limits() -> TraceLimits {
    TraceLimits {
        max_flows: 16,
        max_hops: 512,
        max_output_bytes: MAX_FACT_BYTES * 16,
    }
}
fn fixture() -> RevisionedSnapshot {
    let mut ex = adapters_lang_ts::extract_source(
        b"import {Resolver, Query} from '@nestjs/graphql';\n@Resolver() export class OrderResolver { @Query() order() {} }",
        "packages/core/src/api/resolvers/shop/order.resolver.ts",
        &adapters_lang_ts::SourceId { repo: "fixture/repo", commit: "fixed" },
    ).unwrap();
    ex.close_over_endpoints();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (ex.nodes, ex.edges),
    }
}
fn candidate(fact: &OverlayFact) -> &Node {
    let OverlayItem::Node(n) = &fact.item else {
        panic!("node-only producer")
    };
    n
}
fn handled(base: &RevisionedSnapshot) -> &Edge {
    base.graph.1.iter().find(|e| e.label == "HANDLES").unwrap()
}
fn chain(kind: &str, length: usize) -> RevisionedSnapshot {
    let original = fixture();
    let mut base = original.clone();
    let template = original
        .graph
        .0
        .iter()
        .find(|n| n.id == handled(&original).dst)
        .unwrap();
    base.graph = (Vec::new(), Vec::new());
    for i in 0..=length {
        let mut n = template.clone();
        n.id = format!("step:{i}");
        n.label = if i == 0 { kind } else { "Symbol" }.into();
        n.props["name"] = json!(n.id);
        base.graph.0.push(n);
        if i > 0 {
            let mut edge = handled(&original).clone();
            edge.src = format!("step:{}", i - 1);
            edge.dst = format!("step:{i}");
            edge.label = "CALLS".into();
            base.graph.1.push(edge);
        }
    }
    base
}

// AC-0516: real server traces retain exact cited support, weak provenance and base immutability.
#[test]
fn business_flows_use_real_graphql_traces_and_exact_support() {
    let base = fixture();
    let before = base.clone();
    let traced = flowtracer::trace_bounded(&base.graph.0, &base.graph.1, limits()).unwrap();
    assert_eq!(traced.len(), 1);
    assert!(!traced[0].hops.is_empty());
    assert_eq!(traced[0].trigger_kind, "Endpoint");
    let report = derive_business_flows(&base, limits()).unwrap();
    assert_eq!(report.facts.len(), 1);
    assert_eq!(report.invalid_flows, 0);
    let fact = &report.facts[0];
    let n = candidate(fact);
    let e = handled(&base);
    assert_eq!(n.label, "BusinessFlow");
    assert_eq!(n.props["trace"], serde_json::to_value(&traced[0]).unwrap());
    assert_eq!(
        fact.cites,
        BTreeSet::from([
            BaseRef::Node(e.src.clone()),
            BaseRef::Node(e.dst.clone()),
            BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone())
        ])
    );
    let p: Provenance = serde_json::from_value(n.props["prov"].clone()).unwrap();
    assert_eq!(
        (p.tier, p.confidence_tier),
        (Tier::Semantic, ConfidenceTier::InferredWeak)
    );
    assert_eq!(
        p.method,
        ProducingMethod::Rule {
            rule: BUSINESS_FLOW_RULE.into(),
            version: BUSINESS_FLOW_RULE_VERSION.into()
        }
    );
    let mut expected = BTreeSet::new();
    for props in base
        .graph
        .0
        .iter()
        .filter(|n| n.id == e.src || n.id == e.dst)
        .map(|n| &n.props)
        .chain(std::iter::once(&e.props))
    {
        let prov: Provenance = serde_json::from_value(props["prov"].clone()).unwrap();
        for v in prov.evidence {
            expected.insert((v.repo, v.path, v.byte_start, v.byte_end, v.commit_sha));
        }
    }
    let actual: BTreeSet<_> = p
        .evidence
        .iter()
        .map(|v| {
            (
                v.repo.clone(),
                v.path.clone(),
                v.byte_start,
                v.byte_end,
                v.commit_sha.clone(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert!(p.validate().is_ok());
    let mut overlay = core_graph::overlay::OverlayStore::open_in_memory().unwrap();
    overlay.bind_instance("fixture").unwrap();
    overlay
        .publish(
            &base,
            core_graph::overlay::OverlayRecipe {
                recipe: BUSINESS_FLOW_RULE.into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            report.facts,
        )
        .unwrap();
    assert_eq!(base, before);
}

// AC-0516: every trigger is supported; ambiguous repeated triggers cannot produce duplicate candidates.
#[test]
fn business_flows_cover_every_trigger_and_reverse_stored_edges() {
    for kind in [
        "Screen",
        "ExtensionContext",
        "Command",
        "Schedule",
        "Endpoint",
        "Channel",
    ] {
        let mut base = chain(kind, 1);
        if kind == "Channel" {
            let e = &mut base.graph.1[0];
            std::mem::swap(&mut e.src, &mut e.dst);
            e.label = "SUBSCRIBES".into();
        }
        let report = derive_business_flows(&base, limits()).unwrap();
        assert_eq!(report.facts.len(), 1, "{kind}");
        assert_eq!(
            candidate(&report.facts[0]).props["trace"]["trigger_kind"],
            kind
        );
        let mut repeated = base.clone();
        repeated.graph.0.push(base.graph.0[0].clone());
        let traces =
            flowtracer::trace_bounded(&repeated.graph.0, &repeated.graph.1, limits()).unwrap();
        if kind == "Screen" {
            // Anchor tracing can repeat identical flows; publication must not.
            assert_eq!(traces.len(), 2);
            assert_eq!(
                serde_json::to_value(&traces[0]).unwrap(),
                serde_json::to_value(&traces[1]).unwrap()
            );
        }
        let omitted = derive_business_flows(&repeated, limits()).unwrap();
        assert_eq!(omitted.invalid_flows, traces.len(), "{kind}");
        assert!(omitted.facts.is_empty(), "{kind}");
        if kind == "Channel" {
            assert!(report.facts[0].cites.contains(&BaseRef::Edge(
                "step:1".into(),
                "step:0".into(),
                "SUBSCRIBES".into()
            )));
            assert!(!report.facts[0].cites.contains(&BaseRef::Edge(
                "step:0".into(),
                "step:1".into(),
                "SUBSCRIBES".into()
            )));
        }
    }
}

// AC-0516: Gap details and depth-limited partial state survive; empty traces are named omissions.
#[test]
fn business_flows_preserve_gap_depth_and_empty_state() {
    let mut gap = chain("Endpoint", 1);
    gap.graph.0[1].label = "Gap".into();
    gap.graph.0[1].props["reason"] = json!("runtime target unresolved");
    gap.graph.0[1].props["attempted_tiers"] = json!(["T0", "T1"]);
    gap.graph.0[1].props["prov"]["confidence_tier"] = json!("Gap");
    gap.graph.1[0].props["prov"]["confidence_tier"] = json!("Gap");
    let report = derive_business_flows(&gap, limits()).unwrap();
    let trace = &candidate(&report.facts[0]).props["trace"];
    assert_eq!(trace["status"], "Partial");
    assert_eq!(trace["depth_limited"], false);
    assert_eq!(trace["hops"][0]["gap_reason"], "runtime target unresolved");
    assert_eq!(trace["hops"][0]["attempted_tiers"], json!(["T0", "T1"]));
    assert!(
        report.facts[0]
            .cites
            .contains(&BaseRef::Node("step:1".into()))
    );
    let deep = chain("Endpoint", 70); // reaches the existing tracer depth bound
    let expected = flowtracer::trace_bounded(&deep.graph.0, &deep.graph.1, limits()).unwrap();
    assert!(expected[0].depth_limited);
    let report = derive_business_flows(&deep, limits()).unwrap();
    assert_eq!(
        candidate(&report.facts[0]).props["trace"],
        serde_json::to_value(&expected[0]).unwrap()
    );
    assert_eq!(
        candidate(&report.facts[0]).props["trace"]["status"],
        "Partial"
    );
    let empty = chain("Endpoint", 0);
    let report = derive_business_flows(&empty, limits()).unwrap();
    assert!(report.facts.is_empty());
    assert_eq!(report.empty_flows, 1);
}

// AC-0516: invalid or ambiguous support fails closed and trace limits return no partial report.
#[test]
fn business_flows_refuse_invalid_support_and_trace_overflow() {
    for case in [
        "duplicate-node",
        "duplicate-edge",
        "missing-node",
        "bad-node",
        "bad-edge",
        "no-evidence",
        "bad-span",
    ] {
        let mut base = chain("Endpoint", 1);
        match case {
            "duplicate-node" => base.graph.0.push(base.graph.0[0].clone()),
            "duplicate-edge" => base.graph.1.push(base.graph.1[0].clone()),
            "missing-node" => {
                base.graph.0.pop();
            }
            "bad-node" => base.graph.0[1].props["prov"] = json!(null),
            "bad-edge" => {
                base.graph.1[0].props["prov"]["tier"] = json!("Semantic");
                base.graph.1[0].props["prov"]["confidence_tier"] = json!("Confirmed");
            }
            "no-evidence" => base.graph.0[1].props["prov"]["evidence"] = json!([]),
            "bad-span" => base.graph.0[1].props["prov"]["evidence"][0]["byte_end"] = json!(0),
            _ => unreachable!(),
        }
        let report = derive_business_flows(&base, limits()).unwrap();
        assert!(report.facts.is_empty(), "{case}");
        assert!(report.invalid_flows > 0, "{case}");
    }
    let base = fixture();
    for bound in [
        TraceLimits {
            max_flows: 0,
            ..limits()
        },
        TraceLimits {
            max_hops: 0,
            ..limits()
        },
        TraceLimits {
            max_output_bytes: 1,
            ..limits()
        },
    ] {
        assert_eq!(
            match derive_business_flows(&base, bound).unwrap_err() {
                BusinessFlowOmission::Trace(e) => e,
                _ => panic!("trace omission"),
            },
            flowtracer::trace_bounded(&base.graph.0, &base.graph.1, bound).unwrap_err()
        );
    }
}

// AC-0516: identity binds content deterministically and the existing per-fact cap omits oversized candidates.
#[test]
fn business_flows_bind_content_and_omit_oversized_facts() {
    let mut base = fixture();
    let before = base.clone();
    let original = derive_business_flows(&base, limits()).unwrap();
    base.graph.0.reverse();
    base.graph.1.reverse();
    assert_eq!(derive_business_flows(&base, limits()).unwrap(), original);
    let dst = handled(&base).dst.clone();
    base.graph.0.iter_mut().find(|n| n.id == dst).unwrap().props["name"] =
        json!("changed handler display");
    let changed = derive_business_flows(&base, limits()).unwrap();
    assert_ne!(
        candidate(&changed.facts[0]).id,
        candidate(&original.facts[0]).id
    );
    assert_ne!(
        candidate(&changed.facts[0]).props["prov"]["content_hash"],
        candidate(&original.facts[0]).props["prov"]["content_hash"]
    );
    let mut large = before.clone();
    large
        .graph
        .0
        .iter_mut()
        .find(|n| n.label == "Endpoint")
        .unwrap()
        .props["path"] = json!("x".repeat(MAX_FACT_BYTES));
    let report = derive_business_flows(&large, limits()).unwrap();
    assert!(report.facts.is_empty());
    assert_eq!(report.oversized_flows, 1);
    assert_eq!(derive_business_flows(&before, limits()).unwrap(), original);
}

// AC-0516: cumulative allowance stops repeated large-support decoding before amplified retention.
#[test]
fn business_flows_bound_shared_support_before_repeated_decode() {
    let mut base = chain("Endpoint", 1);
    let mut trigger = base.graph.0.remove(0);
    let edge = base.graph.1.remove(0);
    base.graph.0[0].props["prov"]["evidence"][0]["path"] = json!("p".repeat(MAX_FACT_BYTES / 8));
    for i in 0..12 {
        trigger.id = format!("trigger:{i:02}");
        base.graph.0.push(trigger.clone());
        let mut e = edge.clone();
        e.src = trigger.id.clone();
        base.graph.1.push(e);
    }
    let mut single = base.clone();
    single
        .graph
        .0
        .retain(|n| n.id == "step:1" || n.id == "trigger:00");
    single.graph.1.retain(|e| e.src == "trigger:00");
    let one = derive_business_flows(&single, limits()).unwrap();
    let output = serde_json::to_vec(&one.facts[0]).unwrap().len();
    assert!(output < MAX_FACT_BYTES); // individually legal, amplified when shared
    let provenance_bytes = single
        .graph
        .0
        .iter()
        .map(|n| &n.props)
        .chain(single.graph.1.iter().map(|e| &e.props))
        .map(|p| serde_json::to_vec(&p["prov"]).unwrap().len())
        .sum::<usize>();
    let cost = provenance_bytes + output;
    assert_eq!(
        derive_business_flows(
            &single,
            TraceLimits {
                max_output_bytes: cost,
                ..limits()
            }
        )
        .unwrap(),
        one
    );
    assert_eq!(
        derive_business_flows(
            &single,
            TraceLimits {
                max_output_bytes: cost - 1,
                ..limits()
            }
        )
        .unwrap_err(),
        BusinessFlowOmission::ProjectionBytes
    );
    let shared = serde_json::to_vec(&base.graph.0[0].props["prov"])
        .unwrap()
        .len();
    let bounded = TraceLimits {
        max_output_bytes: cost + shared - 1,
        ..limits()
    };
    let traced = flowtracer::trace_bounded(&base.graph.0, &base.graph.1, bounded).unwrap();
    assert_eq!(traced.len(), 12); // all share the same large participant evidence
    DECODED_SUPPORTS.with(|n| n.set(0));
    let error = derive_business_flows(&base, bounded).unwrap_err();
    assert_eq!(error.code(), "business-flow-projection-bytes");
    assert_eq!(error, BusinessFlowOmission::ProjectionBytes); // no partial report
    assert_eq!(DECODED_SUPPORTS.with(|n| n.get()), 3); // first flow only
    let error = derive_business_flows(
        &base,
        TraceLimits {
            max_hops: 11,
            ..limits()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        BusinessFlowOmission::Trace(TraceLimitExceeded {
            limit: TraceLimit::Hops,
            maximum: 11
        })
    ));
    assert_eq!(error.code(), "business-flow-trace-hops");
}
