// AC-0304 / T-0304: bounded windows and selected fact storage.
use super::*;
use crate::{
    GraphPatch, GraphStore,
    source::{SourceBinding, node_digest},
};
use serde_json::json;
fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({"name":id}),
    }
}
fn limits(rows: usize) -> SnapshotReadLimits {
    SnapshotReadLimits {
        max_rows: rows,
        ..Default::default()
    }
}
fn nodes(store: &SqliteGraphStore, rows: usize) -> FactWindow<Node, String> {
    store
        .read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: limits(rows),
        })
        .unwrap()
}

#[test]
fn scoped_windows_and_one_selected_fact_work_above_complete_snapshot_ceiling() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let tx = store.conn.transaction().unwrap();
    {
        let mut insert = tx
            .prepare("INSERT INTO nodes(id,label,props) VALUES(?1,'Symbol','{}')")
            .unwrap();
        for i in 0..50_001 {
            insert.execute([format!("node:{i:08}")]).unwrap();
        }
    }
    tx.commit().unwrap();
    assert!(matches!(
        store.read_snapshot_bounded(SnapshotReadLimits::default()),
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
    ));
    let first = nodes(&store, 2);
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.scanned_raw_rows, 2);
    assert_eq!(first.remaining, Remaining::More);
    let next = store
        .read_node_window(&NodeWindowRequest {
            expected_revision: Some(first.revision.clone()),
            after_id: first.next_key.clone(),
            limits: limits(2),
        })
        .unwrap();
    assert_eq!(next.rows[0].id, "node:00000002");
    let selected = store
        .read_selected_facts_at_revision(
            &[FactKey::Node {
                id: "node:00040000".into(),
            }],
            &first.revision,
            limits(1),
        )
        .unwrap();
    assert_eq!(selected.nodes.len(), 1);
    assert!(selected.edges.is_empty());
    assert!(
        matches!(&selected.selections[0].state,FactSourceState::Absent{fact_digest} if fact_digest==&node_digest(&selected.nodes[0]).unwrap())
    );
}

#[test]
fn scoped_node_and_composite_edge_keysets_match_sqlite_binary_order() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["a", "b", "é", ""] {
        store.put_node(&node(id)).unwrap();
    }
    let mut after = None;
    let mut revision = None;
    let mut recovered = Vec::new();
    loop {
        let page = store
            .read_node_window(&NodeWindowRequest {
                expected_revision: revision,
                after_id: after,
                limits: limits(1),
            })
            .unwrap();
        recovered.extend(page.rows.into_iter().map(|n| n.id));
        if page.remaining == Remaining::Exhausted {
            break;
        }
        after = page.next_key;
        revision = Some(page.revision);
    }
    assert_eq!(recovered, ["", "a", "b", "é"]);
    for (src, dst, label) in [
        ("a", "b", "Z"),
        ("a", "é", "A"),
        ("a", "b", "A"),
        ("b", "a", "A"),
    ] {
        store
            .put_edge(&Edge {
                src: src.into(),
                dst: dst.into(),
                label: label.into(),
                props: json!({}),
            })
            .unwrap();
    }
    let mut after = None;
    let mut revision = None;
    let mut recovered = Vec::new();
    loop {
        let page = store
            .read_edge_window(&EdgeWindowRequest {
                expected_revision: revision,
                after_key: after,
                limits: limits(1),
            })
            .unwrap();
        recovered.extend(page.rows.into_iter().map(|e| (e.src, e.dst, e.label)));
        if page.remaining == Remaining::Exhausted {
            break;
        }
        after = page.next_key;
        revision = Some(page.revision);
    }
    assert_eq!(
        recovered,
        vec![
            ("a".into(), "b".into(), "A".into()),
            ("a".into(), "b".into(), "Z".into()),
            ("a".into(), "é".into(), "A".into()),
            ("b".into(), "a".into(), "A".into())
        ]
    );
}

