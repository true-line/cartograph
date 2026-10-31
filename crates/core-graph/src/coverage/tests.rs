use super::*;
use crate::{GraphPatch, GraphStore};
use serde_json::json;

fn record(repo: &str, body: serde_json::Value) -> CoverageRecord {
    CoverageRecord {
        repo_key: repo.into(),
        registered_source_id: Some("src_0123".into()),
        ingest_id: "job-1".into(),
        config_hash: "config-v1:abc".into(),
        inventory_json: body.to_string(),
    }
}

fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Module".into(),
        props: json!({}),
    }
}

fn publish(
    store: &mut SqliteGraphStore,
    repo: &str,
    nodes: &[&str],
    coverage: Option<&CoverageRecord>,
) -> bool {
    let expected = store.read_snapshot().unwrap();
    let patch = GraphPatch {
        upsert_nodes: nodes.iter().map(|id| node(id)).collect(),
        ..GraphPatch::default()
    };
    store
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &expected,
            &patch,
            repo,
            &[],
            coverage,
            crate::source::SourceOmissions::NONE,
        )
        .unwrap()
}

// AC-0239: an inventory is published with its repo's facts in one write
// transaction, read back with the same graph revision, replaced by the next
// publication and removed with the graph. (T-0239)
#[test]
fn coverage_inventory_publishes_and_reads_with_its_facts() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let first = record(
        "acme/api",
        json!({"inventory": "coverage@1", "limited": true}),
    );
    assert!(publish(&mut store, "acme/api", &["mod:a"], Some(&first)));
    let ((nodes, _), coverage) = store.read_snapshot_with_coverage().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        coverage,
        [StoredCoverage {
            inventory_hash: first.inventory_hash(),
            record: first.clone(),
        }]
    );

    // A stale snapshot publishes neither facts nor inventory.
    let stale = store.read_snapshot().unwrap();
    assert!(publish(&mut store, "acme/web", &["mod:w"], None));
    let other = record(
        "acme/api",
        json!({"inventory": "coverage@1", "limited": false}),
    );
    assert!(
        !store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &stale,
                &GraphPatch {
                    upsert_nodes: vec![node("mod:b")],
                    ..GraphPatch::default()
                },
                "acme/api",
                &[],
                Some(&other),
                crate::source::SourceOmissions::NONE,
            )
            .unwrap()
    );
    assert_eq!(store.coverage_records().unwrap()[0].record, first);

    // A publication without an inventory removes the old one: it no longer
    // describes the published facts.
    assert!(publish(&mut store, "acme/api", &["mod:a"], None));
    assert!(store.coverage_records().unwrap().is_empty());

    assert!(publish(&mut store, "acme/api", &["mod:a"], Some(&first)));
    store.clear().unwrap();
    assert!(store.coverage_records().unwrap().is_empty());
}

// AC-0239: a record with unbounded identities or a non-object body is never
// written, and a stored body that no longer matches its hash is never read.
// (T-0239)
#[test]
fn coverage_records_are_validated_and_their_bodies_hash_checked() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let wrong_owner = record("acme/web", json!({}));
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "acme/api",
                &[],
                Some(&wrong_owner),
                crate::source::SourceOmissions::NONE,
            )
            .is_err()
    );
    let mut not_object = record("acme/api", json!({}));
    not_object.inventory_json = "[]".into();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "acme/api",
                &[],
                Some(&not_object),
                crate::source::SourceOmissions::NONE,
            )
            .is_err()
    );
    // Rolled back with the failed publication: nothing recorded.
    assert!(store.coverage_records().unwrap().is_empty());

    let good = record("acme/api", json!({"limited": true}));
    assert!(publish(&mut store, "acme/api", &["mod:a"], Some(&good)));
    store
        .conn
        .execute(
            "UPDATE coverage_inventories SET inventory_json = '{\"limited\":false}'",
            [],
        )
        .unwrap();
    assert!(store.coverage_records().is_err());
    assert!(store.read_snapshot_with_coverage().is_err());
}

// AC-0239: the inventory tables are exactly as created; a foreign or altered
// object fails closed instead of qualifying a read. (T-0239)
#[test]
fn coverage_schema_is_exact_and_refuses_foreign_objects() {
    let store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER coverage_rewrite AFTER INSERT ON coverage_inventories BEGIN DELETE FROM coverage_inventories; END",
        )
        .unwrap();
    assert!(store.coverage_records().is_err());
}

