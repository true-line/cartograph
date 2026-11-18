//! Named logical payload/slot charges for the existing bounded tracer.
//! This is deliberately independent of allocator capacity and native RSS.
use super::*;
use std::{collections::HashSet, mem::size_of};

/// Version of the logical accounting contract, independent of flow wire IDs.
pub const TRACE_WORKSPACE_MODEL: &str = "flowtracer-logical-workspace-v1";

/// Separately admitted graph ceilings. Rows count nodes plus edges; values
/// bound all JSON value occurrences in the input properties. The caller must
/// establish these ceilings before materializing/tracing the input graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceGraphBound {
    /// Complete admitted input graph UTF-8/serialized payload ceiling.
    pub bytes: usize,
    /// Combined node and edge count ceiling, with unique graph identities.
    pub rows: usize,
    /// JSON property value count ceiling, including array elements.
    pub values: usize,
}

/// Module-owned logical charges, in bytes of the named payload/slot currency.
/// Borrowed graph strings are never charged as newly owned string payload.
///
/// Map/set links are uniform logical entry charges (three/two usize links),
/// not a promise about BTree/HashMap layouts. Capacity slack, collection-private
/// construction storage, allocator metadata, native stacks, input graph storage
/// and whole-process RSS are outside this contract. Stable sorts receive one
/// full element-slot workspace role, separately from their retained vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceWorkspaceBound {
    /// Version of this accounting contract.
    pub model: &'static str,
    /// Admitted returned UTF-8 payload plus Flow/Hop/nested vector element headers.
    pub retained_output_bytes: usize,
    /// Borrowed by-id/adjacency indexes, publication/coverage sets and trigger list.
    pub borrowed_index_bytes: usize,
    /// Current DFS candidate/visited/seen/stack roles, excluding a HopView.
    pub traversal_bytes: usize,
    /// Whole pre-admission outgoing fan-out; independent of the hop limit.
    pub candidate_fanout_bytes: usize,
    /// One provenance/evidence view or fallback hash under construction.
    pub hop_view_bytes: usize,
    /// Largest single stable-sort element workspace.
    pub sort_workspace_bytes: usize,
    /// Output and indexes plus the larger of traversal/view or sorting phases.
    pub peak_bytes: usize,
}

fn slots<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
fn set_slots<K>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<K>().checked_add(size_of::<usize>().checked_mul(2)?)?)
}
fn map_slots<K, V>(count: usize) -> Option<usize> {
    count.checked_mul(
        size_of::<K>()
            .checked_add(size_of::<V>())?
            .checked_add(size_of::<usize>().checked_mul(3)?)?,
    )
}
fn sum(parts: &[usize]) -> Option<usize> {
    parts
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(*part))
}

/// Most evidence elements that `output_bytes` of compact flow JSON can hold.
/// Flow output always writes an evidence element as the full five-field
/// object, so none is shorter than that object with empty strings and
/// one-digit offsets. Measured from the real serializer, not restated.
pub fn output_evidence_bound(output_bytes: usize) -> Option<usize> {
    let smallest = serde_json::to_vec(&EvidenceRef {
        repo: String::new(),
        path: String::new(),
        byte_start: 0,
        byte_end: 0,
        commit_sha: String::new(),
    })
    .ok()?
    .len();
    output_bytes.checked_div(smallest)
}

/// Most attempted-tier elements that `output_bytes` of compact flow JSON can
/// hold. Each is a JSON string of at least its two quotes, and `k` of them in
/// one array need `k - 1` commas and the two brackets: more than three bytes
/// each.
fn output_attempted_tier_bound(output_bytes: usize) -> usize {
    output_bytes / MINIMUM_ATTEMPTED_TIER_BYTES
}
const MINIMUM_ATTEMPTED_TIER_BYTES: usize = 3;

