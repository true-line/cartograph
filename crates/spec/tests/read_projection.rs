//! The read service consumes the compiler's policy, not a parallel tier rule.
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier as C, Provenance, Tier};
use serde_json::json;
use spec::{ExportMode, project_recovered_facts};
use std::collections::BTreeSet;

#[test]
fn read_projection_preserves_original_facts_and_compiler_policy() {
    // AC-0288: filtering is not H3 activation or a source-receipt identity.
    let variants = [
        ("weak", Tier::Agentic, C::InferredWeak),
        ("strong", Tier::Semantic, C::InferredStrong),
        ("confirmed", Tier::Deterministic, C::Confirmed),
        ("gap", Tier::Deterministic, C::Gap),
    ];
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for (id, tier, confidence) in variants {
        let prov = Provenance::new(tier, confidence, vec![], "fixture", id.as_bytes()).unwrap();
        let props = json!({"prov": prov, "name": id});
        nodes.push(Node {
            id: id.into(),
            label: "Symbol".into(),
            props: props.clone(),
        });
        edges.push(Edge {
            src: "missing-endpoint".into(),
            dst: id.into(),
            label: "CALLS".into(),
            props,
        });
    }
    // Invalid provenance remains an explicit gap under the existing compiler
    // policy. The selector does not repair or silently upgrade the stored bytes.
    nodes.push(Node {
        id: "invalid".into(),
        label: "Symbol".into(),
        props: json!({"prov": {"tier": "bogus"}}),
    });
    let original = nodes.clone();
    let original_edges = edges.clone();
    let rejected: BTreeSet<_> = nodes
        .iter()
        .filter_map(|n| n.props["prov"]["content_hash"].as_str().map(str::to_owned))
        .collect();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let selected = project_recovered_facts(&nodes, &edges, mode, &rejected).0;
        assert_eq!(
            selected.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["confirmed", "gap", "invalid"]
        );
        assert!(
            selected
                .iter()
                .all(|n| nodes.iter().any(|raw| std::ptr::eq(*n, raw)))
        );
        let selected_edges = project_recovered_facts(&nodes, &edges, mode, &rejected).1;
        assert_eq!(
            selected_edges
                .iter()
                .map(|e| e.dst.as_str())
                .collect::<Vec<_>>(),
            ["confirmed", "gap"]
        );
        assert!(selected_edges.iter().all(|e| e.src == "missing-endpoint"));
    }
    let none = BTreeSet::new();
    assert_eq!(
        project_recovered_facts(&nodes, &edges, ExportMode::VerifiedOnly, &none)
            .0
            .len(),
        4
    );
    assert_eq!(
        project_recovered_facts(&nodes, &edges, ExportMode::BestEffort, &none)
            .0
            .len(),
        5
    );
    assert_eq!(
        project_recovered_facts(&nodes, &edges, ExportMode::VerifiedOnly, &none)
            .1
            .len(),
        3
    );
    assert_eq!(
        project_recovered_facts(&nodes, &edges, ExportMode::BestEffort, &none)
            .1
            .len(),
        4
    );
    assert_eq!(nodes, original);
    assert_eq!(edges, original_edges);
}

#[test]
fn hidden_business_rules_cannot_leak_via_confirmed_adjacency() {
    // AC-0288: use the complete selected-compiler base projection.
    let strong = Provenance::new(
        Tier::Semantic,
        C::InferredStrong,
        vec![],
        "fixture",
        b"rule",
    )
    .unwrap();
    let confirmed = Provenance::new(
        Tier::Deterministic,
        C::Confirmed,
        vec![],
        "fixture",
        b"owner",
    )
    .unwrap();
    let nodes = vec![
        Node {
            id: "rule".into(),
            label: "BusinessRule".into(),
            props: json!({"prov": strong}),
        },
        Node {
            id: "owner".into(),
            label: "Symbol".into(),
            props: json!({"prov": confirmed}),
        },
    ];
    let edges = vec![Edge {
        src: "owner".into(),
        dst: "rule".into(),
        label: "GOVERNS".into(),
        props: json!({"prov": confirmed}),
    }];
    let rejected = BTreeSet::from([strong.content_hash]);
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let (selected, relations) = project_recovered_facts(&nodes, &edges, mode, &rejected);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "owner");
        assert!(relations.is_empty());
        let (_, visible_relations) =
            project_recovered_facts(&nodes, &edges, mode, &BTreeSet::new());
        assert_eq!(visible_relations.len(), 1);
    }
}

#[test]
fn catalog_metadata_matches_produced_artifacts_without_claiming_content() {
    // AC-0288: the index enumerates producers; only compiled content has an ID.
    let catalog = spec::artifact_catalog();
    let bundle = spec::compile_spec(&[], &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
    assert_eq!(catalog.len(), bundle.artifacts.len());
    for (descriptor, produced) in catalog.iter().zip(&bundle.artifacts) {
        assert_eq!(descriptor.id, produced.id);
        assert_eq!(descriptor.file_name, produced.file_name);
        assert_eq!(descriptor.title, produced.title);
        assert_eq!(descriptor.format, produced.format);
        assert_eq!(descriptor.schema, produced.schema);
        assert!(
            serde_json::to_value(descriptor)
                .unwrap()
                .get("content_id")
                .is_none()
        );
        assert!(!produced.content_id.is_empty());
    }
}