// AC-0239: a bounded read counts and measures the inventories before copying
// them, admits the graph under its own limits and returns exactly the
// unbounded read when both fit. (T-0239)
#[test]
fn bounded_coverage_read_admits_inventories_before_copying_them() {
    // AC-0507: the revisioned sibling preserves payload and all admission errors.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for repo in ["acme/api", "acme/web"] {
        let body = json!({"inventory": "coverage@1", "repo": repo});
        assert!(publish(
            &mut store,
            repo,
            &[&format!("mod:{repo}")],
            Some(&record(repo, body))
        ));
    }
    let unbounded = store.read_snapshot_with_coverage().unwrap();
    let graph = SnapshotReadLimits::default();
    assert_eq!(
        store
            .read_snapshot_with_coverage_bounded(graph, CoverageReadLimits::default())
            .unwrap(),
        unbounded
    );
    let (snapshot, recorded) = store
        .read_snapshot_with_coverage_and_revision_bounded(graph, CoverageReadLimits::default())
        .unwrap();
    assert_eq!((snapshot.graph, recorded), unbounded);
    assert_eq!(snapshot.revision, store.read_revision().unwrap());
    let stored: usize = unbounded
        .1
        .iter()
        .map(|stored| {
            let record = &stored.record;
            record.repo_key.len()
                + record.registered_source_id.as_ref().map_or(0, String::len)
                + record.ingest_id.len()
                + record.config_hash.len()
                + stored.inventory_hash.len()
                + record.inventory_json.len()
        })
        .sum();
    let exact = CoverageReadLimits {
        max_records: 2,
        max_bytes: stored,
    };
    assert!(
        store
            .read_snapshot_with_coverage_bounded(graph, exact)
            .is_ok()
    );
    assert!(
        store
            .read_snapshot_with_coverage_and_revision_bounded(graph, exact)
            .is_ok()
    );
    for (limits, error) in [
        (
            CoverageReadLimits {
                max_records: 1,
                ..exact
            },
            SnapshotBoundsError::CoverageRecordLimit,
        ),
        (
            CoverageReadLimits {
                max_bytes: stored - 1,
                ..exact
            },
            SnapshotBoundsError::CoverageByteLimit,
        ),
        (
            CoverageReadLimits {
                max_records: 0,
                ..exact
            },
            SnapshotBoundsError::InvalidLimits,
        ),
        (
            CoverageReadLimits {
                max_bytes: CoverageReadLimits::MAX_BYTES + 1,
                ..exact
            },
            SnapshotBoundsError::InvalidLimits,
        ),
    ] {
        assert!(matches!(
            store.read_snapshot_with_coverage_bounded(graph, limits),
            Err(GraphError::SnapshotBounds(actual)) if actual == error
        ));
        assert!(matches!(
            store.read_snapshot_with_coverage_and_revision_bounded(graph, limits),
            Err(GraphError::SnapshotBounds(actual)) if actual == error
        ));
    }
    let tight_graph = SnapshotReadLimits {
        max_rows: 1,
        ..graph
    };
    assert!(matches!(
        store.read_snapshot_with_coverage_bounded(tight_graph, exact),
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
    ));
    assert!(matches!(
        store.read_snapshot_with_coverage_and_revision_bounded(tight_graph, exact),
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
    ));
    assert_eq!(
        store
            .read_snapshot_with_coverage_bounded(graph, exact)
            .unwrap(),
        unbounded
    );
    assert!(
        store
            .read_snapshot_with_coverage_and_revision_bounded(graph, exact)
            .is_ok()
    );
}

// Scoped coverage acquisition must not materialize unrelated graph bodies.
#[test]
fn revisioned_coverage_is_bounded_and_independent_of_graph_body_size() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let inventory = record("acme/api", json!({"limited": true}));
    assert!(publish(
        &mut store,
        "acme/api",
        &["mod:a"],
        Some(&inventory)
    ));
    store
        .conn
        .execute(
            "UPDATE nodes SET props = ?1 WHERE id = 'mod:a'",
            [json!({"large": "x".repeat(4096)}).to_string()],
        )
        .unwrap();
    let (revision, records) = store
        .read_coverage_records_with_revision_bounded(CoverageReadLimits {
            max_records: 1,
            max_bytes: 1024,
        })
        .unwrap();
    assert_eq!(revision, store.read_revision().unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].record, inventory);
    assert!(
        store
            .read_coverage_records_with_revision_bounded(CoverageReadLimits {
                max_records: 1,
                max_bytes: 1
            })
            .is_err()
    );
    assert!(
        store
            .read_coverage_records_with_revision_bounded(CoverageReadLimits {
                max_records: 0,
                max_bytes: 1024
            })
            .is_err()
    );
    store
        .conn
        .execute("UPDATE coverage_inventories SET ingest_id = 'job-2'", [])
        .unwrap();
    let (next, records) = store
        .read_coverage_records_with_revision_bounded(CoverageReadLimits {
            max_records: 1,
            max_bytes: 1024,
        })
        .unwrap();
    assert_ne!(revision, next);
    assert_eq!(records[0].record.ingest_id, "job-2");
}

