//! #718 / AC-0288 real-store read-session foundation, not MCP acceptance.
use super::*;
use agents::{AssertionDecision, CuratableAssertion};
use context_hub::coverage::CoverageStatus;
use context_hub::{DiscoveryQuery, FactReference};
use core_graph::coverage::CoverageRecord;
use core_graph::{GraphPatch, GraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

fn assertion(id: &str, confidence: ConfidenceTier) -> CuratableAssertion {
    CuratableAssertion {
        subject_id: id.into(),
        summary: id.into(),
        provenance: Provenance::new(
            match confidence {
                ConfidenceTier::Confirmed => Tier::Deterministic,
                ConfidenceTier::InferredStrong => Tier::Semantic,
                _ => Tier::Agentic,
            },
            confidence,
            vec![EvidenceRef {
                repo: "fixture/repo".into(),
                path: "source.ts".into(),
                byte_start: 0,
                byte_end: 4,
                commit_sha: "fixed".into(),
            }],
            "fixture",
            id.as_bytes(),
        )
        .unwrap(),
    }
}
fn graph_node(assertion: &CuratableAssertion) -> Node {
    Node {
        id: assertion.subject_id.clone(),
        label: "Module".into(),
        props: json!({"name":assertion.subject_id,"prov":assertion.provenance}),
    }
}
fn fixture() -> (Mutex<SqliteGraphStore>, Mutex<DecisionLog>) {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for (id, tier) in [
        ("module:confirmed", ConfidenceTier::Confirmed),
        ("module:strong", ConfidenceTier::InferredStrong),
        ("module:weak", ConfidenceTier::InferredWeak),
    ] {
        graph.put_node(&graph_node(&assertion(id, tier))).unwrap();
    }
    (
        Mutex::new(graph),
        Mutex::new(DecisionLog::open(":memory:").unwrap()),
    )
}
fn scopes() -> Vec<ScopedRepo> {
    vec![ScopedRepo {
        repo_key: "fixture/repo".into(),
        registered_source_id: Some("src:fixture".into()),
        config_hash: "config".into(),
        layer_hints: None,
    }]
}
fn acquire(
    graph: &Mutex<SqliteGraphStore>,
    decisions: &Mutex<DecisionLog>,
    mode: ExportMode,
) -> ReadSession {
    ReadSession::acquire(
        graph,
        decisions,
        || Ok(scopes()),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        mode,
    )
    .unwrap()
}
fn inventory() -> CoverageRecord {
    CoverageRecord {repo_key:"fixture/repo".into(),registered_source_id:Some("src:fixture".into()),ingest_id:"job:1".into(),config_hash:"config".into(),inventory_json:json!({"inventory":"coverage@1","repo":"fixture/repo","commit":"a".repeat(40),"layer_hints":[],"producers":[{"id":"fixture","state":"ran"}],"missing_inputs":[],"unsupported":[],"limited":false}).to_string()}
}
fn query() -> QualifiedRequest<QueryRequest> {
    QualifiedRequest {
        request: QueryRequest::default(),
        context_id: None,
    }
}
fn discovery() -> QualifiedRequest<DiscoveryRequest> {
    QualifiedRequest {
        request: DiscoveryRequest {
            query: DiscoveryQuery::Text {
                text: "module".into(),
            },
            labels: vec![],
            max_candidates: 10,
            max_bytes: 16384,
            cursor: None,
        },
        context_id: None,
    }
}

#[test]
fn raw_and_filtered_identities_remain_distinct_and_policy_is_frozen() {
    let (graph, decisions) = fixture();
    let raw = graph.lock().unwrap().read_snapshot().unwrap();
    let raw_id = ContextSnapshot::new(raw.0.clone(), raw.1.clone())
        .unwrap()
        .id()
        .to_owned();
    let verified = acquire(&graph, &decisions, ExportMode::VerifiedOnly);
    let best = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert_eq!(verified.basis().recovered_graph_id, raw_id);
    assert_eq!(best.basis().recovered_graph_id, raw_id);
    assert_ne!(verified.basis().projection_snapshot_id, raw_id);
    assert_eq!(best.basis().projection_snapshot_id, raw_id);
    assert!(!best.basis().h3_curated_available);
    assert!(verified.qualified.shares_snapshot(&verified.snapshot));
    assert_eq!(verified.base.graph.0, raw.0);
    assert_eq!(verified.base.graph.1, raw.1);
    decisions
        .lock()
        .unwrap()
        .record_assertion(
            &assertion("module:strong", ConfidenceTier::InferredStrong),
            AssertionDecision::Rejected,
            None,
        )
        .unwrap();
    let rejected = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert_ne!(
        rejected.basis().rejection_decision_digest,
        best.basis().rejection_decision_digest
    );
    assert_eq!(
        best.discover(discovery())
            .unwrap()
            .response
            .total_candidates,
        3
    );
    assert_eq!(
        rejected
            .discover(discovery())
            .unwrap()
            .response
            .total_candidates,
        2
    );
    assert_eq!(rejected.rejected_hashes.len(), 1);
    assert_eq!(graph.lock().unwrap().read_snapshot().unwrap(), raw);
    graph
        .lock()
        .unwrap()
        .put_node(&graph_node(&assertion(
            "module:new",
            ConfidenceTier::Confirmed,
        )))
        .unwrap();
    assert_eq!(
        best.discover(discovery())
            .unwrap()
            .response
            .total_candidates,
        3
    );
    assert_ne!(
        acquire(&graph, &decisions, ExportMode::BestEffort)
            .basis()
            .recovered_graph_id,
        raw_id
    );
}

#[test]
fn accepted_annotation_changes_do_not_invent_a_curated_revision() {
    let (graph, decisions) = fixture();
    let first = acquire(&graph, &decisions, ExportMode::BestEffort);
    let mut reads = 0;
    let session = ReadSession::acquire(
        &graph,
        &decisions,
        || {
            reads += 1;
            if reads == 1 {
                decisions
                    .lock()
                    .unwrap()
                    .record_assertion(
                        &assertion("module:strong", ConfidenceTier::InferredStrong),
                        AssertionDecision::Accepted,
                        Some("historical note"),
                    )
                    .unwrap();
            }
            Ok(scopes())
        },
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    )
    .unwrap();
    assert_eq!(session.basis(), first.basis());
    assert!(!session.basis().h3_curated_available);
}

#[test]
fn actual_decision_or_registration_drift_fails_closed() {
    let (graph, decisions) = fixture();
    let mut reads = 0;
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || {
            reads += 1;
            if reads == 1 {
                decisions
                    .lock()
                    .unwrap()
                    .record_assertion(
                        &assertion("module:strong", ConfidenceTier::InferredStrong),
                        AssertionDecision::Rejected,
                        None,
                    )
                    .unwrap();
            }
            Ok(scopes())
        },
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::StaleBasis)));
    let mut reads = 0;
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || {
            reads += 1;
            let mut scope = scopes();
            if reads == 2 {
                scope[0].registered_source_id = Some("src:replacement".into());
            }
            Ok(scope)
        },
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::StaleBasis)));
}