#[test]
fn scoped_continuations_require_matching_revision_and_legal_key_budget() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store.put_node(&node("b")).unwrap();
    let first = nodes(&store, 1);
    assert!(
        store
            .read_node_window(&NodeWindowRequest {
                expected_revision: None,
                after_id: Some("a".into()),
                limits: limits(1)
            })
            .is_err()
    );
    assert!(
        store
            .read_node_window(&NodeWindowRequest {
                expected_revision: None,
                after_id: None,
                limits: limits(0)
            })
            .is_err()
    );
    assert!(
        store
            .read_node_window(&NodeWindowRequest {
                expected_revision: Some(first.revision.clone()),
                after_id: Some("x".repeat(1025)),
                limits: SnapshotReadLimits {
                    max_bytes: 1024,
                    ..limits(1)
                }
            })
            .is_err()
    );
    store.put_node(&node("c")).unwrap();
    assert!(matches!(
        store.read_node_window(&NodeWindowRequest {
            expected_revision: Some(first.revision.clone()),
            after_id: first.next_key,
            limits: limits(1)
        }),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
    assert!(matches!(
        store.read_selected_facts_at_revision(
            &[FactKey::Node { id: "a".into() }],
            &first.revision,
            limits(1)
        ),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}

#[test]
fn scoped_lookahead_does_not_decode_or_skip_corrupt_or_oversized_next_row() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO nodes(id,label,props) VALUES('b','Symbol','not json')",
            [],
        )
        .unwrap();
    let first = nodes(&store, 1);
    assert_eq!(first.remaining, Remaining::More);
    assert_eq!(first.rows[0].id, "a");
    let request = NodeWindowRequest {
        expected_revision: Some(first.revision.clone()),
        after_id: first.next_key,
        limits: limits(1),
    };
    assert!(matches!(
        store.read_node_window(&request),
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::InvalidJson))
    ));
    assert!(store.conn.is_autocommit());
    store
        .conn
        .execute(
            "UPDATE nodes SET props=?1 WHERE id='b'",
            [format!("\"{}\"", "x".repeat(4096))],
        )
        .unwrap();
    let first = nodes(&store, 1);
    assert!(matches!(
        store.read_node_window(&NodeWindowRequest {
            expected_revision: Some(first.revision),
            after_id: first.next_key,
            limits: SnapshotReadLimits {
                max_bytes: 1024,
                ..limits(1)
            }
        }),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::RawByteLimit
        ))
    ));
}

#[test]
fn scoped_row_structure_utf8_and_canonical_budgets_fail_closed() {
    for (props, expected) in [
        ("CAST(x'80' AS TEXT)", SnapshotBoundsError::InvalidRow),
        ("'invalid'", SnapshotBoundsError::InvalidJson),
    ] {
        let store = SqliteGraphStore::open_in_memory().unwrap();
        store
            .conn
            .execute_batch(&format!(
                "INSERT INTO nodes(id,label,props) VALUES('a','Symbol',{props})"
            ))
            .unwrap();
        assert!(
            matches!(store.read_node_window(&NodeWindowRequest{expected_revision:None,after_id:None,limits:limits(1)}),Err(GraphError::SnapshotBounds(actual)) if actual==expected)
        );
        assert!(store.conn.is_autocommit());
    }
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .put_node(&Node {
            id: "a".into(),
            label: "\n".repeat(512),
            props: json!({}),
        })
        .unwrap();
    assert!(matches!(
        store.read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: SnapshotReadLimits {
                max_bytes: 800,
                ..limits(1)
            }
        }),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::CanonicalByteLimit
        ))
    ));
    store
        .put_node(&Node {
            id: "a".into(),
            label: "Symbol".into(),
            props: json!({"a":{"b":{"c":1}}}),
        })
        .unwrap();
    assert!(matches!(
        store.read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: SnapshotReadLimits {
                max_depth: 5,
                ..limits(1)
            }
        }),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::JsonDepthLimit
        ))
    ));
    assert!(matches!(
        store.read_node_window(&NodeWindowRequest {
            expected_revision: None,
            after_id: None,
            limits: SnapshotReadLimits {
                max_values: 4,
                ..limits(1)
            }
        }),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::JsonValueLimit
        ))
    ));
}