// AC-0507: one WAL snapshot binds revision, nodes, edges and coverage, including
// a coverage-only commit whose graph content does not change.
#[test]
fn revisioned_covered_snapshot_is_coherent_across_wal_publications() {
    for change_graph in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut reader = SqliteGraphStore::open(&path).unwrap();
        assert!(publish(
            &mut reader,
            "acme/api",
            &["mod:a"],
            Some(&record("acme/api", json!({"run":1})))
        ));
        reader
            .put_edge(&Edge {
                src: "mod:a".into(),
                dst: "mod:a".into(),
                label: "CALLS".into(),
                props: json!({}),
            })
            .unwrap();
        let graph_limits = SnapshotReadLimits::default();
        let coverage_limits = CoverageReadLimits::default();
        let before = reader
            .read_snapshot_with_coverage_and_revision_bounded(graph_limits, coverage_limits)
            .unwrap();
        let mut writer = SqliteGraphStore::open(&path).unwrap();
        let expected = before.0.graph.clone();
        let mut changed_node = expected.0[0].clone();
        changed_node.props = json!({"run":2});
        let mut changed_edge = expected.1[0].clone();
        changed_edge.props = json!({"run":2});
        let replacement = record("acme/api", json!({"run":2}));
        let published_record = replacement.clone();
        reader.set_snapshot_after_nodes_hook(move || {
            let patch = if change_graph {
                GraphPatch {
                    upsert_nodes: vec![changed_node],
                    upsert_edges: vec![changed_edge],
                    ..GraphPatch::default()
                }
            } else {
                GraphPatch::default()
            };
            assert!(
                writer.apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                    &expected,
                    &patch,
                    "acme/api",
                    &[],
                    Some(&published_record),
                    crate::source::SourceOmissions::NONE,
                )?
            );
            Ok(())
        });
        assert_eq!(
            reader
                .read_snapshot_with_coverage_and_revision_bounded(graph_limits, coverage_limits)
                .unwrap(),
            before
        );
        let after = reader
            .read_snapshot_with_coverage_and_revision_bounded(graph_limits, coverage_limits)
            .unwrap();
        assert_eq!(
            after.0.revision.graph_instance,
            before.0.revision.graph_instance
        );
        assert!(after.0.revision.generation > before.0.revision.generation);
        assert_eq!(after.1[0].record, replacement);
        assert_eq!(after.1[0].inventory_hash, replacement.inventory_hash());
        if change_graph {
            assert_eq!(after.0.graph.0[0].props, json!({"run":2}));
            assert_eq!(after.0.graph.1[0].props, json!({"run":2}));
        } else {
            assert_eq!(after.0.graph, before.0.graph);
        }
    }
}

// AC-0507: validate generation/instance metadata for the new revisioned read;
// invalid caller limits still win before metadata access.
#[test]
fn revisioned_covered_snapshot_refuses_invalid_revision_metadata() {
    for corrupt in [
        "DROP TRIGGER graph_generation_nodes_insert",
        "UPDATE graph_instance SET instance = 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz'",
    ] {
        let store = SqliteGraphStore::open_in_memory().unwrap();
        store.conn.execute_batch(corrupt).unwrap();
        assert!(matches!(
            store.read_snapshot_with_coverage_and_revision_bounded(
                SnapshotReadLimits::default(),
                CoverageReadLimits::default()
            ),
            Err(GraphError::Generation(
                crate::GenerationError::InvalidSchema
            ))
        ));
        assert!(matches!(
            store.read_snapshot_with_coverage_and_revision_bounded(
                SnapshotReadLimits {
                    max_bytes: 0,
                    ..SnapshotReadLimits::default()
                },
                CoverageReadLimits::default()
            ),
            Err(GraphError::SnapshotBounds(
                SnapshotBoundsError::InvalidLimits
            ))
        ));
        assert!(store.conn.is_autocommit());
        assert!(
            store
                .read_snapshot_with_coverage_bounded(
                    SnapshotReadLimits::default(),
                    CoverageReadLimits::default()
                )
                .is_ok()
        );
    }
}
