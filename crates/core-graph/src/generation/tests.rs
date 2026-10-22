use super::*;
use crate::{
    GraphPatch, GraphStore,
    coverage::CoverageRecord,
    source::{FactKey, SourceBinding, node_digest},
};
use serde_json::json;

fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({"name":id}),
    }
}
fn edge() -> Edge {
    Edge {
        src: "a".into(),
        dst: "b".into(),
        label: "CALLS".into(),
        props: json!({}),
    }
}
fn revision(store: &SqliteGraphStore) -> StoreRevision {
    let tx = store.conn.unchecked_transaction().unwrap();
    let revision = read_store_revision(&tx).unwrap();
    tx.commit().unwrap();
    revision
}
fn drop_schema(connection: &Connection) {
    for (kind, name, _) in schema().into_iter().rev() {
        connection
            .execute_batch(&format!("DROP {kind} {name}"))
            .unwrap();
    }
}
fn binding(node: &Node) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_node(node),
        repo_key: "repo/a".into(),
        receipt_id: format!("ts-primary-v1:{}", "a".repeat(64)),
        emitted_fact_digest: node_digest(node).unwrap(),
    }
}

#[test]
fn generation_tracks_all_public_fact_writers_and_noop_policy() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    assert_eq!(revision(&store).generation, 0);
    let mut before = revision(&store);
    store.put_node(&node("a")).unwrap();
    assert!(revision(&store).generation > before.generation);
    before = revision(&store);
    store.put_node(&node("a")).unwrap();
    assert!(revision(&store).generation > before.generation);
    store.put_node(&node("b")).unwrap();
    before = revision(&store);
    store.put_edge(&edge()).unwrap();
    assert!(revision(&store).generation > before.generation);
    before = revision(&store);
    store.delete_edge("a", "b", "CALLS").unwrap();
    assert!(revision(&store).generation > before.generation);
    before = revision(&store);
    store.delete_edge("a", "b", "CALLS").unwrap();
    assert_eq!(revision(&store), before);
    store.put_edge(&edge()).unwrap();
    before = revision(&store);
    store.delete_edges_from_with_label("a", "CALLS").unwrap();
    assert!(revision(&store).generation > before.generation);
    store.put_edge(&edge()).unwrap();
    before = revision(&store);
    store.delete_node("b").unwrap();
    assert!(revision(&store).generation > before.generation);
    before = revision(&store);
    let old = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_if_snapshot_matches(
                &old,
                &GraphPatch {
                    upsert_nodes: vec![node("c")],
                    ..Default::default()
                }
            )
            .unwrap()
    );
    assert!(revision(&store).generation > before.generation);
    before = revision(&store);
    assert!(
        !store
            .apply_patch_if_snapshot_matches(&old, &GraphPatch::default())
            .unwrap()
    );
    assert_eq!(revision(&store), before);
    store.clear().unwrap();
    before = revision(&store);
    store.clear().unwrap();
    assert_eq!(revision(&store), before);
}

#[test]
fn generation_includes_binding_only_and_coverage_only_publications_and_invalidation() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    let graph = store.read_snapshot().unwrap();
    let before = revision(&store);
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &graph,
                &GraphPatch::default(),
                "repo/a",
                &[binding(&node("a"))]
            )
            .unwrap()
    );
    assert!(revision(&store).generation > before.generation);
    let before = revision(&store);
    let coverage = CoverageRecord {
        repo_key: "repo/a".into(),
        registered_source_id: None,
        ingest_id: "ingest-1".into(),
        config_hash: "config-1".into(),
        inventory_json: "{}".into(),
    };
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &graph,
                &GraphPatch::default(),
                "repo/a",
                &[binding(&node("a"))],
                Some(&coverage)
            )
            .unwrap()
    );
    assert!(revision(&store).generation > before.generation);
    let before = revision(&store);
    store.put_node(&node("a")).unwrap();
    assert!(revision(&store).generation >= before.generation + 2);
    assert!(
        store
            .current_source_binding(&FactKey::from_node(&node("a")))
            .unwrap()
            .is_none()
    );
    store
        .read_snapshot_with_coverage_bounded(
            SnapshotReadLimits::default(),
            crate::coverage::CoverageReadLimits::default(),
        )
        .unwrap();
    store
        .read_source_selection_snapshot_bounded(
            &[FactKey::from_node(&node("a"))],
            SnapshotReadLimits::default(),
        )
        .unwrap();
}

