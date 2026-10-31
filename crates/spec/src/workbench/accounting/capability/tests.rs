// AC-0508: Capability compiler workspace composition (#1466).
use super::*;
use core_graph::{
    RevisionedSnapshot, StoreRevision,
    overlay::{OverlayFact, OverlayItem},
};
use core_prov::{ConfidenceTier, ProducingMethod, Tier};
use serde_json::json;
use std::mem::size_of_val;

fn inputs() -> (TraceGraphBound, TraceLimits, SelectedCompileLimits) {
    (
        TraceGraphBound {
            bytes: 4096,
            rows: 20,
            values: 100,
        },
        TraceLimits {
            max_flows: 1,
            max_hops: 0,
            max_output_bytes: 8192,
        },
        SelectedCompileLimits {
            max_bytes: 32768,
            adr: crate::AdrLimits {
                max_rows: 30,
                max_output_bytes: 4096,
            },
        },
    )
}

#[test]
fn capability_roles_grow_before_phase_max_and_refuse_overflow() {
    let (graph, trace, compile) = inputs();
    let a = logical_capability_compile_bound(graph, trace, compile, 1, 4096).unwrap();
    let count = logical_capability_compile_bound(graph, trace, compile, 2, 4096).unwrap();
    assert!(count.lookup_bytes > a.lookup_bytes);
    assert!(count.story_support_bytes > a.story_support_bytes);
    assert!(count.renderer_extra_bytes > a.renderer_extra_bytes);
    let bytes = logical_capability_compile_bound(graph, trace, compile, 1, 8192).unwrap();
    assert!(bytes.max_citations > a.max_citations);
    assert!(bytes.story_support_bytes > a.story_support_bytes);
    assert!(bytes.validation_scratch_bytes > a.validation_scratch_bytes);
    assert!(bytes.renderer_extra_bytes > a.renderer_extra_bytes);
    let cite = count_bytes(&BaseRef::Node(String::new()));
    let before = logical_capability_compile_bound(graph, trace, compile, 1, cite - 1).unwrap();
    let at = logical_capability_compile_bound(graph, trace, compile, 1, cite).unwrap();
    assert_eq!(before.max_citations, 0);
    assert_eq!(at.max_citations, 1);
    assert!(
        at.story_support_bytes >= before.story_support_bytes + size_of::<(&BaseRef, Support<'_>)>()
    );
    for (n, bytes) in [(usize::MAX, 1), (1, usize::MAX)] {
        assert!(logical_capability_compile_bound(graph, trace, compile, n, bytes).is_none());
    }
    assert!(
        logical_capability_compile_bound(
            TraceGraphBound {
                values: usize::MAX,
                ..graph
            },
            trace,
            compile,
            1,
            1
        )
        .is_none()
    );
}

fn count_bytes(value: &impl Serialize) -> usize {
    count(value).unwrap()
}

#[test]
fn capability_bound_covers_real_shared_support_indexes_and_official_output() {
    let (_, trace, compile) = inputs();
    let evidence = EvidenceRef {
        repo: "repo".into(),
        path: "café.ts".into(),
        byte_start: 0,
        byte_end: 8,
        commit_sha: "fixed".into(),
    };
    let support = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![evidence.clone()],
        "fixture",
        b"base",
    )
    .unwrap();
    let nodes: Vec<_> = (0..16).map(|i| Node {
        id: format!("base:{i}"), label: if i == 0 { "Endpoint" } else { "Symbol" }.into(),
        props: json!({"name":format!("shared {i}"), "method":"GET", "path":"/", "authenticated":false, "nested":[{"value":"é\n"}], "prov":support}),
    }).collect();
    let base = RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "0".repeat(32),
            generation: 1,
        },
        graph: (nodes, vec![]),
    };
    let cites: BTreeSet<_> = base
        .graph
        .0
        .iter()
        .map(|n| BaseRef::Node(n.id.clone()))
        .collect();
    let p = Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
        vec![evidence],
        "fixture",
        b"capability",
    )
    .unwrap()
    .with_method(ProducingMethod::Rule {
        rule: "knowledge.capability".into(),
        version: "1".into(),
    });
    let facts: Vec<_> = (0..2)
        .map(|i| OverlayFact {
            item: OverlayItem::Node(Node {
                id: format!("cap:{i}"),
                label: "Capability".into(),
                props: json!({"name":"derived", "prov":p}),
            }),
            cites: cites.clone(),
        })
        .collect();
    let candidates: Vec<_> = facts
        .iter()
        .map(|f| {
            let OverlayItem::Node(n) = &f.item else {
                unreachable!()
            };
            CapabilityStory {
                capability: n,
                cites: &f.cites,
            }
        })
        .collect();
    let bytes = count_bytes(&base.graph);
    let graph = TraceGraphBound {
        bytes,
        rows: base.graph.0.len(),
        values: bytes,
    };
    let selected_bytes = facts.iter().map(count_bytes).sum();
    let bound =
        logical_capability_compile_bound(graph, trace, compile, facts.len(), selected_bytes)
            .unwrap();
    // Independently materialize actual compiler roles and count their owners.
    let lookup: BTreeMap<_, _> = base
        .graph
        .0
        .iter()
        .map(|n| (n.id.as_str(), Some(n)))
        .collect();
    let visible: BTreeSet<_> = lookup.keys().copied().collect();
    let ordered: BTreeMap<_, _> = candidates
        .iter()
        .map(|c| (c.capability.id.as_str(), c))
        .collect();
    let stories: Vec<_> = ordered
        .values()
        .map(|c| Story {
            node: c.capability,
            support: c
                .cites
                .iter()
                .map(|cite| {
                    let BaseRef::Node(id) = cite else {
                        unreachable!()
                    };
                    (cite, Support::Node(lookup[id.as_str()].unwrap()))
                })
                .collect(),
        })
        .collect();
    let support_count: usize = stories.iter().map(|s| s.support.len()).sum();
    assert!(bound.max_citations >= support_count);
    assert!(
        bound.story_support_bytes
            >= size_of_val(stories.as_slice())
                + support_count * size_of::<(&BaseRef, Support<'_>)>()
    );
    let actual_lookup = map_slots::<&str, Option<&Node>>(lookup.len()).unwrap()
        + set_slots::<&str>(visible.len()).unwrap()
        + map_slots::<&str, &CapabilityStory<'_>>(ordered.len()).unwrap();
    assert!(bound.lookup_bytes >= actual_lookup);
    let decoded = Provenance::deserialize(&candidates[0].capability.props["prov"]).unwrap();
    assert!(
        bound.validation_scratch_bytes
            >= count_bytes(&decoded) + size_of_val(decoded.evidence.as_slice())
    );
    let output = compile_capability_user_stories(
        &base,
        &candidates,
        ExportMode::BestEffort,
        &BTreeSet::new(),
        compile,
    )
    .unwrap();
    let SelectedArtifactOutcome::Produced(artifact) = output else {
        panic!("fixture fits")
    };
    assert_eq!(artifact.assertions.len(), facts.len() + support_count);
    assert!(artifact.verify_identity());
    let ordinary = logical_compile_bound(graph, trace, compile).unwrap();
    assert!(count_bytes(&artifact) <= ordinary.artifact_bytes);
    assert!(
        bound.peak_bytes
            >= ordinary.peak_bytes + bound.validation_retained_bytes + bound.renderer_extra_bytes
    );
    // security=None still creates the ordinary security rows within the same maximum.
    let maximum = crate::security_projection_bound(graph.bytes, graph.rows, graph.values).unwrap();
    let security = crate::security::derive_security_findings(&base.graph.0, &base.graph.1);
    assert!(!security.is_empty());
    assert_eq!(
        security,
        crate::derive_security_findings_bounded(
            &base.graph.0,
            &base.graph.1,
            crate::SecurityLimits {
                max_rows: maximum.rows,
                max_output_bytes: maximum.bytes
            }
        )
        .unwrap()
    );
}