#[test]
fn scope_sets_normalize_order_and_hints_without_accepting_duplicate_repos() {
    let (graph, decisions) = fixture();
    let mut reads = 0;
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || {
            reads += 1;
            let mut scope = scopes();
            scope[0].layer_hints = Some(if reads == 1 {
                vec!["server".into(), "infra".into(), "server".into()]
            } else {
                vec!["infra".into(), "server".into()]
            });
            let mut other = scope[0].clone();
            other.repo_key = "other/repo".into();
            scope.push(other);
            if reads == 2 {
                scope.reverse();
            }
            Ok(scope)
        },
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(result.is_ok());
    let duplicate = ReadSession::acquire(
        &graph,
        &decisions,
        || Ok([scopes(), scopes()].concat()),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(duplicate, Err(ReadError::InvalidRequest)));
}

#[test]
fn graph_and_coverage_limits_are_store_admitted_and_errors_are_source_free() {
    let (graph, decisions) = fixture();
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || Ok(scopes()),
        SnapshotReadLimits {
            max_rows: 1,
            ..SnapshotReadLimits::default()
        },
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::Limit)));
    assert_eq!(ReadError::Limit.to_string(), "response_limit");
    assert_eq!(ReadError::Storage.to_string(), "storage_unavailable");
    let session = acquire(&graph, &decisions, ExportMode::BestEffort);
    let mut tiny = query();
    tiny.request.max_bytes = 1;
    assert_eq!(session.query(tiny).unwrap_err(), ReadError::Limit);
    let mut store = graph.lock().unwrap();
    let expected = store.read_snapshot().unwrap();
    let record = inventory();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "fixture/repo",
                &[],
                Some(&record),
                core_graph::source::SourceOmissions::NONE,
            )
            .unwrap()
    );
    drop(store);
    let recorded = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert_eq!(
        recorded.query(query()).unwrap().coverage.repos[0].status,
        CoverageStatus::Recorded
    );
    assert_eq!(recorded.base.revision, recorded.basis().base_revision);
    assert_eq!(
        recorded.base.graph,
        graph.lock().unwrap().read_snapshot().unwrap()
    );
    // Repeat identical inventory publication: content/coverage identities stay
    // equal, but the mutation token and complete session basis must advance.
    let mut store = graph.lock().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &recorded.base.graph,
                &GraphPatch::default(),
                "fixture/repo",
                &[],
                Some(&record),
                core_graph::source::SourceOmissions::NONE,
            )
            .unwrap()
    );
    drop(store);
    let repeated = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert_eq!(recorded.base.graph, repeated.base.graph);
    assert_eq!(recorded.basis().coverage_id, repeated.basis().coverage_id);
    assert_eq!(
        recorded.basis().recovered_graph_id,
        repeated.basis().recovered_graph_id
    );
    assert_ne!(recorded.base.revision, repeated.base.revision);
    assert_ne!(recorded.basis_id().unwrap(), repeated.basis_id().unwrap());
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || Ok(scopes()),
        SnapshotReadLimits::default(),
        CoverageReadLimits {
            max_bytes: 1,
            ..CoverageReadLimits::default()
        },
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::Limit)));
    // Separate nonzero graph byte limit is rejected before full materialization.
    let result = ReadSession::acquire(
        &graph,
        &decisions,
        || Ok(scopes()),
        SnapshotReadLimits {
            max_bytes: 1,
            ..SnapshotReadLimits::default()
        },
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::Limit)));
}