#[test]
fn generation_rolls_back_failed_patch_and_explicit_transaction() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    let old = store.read_snapshot().unwrap();
    let before = revision(&store);
    let patch = GraphPatch {
        upsert_nodes: vec![node("b")],
        upsert_edges: vec![Edge {
            dst: "missing".into(),
            ..edge()
        }],
        ..Default::default()
    };
    assert!(store.apply_patch_if_snapshot_matches(&old, &patch).is_err());
    assert_eq!(revision(&store), before);
    assert_eq!(store.read_snapshot().unwrap(), old);
    {
        let tx = store.conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO nodes(id,label) VALUES('temporary','Symbol')",
            [],
        )
        .unwrap();
    }
    assert_eq!(revision(&store), before);
    assert_eq!(store.read_snapshot().unwrap(), old);
    let mut bad = binding(&node("a"));
    bad.emitted_fact_digest = format!("node-v1:{}", "b".repeat(64));
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &old,
                &GraphPatch::default(),
                "repo/a",
                &[bad]
            )
            .is_err()
    );
    assert_eq!(revision(&store), before);
}

#[test]
fn generation_exhaustion_is_typed_atomic_and_does_not_convert_to_real() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store
        .conn
        .execute("UPDATE graph_generation SET generation=?1", [i64::MAX])
        .unwrap();
    let before = revision(&store);
    let old = store.read_snapshot().unwrap();
    assert!(matches!(
        store.put_node(&node("b")),
        Err(GraphError::Generation(GenerationError::Exhausted))
    ));
    assert_eq!(revision(&store), before);
    assert_eq!(store.read_snapshot().unwrap(), old);
    let ty: String = store
        .conn
        .query_row("SELECT typeof(generation) FROM graph_generation", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(ty, "integer");
    assert!(matches!(
        store.clear(),
        Err(GraphError::Generation(GenerationError::Exhausted))
    ));
    assert_eq!(store.read_snapshot().unwrap(), old);
}

#[test]
fn generation_migration_preserves_legacy_facts_bindings_and_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut store = SqliteGraphStore::open(&path).unwrap();
    store.put_node(&node("a")).unwrap();
    let graph = store.read_snapshot().unwrap();
    store
        .apply_patch_with_source_bindings_if_snapshot_matches(
            &graph,
            &GraphPatch::default(),
            "repo/a",
            &[binding(&node("a"))],
        )
        .unwrap();
    let coverage = CoverageRecord {
        repo_key: "repo/a".into(),
        registered_source_id: None,
        ingest_id: "legacy-ingest".into(),
        config_hash: "legacy-config".into(),
        inventory_json: "{}".into(),
    };
    store
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &graph,
            &GraphPatch::default(),
            "repo/a",
            &[binding(&node("a"))],
            Some(&coverage),
        )
        .unwrap();
    let coverage_before = store.coverage_records().unwrap();
    let instance = store.instance_id().unwrap();
    drop_schema(&store.conn);
    drop(store);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(revision(&store).generation, 0);
    assert_eq!(store.instance_id().unwrap(), instance);
    assert_eq!(store.coverage_records().unwrap(), coverage_before);
    assert_eq!(store.read_snapshot().unwrap(), graph);
    assert_eq!(
        store
            .current_source_binding(&FactKey::from_node(&node("a")))
            .unwrap(),
        Some(binding(&node("a")))
    );
    let before = revision(&store);
    drop(store);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(revision(&store), before);
    assert_ne!(
        revision(&SqliteGraphStore::open_in_memory().unwrap()).graph_instance,
        instance
    );
}