/// Derive the bounded tracer's logical peak workspace with checked arithmetic.
/// Returns `None` if any role/count cannot be represented; zero trace limits
/// retain their existing meaning and are not silently widened.
///
/// The existing tracer owns output only after whole-flow admission. Its UTF-8
/// payload is therefore bounded by `max_output_bytes`; each flow/hop also
/// consumes at least one serialized byte. Evidence/attempted-tier arrays can
/// copy input values into every admitted hop, so their element count is
/// bounded by admitted hops times input values, and by the output bytes
/// divided by the smallest serialized form of one such element.
///
/// The graph bytes ceiling is accepted as part of the explicit admitted graph
/// contract, but is not added: indexes/views borrow its strings and native output
/// payload already has its own byte ceiling. There is no hidden graph clone here.
/// This helper does not account `trace` (the unbounded entry point), compiler flow
/// projection, graph acquisition, or caller serialization of returned flows.
pub fn logical_workspace_bound(
    graph: TraceGraphBound,
    limits: TraceLimits,
) -> Option<TraceWorkspaceBound> {
    let rows = graph.rows;
    let flows = limits.max_flows.min(rows).min(limits.max_output_bytes);
    let hops = limits.max_hops.min(limits.max_output_bytes);
    let nested = hops.checked_mul(graph.values)?;
    let evidence = nested.min(output_evidence_bound(limits.max_output_bytes)?);
    let attempted_tiers = nested.min(output_attempted_tier_bound(limits.max_output_bytes));
    let retained_output_bytes = sum(&[
        limits.max_output_bytes,
        size_of::<Vec<Flow>>(),
        slots::<Flow>(flows)?,
        slots::<Hop>(hops)?,
        slots::<EvidenceRef>(evidence)?,
        slots::<String>(attempted_tiers)?, // attempted_tiers element headers
    ])?;
    // Each edge enters at most one of the three adjacency maps. Their combined
    // key count and combined Vec<&Edge> element count are each <= graph rows.
    // Keys, publication/coverage IDs and trigger references remain borrowed.
    let borrowed_index_bytes = sum(&[
        size_of::<BTreeMap<&str, &Node>>(),
        map_slots::<&str, &Node>(rows)?,
        size_of::<BTreeMap<&str, Vec<&Edge>>>().checked_mul(3)?,
        map_slots::<&str, Vec<&Edge>>(rows)?,
        slots::<&Edge>(rows)?,
        size_of::<HashSet<&str>>().checked_mul(2)?,
        set_slots::<&str>(rows)?, // published_to: <= one destination per edge
        set_slots::<&str>(rows.min(hops))?, // covered: returned FETCHES destinations
        size_of::<Vec<&Node>>(),
        slots::<&Node>(rows)?, // anchors OR rest; their owning iterators do not coexist
    ])?;
    // next is built before admit_hop. H=0 still permits an N-edge borrowed
    // fan-out allocation. seen_hops inserts the failing H+1 key before admission.
    let candidate_fanout_bytes = slots::<Candidate<'_>>(rows)?;
    let seen = rows.min(limits.max_hops.checked_add(1)?);
    let traversal_bytes = sum(&[
        size_of::<Vec<Candidate<'_>>>().checked_mul(2)?, // hops and next
        slots::<Candidate<'_>>(rows.min(limits.max_hops))?,
        candidate_fanout_bytes,
        size_of::<HashSet<(&str, &str, &str)>>(),
        set_slots::<(&str, &str, &str)>(seen)?,
        size_of::<HashSet<&str>>(),
        set_slots::<&str>(rows)?, // visited includes external edge destinations
        size_of::<Vec<(&str, usize)>>(),
        slots::<(&str, usize)>(rows.checked_add(1)?)?, // one push per edge plus trigger
        size_of::<FlowView<'_, '_>>(),
        size_of::<TraceBudget>(),
    ])?;
    // Deserialization borrows every source-authored string. Its only vector is
    // EvidenceView elements. Invalid provenance drops any partial evidence view
    // before constructing the fixed lowercase fallback content hash.
    let validation = size_of::<ProvenanceView<'_>>().checked_add(size_of::<Provenance>())?;
    let view_header = size_of::<HopView<'_>>().max(validation);
    let borrowed_view = view_header.checked_add(slots::<EvidenceView<'_>>(graph.values)?)?;
    let fallback = size_of::<blake3::Hasher>().checked_add(blake3::OUT_LEN.checked_mul(2)?)?;
    let hop_view_bytes = borrowed_view.max(fallback);
    let sort_workspace_bytes = slots::<&Edge>(rows)?
        .max(slots::<&Node>(rows)?)
        .max(slots::<Flow>(flows)?);
    let peak_bytes = sum(&[
        retained_output_bytes,
        borrowed_index_bytes,
        traversal_bytes
            .checked_add(hop_view_bytes)?
            .max(sort_workspace_bytes),
    ])?;
    Some(TraceWorkspaceBound {
        model: TRACE_WORKSPACE_MODEL,
        retained_output_bytes,
        borrowed_index_bytes,
        traversal_bytes,
        candidate_fanout_bytes,
        hop_view_bytes,
        sort_workspace_bytes,
        peak_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> TraceGraphBound {
        TraceGraphBound {
            bytes: 4096,
            rows: 20,
            values: 100,
        }
    }
    fn limits() -> TraceLimits {
        TraceLimits {
            max_flows: 10,
            max_hops: 30,
            max_output_bytes: 8192,
        }
    }

    #[test]
    fn workspace_checked_arithmetic_rejects_unrepresentable_roles() {
        for graph in [
            TraceGraphBound {
                rows: usize::MAX,
                ..graph()
            },
            TraceGraphBound {
                values: usize::MAX,
                ..graph()
            },
        ] {
            assert!(logical_workspace_bound(graph, limits()).is_none());
        }
        for limits in [
            TraceLimits {
                max_hops: usize::MAX,
                ..limits()
            },
            TraceLimits {
                max_output_bytes: usize::MAX,
                ..limits()
            },
        ] {
            assert!(logical_workspace_bound(graph(), limits).is_none());
        }
    }

    #[test]
    fn workspace_roles_are_monotone_without_copying_borrowed_graph_payload() {
        let baseline = logical_workspace_bound(graph(), limits()).unwrap();
        assert_eq!(baseline.model, TRACE_WORKSPACE_MODEL);
        for wider in [
            TraceGraphBound {
                rows: 40,
                ..graph()
            },
            TraceGraphBound {
                values: 200,
                ..graph()
            },
            TraceGraphBound {
                bytes: 8192,
                ..graph()
            },
        ] {
            assert!(
                logical_workspace_bound(wider, limits()).unwrap().peak_bytes >= baseline.peak_bytes
            );
        }
        // Only borrowed string lengths changed: no fictitious raw graph clone.
        assert_eq!(
            logical_workspace_bound(
                TraceGraphBound {
                    bytes: usize::MAX,
                    ..graph()
                },
                limits()
            )
            .unwrap(),
            baseline
        );
        for wider in [
            TraceLimits {
                max_flows: 20,
                ..limits()
            },
            TraceLimits {
                max_hops: 60,
                ..limits()
            },
            TraceLimits {
                max_output_bytes: 16384,
                ..limits()
            },
        ] {
            assert!(
                logical_workspace_bound(graph(), wider).unwrap().peak_bytes >= baseline.peak_bytes
            );
        }
    }

    fn hop(evidence: usize, tiers: usize) -> Hop {
        let mut provenance = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Gap,
            vec![
                EvidenceRef {
                    repo: String::new(),
                    path: String::new(),
                    byte_start: 0,
                    byte_end: 0,
                    commit_sha: String::new(),
                };
                evidence
            ],
            "",
            b"",
        )
        .unwrap();
        provenance.content_hash.clear();
        Hop {
            label: String::new(),
            src: String::new(),
            dst: String::new(),
            src_name: String::new(),
            dst_name: String::new(),
            tier: String::new(),
            confidence: String::new(),
            evidence: None,
            provenance,
            gap_reason: None,
            attempted_tiers: vec![String::new(); tiers],
        }
    }

    #[test]
    fn nested_output_slots_follow_the_smallest_serialized_elements() {
        // AC-0732: the densest evidence and attempted-tier arrays (empty
        // strings, one-digit offsets) meet their element bounds exactly, and
        // one byte less admits one element fewer.
        let wire = |value: &Hop| serde_json::to_vec(value).unwrap().len();
        for count in [1usize, 2, 7, 500] {
            let evidence = wire(&hop(count, 0)) - wire(&hop(0, 0));
            // `count` objects and the commas between them.
            let smallest = (evidence + 1) / count - 1;
            assert_eq!(evidence, count * smallest + count - 1);
            assert_eq!(output_evidence_bound(count * smallest), Some(count));
            assert_eq!(output_evidence_bound(count * smallest - 1), Some(count - 1));
            assert!(output_evidence_bound(evidence).unwrap() >= count);
            let tiers = wire(&hop(0, count)) - wire(&hop(0, 0));
            assert_eq!(tiers, count * MINIMUM_ATTEMPTED_TIER_BYTES - 1);
            assert_eq!(output_attempted_tier_bound(tiers + 1), count);
            assert_eq!(output_attempted_tier_bound(tiers), count - 1);
        }
        // A whole admitted output of the densest hop stays inside the role.
        let dense = vec![Flow {
            trigger: String::new(),
            trigger_kind: String::new(),
            trigger_name: String::new(),
            hops: vec![hop(300, 0), hop(0, 300)],
            status: FlowStatus::Inferred,
            score: 0.0,
            depth_limited: false,
        }];
        let output = serde_json::to_vec(&dense).unwrap().len();
        let bound = |output| {
            logical_workspace_bound(
                TraceGraphBound {
                    bytes: 0,
                    rows: 2,
                    values: 300,
                },
                TraceLimits {
                    max_flows: 1,
                    max_hops: 2,
                    max_output_bytes: output,
                },
            )
            .unwrap()
            .retained_output_bytes
        };
        let owned = output
            + size_of::<Vec<Flow>>()
            + size_of::<Flow>()
            + 2 * size_of::<Hop>()
            + 300 * size_of::<EvidenceRef>()
            + 300 * size_of::<String>();
        assert!(owned <= bound(output));
        // The element roles no longer grow by one slot per output byte.
        assert!(bound(2 * output) - bound(output) < size_of::<String>() * output);
    }

    #[test]
    fn zero_hop_budget_still_charges_complete_borrowed_fanout() {
        let zero = logical_workspace_bound(
            graph(),
            TraceLimits {
                max_hops: 0,
                ..limits()
            },
        )
        .unwrap();
        assert_eq!(
            zero.candidate_fanout_bytes,
            graph().rows * size_of::<Candidate<'_>>()
        );
        assert!(zero.traversal_bytes >= zero.candidate_fanout_bytes);
    }

    #[test]
    fn provenance_workspace_counts_borrowed_evidence_slots_and_fixed_fallback() {
        let edge = Edge {
            src: "source".into(),
            dst: "target".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "tier":"Deterministic", "confidence_tier":"Confirmed", "evidence":[
                    {"repo":"repo", "path":"x".repeat(10000), "byte_start":0, "byte_end":1, "commit_sha":"sha"},
                    {"repo":"repo", "path":"other", "byte_start":1, "byte_end":2, "commit_sha":"sha"}
                ], "extractor_id":"fixture", "content_hash":"hash"
            }}),
        };
        let view = HopView::new(
            Candidate {
                edge: &edge,
                reversed: false,
            },
            &BTreeMap::new(),
        );
        assert_eq!(view.provenance.evidence.len(), 2);
        assert_eq!(
            view.provenance.evidence[0].path.as_ptr(),
            edge.props["prov"]["evidence"][0]["path"]
                .as_str()
                .unwrap()
                .as_ptr()
        );
        let bound = logical_workspace_bound(
            TraceGraphBound {
                bytes: 20000,
                rows: 1,
                values: 32,
            },
            limits(),
        )
        .unwrap();
        assert!(
            bound.hop_view_bytes
                >= size_of::<HopView<'_>>()
                    + view.provenance.evidence.len() * size_of::<EvidenceView<'_>>()
        );
        assert!(bound.hop_view_bytes >= size_of::<blake3::Hasher>() + blake3::OUT_LEN * 2);
    }
}