#[test]
fn scoped_selected_bindings_are_exact_coherent_and_do_not_read_other_bodies() {
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store.put_node(&node("unselected")).unwrap();
    let binding = SourceBinding {
        fact: FactKey::from_node(&node("a")),
        repo_key: "repo/a".into(),
        receipt_id: format!("ts-primary-v1:{}", "a".repeat(64)),
        emitted_fact_digest: node_digest(&node("a")).unwrap(),
    };
    let graph = store.read_snapshot().unwrap();
    store
        .apply_patch_with_source_bindings_if_snapshot_matches(
            &graph,
            &GraphPatch::default(),
            "repo/a",
            std::slice::from_ref(&binding),
        )
        .unwrap();
    store
        .conn
        .execute("UPDATE nodes SET props='invalid' WHERE id='unselected'", [])
        .unwrap();
    let revision = {
        let tx = store.conn.unchecked_transaction().unwrap();
        let revision = read_store_revision(&tx).unwrap();
        tx.commit().unwrap();
        revision
    };
    let selected = store
        .read_selected_facts_at_revision(
            &[
                FactKey::Node {
                    id: "missing".into(),
                },
                binding.fact.clone(),
            ],
            &revision,
            limits(2),
        )
        .unwrap();
    assert_eq!(selected.nodes, vec![node("a")]);
    assert!(
        matches!(&selected.selections[0].state,FactSourceState::Present{binding:actual,..} if actual==&binding)
    );
    assert!(matches!(
        selected.selections[1].state,
        FactSourceState::Missing
    ));
    store
        .conn
        .execute(
            "UPDATE source_node_bindings SET emitted_fact_digest=?1 WHERE node_id='a'",
            [format!("node-v1:{}", "b".repeat(64))],
        )
        .unwrap();
    let revision = {
        let tx = store.conn.unchecked_transaction().unwrap();
        read_store_revision(&tx).unwrap()
    };
    let selected = store
        .read_selected_facts_at_revision(&[binding.fact], &revision, limits(1))
        .unwrap();
    assert!(matches!(
        &selected.selections[0].state,
        FactSourceState::Invalid {
            fact_digest: Some(_)
        }
    ));
}

#[test]
fn scoped_rejects_nonbinary_primary_key_order_instead_of_skipping_rows() {
    let store = SqliteGraphStore::open_in_memory().unwrap();
    // A compatible-looking ordinary table can still have different comparison
    // semantics. Keyset continuation cannot safely use it as BINARY order.
    store.conn.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE source_node_bindings; DROP TABLE source_edge_bindings; DROP TABLE edges; DROP TABLE nodes; CREATE TABLE nodes(id TEXT PRIMARY KEY COLLATE NOCASE,label TEXT NOT NULL,props TEXT NOT NULL DEFAULT '{}') STRICT; CREATE TABLE edges(src TEXT NOT NULL,dst TEXT NOT NULL,label TEXT NOT NULL,props TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(src,dst,label)) STRICT").unwrap();
    // The missing generation triggers also correctly refuse this changed schema.
    assert!(
        store
            .read_node_window(&NodeWindowRequest {
                expected_revision: None,
                after_id: None,
                limits: limits(1)
            })
            .is_err()
    );
    // Exercise the ordering preflight independently of the generation guard.
    assert!(matches!(
        preflight_schema(&store.conn),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::InvalidSchema
        ))
    ));
}