#[test]
fn generation_schema_refuses_partial_future_and_foreign_trigger_without_repair() {
    for corruption in [
        "DROP TRIGGER graph_generation_nodes_insert",
        "PRAGMA ignore_check_constraints=ON; UPDATE graph_generation SET version=2",
        "CREATE TRIGGER hostile AFTER INSERT ON graph_generation BEGIN SELECT 1; END",
        "DELETE FROM graph_generation",
        "PRAGMA ignore_check_constraints=ON; UPDATE graph_generation SET generation=-1",
        "PRAGMA ignore_check_constraints=ON; INSERT INTO graph_generation VALUES(2,1,0)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        store.put_node(&node("a")).unwrap();
        store.conn.execute_batch(corruption).unwrap();
        store.conn.pragma_update(None, "user_version", 0).unwrap();
        drop(store);
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_called = std::sync::Arc::clone(&called);
        assert!(
            SqliteGraphStore::open_with_reset_hook(
                &path,
                Box::new(move || {
                    hook_called.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
            )
            .is_err(),
            "{corruption}"
        );
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        let raw = Connection::open(&path).unwrap();
        assert_eq!(
            raw.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER graph_generation_fake AFTER INSERT ON nodes BEGIN SELECT 1; END",
        )
        .unwrap();
    assert!(
        store
            .read_source_selection_snapshot_bounded(&[], SnapshotReadLimits::default())
            .is_err()
    );
}

#[test]
fn generation_reset_is_atomic_and_advances_even_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let store = SqliteGraphStore::open(&path).unwrap();
    let before = revision(&store);
    store.conn.pragma_update(None, "user_version", 0).unwrap();
    drop(store);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert!(revision(&store).generation > before.generation);
    drop(store);
    let mut store = SqliteGraphStore::open(&path).unwrap();
    store.put_node(&node("a")).unwrap();
    let before = revision(&store);
    store.conn.pragma_update(None, "user_version", 0).unwrap();
    drop(store);
    assert!(
        SqliteGraphStore::open_with_reset_hook(&path, Box::new(|| Err("refuse reset".into())))
            .is_err()
    );
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        raw.query_row("SELECT generation FROM graph_generation", [], |r| r
            .get::<_, i64>(0))
            .unwrap() as u64,
        before.generation
    );
    assert_eq!(
        raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(raw);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert!(store.read_snapshot().unwrap().0.is_empty());
    assert!(revision(&store).generation > before.generation);
}

#[test]
fn generation_observes_already_open_legacy_connection_and_coherent_reader() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut reader = SqliteGraphStore::open(&path).unwrap();
    reader.put_node(&node("a")).unwrap();
    drop_schema(&reader.conn);
    let mut legacy = Connection::open(&path).unwrap();
    let mut old_statement = legacy
        .prepare("INSERT INTO nodes(id,label) VALUES('b','Symbol')")
        .unwrap();
    drop(reader);
    let reader = SqliteGraphStore::open(&path).unwrap();
    let before = reader
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    old_statement.execute([]).unwrap();
    drop(old_statement);
    assert!(revision(&reader).generation > before.revision.generation);
    let tx = legacy.transaction().unwrap();
    tx.execute("UPDATE nodes SET props='{}'", []).unwrap();
    tx.rollback().unwrap();
    let before = reader
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    reader.set_snapshot_after_nodes_hook(move || {
        legacy.execute("UPDATE nodes SET label='Changed' WHERE id='a'", [])?;
        Ok(())
    });
    let during = reader
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    assert_eq!(during, before);
    assert!(matches!(
        reader.read_snapshot_at_revision_bounded(
            Some(&before.revision),
            SnapshotReadLimits::default()
        ),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}

#[test]
fn generation_nested_binding_invalidation_overflow_rolls_back_whole_statement() {
    for recursive in [false, true] {
        for foreign_keys in [false, true] {
            let mut store = SqliteGraphStore::open_in_memory().unwrap();
            store
                .conn
                .pragma_update(None, "recursive_triggers", recursive)
                .unwrap();
            store
                .conn
                .pragma_update(None, "foreign_keys", foreign_keys)
                .unwrap();
            store.put_node(&node("a")).unwrap();
            store.put_node(&node("b")).unwrap();
            let edges: Vec<_> = ["CALLS", "READS", "WRITES"]
                .into_iter()
                .map(|label| Edge {
                    label: label.into(),
                    ..edge()
                })
                .collect();
            for edge in &edges {
                store.put_edge(edge).unwrap();
            }
            let mut bindings = vec![binding(&node("a"))];
            bindings.extend(edges.iter().map(|edge| SourceBinding {
                fact: FactKey::from_edge(edge),
                repo_key: "repo/a".into(),
                receipt_id: format!("ts-primary-v1:{}", "b".repeat(64)),
                emitted_fact_digest: crate::source::edge_digest(edge).unwrap(),
            }));
            let graph = store.read_snapshot().unwrap();
            store
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &graph,
                    &GraphPatch::default(),
                    "repo/a",
                    &bindings,
                )
                .unwrap();
            store
                .conn
                .execute("UPDATE graph_generation SET generation=?1", [i64::MAX - 2])
                .unwrap();
            let before = revision(&store);
            let error = store
                .conn
                .execute("DELETE FROM edges WHERE src='a'", [])
                .unwrap_err();
            assert!(matches!(
                GraphError::from(error),
                GraphError::Generation(GenerationError::Exhausted)
            ));
            assert_eq!(revision(&store), before);
            assert_eq!(store.read_snapshot().unwrap(), graph);
            for binding in &bindings {
                assert_eq!(
                    store.current_source_binding(&binding.fact).unwrap(),
                    Some(binding.clone())
                );
            }
            assert!(matches!(
                store.delete_node("a"),
                Err(GraphError::Generation(GenerationError::Exhausted))
            ));
            assert_eq!(revision(&store), before);
            assert_eq!(store.read_snapshot().unwrap(), graph);
            for binding in &bindings {
                assert_eq!(
                    store.current_source_binding(&binding.fact).unwrap(),
                    Some(binding.clone())
                );
            }
            store
                .conn
                .execute("UPDATE graph_generation SET generation=0", [])
                .unwrap();
            store
                .conn
                .execute("DELETE FROM edges WHERE src='a'", [])
                .unwrap();
            assert_eq!(revision(&store).generation, 6);
        }
    }
}

#[test]
fn generation_reset_overflow_after_deletions_restores_all_publication_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut store = SqliteGraphStore::open(&path).unwrap();
    store.put_node(&node("a")).unwrap();
    store.put_node(&node("b")).unwrap();
    store.put_edge(&edge()).unwrap();
    let graph = store.read_snapshot().unwrap();
    let bindings = vec![
        binding(&node("a")),
        SourceBinding {
            fact: FactKey::from_edge(&edge()),
            repo_key: "repo/a".into(),
            receipt_id: format!("ts-primary-v1:{}", "c".repeat(64)),
            emitted_fact_digest: crate::source::edge_digest(&edge()).unwrap(),
        },
    ];
    let coverage = CoverageRecord {
        repo_key: "repo/a".into(),
        registered_source_id: None,
        ingest_id: "reset-ingest".into(),
        config_hash: "config".into(),
        inventory_json: "{}".into(),
    };
    store
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &graph,
            &GraphPatch::default(),
            "repo/a",
            &bindings,
            Some(&coverage),
        )
        .unwrap();
    let coverage_before = store.coverage_records().unwrap();
    store
        .conn
        .execute("UPDATE graph_generation SET generation=?1", [i64::MAX - 3])
        .unwrap();
    store.conn.pragma_update(None, "user_version", 0).unwrap();
    let before = revision(&store);
    drop(store);
    assert!(matches!(
        SqliteGraphStore::open(&path),
        Err(GraphError::Generation(GenerationError::Exhausted))
    ));
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        raw.query_row("SELECT generation FROM graph_generation", [], |r| r
            .get::<_, i64>(0))
            .unwrap() as u64,
        before.generation
    );
    assert_eq!(
        raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    // Restore only the version marker to inspect the preserved failed-reset data.
    raw.pragma_update(None, "user_version", crate::GRAPH_SCHEMA_VERSION)
        .unwrap();
    drop(raw);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(store.read_snapshot().unwrap(), graph);
    assert_eq!(store.coverage_records().unwrap(), coverage_before);
    for binding in bindings {
        assert_eq!(
            store.current_source_binding(&binding.fact).unwrap(),
            Some(binding)
        );
    }
}