#[cfg(test)]
mod parsing_parity_tests {
    use super::*;
    use serde_json::{Value, json};

    fn provenance() -> Value {
        json!({"tier":"Deterministic","confidence_tier":"Confirmed","evidence":[],"extractor_id":"fixture","content_hash":"hash"})
    }
    fn parity(value: &Value) {
        let previous = ProvenanceView::deserialize(value);
        let borrowed = ProvenanceView::borrow(value);
        assert_eq!(
            previous.is_ok(),
            borrowed.is_some(),
            "accepted shape changed: {value}"
        );
        if let (Ok(previous), Some(borrowed)) = (previous, borrowed) {
            assert_eq!(
                serde_json::to_vec(&previous).unwrap(),
                serde_json::to_vec(&borrowed).unwrap()
            );
        }
    }

    #[test]
    fn borrowed_provenance_parser_preserves_serde_shapes_without_source_diagnostics() {
        let base = provenance();
        parity(&base);
        for field in [
            "tier",
            "confidence_tier",
            "evidence",
            "extractor_id",
            "content_hash",
        ] {
            let mut missing = base.clone();
            missing.as_object_mut().unwrap().remove(field);
            parity(&missing);
            for wrong in [
                Value::Null,
                json!(true),
                json!(3),
                json!(1.0),
                json!([]),
                json!({}),
                json!("unexpected".repeat(10000)),
            ] {
                let mut value = base.clone();
                value[field] = wrong;
                parity(&value);
            }
        }
        for tier in ["Deterministic", "Dynamic", "Semantic", "Agentic"] {
            for confidence in ["Confirmed", "InferredStrong", "InferredWeak", "Gap"] {
                let mut value = base.clone();
                value["tier"] = json!({tier: null});
                value["confidence_tier"] = json!({confidence: null});
                parity(&value);
            }
        }
        for method in [
            json!({"kind":"unrecorded","ignored":{"large":"x".repeat(10000)}}),
            json!({"kind":"rule","rule":"r","version":"v"}),
            json!({"kind":"model","model":"m"}),
            json!({"kind":"human","actor":"h"}),
            Value::Null,
            json!({"kind":"model"}),
            json!({"kind":"x".repeat(10000)}),
            json!({"kind":false}),
            json!({"kind":"rule","rule":"r","version":3}),
        ] {
            let mut value = base.clone();
            value["method"] = method;
            parity(&value);
        }
        for sequence in [
            json!(["Deterministic", "Confirmed", [], "extractor", "hash"]),
            json!(["Deterministic","Confirmed",[],"extractor","hash",{"kind":"model","model":"m"}]),
            json!([
                "Deterministic",
                "Confirmed",
                [["repo", "file", 0, 1, "sha"]],
                "extractor",
                "hash"
            ]),
            json!(["Deterministic", "Confirmed", [], "extractor", "hash", null]),
            json!(["Deterministic","Confirmed",[],"extractor","hash",{"kind":"unrecorded"},1]),
        ] {
            parity(&sequence);
        }
        for method in [
            json!(["rule", "r", "v"]),
            json!(["model", "m"]),
            json!(["human", "h"]),
            json!(["unrecorded"]),
            json!(["unrecorded", 1]),
            json!(["model", "m", 1]),
            json!(["model"]),
        ] {
            let mut value = base.clone();
            value["method"] = method;
            parity(&value);
        }
        for kind in [
            json!(0),
            json!(1),
            json!(2),
            json!(3),
            json!(4),
            json!(-1),
            json!(1.0),
        ] {
            let mut value = base.clone();
            value["method"] = json!({"kind":kind,"rule":"r","version":"v","model":"m","actor":"a"});
            parity(&value);
            value["method"] = json!([kind, "m"]);
            parity(&value);
        }
        let evidence =
            json!({"repo":"repo","path":"file","byte_start":0,"byte_end":1,"commit_sha":"sha"});
        for field in ["repo", "path", "byte_start", "byte_end", "commit_sha"] {
            for wrong in [
                Value::Null,
                json!(true),
                json!(-1),
                json!(1.0),
                json!(u64::MAX),
                json!([]),
                json!({}),
                json!("x".repeat(10000)),
            ] {
                let mut entry = evidence.clone();
                entry[field] = wrong;
                let mut value = base.clone();
                value["evidence"] = json!([entry]);
                parity(&value);
            }
        }
    }