#[test]
fn bounded_decision_observation_refuses_oversized_stored_annotations() {
    let (graph, _) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut decisions = DecisionLog::open(&path).unwrap();
    decisions
        .record_assertion(
            &assertion("module:strong", ConfidenceTier::InferredStrong),
            AssertionDecision::Accepted,
            None,
        )
        .unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE assertion_decisions SET note=?1",
            ["private annotation".repeat(2048)],
        )
        .unwrap();
    let result = ReadSession::acquire(
        &graph,
        &Mutex::new(decisions),
        || Ok(scopes()),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(result, Err(ReadError::Limit)));
}

#[test]
fn query_and_discovery_preserve_actual_qualification_and_stale_cursors() {
    let (graph, decisions) = fixture();
    let session = acquire(&graph, &decisions, ExportMode::VerifiedOnly);
    let page = session.query(query()).unwrap();
    assert_eq!(page.coverage.repos[0].status, CoverageStatus::Unrecorded);
    assert!(!page.coverage.exhaustive);
    assert_eq!(page.context_id, session.basis().context_id);
    assert_eq!(
        page.response.snapshot_id,
        session.basis().projection_snapshot_id
    );
    assert_eq!(page.response.facts.len(), 2);
    assert!(page.response.facts.iter().all(|fact| fact.reference
        != FactReference::Node {
            id: "module:weak".into()
        }));
    let mut request = discovery();
    request.request.max_candidates = 1;
    let first = session.discover(request.clone()).unwrap();
    assert_eq!(first.coverage, page.coverage);
    assert_eq!(first.response.snapshot_id, page.response.snapshot_id);
    request.request.cursor = first.response.next_cursor;
    assert!(request.request.cursor.is_some());
    assert_eq!(
        session.discover(request.clone()).unwrap_err(),
        ReadError::StaleBasis
    );
    request.context_id = Some(session.basis().context_id.clone());
    assert!(session.discover(request).is_ok());
}