#[test]
fn generation_waiting_opener_rechecks_version_after_competing_migration_commit() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WAITING: AtomicBool = AtomicBool::new(false);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let store = SqliteGraphStore::open(&path).unwrap();
    store.conn.pragma_update(None, "user_version", 0).unwrap();
    drop(store);
    let mut first = Connection::open(&path).unwrap();
    let second = Connection::open(&path).unwrap();
    second
        .busy_handler(Some(|_| {
            WAITING.store(true, Ordering::SeqCst);
            std::thread::yield_now();
            true
        }))
        .unwrap();
    let tx = first
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    tx.pragma_update(None, "user_version", crate::GRAPH_SCHEMA_VERSION)
        .unwrap();
    tx.execute(
        "INSERT INTO nodes(id,label) VALUES('newly-published','Symbol')",
        [],
    )
    .unwrap();
    WAITING.store(false, Ordering::SeqCst);
    let opener = std::thread::spawn(move || SqliteGraphStore::init(second));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !WAITING.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    tx.commit().unwrap();
    let opened = opener.join().unwrap().unwrap();
    assert!(opened.get_node("newly-published").unwrap().is_some());
    assert_eq!(revision(&opened).generation, 1);
}

#[test]
fn generation_all_fifteen_raw_row_events_advance_and_storage_error_has_source() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store.put_node(&node("b")).unwrap();
    store.put_edge(&edge()).unwrap();
    let statements = [
        "INSERT INTO nodes(id,label) VALUES('c','Symbol')",
        "UPDATE nodes SET props='{}' WHERE id='c'",
        "DELETE FROM nodes WHERE id='c'",
        "INSERT INTO edges(src,dst,label) VALUES('b','a','READS')",
        "UPDATE edges SET props='{}' WHERE src='b' AND dst='a'",
        "DELETE FROM edges WHERE src='b' AND dst='a'",
        "INSERT INTO source_node_bindings VALUES('a','repo/a','receipt', 'node-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
        "UPDATE source_node_bindings SET receipt_id='other' WHERE node_id='a'",
        "DELETE FROM source_node_bindings WHERE node_id='a'",
        "INSERT INTO source_edge_bindings VALUES('a','CALLS','b','repo/a','receipt','edge-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
        "UPDATE source_edge_bindings SET receipt_id='other' WHERE source='a'",
        "DELETE FROM source_edge_bindings WHERE source='a'",
        "INSERT INTO coverage_inventories VALUES('repo/a',NULL,'ingest','config','hash','{}')",
        "UPDATE coverage_inventories SET ingest_id='other' WHERE repo_key='repo/a'",
        "DELETE FROM coverage_inventories WHERE repo_key='repo/a'",
    ];
    for sql in statements {
        let before = revision(&store);
        store.conn.execute(sql, []).unwrap();
        assert!(revision(&store).generation > before.generation, "{sql}");
    }
    let error = GraphError::from(rusqlite::Error::InvalidQuery);
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn generation_oversized_trigger_metadata_refuses_bounded_coverage_and_source_reads() {
    let store = SqliteGraphStore::open_in_memory().unwrap();
    // Keep the expected trigger name/count but replace its body with oversized
    // SQL. The generation metadata preflight must reject it before either
    // owner's owned_schema can copy that body into a Rust String.
    let sql = format!(
        "DROP TRIGGER graph_generation_coverage_inventories_insert; CREATE TRIGGER graph_generation_coverage_inventories_insert AFTER INSERT ON coverage_inventories BEGIN SELECT '{}'; END",
        "x".repeat(64 * 1024)
    );
    store.conn.execute_batch(&sql).unwrap();
    let error = store
        .read_snapshot_with_coverage_bounded(
            SnapshotReadLimits::default(),
            crate::coverage::CoverageReadLimits::default(),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        GraphError::Generation(GenerationError::InvalidSchema)
    ));
    let error = store
        .read_source_selection_snapshot_bounded(&[], SnapshotReadLimits::default())
        .unwrap_err();
    assert!(matches!(
        error,
        GraphError::Generation(GenerationError::InvalidSchema)
    ));
    assert!(store.conn.is_autocommit());
}

#[test]
fn generation_oversized_instance_refuses_before_bounded_snapshot_text_copy() {
    let store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .conn
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE graph_instance SET instance=?1",
            ["a".repeat(64 * 1024)],
        )
        .unwrap();
    assert!(matches!(
        store.read_snapshot_with_revision_bounded(SnapshotReadLimits::default()),
        Err(GraphError::Generation(GenerationError::InvalidSchema))
    ));
    assert!(store.conn.is_autocommit());
}