    #[test]
    fn borrowed_traversal_roles_cover_wide_external_fanout_and_unchanged_output() {
        let nodes = vec![Node {
            id: "screen".into(),
            label: "Screen".into(),
            props: json!({}),
        }];
        let edges: Vec<_> = (0..100)
            .map(|i| Edge {
                src: "screen".into(),
                dst: format!("external:{i}"),
                label: "CALLS".into(),
                props: json!({}),
            })
            .collect();
        let limits = TraceLimits {
            max_flows: 1,
            max_hops: 100,
            max_output_bytes: 1024 * 1024,
        };
        let bounded = crate::trace_bounded(&nodes, &edges, limits).unwrap();
        assert_eq!(
            serde_json::to_vec(&bounded).unwrap(),
            serde_json::to_vec(&crate::trace(&nodes, &edges)).unwrap()
        );
        assert_eq!(bounded[0].hops.len(), 100);
        let graph = TraceGraphBound {
            bytes: serde_json::to_vec(&(&nodes, &edges)).unwrap().len(),
            rows: nodes.len() + edges.len(),
            values: 1024,
        };
        let bound = logical_workspace_bound(graph, limits).unwrap();
        assert!(
            bound.retained_output_bytes
                >= serde_json::to_vec(&bounded).unwrap().len()
                    + size_of::<Flow>()
                    + bounded[0].hops.len() * size_of::<Hop>()
        );
        let denied = TraceLimits {
            max_hops: 0,
            ..limits
        };
        assert_eq!(
            crate::trace_bounded(&nodes, &edges, denied)
                .unwrap_err()
                .limit,
            TraceLimit::Hops
        );
        assert!(
            logical_workspace_bound(graph, denied)
                .unwrap()
                .candidate_fanout_bytes
                >= edges.len() * size_of::<Candidate<'_>>()
        );
    }
}