#[test]
fn scoped_window_revision_and_lookahead_share_one_read_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut reader = SqliteGraphStore::open(&path).unwrap();
    reader.put_node(&node("a")).unwrap();
    let before = nodes(&reader, 1);
    let mut writer = SqliteGraphStore::open(&path).unwrap();
    reader.set_snapshot_after_nodes_hook(move || writer.put_node(&node("b")));
    let during = nodes(&reader, 1);
    assert_eq!(during, before);
    assert_eq!(during.remaining, Remaining::Exhausted);
    assert!(matches!(
        reader.read_node_window(&NodeWindowRequest {
            expected_revision: Some(before.revision),
            after_id: Some("a".into()),
            limits: limits(1)
        }),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
    assert_eq!(nodes(&reader, 1).remaining, Remaining::More);
}

#[test]
fn scoped_selected_fact_and_binding_share_one_read_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut reader = SqliteGraphStore::open(&path).unwrap();
    reader.put_node(&node("a")).unwrap();
    let binding = SourceBinding {
        fact: FactKey::from_node(&node("a")),
        repo_key: "repo/a".into(),
        receipt_id: format!("ts-primary-v1:{}", "a".repeat(64)),
        emitted_fact_digest: node_digest(&node("a")).unwrap(),
    };
    let graph = reader.read_snapshot().unwrap();
    reader
        .apply_patch_with_source_bindings_if_snapshot_matches(
            &graph,
            &GraphPatch::default(),
            "repo/a",
            std::slice::from_ref(&binding),
        )
        .unwrap();
    let before = nodes(&reader, 1);
    let mut writer = SqliteGraphStore::open(&path).unwrap();
    reader.set_source_binding_after_lookup_hook(move || writer.put_node(&node("a")));
    let selected = reader
        .read_selected_facts_at_revision(
            std::slice::from_ref(&binding.fact),
            &before.revision,
            limits(1),
        )
        .unwrap();
    assert_eq!(selected.revision, before.revision);
    assert!(matches!(
        selected.selections[0].state,
        FactSourceState::Present { .. }
    ));
    assert!(matches!(
        reader.read_selected_facts_at_revision(&[binding.fact], &before.revision, limits(1)),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}

#[test]
fn scoped_binary_queries_override_column_default_collations() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE nodes(id TEXT COLLATE NOCASE,label TEXT NOT NULL,props TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(id COLLATE BINARY)) STRICT;
        CREATE TABLE edges(src TEXT COLLATE NOCASE NOT NULL,dst TEXT COLLATE NOCASE NOT NULL,label TEXT COLLATE NOCASE NOT NULL,props TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(src COLLATE BINARY,dst COLLATE BINARY,label COLLATE BINARY)) STRICT").unwrap();
    conn.pragma_update(None, "user_version", crate::GRAPH_SCHEMA_VERSION)
        .unwrap();
    let store = SqliteGraphStore::init(conn).unwrap();
    // This deliberately nonstandard parent collation cannot satisfy SQLite's
    // foreign-key collation rules; exercise the read contract with FK disabled.
    store
        .conn
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    for id in ["a", "A", "b"] {
        store
            .conn
            .execute("INSERT INTO nodes VALUES(?1,'Symbol','{}')", [id])
            .unwrap();
    }
    let expected = ["A", "a", "b"];
    let mut after = None;
    let mut revision = None;
    for (index, id) in expected.iter().enumerate() {
        let page = store
            .read_node_window(&NodeWindowRequest {
                expected_revision: revision,
                after_id: after,
                limits: limits(1),
            })
            .unwrap();
        assert_eq!(page.rows[0].id, *id);
        assert_eq!(
            page.remaining,
            if index == 2 {
                Remaining::Exhausted
            } else {
                Remaining::More
            }
        );
        let selected = store
            .read_selected_facts_at_revision(
                &[FactKey::Node { id: (*id).into() }],
                &page.revision,
                limits(1),
            )
            .unwrap();
        assert_eq!(selected.nodes[0].id, *id);
        after = page.next_key;
        revision = Some(page.revision);
    }
    let keys = [
        ("A", "a", "a"),
        ("a", "A", "a"),
        ("a", "a", "A"),
        ("a", "a", "a"),
    ];
    for (src, dst, label) in keys {
        store
            .conn
            .execute(
                "INSERT INTO edges VALUES(?1,?2,?3,'{}')",
                rusqlite::params![src, dst, label],
            )
            .unwrap();
    }
    let mut after = None;
    let mut revision = None;
    for (index, (src, dst, label)) in keys.into_iter().enumerate() {
        let page = store
            .read_edge_window(&EdgeWindowRequest {
                expected_revision: revision,
                after_key: after,
                limits: limits(1),
            })
            .unwrap();
        assert_eq!(
            (&*page.rows[0].src, &*page.rows[0].dst, &*page.rows[0].label),
            (src, dst, label)
        );
        assert_eq!(
            page.remaining,
            if index == 3 {
                Remaining::Exhausted
            } else {
                Remaining::More
            }
        );
        let selected = store
            .read_selected_facts_at_revision(
                &[FactKey::Edge {
                    source: src.into(),
                    destination: dst.into(),
                    label: label.into(),
                }],
                &page.revision,
                limits(1),
            )
            .unwrap();
        assert_eq!(selected.edges, page.rows);
        after = page.next_key;
        revision = Some(page.revision);
    }
    // Explicit collations retain the primary-key seek rather than sorting the table.
    for sql in [
        "EXPLAIN QUERY PLAN SELECT id FROM nodes WHERE id COLLATE BINARY > 'A' ORDER BY id COLLATE BINARY LIMIT 1",
        "EXPLAIN QUERY PLAN SELECT src FROM edges WHERE (src,dst,label) > ('A' COLLATE BINARY,'a' COLLATE BINARY,'a' COLLATE BINARY) ORDER BY src COLLATE BINARY,dst COLLATE BINARY,label COLLATE BINARY LIMIT 1",
    ] {
        let mut stmt = store.conn.prepare(sql).unwrap();
        let plan: Vec<String> = stmt
            .query_map([], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("SEARCH") && line.contains("INDEX")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

// AC-0306: omission evidence is selected in the same bounded pinned transaction.
#[test]
fn scoped_selected_omission_is_digest_bound_and_does_not_decode_unselected_rows() {
    use crate::source::{OmissionLookup, ReceiptOmissionKind, SourceOmission, SourceOmissions};
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let selected = node("selected");
    store.put_node(&selected).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO nodes(id,label,props) VALUES('poison','Symbol','not json')",
            [],
        )
        .unwrap();
    let key = FactKey::from_node(&selected);
    // Independent expected snapshot avoids decoding the deliberately corrupt unrelated row.
    store
        .conn
        .execute("DELETE FROM nodes WHERE id='poison'", [])
        .unwrap();
    let snapshot = store.read_snapshot().unwrap();
    let omissions = [SourceOmission {
        fact: key.clone(),
        emitted_fact_digest: node_digest(&selected).unwrap(),
        kind: ReceiptOmissionKind::Rules,
    }];
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                "fixture",
                &[],
                None,
                SourceOmissions {
                    records: &omissions,
                    complete: true
                }
            )
            .unwrap()
    );
    store
        .conn
        .execute(
            "INSERT INTO nodes(id,label,props) VALUES('poison','Symbol','not json')",
            [],
        )
        .unwrap();
    let revision = store.read_revision().unwrap();
    let result = store
        .read_selected_facts_at_revision(std::slice::from_ref(&key), &revision, limits(1))
        .unwrap();
    assert_eq!(result.nodes, vec![selected.clone()]);
    assert_eq!(
        result.selections[0].omission,
        OmissionLookup::Omitted {
            repo_key: "fixture".into(),
            kind: ReceiptOmissionKind::Rules
        }
    );
    store
        .conn
        .execute(
            "UPDATE source_node_omissions SET emitted_fact_digest=?1",
            [format!("node-v1:{}", "0".repeat(64))],
        )
        .unwrap();
    let result = store
        .read_selected_facts_at_revision(&[key], &revision, limits(1))
        .unwrap();
    assert_eq!(result.selections[0].omission, OmissionLookup::Unrecorded);
}

// AC-0306: omission and graph observations share the exact SQLite snapshot.
#[test]
fn scoped_selected_omission_is_pinned_while_supported_publication_commits() {
    use crate::source::{OmissionLookup, ReceiptOmissionKind, SourceOmission, SourceOmissions};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let mut reader = SqliteGraphStore::open(&path).unwrap();
    let fact = node("selected");
    reader.put_node(&fact).unwrap();
    let key = FactKey::from_node(&fact);
    let snapshot = reader.read_snapshot().unwrap();
    let omissions = [SourceOmission {
        fact: key.clone(),
        emitted_fact_digest: node_digest(&fact).unwrap(),
        kind: ReceiptOmissionKind::Declarations,
    }];
    reader
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &snapshot,
            &GraphPatch::default(),
            "repo",
            &[],
            None,
            SourceOmissions {
                records: &omissions,
                complete: true,
            },
        )
        .unwrap();
    let revision = reader.read_revision().unwrap();
    let mut writer = SqliteGraphStore::open(&path).unwrap();
    reader.set_source_binding_after_lookup_hook(move || {
        assert!(
            writer.apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                "repo",
                &[],
                None,
                SourceOmissions::NONE
            )?
        );
        Ok(())
    });
    let selected = reader
        .read_selected_facts_at_revision(std::slice::from_ref(&key), &revision, limits(1))
        .unwrap();
    assert_eq!(
        selected.selections[0].omission,
        OmissionLookup::Omitted {
            repo_key: "repo".into(),
            kind: ReceiptOmissionKind::Declarations
        }
    );
    assert!(matches!(
        reader.read_selected_facts_at_revision(&[key], &revision, limits(1)),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}

// AC-0304: cumulative paging charges decoder work, not only normalized values.
#[test]
fn scoped_window_json_accounting_preserves_duplicate_property_values() {
    let store = SqliteGraphStore::open_in_memory().unwrap();
    store
        .conn
        .execute(
            "INSERT INTO nodes(id,label,props) VALUES('a','Symbol',?1)",
            [r#"{"x":1,"x":2}"#],
        )
        .unwrap();
    let duplicate = nodes(&store, 1);
    store
        .conn
        .execute("UPDATE nodes SET props=?1 WHERE id='a'", [r#"{"x":2}"#])
        .unwrap();
    let normalized = nodes(&store, 1);
    assert_eq!(duplicate.rows, normalized.rows);
    assert_eq!(duplicate.canonical_bytes, normalized.canonical_bytes);
    assert_eq!(duplicate.json_values, normalized.json_values + 1);
    assert!(duplicate.raw_bytes > normalized.raw_bytes);
}

#[test]
fn selected_facts_read_the_requested_binding_role() {
    // AC-1051 (ADR-0079): the same revision-bound selection reads a fact's
    // observed binding when asked for the observed role, and its primary one
    // otherwise; a fact with only an observed binding is Absent as primary,
    // and a primary omission never reads as an observed one.
    use crate::source::{OmissionLookup, ReceiptOmissionKind, SourceOmission, SourceOmissions};
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    store.put_node(&node("b")).unwrap();
    let omitted = [SourceOmission {
        fact: FactKey::from_node(&node("b")),
        emitted_fact_digest: node_digest(&node("b")).unwrap(),
        kind: ReceiptOmissionKind::StoreCapacity,
    }];
    let observed = SourceBinding {
        fact: FactKey::from_node(&node("a")),
        repo_key: "repo/a".into(),
        receipt_id: format!("obs-input-v1:{}", "c".repeat(64)),
        emitted_fact_digest: node_digest(&node("a")).unwrap(),
    };
    let snapshot = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                "repo/a",
                &[],
                std::slice::from_ref(&observed),
                None,
                SourceOmissions {
                    records: &omitted,
                    complete: true,
                },
            )
            .unwrap()
    );
    let revision = store.read_revision().unwrap();
    let keys = [observed.fact.clone()];
    let as_observed = store
        .read_selected_facts_at_revision_as(
            &keys,
            &revision,
            limits(1),
            crate::source::BindingRole::Observed,
        )
        .unwrap();
    assert!(
        matches!(&as_observed.selections[0].state, FactSourceState::Present { binding, .. } if binding == &observed)
    );
    let as_primary = store
        .read_selected_facts_at_revision(&keys, &revision, limits(1))
        .unwrap();
    assert!(matches!(
        as_primary.selections[0].state,
        FactSourceState::Absent { .. }
    ));
    let omitted_key = [omitted[0].fact.clone()];
    let primary_omitted = store
        .read_selected_facts_at_revision(&omitted_key, &revision, limits(1))
        .unwrap();
    assert!(matches!(
        primary_omitted.selections[0].omission,
        OmissionLookup::Omitted { .. }
    ));
    let observed_omitted = store
        .read_selected_facts_at_revision_as(
            &omitted_key,
            &revision,
            limits(1),
            crate::source::BindingRole::Observed,
        )
        .unwrap();
    assert!(matches!(
        observed_omitted.selections[0].state,
        FactSourceState::Absent { .. }
    ));
    assert_eq!(
        observed_omitted.selections[0].omission,
        OmissionLookup::Unrecorded
    );
}