#[test]
fn frozen_evidence_visibility_preserves_hidden_rule_adjacency_and_policy() {
    // AC-0288: evidence visibility uses the same complete compiler projection.
    let (graph, decisions) = fixture();
    let mut rule = graph_node(&assertion("rule:hidden", ConfidenceTier::InferredWeak));
    rule.label = "BusinessRule".into();
    let edge = Edge {
        src: "module:confirmed".into(),
        label: "HAS_RULE".into(),
        dst: rule.id.clone(),
        props: graph_node(&assertion("edge:confirmed", ConfidenceTier::Confirmed)).props,
    };
    {
        let mut graph = graph.lock().unwrap();
        graph.put_node(&rule).unwrap();
        graph.put_edge(&edge).unwrap();
    }
    let hidden = FactReference::Node { id: rule.id };
    let adjacent = FactReference::Edge {
        source: edge.src,
        label: edge.label,
        destination: edge.dst,
    };
    let verified = acquire(&graph, &decisions, ExportMode::VerifiedOnly);
    let best = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert!(!verified.visible_facts.contains(&hidden));
    assert!(!verified.visible_facts.contains(&adjacent));
    assert!(best.visible_facts.contains(&hidden));
    assert!(best.visible_facts.contains(&adjacent));
    decisions
        .lock()
        .unwrap()
        .record_assertion(
            &assertion("rule:hidden", ConfidenceTier::InferredWeak),
            AssertionDecision::Rejected,
            None,
        )
        .unwrap();
    let rejected = acquire(&graph, &decisions, ExportMode::BestEffort);
    assert!(!rejected.visible_facts.contains(&hidden));
    assert!(!rejected.visible_facts.contains(&adjacent));
    assert!(best.visible_facts.contains(&hidden));
    assert!(best.visible_facts.contains(&adjacent));
}

// A writer commits during the production bounded read, not via a sleep/race.
#[test]
fn complete_session_refuses_wal_drift_and_then_captures_matching_revision_and_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut reader = SqliteGraphStore::open(&path).unwrap();
    let original = graph_node(&assertion("module:confirmed", ConfidenceTier::Confirmed));
    reader.put_node(&original).unwrap();
    let expected = reader.read_snapshot().unwrap();
    let before = reader.read_revision().unwrap();
    let mut writer = SqliteGraphStore::open(&path).unwrap();
    let mut changed = original;
    changed.props["name"] = json!("new publication");
    reader.set_snapshot_after_nodes_hook(move || {
        assert!(
            writer.apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![changed],
                    ..GraphPatch::default()
                },
                "fixture/repo",
                &[],
                Some(&inventory()),
                core_graph::source::SourceOmissions::NONE,
            )?
        );
        Ok(())
    });
    let graph = Mutex::new(reader);
    let decisions = Mutex::new(DecisionLog::open(":memory:").unwrap());
    let acquire = || {
        ReadSession::acquire(
            &graph,
            &decisions,
            || Ok(scopes()),
            SnapshotReadLimits::default(),
            CoverageReadLimits::default(),
            ExportMode::BestEffort,
        )
    };
    assert!(matches!(acquire(), Err(ReadError::StaleBasis)));
    let current = acquire().unwrap();
    assert_ne!(current.base.revision, before);
    assert_eq!(
        current.base.revision,
        graph.lock().unwrap().read_revision().unwrap()
    );
    assert_eq!(current.base.revision, current.basis().base_revision);
    assert_eq!(
        current.base.graph,
        graph.lock().unwrap().read_snapshot().unwrap()
    );
    assert_eq!(current.base.graph.0[0].props["name"], "new publication");
    assert_eq!(
        current.query(query()).unwrap().coverage.repos[0].status,
        CoverageStatus::Recorded
    );
}
