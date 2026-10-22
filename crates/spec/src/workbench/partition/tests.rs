use super::*;
use core_graph::{GraphStore, SnapshotReadLimits, SqliteGraphStore};
use serde_json::json;
fn node(id: &str, label: &str, confidence: ConfidenceTier) -> Node {
    let tier = if confidence == ConfidenceTier::InferredWeak {
        Tier::Agentic
    } else {
        Tier::Deterministic
    };
    let prov = Provenance::new(
        tier,
        confidence,
        vec![core_prov::EvidenceRef {
            repo: "test/repo".into(),
            path: "source.ts".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "abc".into(),
        }],
        "partition.fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({"name":id,"prov":prov}),
    }
}
fn edge(src: &str, dst: &str, label: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: node(
            &format!("{src}:{label}:{dst}"),
            "Symbol",
            ConfidenceTier::Confirmed,
        )
        .props,
    }
}
fn compile(
    store: &SqliteGraphStore,
    kind: PartitionArtifactKind,
    root: &str,
    limits: SnapshotReadLimits,
    max: usize,
) -> Result<PartitionArtifact, PartitionError> {
    store.with_scoped_reader(&store.read_revision().unwrap(), limits, |reader| {
        compile_partition_selected(
            reader,
            kind,
            root,
            &format!("mcp-scoped-session-v1:{}", "a".repeat(64)),
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
            max,
        )
    })
}
fn fixture() -> SqliteGraphStore {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for n in [
        node("entity:a", "DataEntity", ConfidenceTier::Confirmed),
        node("entity:b", "DataEntity", ConfidenceTier::Confirmed),
        node("gap:target", "Gap", ConfidenceTier::Gap),
        node("caller", "Symbol", ConfidenceTier::Confirmed),
    ] {
        store.put_node(&n).unwrap();
    }
    for e in [
        edge("entity:a", "entity:b", "REFERENCES"),
        edge("entity:b", "gap:target", "REFERENCES"),
        edge("caller", "entity:a", "WRITES"),
    ] {
        store.put_edge(&e).unwrap();
    }
    store
}
#[test]
fn data_partition_matches_exact_component_oracle_and_two_entry_roots_share_identity() {
    let store = fixture();
    let first = compile(
        &store,
        PartitionArtifactKind::DataModel,
        "entity:a",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    let second = compile(
        &store,
        PartitionArtifactKind::DataModel,
        "entity:b",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    assert_eq!(first.roots, ["entity:a", "entity:b"]);
    assert_eq!(first.content_id, second.content_id);
    assert_eq!(first.partition_basis_id, second.partition_basis_id);
    assert_eq!(first.content, second.content);
    assert!(first.content.contains("gap:target"));
    assert!(!first.complete_system);
    let (nodes, edges) = store.read_snapshot().unwrap();
    let (n, e) = selected::project_recovered_facts(
        &nodes,
        &edges,
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
    );
    let oracle = producers::data(&n, &e, None).unwrap();
    assert!(first.content.ends_with(&oracle.content));
    assert_eq!(first.assertions, oracle.assertions);
    assert_ne!(first.schema, SpecArtifact::expected_schema("data-model"));
}
#[test]
fn partition_limits_and_dangling_or_hidden_roots_never_emit_prefix_artifacts() {
    let mut store = fixture();
    let full = compile(
        &store,
        PartitionArtifactKind::DataModel,
        "entity:a",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    let exact = serde_json::to_vec(&full).unwrap().len();
    assert!(
        compile(
            &store,
            PartitionArtifactKind::DataModel,
            "entity:a",
            SnapshotReadLimits::default(),
            exact
        )
        .is_ok()
    );
    assert!(matches!(
        compile(
            &store,
            PartitionArtifactKind::DataModel,
            "entity:a",
            SnapshotReadLimits::default(),
            exact - 1
        ),
        Err(PartitionError::Limit)
    ));
    assert!(
        compile(
            &store,
            PartitionArtifactKind::DataModel,
            "entity:a",
            SnapshotReadLimits {
                max_rows: 2,
                ..Default::default()
            },
            100_000
        )
        .is_err()
    );
    assert!(matches!(
        compile(
            &store,
            PartitionArtifactKind::DataModel,
            "missing",
            SnapshotReadLimits::default(),
            100_000
        ),
        Err(PartitionError::Unavailable)
    ));
    store
        .put_node(&node("hidden", "DataEntity", ConfidenceTier::InferredWeak))
        .unwrap();
    assert!(matches!(
        compile(
            &store,
            PartitionArtifactKind::DataModel,
            "hidden",
            SnapshotReadLimits::default(),
            100_000
        ),
        Err(PartitionError::Unavailable)
    ));
}
#[test]
fn data_partition_reuses_hidden_business_rule_relationship_policy() {
    let mut store = fixture();
    store
        .put_node(&node(
            "hidden-rule",
            "BusinessRule",
            ConfidenceTier::InferredWeak,
        ))
        .unwrap();
    store
        .put_edge(&edge("hidden-rule", "entity:a", "READS"))
        .unwrap();
    let artifact = compile(
        &store,
        PartitionArtifactKind::DataModel,
        "entity:a",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    assert!(!artifact.content.contains("hidden-rule"));
    assert!(
        !artifact
            .assertions
            .iter()
            .any(|a| a.subject_id.contains("hidden-rule"))
    );
}
#[test]
fn rule_partition_has_explicit_inventory_root_and_index_is_qualified_json() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for n in [
        node("rule:one", "BusinessRule", ConfidenceTier::Confirmed),
        node("rule:context", "BusinessRule", ConfidenceTier::Confirmed),
    ] {
        store.put_node(&n).unwrap();
    }
    store
        .put_edge(&edge("rule:one", "rule:context", "DEPENDS_ON"))
        .unwrap();
    use core_graph::rules::{
        GuardedExitEvidence, Interpretation, InterpretationStatus, LocalExit, RuleKind,
    };
    let payload = GuardedExitEvidence {
        schema_version: 1,
        kind: RuleKind::GuardedExit,
        owner_id: "owner:lookup-only".into(),
        exit_source: core_prov::EvidenceRef {
            repo: "test/repo".into(),
            path: "source.ts".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "abc".into(),
        },
        source_order: 0,
        conditions: vec![],
        effect: LocalExit::Return { value: None },
        dependencies: vec![],
        interpretation: Interpretation {
            execution_predicate: InterpretationStatus::NotEstablished,
            consumer_effect: InterpretationStatus::NotEstablished,
            gap_ids: vec![],
        },
        redactions: vec![],
        local_definitions: None,
    };
    let typed = serde_json::to_value(&payload).unwrap();
    GuardedExitEvidence::from_value(typed.clone()).unwrap();
    let mut rule = node("rule:one", "BusinessRule", ConfidenceTier::Confirmed);
    rule.props["rule"] = typed;
    store.put_node(&rule).unwrap();
    store
        .put_node(&node(
            "owner:lookup-only",
            "Symbol",
            ConfidenceTier::Confirmed,
        ))
        .unwrap();
    let md = compile(
        &store,
        PartitionArtifactKind::RuleEvidence,
        "rule:one",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    assert!(md.content.contains("Observation rule:one"));
    assert!(md.content.contains("owner:lookup-only"));
    assert!(!md.content.contains("Observation rule:context"));
    let index = compile(
        &store,
        PartitionArtifactKind::RuleEvidenceIndex,
        "rule:one",
        SnapshotReadLimits::default(),
        100_000,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&index.content).unwrap();
    assert_eq!(value["partition"]["complete_system"], false);
    let (nodes, edges) = store.read_snapshot().unwrap();
    let (n, e) = selected::project_recovered_facts(
        &nodes,
        &edges,
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
    );
    let roots = BTreeSet::from(["rule:one".into()]);
    let oracle = crate::rules::inventory_selected_roots(
        &n,
        &e,
        ExportMode::VerifiedOnly,
        crate::rules::InventorySelection {
            markdown: false,
            index: true,
        },
        None,
        Some(&roots),
    )
    .unwrap();
    assert_eq!(
        value["inventory"],
        serde_json::from_str::<serde_json::Value>(&oracle.index.unwrap()).unwrap()
    );
}

#[test]
fn partition_workspace_names_new_owners_and_checks_arithmetic() {
    let graph = flowtracer::TraceGraphBound {
        bytes: 4096,
        rows: 32,
        values: 256,
    };
    let trace = flowtracer::TraceLimits {
        max_flows: 4,
        max_hops: 8,
        max_output_bytes: 4096,
    };
    let limits = SelectedCompileLimits {
        max_bytes: 4096,
        adr: crate::AdrLimits {
            max_rows: 32,
            max_output_bytes: 4096,
        },
    };
    let bound = logical_partition_bound(graph, trace, limits).unwrap();
    assert!(
        bound.peak_bytes
            >= bound.acquisition_bytes
                + bound.frontier_bytes
                + bound.projection_bytes
                + bound.renderer_bytes
                + bound.artifact_bytes
    );
    let larger = logical_partition_bound(
        flowtracer::TraceGraphBound {
            bytes: 4097,
            ..graph
        },
        trace,
        limits,
    )
    .unwrap();
    assert!(larger.peak_bytes >= bound.peak_bytes);
    assert!(
        logical_partition_bound(
            flowtracer::TraceGraphBound {
                bytes: usize::MAX,
                ..graph
            },
            trace,
            limits
        )
        .is_none()
    );
}

fn typed_rule(owner: &str, dependencies: Vec<core_graph::rules::RuleDependency>) -> Node {
    use core_graph::rules::*;
    let payload = GuardedExitEvidence {
        schema_version: 1,
        kind: RuleKind::GuardedExit,
        owner_id: owner.into(),
        exit_source: core_prov::EvidenceRef {
            repo: "test/repo".into(),
            path: "source.ts".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "abc".into(),
        },
        source_order: 0,
        conditions: vec![],
        effect: LocalExit::Return { value: None },
        dependencies,
        interpretation: Interpretation {
            execution_predicate: InterpretationStatus::NotEstablished,
            consumer_effect: InterpretationStatus::NotEstablished,
            gap_ids: vec![],
        },
        redactions: vec![],
        local_definitions: None,
    };
    let mut rule = node("rule:one", "BusinessRule", ConfidenceTier::Confirmed);
    rule.props["rule"] = serde_json::to_value(payload).unwrap();
    GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    rule
}
#[test]
fn partition_dangling_data_endpoint_and_rule_dereferences_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut data = SqliteGraphStore::open(&path).unwrap();
    data.put_node(&node("entity", "DataEntity", ConfidenceTier::Confirmed))
        .unwrap();
    // Simulate a supported older/raw connection with FK enforcement off. The
    // generation trigger still advances, but the dangling edge cannot close.
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.pragma_update(None, "foreign_keys", false).unwrap();
    raw.execute(
        "INSERT INTO edges(src,dst,label,props) VALUES('entity','missing','REFERENCES','{}')",
        [],
    )
    .unwrap();
    assert!(matches!(
        compile(
            &data,
            PartitionArtifactKind::DataModel,
            "entity",
            SnapshotReadLimits::default(),
            100_000
        ),
        Err(PartitionError::Unavailable)
    ));
    for missing_owner in [true, false] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let dependencies = if missing_owner {
            vec![]
        } else {
            vec![core_graph::rules::RuleDependency {
                role: core_graph::rules::DependencyRole::ExitValue,
                source: core_prov::EvidenceRef {
                    repo: "test/repo".into(),
                    path: "source.ts".into(),
                    byte_start: 0,
                    byte_end: 1,
                    commit_sha: "abc".into(),
                },
                resolution: core_graph::rules::DependencyResolution::Target {
                    node_id: "missing:dependency".into(),
                },
            }]
        };
        store.put_node(&typed_rule("owner", dependencies)).unwrap();
        if !missing_owner {
            store
                .put_node(&node("owner", "Symbol", ConfidenceTier::Confirmed))
                .unwrap();
        }
        assert!(matches!(
            compile(
                &store,
                PartitionArtifactKind::RuleEvidence,
                "rule:one",
                SnapshotReadLimits::default(),
                100_000
            ),
            Err(PartitionError::Unavailable)
        ));
    }
}
#[test]
fn rule_reference_set_is_accounted_before_the_visit_cap_refuses() {
    use core_graph::rules::*;
    let dependencies = (0..32)
        .map(|i| RuleDependency {
            role: DependencyRole::ExitValue,
            source: core_prov::EvidenceRef {
                repo: "test/repo".into(),
                path: "source.ts".into(),
                byte_start: 0,
                byte_end: 1,
                commit_sha: "abc".into(),
            },
            resolution: DependencyResolution::Target {
                node_id: format!("target:{i}"),
            },
        })
        .collect();
    let rule = typed_rule("owner", dependencies);
    assert_eq!(crate::rules::partition_references(&rule).len(), 33);
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&rule).unwrap();
    let limits = SnapshotReadLimits {
        max_rows: 1,
        ..Default::default()
    };
    assert!(matches!(
        compile(
            &store,
            PartitionArtifactKind::RuleEvidence,
            "rule:one",
            limits,
            100_000
        ),
        Err(PartitionError::Graph(
            core_graph::GraphError::SnapshotBounds(_)
        ))
    ));
    let graph = flowtracer::TraceGraphBound {
        bytes: limits.max_bytes,
        rows: 1,
        values: limits.max_values,
    };
    let trace = flowtracer::TraceLimits {
        max_flows: 1,
        max_hops: 1,
        max_output_bytes: 100_000,
    };
    let bound = logical_partition_bound(
        graph,
        trace,
        SelectedCompileLimits {
            max_bytes: 100_000,
            adr: crate::AdrLimits {
                max_rows: 1,
                max_output_bytes: 100_000,
            },
        },
    )
    .unwrap();
    assert!(
        bound.acquisition_bytes
            >= crate::workbench::accounting::set_slots::<String>(limits.max_values).unwrap()
    );
}

#[test]
fn nested_typed_rule_vectors_are_charged_by_values_before_row_admission() {
    use core_graph::rules::*;
    let dependencies = (0..32)
        .map(|i| RuleDependency {
            role: DependencyRole::ExitValue,
            source: core_prov::EvidenceRef {
                repo: "test/repo".into(),
                path: "source.ts".into(),
                byte_start: 0,
                byte_end: 1,
                commit_sha: "abc".into(),
            },
            resolution: DependencyResolution::Target {
                node_id: format!("target:{i}"),
            },
        })
        .collect();
    let rule = typed_rule("owner", dependencies);
    let parsed = GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    assert_eq!(parsed.dependencies.len(), 32);
    let graph = flowtracer::TraceGraphBound {
        bytes: 64 * 1024,
        rows: 1,
        values: 1024,
    };
    let trace = flowtracer::TraceLimits {
        max_flows: 1,
        max_hops: 1,
        max_output_bytes: 100_000,
    };
    let limits = SelectedCompileLimits {
        max_bytes: 100_000,
        adr: crate::AdrLimits {
            max_rows: 1,
            max_output_bytes: 100_000,
        },
    };
    let bound = logical_partition_bound(graph, trace, limits).unwrap();
    for header in [
        std::mem::size_of::<BranchCondition>(),
        std::mem::size_of::<RuleDependency>(),
        std::mem::size_of::<SourceRedaction>(),
        std::mem::size_of::<String>(),
        std::mem::size_of::<RedactionReason>(),
        std::mem::size_of::<LocalDefinition>(),
        std::mem::size_of::<core_prov::EvidenceRef>(),
        std::mem::size_of::<DefinitionDependency>(),
        std::mem::size_of::<DefinitionExpressionNode>(),
    ] {
        assert!(bound.typed_rule_vector_bytes >= graph.values * header);
    }
    assert!(
        bound.typed_rule_vector_bytes
            >= parsed.dependencies.len() * std::mem::size_of::<RuleDependency>()
    );
    let more_rows = logical_partition_bound(
        flowtracer::TraceGraphBound { rows: 2, ..graph },
        trace,
        limits,
    )
    .unwrap();
    assert_eq!(
        bound.typed_rule_vector_bytes,
        more_rows.typed_rule_vector_bytes
    );
    let more_values = logical_partition_bound(
        flowtracer::TraceGraphBound {
            values: 1025,
            ..graph
        },
        trace,
        limits,
    )
    .unwrap();
    assert!(more_values.typed_rule_vector_bytes > bound.typed_rule_vector_bytes);
    assert!(
        logical_partition_bound(
            flowtracer::TraceGraphBound {
                values: usize::MAX,
                ..graph
            },
            trace,
            limits
        )
        .is_none()
    );
}
