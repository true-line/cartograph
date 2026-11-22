use super::*;
use crate::{GRAPH_SCHEMA_VERSION, GraphStore};

/// Omission reads through the same-revision selection snapshot, returned in
/// caller key order (selections themselves are sorted by fact identity).
trait OmissionReads {
    fn receipt_omission(&self, fact: &FactKey) -> Result<OmissionLookup, GraphError>;
    fn receipt_omissions(&self, facts: &[FactKey]) -> Result<Vec<OmissionLookup>, GraphError>;
}

impl OmissionReads for SqliteGraphStore {
    fn receipt_omission(&self, fact: &FactKey) -> Result<OmissionLookup, GraphError> {
        Ok(self
            .receipt_omissions(std::slice::from_ref(fact))?
            .remove(0))
    }

    fn receipt_omissions(&self, facts: &[FactKey]) -> Result<Vec<OmissionLookup>, GraphError> {
        let snapshot = self.read_source_selection_snapshot(facts)?;
        Ok(facts
            .iter()
            .map(|fact| {
                snapshot
                    .selections
                    .iter()
                    .find(|selection| &selection.fact == fact)
                    .expect("every requested fact is selected")
                    .omission
                    .clone()
            })
            .collect())
    }
}

fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: serde_json::json!({"prov": {"content_hash": "semantic-only"}, "name": id}),
    }
}

fn edge(id: &str) -> Edge {
    Edge {
        src: id.into(),
        dst: id.into(),
        label: "GOVERNS".into(),
        props: serde_json::json!({"observation": "lexical_owner"}),
    }
}

fn node_binding(node: &Node, repo: &str, receipt: &str) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_node(node),
        repo_key: repo.into(),
        receipt_id: format!(
            "ts-primary-v1:{}",
            core_prov::content_hash(receipt.as_bytes())
        ),
        emitted_fact_digest: node_digest(node).unwrap(),
    }
}

fn edge_binding(edge: &Edge, repo: &str) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_edge(edge),
        repo_key: repo.into(),
        receipt_id: format!("ts-primary-v1:{}", core_prov::content_hash(repo.as_bytes())),
        emitted_fact_digest: edge_digest(edge).unwrap(),
    }
}

fn publish(store: &mut SqliteGraphStore, repo: &str, bindings: &[SourceBinding]) {
    let snapshot = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                repo,
                bindings,
            )
            .unwrap()
    );
}

fn seed(
    store: &mut SqliteGraphStore,
) -> (SourceBinding, SourceBinding, SourceBinding, SourceBinding) {
    for id in ["a", "b"] {
        store.put_node(&node(id)).unwrap();
        store.put_edge(&edge(id)).unwrap();
    }
    let a = node_binding(&node("a"), "repo/a", "a-original");
    let ae = edge_binding(&edge("a"), "repo/a");
    let b = node_binding(&node("b"), "repo/b", "b-original");
    let be = edge_binding(&edge("b"), "repo/b");
    publish(store, "repo/a", &[a.clone(), ae.clone()]);
    publish(store, "repo/b", &[b.clone(), be.clone()]);
    (a, ae, b, be)
}

#[test]
fn complete_fact_digests_bind_type_all_properties_and_array_order() {
    // AC-0149, AC-0150: this digest includes all emitted fields, independently
    // of the producer's often narrower prov.content_hash.
    let mut original = node("same");
    original.props = serde_json::from_str(
        r#"{"z":[1,2],"nested":{"b":2,"a":1},"prov":{"content_hash":"unchanged"}}"#,
    )
    .unwrap();
    let mut reordered = original.clone();
    reordered.props = serde_json::from_str(
        r#"{"prov":{"content_hash":"unchanged"},"nested":{"a":1,"b":2},"z":[1,2]}"#,
    )
    .unwrap();
    let digest = node_digest(&original).unwrap();
    assert_eq!(node_digest(&reordered).unwrap(), digest);
    for field in ["id", "label", "property", "array", "provenance"] {
        let mut changed = original.clone();
        match field {
            "id" => changed.id = "different".into(),
            "label" => changed.label = "Component".into(),
            "property" => changed.props["nested"]["a"] = 3.into(),
            "array" => changed.props["z"] = serde_json::json!([2, 1]),
            "provenance" => changed.props["prov"]["tier"] = "Agentic".into(),
            _ => unreachable!(),
        }
        assert_ne!(node_digest(&changed).unwrap(), digest, "{field}");
    }
    let original_edge = edge("same");
    let edge_hash = edge_digest(&original_edge).unwrap();
    assert_ne!(digest, edge_hash);
    for field in ["source", "destination", "label", "property"] {
        let mut changed = original_edge.clone();
        match field {
            "source" => changed.src = "other".into(),
            "destination" => changed.dst = "other".into(),
            "label" => changed.label = "CALLS".into(),
            "property" => changed.props["extra"] = true.into(),
            _ => unreachable!(),
        }
        assert_ne!(edge_digest(&changed).unwrap(), edge_hash, "{field}");
    }
    assert_eq!(
        serde_json::to_value(FactKey::from_node(&original)).unwrap(),
        serde_json::json!({"kind":"node", "id":"same"})
    );
    assert_eq!(
        serde_json::to_value(FactKey::from_edge(&original_edge)).unwrap(),
        serde_json::json!({"kind":"edge", "source":"same", "label":"GOVERNS", "destination":"same"})
    );
}

#[test]
fn identical_facts_switch_receipts_atomically_without_changing_other_repos() {
    // AC-0150: different captures can emit identical full facts. The explicit
    // current receipt changes, while graph hashes and other repos stay intact.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph.db");
    let mut store = SqliteGraphStore::open(&path).unwrap();
    let (a, ae, b, be) = seed(&mut store);
    let snapshot = store.read_snapshot().unwrap();
    let replacement = node_binding(&node("a"), "repo/a", "a-new-capture");
    assert_eq!(a.emitted_fact_digest, replacement.emitted_fact_digest);
    assert_ne!(a.receipt_id, replacement.receipt_id);
    publish(&mut store, "repo/a", std::slice::from_ref(&replacement));
    assert_eq!(store.read_snapshot().unwrap(), snapshot);
    assert_eq!(
        store.current_source_binding(&a.fact).unwrap(),
        Some(replacement.clone())
    );
    assert_eq!(
        store.current_source_binding(&ae.fact).unwrap(),
        None,
        "new nonparticipating facts lose old associations"
    );
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b.clone(), be.clone()]
    );
    drop(store);
    let mut store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(
        store.current_source_binding(&a.fact).unwrap(),
        Some(replacement)
    );
    publish(&mut store, "repo/a", &[]);
    assert!(store.source_bindings_for_repo("repo/a").unwrap().is_empty());
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b, be]
    );
    assert_eq!(store.read_snapshot().unwrap(), snapshot);
}

#[test]
fn ordinary_graph_mutations_invalidate_touched_source_associations() {
    // AC-0150: even an identical ordinary upsert is a new unbound publication;
    // unrelated facts keep their bindings and graph clear removes only current metadata.
    for action in [
        "node",
        "edge",
        "delete_edge",
        "delete_label",
        "delete_node",
        "patch",
        "clear",
    ] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let (a, ae, b, be) = seed(&mut store);
        match action {
            "node" => store.put_node(&node("a")).unwrap(),
            "edge" => store.put_edge(&edge("a")).unwrap(),
            "delete_edge" => store.delete_edge("a", "a", "GOVERNS").unwrap(),
            "delete_label" => store.delete_edges_from_with_label("a", "GOVERNS").unwrap(),
            "delete_node" => store.delete_node("a").unwrap(),
            "patch" => {
                let snapshot = store.read_snapshot().unwrap();
                assert!(
                    store
                        .apply_patch_if_snapshot_matches(
                            &snapshot,
                            &GraphPatch {
                                upsert_nodes: vec![node("a")],
                                ..GraphPatch::default()
                            }
                        )
                        .unwrap()
                );
            }
            "clear" => store.clear().unwrap(),
            _ => unreachable!(),
        }
        let node_survives = matches!(action, "edge" | "delete_edge" | "delete_label");
        let edge_survives = matches!(action, "node" | "patch");
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap().is_some(),
            node_survives,
            "{action}"
        );
        assert_eq!(
            store.current_source_binding(&ae.fact).unwrap().is_some(),
            edge_survives,
            "{action}"
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            if action == "clear" {
                vec![]
            } else {
                vec![b, be]
            }
        );
    }
}

#[test]
fn raw_second_connection_writes_invalidate_without_foreign_key_pragmas() {
    // AC-0150: triggers cover independent connections, including REPLACE with
    // recursive_triggers/foreign_keys disabled and node deletion leaving an edge.
    for (sql, node_survives, edge_survives) in [
        ("UPDATE nodes SET props = props WHERE id = 'a'", false, true),
        (
            "INSERT OR REPLACE INTO nodes(id,label,props) VALUES ('a','Symbol','{}')",
            false,
            false,
        ),
        (
            "UPDATE edges SET props = props WHERE src = 'a'",
            true,
            false,
        ),
        (
            "INSERT OR REPLACE INTO edges(src,dst,label,props) VALUES ('a','a','GOVERNS','{}')",
            true,
            false,
        ),
        ("DELETE FROM nodes WHERE id = 'a'", false, false),
        ("DELETE FROM edges WHERE src = 'a'", true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, ae, b, be) = seed(&mut store);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "foreign_keys", "OFF").unwrap();
        writer
            .pragma_update(None, "recursive_triggers", "OFF")
            .unwrap();
        writer.execute_batch(sql).unwrap();
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap().is_some(),
            node_survives,
            "{sql}"
        );
        assert_eq!(
            store.current_source_binding(&ae.fact).unwrap().is_some(),
            edge_survives,
            "{sql}"
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            vec![b, be]
        );
    }
}

#[test]
fn source_publication_rolls_back_facts_and_bindings_on_validation_or_write_failure() {
    // AC-0150: clearing old bindings, patch writes and final binding validation
    // are one transaction, including late missing-fact/digest/FK failures.
    for failure in ["missing", "digest", "edge"] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let (a, ae, b, be) = seed(&mut store);
        let snapshot = store.read_snapshot().unwrap();
        let mut changed = node("a");
        changed.props["name"] = "changed".into();
        let mut patch = GraphPatch {
            upsert_nodes: vec![changed.clone()],
            delete_edges: vec![("a".into(), "a".into(), "GOVERNS".into())],
            ..GraphPatch::default()
        };
        let binding = match failure {
            "missing" => node_binding(&node("missing"), "repo/a", "missing"),
            "digest" => a.clone(),
            "edge" => {
                patch.upsert_edges.push(Edge {
                    dst: "missing".into(),
                    ..edge("a")
                });
                node_binding(&changed, "repo/a", "changed")
            }
            _ => unreachable!(),
        };
        assert!(
            store
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &snapshot,
                    &patch,
                    "repo/a",
                    &[binding]
                )
                .is_err()
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(store.read_snapshot().unwrap(), snapshot);
        assert_eq!(
            store.source_bindings_for_repo("repo/a").unwrap(),
            vec![a, ae]
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            vec![b, be]
        );
    }
}

#[test]
fn source_publication_rejects_stale_duplicate_and_cross_repo_bindings() {
    // AC-0150: stale input is a no-write false result; malformed ownership and
    // attempted takeover are errors, including when a patch would erase old metadata.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, ae, b, be) = seed(&mut store);
    let stale = store.read_snapshot().unwrap();
    store.put_node(&node("unrelated")).unwrap();
    let current = store.read_snapshot().unwrap();
    assert!(
        !store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &stale,
                &GraphPatch::default(),
                "repo/a",
                &[]
            )
            .unwrap()
    );
    for bindings in [vec![a.clone(), a.clone()], vec![b.clone()]] {
        assert!(
            store
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &current,
                    &GraphPatch::default(),
                    "repo/a",
                    &bindings
                )
                .is_err()
        );
    }
    let takeover = SourceBinding {
        repo_key: "repo/a".into(),
        ..b.clone()
    };
    let patch = GraphPatch {
        upsert_nodes: vec![node("b")],
        ..GraphPatch::default()
    };
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &current,
                &patch,
                "repo/a",
                &[takeover]
            )
            .is_err()
    );
    assert_eq!(store.read_snapshot().unwrap(), current);
    assert_eq!(
        store.source_bindings_for_repo("repo/a").unwrap(),
        vec![a, ae]
    );
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b, be]
    );
}

#[test]
fn current_binding_read_is_coherent_across_another_connection_publication() {
    // AC-0150, AC-0152: a second connection can publish between association and
    // fact lookup. The result must be wholly old or wholly new, including an
    // identical-fact receipt replacement that a graph snapshot hash cannot see.
    for change_fact in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, _, _, _) = seed(&mut store);
        let mut writer = SqliteGraphStore::open(&path).unwrap();
        let mut changed = node("a");
        if change_fact {
            changed.props["name"] = "new contents".into();
        }
        let replacement = node_binding(&changed, "repo/a", "new capture");
        let after = replacement.clone();
        store.set_source_binding_after_lookup_hook(move || {
            let expected = writer.read_snapshot()?;
            let patch = if change_fact {
                GraphPatch {
                    upsert_nodes: vec![changed],
                    ..GraphPatch::default()
                }
            } else {
                GraphPatch::default()
            };
            assert!(writer.apply_patch_with_source_bindings_if_snapshot_matches(
                &expected,
                &patch,
                "repo/a",
                &[replacement]
            )?);
            Ok(())
        });
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap(),
            Some(a.clone())
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(store.current_source_binding(&a.fact).unwrap(), Some(after));
    }
}

#[test]
fn current_binding_rejects_corrupt_digest_or_missing_fact_and_releases_transaction() {
    // AC-0150, AC-0152: valid-looking stored metadata cannot certify changed or
    // missing facts. Failure releases its read transaction for later operations.
    for missing in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, _, _, _) = seed(&mut store);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "foreign_keys", "OFF").unwrap();
        if missing {
            writer
                .execute("DELETE FROM nodes WHERE id = 'a'", [])
                .unwrap();
            writer.execute("INSERT INTO source_node_bindings(node_id,repo_key,receipt_id,emitted_fact_digest) VALUES ('a',?1,?2,?3)", params![a.repo_key, a.receipt_id, a.emitted_fact_digest]).unwrap();
        } else {
            writer
                .execute(
                    "UPDATE source_node_bindings SET emitted_fact_digest = ?1 WHERE node_id = 'a'",
                    [node_digest(&node("different")).unwrap()],
                )
                .unwrap();
        }
        assert!(matches!(
            store.current_source_binding(&a.fact),
            Err(GraphError::SourceBinding(_))
        ));
        assert!(store.conn.is_autocommit());
        store.set_source_binding_after_lookup_hook(|| {
            Err(GraphError::SourceBinding("test hook failure"))
        });
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("b")))
                .is_err()
        );
        assert!(store.conn.is_autocommit());
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("b")))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn association_schema_migrates_without_clearing_schema_four_facts_and_rejects_incompatibility() {
    // AC-0150: private-table migration preserves existing version-four facts;
    // partial/future schemas and missing invalidation triggers fail, not repair.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-graph.db");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}') STRICT; CREATE TABLE edges (src TEXT NOT NULL REFERENCES nodes(id), dst TEXT NOT NULL REFERENCES nodes(id), label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (src,dst,label)) STRICT; INSERT INTO nodes(id,label) VALUES ('retained','Symbol'); INSERT INTO edges(src,dst,label) VALUES ('retained','retained','CALLS');").unwrap();
    connection
        .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)
        .unwrap();
    drop(connection);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(store.fact_counts().unwrap(), (1, 1));
    assert!(store.source_bindings_for_repo("repo/a").unwrap().is_empty());
    drop(store);
    for corruption in [
        "DROP TABLE source_binding_meta",
        "PRAGMA ignore_check_constraints=ON; UPDATE source_binding_meta SET version=4",
        "DROP TRIGGER source_binding_node_update",
        "CREATE VIEW source_binding_unknown AS SELECT 1",
        "CREATE UNIQUE INDEX unrelated_unique_repo ON source_node_bindings(repo_key)",
        "CREATE TRIGGER unrelated_binding_write AFTER INSERT ON source_node_bindings BEGIN DELETE FROM source_node_bindings WHERE node_id=NEW.node_id; END",
    ] {
        let path = directory.path().join(format!(
            "corrupt-{}.db",
            core_prov::content_hash(corruption.as_bytes())
        ));
        let mut store = SqliteGraphStore::open(&path).unwrap();
        seed(&mut store);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(corruption).unwrap();
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("a")))
                .is_err()
        );
        assert!(SqliteGraphStore::open(&path).is_err(), "{corruption}");
        assert_eq!(
            store.fact_counts().unwrap(),
            (2, 2),
            "failed migration must not clear facts"
        );
    }
}

#[test]
fn source_selection_distinguishes_missing_absent_present_and_orders_unique_keys() {
    // AC-0173, AC-0177: missing facts differ from present unbound facts, directed
    // keys remain distinct, and duplicates do not change deterministic ordering.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, ae, b, be) = seed(&mut store);
    let unbound = node("c");
    store.put_node(&unbound).unwrap();
    let mut unbound_edges = Vec::new();
    for (destination, label) in [("c", "A"), ("b", "Z"), ("b", "A")] {
        let edge = Edge {
            src: "a".into(),
            dst: destination.into(),
            label: label.into(),
            props: serde_json::json!({"direction": destination, "relation": label}),
        };
        store.put_edge(&edge).unwrap();
        unbound_edges.push(edge);
    }
    let missing_node = FactKey::from_node(&node("missing"));
    let missing_edge = FactKey::Edge {
        source: "b".into(),
        label: "A".into(),
        destination: "a".into(),
    };
    let mut expected = BTreeMap::new();
    for binding in [a, ae, b, be] {
        expected.insert(
            binding.fact.clone(),
            FactSourceState::Present {
                fact_digest: binding.emitted_fact_digest.clone(),
                binding,
            },
        );
    }
    expected.insert(missing_node, FactSourceState::Missing);
    expected.insert(missing_edge, FactSourceState::Missing);
    expected.insert(
        FactKey::from_node(&unbound),
        FactSourceState::Absent {
            fact_digest: node_digest(&unbound).unwrap(),
        },
    );
    for edge in unbound_edges {
        expected.insert(
            FactKey::from_edge(&edge),
            FactSourceState::Absent {
                fact_digest: edge_digest(&edge).unwrap(),
            },
        );
    }
    let mut keys: Vec<_> = expected.keys().rev().cloned().collect();
    keys.extend(keys.clone());
    let selection = store.read_source_selection_snapshot(&keys).unwrap();
    assert_eq!(selection.graph, store.read_snapshot().unwrap());
    assert_eq!(
        selection.selections,
        expected
            .into_iter()
            .map(|(fact, state)| FactSourceSelection {
                fact,
                state,
                omission: OmissionLookup::Unrecorded,
            })
            .collect::<Vec<_>>()
    );
    let empty = store.read_source_selection_snapshot(&[]).unwrap();
    assert_eq!(empty.graph, selection.graph);
    assert!(empty.selections.is_empty());
    assert!(store.conn.is_autocommit());
}

#[test]
fn selections_over_an_observed_snapshot_match_a_new_read_and_refuse_a_moved_store() {
    // AC-0659 (#1834): one graph copy serves many selections. Over a
    // snapshot already read, the states of any keys are the ones a new
    // snapshot read returns, bindings and missing facts included; once the
    // store has moved nothing is answered from the old rows, and the same
    // key bound applies.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, ae, b, _) = seed(&mut store);
    let missing = FactKey::Node {
        id: "missing".into(),
    };
    let observed = store.read_source_selection_snapshot(&[]).unwrap();
    for keys in [
        vec![],
        vec![a.fact.clone()],
        vec![
            b.fact.clone(),
            missing.clone(),
            ae.fact.clone(),
            b.fact.clone(),
        ],
    ] {
        let fresh = store.read_source_selection_snapshot(&keys).unwrap();
        assert_eq!(fresh.revision, observed.revision);
        assert_eq!(
            store.read_source_selections_at(&observed, &keys).unwrap(),
            fresh.selections
        );
    }
    assert!(matches!(
        store.read_source_selections_at(
            &observed,
            &vec![a.fact.clone(); MAX_SOURCE_SELECTION_KEYS + 1]
        ),
        Err(GraphError::SourceBinding("too many source selection keys"))
    ));
    store.put_node(&node("later")).unwrap();
    assert!(matches!(
        store.read_source_selections_at(&observed, std::slice::from_ref(&a.fact)),
        Err(GraphError::Generation(crate::GenerationError::Changed))
    ));
    let moved = store.read_source_selection_snapshot(&[]).unwrap();
    assert_ne!(moved.revision, observed.revision);
    assert_eq!(
        store
            .read_source_selections_at(&moved, std::slice::from_ref(&a.fact))
            .unwrap(),
        store
            .read_source_selection_snapshot(std::slice::from_ref(&a.fact))
            .unwrap()
            .selections
    );
    assert!(store.conn.is_autocommit());
}

#[test]
fn source_selection_rejects_raw_key_budget_and_invalid_keys_before_graph_access() {
    // AC-0173: the raw request bound precedes deduplication and database access;
    // malformed keys do not become missing facts, even with a broken schema.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, _, _, _) = seed(&mut store);
    let maximum = vec![a.fact.clone(); MAX_SOURCE_SELECTION_KEYS];
    assert_eq!(
        store
            .read_source_selection_snapshot(&maximum)
            .unwrap()
            .selections
            .len(),
        1
    );
    store
        .conn
        .execute_batch("DROP TRIGGER source_binding_node_update")
        .unwrap();
    assert!(matches!(
        store.read_source_selection_snapshot(&vec![a.fact; MAX_SOURCE_SELECTION_KEYS + 1]),
        Err(GraphError::SourceBinding("too many source selection keys"))
    ));
    for invalid in [
        FactKey::Node { id: String::new() },
        FactKey::Node {
            id: "x".repeat(MAX_ID_BYTES + 1),
        },
        FactKey::Edge {
            source: "a".into(),
            label: "\0".into(),
            destination: "a".into(),
        },
    ] {
        assert!(matches!(
            store.read_source_selection_snapshot(&[invalid]),
            Err(GraphError::SourceBinding("invalid bounded identity"))
        ));
    }
    assert!(matches!(
        store.read_source_selection_snapshot(&maximum),
        Err(GraphError::SourceBinding("incompatible association schema"))
    ));
    assert!(store.conn.is_autocommit());
}

#[test]
fn source_selection_is_coherent_across_graph_and_receipt_publication() {
    // AC-0173, AC-0177: another WAL connection commits during the real read
    // transaction, between graph halves or between selected binding rows. Even
    // equal facts with new receipts must not mix old and new association states.
    for after_nodes in [false, true] {
        for change_facts in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("graph.db");
            let mut store = SqliteGraphStore::open(&path).unwrap();
            let (a, ae, b, be) = seed(&mut store);
            let keys = [a.fact, ae.fact, b.fact, be.fact];
            let before = store.read_source_selection_snapshot(&keys).unwrap();
            let mut writer = SqliteGraphStore::open(&path).unwrap();
            let mut changed_node = node("a");
            let mut changed_edge = edge("a");
            if change_facts {
                changed_node.props["name"] = "new fact".into();
                changed_edge.props["observation"] = "new observation".into();
            }
            let replacement_node = node_binding(&changed_node, "repo/a", "new capture");
            let mut replacement_edge = edge_binding(&changed_edge, "repo/a");
            replacement_edge.receipt_id = replacement_node.receipt_id.clone();
            let expected_node = replacement_node.clone();
            let expected_edge = replacement_edge.clone();
            let publish = move || {
                let expected = writer.read_snapshot()?;
                let patch = if change_facts {
                    GraphPatch {
                        upsert_nodes: vec![changed_node],
                        upsert_edges: vec![changed_edge],
                        ..GraphPatch::default()
                    }
                } else {
                    GraphPatch::default()
                };
                assert!(writer.apply_patch_with_source_bindings_if_snapshot_matches(
                    &expected,
                    &patch,
                    "repo/a",
                    &[replacement_node, replacement_edge],
                )?);
                Ok(())
            };
            if after_nodes {
                store.set_snapshot_after_nodes_hook(publish);
            } else {
                store.set_source_binding_after_lookup_hook(publish);
            }
            assert_eq!(store.read_source_selection_snapshot(&keys).unwrap(), before);
            assert!(store.conn.is_autocommit());
            let after = store.read_source_selection_snapshot(&keys).unwrap();
            assert_eq!(after.graph == before.graph, !change_facts);
            assert_ne!(after.selections, before.selections);
            for binding in [expected_node, expected_edge] {
                assert!(after.selections.contains(&FactSourceSelection {
                    fact: binding.fact.clone(),
                    state: FactSourceState::Present {
                        fact_digest: binding.emitted_fact_digest.clone(),
                        binding,
                    },
                    omission: OmissionLookup::Unrecorded,
                }));
            }
            for selection in before.selections {
                if matches!(&selection.state, FactSourceState::Present { binding, .. } if binding.repo_key == "repo/b")
                {
                    assert!(after.selections.contains(&selection));
                }
            }
        }
    }
}

#[test]
fn source_selection_keeps_malformed_and_orphan_associations_per_key() {
    // AC-0173, AC-0177: invalid metadata cannot become valid absence, including
    // orphan rows left by a connection with foreign-key enforcement disabled.
    for corruption in ["large", "digest", "nul", "utf8", "orphan"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, ae, b, _) = seed(&mut store);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "foreign_keys", "OFF").unwrap();
        writer
            .pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        match corruption {
            "large" => {
                let oversized = "SOURCE_CANARY_".repeat(100_000);
                writer
                    .execute(
                        "UPDATE source_node_bindings SET receipt_id = ?1 WHERE node_id = 'a'",
                        [&oversized],
                    )
                    .unwrap();
                writer
                    .execute(
                        "UPDATE source_edge_bindings SET repo_key = ?1 WHERE source = 'a'",
                        [&oversized],
                    )
                    .unwrap();
            }
            "digest" => {
                writer.execute("UPDATE source_node_bindings SET emitted_fact_digest = ?1 WHERE node_id = 'a'", [node_digest(&node("different")).unwrap()]).unwrap();
                writer.execute("UPDATE source_edge_bindings SET emitted_fact_digest = ?1 WHERE source = 'a'", ["x".repeat(72)]).unwrap();
            }
            "nul" => {
                writer.execute_batch("UPDATE source_node_bindings SET repo_key = char(0) WHERE node_id = 'a'; UPDATE source_edge_bindings SET receipt_id = char(0) WHERE source = 'a'").unwrap();
            }
            "utf8" => {
                writer.execute_batch("UPDATE source_node_bindings SET repo_key = CAST(x'80' AS TEXT) WHERE node_id = 'a'; UPDATE source_edge_bindings SET receipt_id = CAST(x'80' AS TEXT) WHERE source = 'a'").unwrap();
            }
            "orphan" => {
                writer
                    .execute_batch(
                        "DELETE FROM edges WHERE src = 'a'; DELETE FROM nodes WHERE id = 'a'",
                    )
                    .unwrap();
                writer.execute("INSERT INTO source_node_bindings(node_id, repo_key, receipt_id, emitted_fact_digest) VALUES ('a', ?1, ?2, ?3)", params![a.repo_key, a.receipt_id, a.emitted_fact_digest]).unwrap();
                writer.execute("INSERT INTO source_edge_bindings(source, label, destination, repo_key, receipt_id, emitted_fact_digest) VALUES ('a', 'GOVERNS', 'a', ?1, ?2, ?3)", params![ae.repo_key, ae.receipt_id, ae.emitted_fact_digest]).unwrap();
            }
            _ => unreachable!(),
        }
        let selection = store
            .read_source_selection_snapshot(&[a.fact.clone(), ae.fact.clone(), b.fact.clone()])
            .unwrap();
        for binding in [a, ae] {
            assert!(selection.selections.contains(&FactSourceSelection {
                fact: binding.fact,
                state: FactSourceState::Invalid {
                    fact_digest: (corruption != "orphan").then_some(binding.emitted_fact_digest),
                },
                omission: OmissionLookup::Unrecorded,
            }));
        }
        assert!(selection.selections.contains(&FactSourceSelection {
            fact: b.fact.clone(),
            state: FactSourceState::Present {
                fact_digest: b.emitted_fact_digest.clone(),
                binding: b,
            },
            omission: OmissionLookup::Unrecorded,
        }));
        assert!(!format!("{:?}", selection.selections).contains("SOURCE_CANARY_"));
        assert!(store.conn.is_autocommit());
    }
}

#[test]
fn source_selection_reader_preflights_sql_types_and_utf8_byte_lengths() {
    // AC-0173: exercise the bounded SQL reader against malformed storage types
    // independently of STRICT-schema admission. Type and byte bounds precede
    // returned field bodies; Unicode character counts cannot undercount bytes.
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE source_node_bindings (node_id TEXT PRIMARY KEY, repo_key, receipt_id, emitted_fact_digest); CREATE TABLE source_edge_bindings (source TEXT, label TEXT, destination TEXT, repo_key, receipt_id, emitted_fact_digest)").unwrap();
    let a = node_binding(&node("a"), "repo/a", "a");
    let ae = edge_binding(&edge("a"), "repo/a");
    connection
        .execute(
            "INSERT INTO source_node_bindings VALUES ('a', ?1, ?2, ?3)",
            params![a.repo_key, a.receipt_id, a.emitted_fact_digest],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_edge_bindings VALUES ('a', 'GOVERNS', 'a', ?1, ?2, ?3)",
            params![ae.repo_key, ae.receipt_id, ae.emitted_fact_digest],
        )
        .unwrap();
    for table in ["source_node_bindings", "source_edge_bindings"] {
        let fact = if table == "source_node_bindings" {
            &a.fact
        } else {
            &ae.fact
        };
        let original = if table == "source_node_bindings" {
            &a
        } else {
            &ae
        };
        assert!(matches!(
            load_selection_binding(&connection, BindingRole::Primary, fact).unwrap(),
            SelectionBinding::Present(binding) if &binding == original
        ));
        for column in ["repo_key", "receipt_id", "emitted_fact_digest"] {
            for expression in ["zeroblob(1048576)", "42", "NULL", "CAST(x'80' AS TEXT)"] {
                connection
                    .execute(&format!("UPDATE {table} SET {column} = {expression}"), [])
                    .unwrap();
                assert!(matches!(
                    load_selection_binding(&connection, BindingRole::Primary, fact).unwrap(),
                    SelectionBinding::Invalid
                ));
                let binding = if table == "source_node_bindings" {
                    &a
                } else {
                    &ae
                };
                connection.execute(&format!("UPDATE {table} SET repo_key = ?1, receipt_id = ?2, emitted_fact_digest = ?3"), params![binding.repo_key, binding.receipt_id, binding.emitted_fact_digest]).unwrap();
                assert!(matches!(
                    load_selection_binding(&connection, BindingRole::Primary, fact).unwrap(),
                    SelectionBinding::Present(binding) if &binding == original
                ));
            }
        }
        // 200 characters pass a character-count bound of 256, but occupy 400 bytes.
        connection
            .execute(
                &format!("UPDATE {table} SET repo_key = ?1"),
                ["é".repeat(200)],
            )
            .unwrap();
        assert!(matches!(
            load_selection_binding(&connection, BindingRole::Primary, fact).unwrap(),
            SelectionBinding::Invalid
        ));
    }
}

#[test]
fn source_selection_global_failures_release_the_read_transaction() {
    // AC-0173, AC-0177: operational/schema/graph errors remain fatal and all
    // failure paths release their read revision; the one-shot hook is consumed.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, _, _, _) = seed(&mut store);
    let keys = [a.fact];
    let expected = store.read_source_selection_snapshot(&keys).unwrap();
    for after_nodes in [true, false] {
        let failure = || Err(GraphError::Storage(rusqlite::Error::InvalidQuery));
        if after_nodes {
            store.set_snapshot_after_nodes_hook(failure);
        } else {
            store.set_source_binding_after_lookup_hook(failure);
        }
        assert!(matches!(
            store.read_source_selection_snapshot(&keys),
            Err(GraphError::Storage(rusqlite::Error::InvalidQuery))
        ));
        assert!(store.conn.is_autocommit());
        assert_eq!(
            store.read_source_selection_snapshot(&keys).unwrap(),
            expected
        );
    }
    // Malformed graph JSON is global even when the bad fact was not selected.
    store
        .conn
        .execute("UPDATE nodes SET props = '{' WHERE id = 'b'", [])
        .unwrap();
    assert!(matches!(
        store.read_source_selection_snapshot(&keys),
        Err(GraphError::Props(_))
    ));
    assert!(store.conn.is_autocommit());
    store.put_node(&node("b")).unwrap();
    // The repair is a row event, so only the revision differs.
    let repaired = store.read_source_selection_snapshot(&keys).unwrap();
    assert_eq!(
        (&repaired.graph, &repaired.selections),
        (&expected.graph, &expected.selections)
    );
    assert_ne!(repaired.revision, expected.revision);
    store
        .conn
        .execute_batch("DROP TRIGGER source_binding_node_update")
        .unwrap();
    assert!(matches!(
        store.read_source_selection_snapshot(&keys),
        Err(GraphError::SourceBinding("incompatible association schema"))
    ));
    assert!(store.conn.is_autocommit());
}

fn coverage_record(repo: &str) -> crate::coverage::CoverageRecord {
    crate::coverage::CoverageRecord {
        repo_key: repo.into(),
        registered_source_id: None,
        ingest_id: format!("ingest-{repo}"),
        config_hash: "config-v1:0".into(),
        inventory_json: "{}".into(),
    }
}

#[test]
fn fresh_store_is_current_and_exact_v1_migrates_with_empty_omission_tables() {
    // AC-0314: fresh stores open at the current schema; an exact v1 store
    // migrates in one immediate transaction with empty omission tables, while
    // a v1 store with an extra object is rejected. (T-0314)
    let fresh = SqliteGraphStore::open_in_memory().unwrap();
    let fresh_version: i64 = fresh
        .conn
        .query_row("SELECT version FROM source_binding_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(fresh_version, SCHEMA_VERSION);

    let directory = tempfile::tempdir().unwrap();
    let v1_path = directory.path().join("exact-v1.db");
    {
        let connection = Connection::open(&v1_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}') STRICT;
                 CREATE TABLE edges (src TEXT NOT NULL REFERENCES nodes(id), dst TEXT NOT NULL REFERENCES nodes(id), label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (src,dst,label)) STRICT;
                 INSERT INTO nodes(id,label) VALUES ('retained','Symbol');",
            )
            .unwrap();
        for (_, _, sql) in SCHEMA_V1 {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO source_binding_meta(singleton, version) VALUES (1, 1)",
                [],
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)
            .unwrap();
    }

    let migrated = SqliteGraphStore::open(&v1_path).unwrap();
    let migrated_version: i64 = migrated
        .conn
        .query_row("SELECT version FROM source_binding_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migrated_version, SCHEMA_VERSION);
    let counts: (i64, i64, i64) = migrated
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM source_node_omissions),
                    (SELECT count(*) FROM source_edge_omissions),
                    (SELECT count(*) FROM source_omission_index)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0, 0));
    assert_eq!(
        migrated
            .receipt_omission(&FactKey::Node {
                id: "retained".into()
            })
            .unwrap(),
        OmissionLookup::Unrecorded
    );
    drop(migrated);

    let extra_path = directory.path().join("v1-extra.db");
    {
        let connection = Connection::open(&extra_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}') STRICT;
                 CREATE TABLE edges (src TEXT NOT NULL REFERENCES nodes(id), dst TEXT NOT NULL REFERENCES nodes(id), label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (src,dst,label)) STRICT;",
            )
            .unwrap();
        for (_, _, sql) in SCHEMA_V1 {
            connection.execute_batch(sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO source_binding_meta(singleton, version) VALUES (1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute_batch("CREATE INDEX source_binding_extra ON source_node_bindings(receipt_id)")
            .unwrap();
        connection
            .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)
            .unwrap();
    }
    assert!(matches!(
        SqliteGraphStore::open(&extra_path),
        Err(GraphError::SourceBinding("incompatible association schema"))
    ));
}

#[test]
fn receipt_omissions_publish_lookup_and_require_complete_coverage() {
    // AC-0314: published per-fact omission records return Omitted with their
    // kind, unrecorded facts return NotOmitted only when every covered repo has
    // a complete omission index, and SourceOmissions::NONE leaves a repo
    // unrecorded. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let n_omitted = node("repo/a:omitted");
    let n_bound = node("repo/a:bound");
    let e_omitted = edge("repo/a:omitted");
    let patch_a = GraphPatch {
        upsert_nodes: vec![n_omitted.clone(), n_bound.clone()],
        upsert_edges: vec![e_omitted.clone()],
        ..GraphPatch::default()
    };
    let cov_a = coverage_record("repo/a");
    let omissions_a = [
        SourceOmission {
            fact: FactKey::from_node(&n_omitted),
            emitted_fact_digest: node_digest(&n_omitted).unwrap(),
            kind: ReceiptOmissionKind::Declarations,
        },
        SourceOmission {
            fact: FactKey::from_edge(&e_omitted),
            emitted_fact_digest: edge_digest(&e_omitted).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
    ];

    // Before coverage is complete (incomplete producer), unrecorded facts are Unrecorded.
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &patch_a,
                "repo/a",
                &[node_binding(&n_bound, "repo/a", "r1")],
                Some(&cov_a),
                SourceOmissions {
                    records: &omissions_a,
                    complete: false,
                },
            )
            .unwrap()
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&n_omitted))
            .unwrap(),
        OmissionLookup::Omitted {
            repo_key: "repo/a".into(),
            kind: ReceiptOmissionKind::Declarations,
        }
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_edge(&e_omitted))
            .unwrap(),
        OmissionLookup::Omitted {
            repo_key: "repo/a".into(),
            kind: ReceiptOmissionKind::Rules,
        }
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&n_bound))
            .unwrap(),
        OmissionLookup::Unrecorded
    );

    // Republish repo/a with complete = true; n_bound now resolves to NotOmitted.
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "repo/a",
                &[node_binding(&n_bound, "repo/a", "r1")],
                Some(&cov_a),
                SourceOmissions {
                    records: &omissions_a,
                    complete: true,
                },
            )
            .unwrap()
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&n_bound))
            .unwrap(),
        OmissionLookup::NotOmitted
    );

    // A second repo published with coverage and SourceOmissions::NONE has no
    // omission index row, so non-omitted facts degrade back to Unrecorded.
    let n_b = node("repo/b:symbol");
    let cov_b = coverage_record("repo/b");
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![n_b.clone()],
                    ..GraphPatch::default()
                },
                "repo/b",
                &[],
                Some(&cov_b),
                SourceOmissions::NONE,
            )
            .unwrap()
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&n_omitted))
            .unwrap(),
        OmissionLookup::Omitted {
            repo_key: "repo/a".into(),
            kind: ReceiptOmissionKind::Declarations,
        }
    );
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&n_bound))
            .unwrap(),
        OmissionLookup::Unrecorded
    );
    assert_eq!(
        store.receipt_omission(&FactKey::from_node(&n_b)).unwrap(),
        OmissionLookup::Unrecorded
    );
}

#[test]
fn receipt_omissions_deduplicate_by_smallest_kind_and_respect_bounds_and_rewrites() {
    // AC-0314: duplicate facts keep the smallest kind by Ord; over MAX_OMISSIONS
    // records an incomplete index with no rows; rewriting or deleting a fact
    // removes its omission row; batch lookup rejects 66 keys. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let n1 = node("repo/a:n1");
    let n2 = node("repo/a:n2");
    let cov_a = coverage_record("repo/a");
    let expected = store.read_snapshot().unwrap();
    let duplicate_omissions = [
        SourceOmission {
            fact: FactKey::from_node(&n1),
            emitted_fact_digest: node_digest(&n1).unwrap(),
            kind: ReceiptOmissionKind::StoreCapacity,
        },
        SourceOmission {
            fact: FactKey::from_node(&n1),
            emitted_fact_digest: node_digest(&n1).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
        SourceOmission {
            fact: FactKey::from_node(&n1),
            emitted_fact_digest: node_digest(&n1).unwrap(),
            kind: ReceiptOmissionKind::Declarations,
        },
        SourceOmission {
            fact: FactKey::from_node(&n2),
            emitted_fact_digest: node_digest(&n2).unwrap(),
            kind: ReceiptOmissionKind::StoreCapacity,
        },
        SourceOmission {
            fact: FactKey::from_node(&n2),
            emitted_fact_digest: node_digest(&n2).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
    ];
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![n1.clone(), n2.clone()],
                    ..GraphPatch::default()
                },
                "repo/a",
                &[],
                Some(&cov_a),
                SourceOmissions {
                    records: &duplicate_omissions,
                    complete: true,
                },
            )
            .unwrap()
    );
    assert_eq!(
        store
            .receipt_omissions(&[FactKey::from_node(&n1), FactKey::from_node(&n2)])
            .unwrap(),
        vec![
            OmissionLookup::Omitted {
                repo_key: "repo/a".into(),
                kind: ReceiptOmissionKind::Declarations,
            },
            OmissionLookup::Omitted {
                repo_key: "repo/a".into(),
                kind: ReceiptOmissionKind::Rules,
            },
        ]
    );

    // Rewriting n1 via a later patch fires the node trigger and removes its
    // omission row, so lookup returns NotOmitted rather than Omitted.
    let mut rewritten_n1 = n1.clone();
    rewritten_n1.props = serde_json::json!({"rewritten": true});
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![rewritten_n1.clone()],
                    ..GraphPatch::default()
                },
            )
            .unwrap()
    );
    assert_eq!(
        store.receipt_omission(&FactKey::from_node(&n1)).unwrap(),
        OmissionLookup::NotOmitted
    );

    // Deleting n2 removes it from the graph and lookup returns Unrecorded.
    store.delete_node(&n2.id).unwrap();
    assert_eq!(
        store.receipt_omission(&FactKey::from_node(&n2)).unwrap(),
        OmissionLookup::Unrecorded
    );

    // Exceeding MAX_OMISSIONS short-circuits before per-fact SQLite work, so
    // testing MAX_OMISSIONS + 1 in-memory records exercises the real production
    // constant without inserting 262,144 database rows.
    let over_bound = vec![
        SourceOmission {
            fact: FactKey::from_node(&rewritten_n1),
            emitted_fact_digest: node_digest(&rewritten_n1).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        };
        MAX_OMISSIONS + 1
    ];
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "repo/a",
                &[],
                Some(&cov_a),
                SourceOmissions {
                    records: &over_bound,
                    complete: true,
                },
            )
            .unwrap()
    );
    let (node_rows, index_state, index_recorded): (i64, String, i64) = store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM source_node_omissions), state, recorded
             FROM source_omission_index WHERE repo_key = 'repo/a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(node_rows, 0);
    assert_eq!(index_state, "incomplete");
    assert_eq!(index_recorded, (MAX_OMISSIONS + 1) as i64);
    assert_eq!(
        store
            .receipt_omission(&FactKey::from_node(&rewritten_n1))
            .unwrap(),
        OmissionLookup::Unrecorded
    );

    // Batch lookup rejects 66 keys before database access.
    let too_many: Vec<FactKey> = (0..=MAX_SOURCE_SELECTION_KEYS)
        .map(|index| FactKey::Node {
            id: format!("n-{index}"),
        })
        .collect();
    assert!(matches!(
        store.receipt_omissions(&too_many),
        Err(GraphError::SourceBinding("too many source selection keys"))
    ));
}

#[test]
fn omission_only_publication_advances_revision_and_binds_selection_basis() {
    // AC-0314: a publication whose only row events are omission records (an
    // existing unbound fact, no bindings, no coverage, empty patch) still
    // advances the store revision, and a revision-bound selection refuses the
    // stale basis instead of pairing it with the new omission. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let fact_node = node("repo/a:unbound");
    let fact = FactKey::from_node(&fact_node);
    let empty = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_if_snapshot_matches(
                &empty,
                &GraphPatch {
                    upsert_nodes: vec![fact_node.clone()],
                    ..GraphPatch::default()
                },
            )
            .unwrap()
    );
    let limits = SnapshotReadLimits::default();
    let before = store
        .read_source_selection_snapshot_bounded(std::slice::from_ref(&fact), limits)
        .unwrap();
    assert_eq!(before.selections[0].omission, OmissionLookup::Unrecorded);

    let records = [SourceOmission {
        fact: fact.clone(),
        emitted_fact_digest: node_digest(&fact_node).unwrap(),
        kind: ReceiptOmissionKind::StoreCapacity,
    }];
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "repo/a",
                &[],
                None,
                SourceOmissions {
                    records: &records,
                    complete: true,
                },
            )
            .unwrap()
    );
    let after = store
        .read_source_selection_snapshot_bounded(std::slice::from_ref(&fact), limits)
        .unwrap();
    assert_eq!(after.graph, before.graph);
    assert_eq!(
        after.selections[0].omission,
        OmissionLookup::Omitted {
            repo_key: "repo/a".into(),
            kind: ReceiptOmissionKind::StoreCapacity,
        }
    );
    assert_ne!(after.revision, before.revision);
    assert!(matches!(
        store.read_source_selection_snapshot_at_revision_bounded(
            &before.revision,
            std::slice::from_ref(&fact),
            limits,
        ),
        Err(GraphError::Generation(crate::GenerationError::Changed))
    ));
    assert_eq!(
        store
            .read_source_selection_snapshot_at_revision_bounded(
                &after.revision,
                std::slice::from_ref(&fact),
                limits,
            )
            .unwrap(),
        after
    );

    // Clearing the records with NONE is also a revision-advancing row event;
    // republishing NONE with nothing recorded is not.
    let expected = store.read_snapshot().unwrap();
    store
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &expected,
            &GraphPatch::default(),
            "repo/a",
            &[],
            None,
            SourceOmissions::NONE,
        )
        .unwrap();
    let cleared = store
        .read_source_selection_snapshot_bounded(std::slice::from_ref(&fact), limits)
        .unwrap();
    assert_eq!(cleared.selections[0].omission, OmissionLookup::Unrecorded);
    assert_ne!(cleared.revision, after.revision);
}

#[test]
fn bounded_omission_reads_preflight_coverage_schema_and_reject_malformed_cells() {
    // AC-0314: the bounded selection bounds the coverage schema before copying
    // it, malformed omission cells are never positive evidence, and clearing a
    // fact-free store with an omission index advances the revision. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let fact_node = node("repo/a:omitted");
    let fact = FactKey::from_node(&fact_node);
    let records = [SourceOmission {
        fact: fact.clone(),
        emitted_fact_digest: node_digest(&fact_node).unwrap(),
        kind: ReceiptOmissionKind::Rules,
    }];
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![fact_node.clone()],
                    ..GraphPatch::default()
                },
                "repo/a",
                &[],
                None,
                SourceOmissions {
                    records: &records,
                    complete: true,
                },
            )
            .unwrap()
    );
    let limits = SnapshotReadLimits::default();
    let select = |store: &SqliteGraphStore| {
        store
            .read_source_selection_snapshot_bounded(std::slice::from_ref(&fact), limits)
            .map(|snapshot| snapshot.selections[0].omission.clone())
    };
    assert!(matches!(
        select(&store).unwrap(),
        OmissionLookup::Omitted { .. }
    ));

    // Malformed cells (bypassing CHECK constraints) read as Unrecorded.
    for corruption in [
        "UPDATE source_node_omissions SET kind = 'bogus'",
        "UPDATE source_node_omissions SET repo_key = printf('%.300c', 'r')",
    ] {
        let snapshot = store.conn.query_row(
            "SELECT repo_key, kind FROM source_node_omissions",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        let (repo_key, kind) = snapshot.unwrap();
        store
            .conn
            .execute_batch(&format!(
                "PRAGMA ignore_check_constraints = 1; {corruption}; PRAGMA ignore_check_constraints = 0;"
            ))
            .unwrap();
        assert_eq!(select(&store).unwrap(), OmissionLookup::Unrecorded);
        store
            .conn
            .execute(
                "UPDATE source_node_omissions SET repo_key = ?1, kind = ?2",
                params![repo_key, kind],
            )
            .unwrap();
        assert!(matches!(
            select(&store).unwrap(),
            OmissionLookup::Omitted { .. }
        ));
    }

    // An oversized foreign coverage object is refused by the bounded preflight
    // before the exact-schema decoder copies it.
    let padding = "x".repeat(5000);
    store
        .conn
        .execute_batch(&format!(
            "CREATE TRIGGER coverage_foreign AFTER INSERT ON coverage_inventories BEGIN SELECT '{padding}'; END"
        ))
        .unwrap();
    assert!(matches!(
        select(&store),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::InvalidSchema
        ))
    ));
    store
        .conn
        .execute_batch("DROP TRIGGER coverage_foreign")
        .unwrap();

    // Clearing a store whose only remaining row is an omission index advances
    // the revision.
    store.clear().unwrap();
    let expected = store.read_snapshot().unwrap();
    store
        .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &expected,
            &GraphPatch::default(),
            "repo/a",
            &[],
            None,
            SourceOmissions {
                records: &[],
                complete: true,
            },
        )
        .unwrap();
    let before = store
        .read_snapshot_with_revision_bounded(limits)
        .unwrap()
        .revision;
    store.clear().unwrap();
    assert_ne!(
        store
            .read_snapshot_with_revision_bounded(limits)
            .unwrap()
            .revision,
        before
    );
    let after = store
        .read_snapshot_with_revision_bounded(limits)
        .unwrap()
        .revision;
    store.clear().unwrap();
    assert_eq!(
        store
            .read_snapshot_with_revision_bounded(limits)
            .unwrap()
            .revision,
        after
    );
}

#[test]
fn omission_records_require_the_omitted_digest_and_no_retained_binding() {
    // AC-0314: an omitted occurrence describes the published fact only when
    // its emitted digest is the fact's current digest, and a fact whose
    // primary receipt was retained (it has a binding) is never Omitted, even
    // when another collapsed occurrence of it was omitted. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let bound = node("repo/a:bound");
    let stale = node("repo/a:stale");
    let exact = node("repo/a:exact");
    let older_stale = Node {
        props: serde_json::json!({"prov": {"content_hash": "older"}, "name": "stale"}),
        ..stale.clone()
    };
    let records = [
        SourceOmission {
            fact: FactKey::from_node(&bound),
            emitted_fact_digest: node_digest(&bound).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
        SourceOmission {
            fact: FactKey::from_node(&stale),
            emitted_fact_digest: node_digest(&older_stale).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
        SourceOmission {
            fact: FactKey::from_node(&exact),
            emitted_fact_digest: node_digest(&older_stale).unwrap(),
            kind: ReceiptOmissionKind::Declarations,
        },
        SourceOmission {
            fact: FactKey::from_node(&exact),
            emitted_fact_digest: node_digest(&exact).unwrap(),
            kind: ReceiptOmissionKind::StoreCapacity,
        },
    ];
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![bound.clone(), stale.clone(), exact.clone()],
                    ..GraphPatch::default()
                },
                "repo/a",
                &[node_binding(&bound, "repo/a", "r1")],
                Some(&coverage_record("repo/a")),
                SourceOmissions {
                    records: &records,
                    complete: true,
                },
            )
            .unwrap()
    );
    assert_eq!(
        store
            .receipt_omissions(&[
                FactKey::from_node(&bound),
                FactKey::from_node(&stale),
                FactKey::from_node(&exact),
            ])
            .unwrap(),
        vec![
            OmissionLookup::NotOmitted,
            OmissionLookup::NotOmitted,
            // Only the occurrence emitted for the published content counts,
            // even though the stale one has a smaller kind.
            OmissionLookup::Omitted {
                repo_key: "repo/a".into(),
                kind: ReceiptOmissionKind::StoreCapacity,
            },
        ]
    );
    let malformed = [SourceOmission {
        fact: FactKey::from_node(&exact),
        emitted_fact_digest: "short".into(),
        kind: ReceiptOmissionKind::Rules,
    }];
    let expected = store.read_snapshot().unwrap();
    assert!(matches!(
        store.apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
            &expected,
            &GraphPatch::default(),
            "repo/a",
            &[],
            None,
            SourceOmissions {
                records: &malformed,
                complete: true,
            },
        ),
        Err(GraphError::SourceBinding("invalid omission digest"))
    ));
}

#[test]
fn later_binding_from_another_repo_takes_precedence_over_existing_omission() {
    // AC-0314: repo A records omissions for an unbound node and edge; repo B
    // then binds both with an empty patch and SourceOmissions::NONE. A's rows
    // survive B's repo-scoped replacement, but the selection never reports
    // Omitted beside a current binding. (T-0314)
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let shared_node = node("shared");
    let shared_edge = edge("shared");
    let facts = [
        FactKey::from_node(&shared_node),
        FactKey::from_edge(&shared_edge),
    ];
    let records = [
        SourceOmission {
            fact: facts[0].clone(),
            emitted_fact_digest: node_digest(&shared_node).unwrap(),
            kind: ReceiptOmissionKind::Rules,
        },
        SourceOmission {
            fact: facts[1].clone(),
            emitted_fact_digest: edge_digest(&shared_edge).unwrap(),
            kind: ReceiptOmissionKind::Declarations,
        },
    ];
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch {
                    upsert_nodes: vec![shared_node.clone()],
                    upsert_edges: vec![shared_edge.clone()],
                    ..GraphPatch::default()
                },
                "repo/a",
                &[],
                None,
                SourceOmissions {
                    records: &records,
                    complete: true,
                },
            )
            .unwrap()
    );
    let omissions = |store: &SqliteGraphStore| {
        store
            .read_source_selection_snapshot(&facts)
            .unwrap()
            .selections
            .into_iter()
            .map(|selection| (selection.state, selection.omission))
            .collect::<Vec<_>>()
    };
    assert!(
        omissions(&store)
            .iter()
            .all(|(_, omission)| matches!(omission, OmissionLookup::Omitted { .. }))
    );

    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "repo/b",
                &[
                    node_binding(&shared_node, "repo/b", "rb"),
                    edge_binding(&shared_edge, "repo/b"),
                ],
                None,
                SourceOmissions::NONE,
            )
            .unwrap()
    );
    let rows: i64 = store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM source_node_omissions) + (SELECT count(*) FROM source_edge_omissions)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2, "repo A's rows survive repo B's scoped replacement");
    for (state, omission) in omissions(&store) {
        assert!(
            matches!(state, FactSourceState::Present { .. }),
            "{state:?}"
        );
        assert!(
            !matches!(omission, OmissionLookup::Omitted { .. }),
            "{omission:?}"
        );
    }
}

fn observation(node: &Node, repo: &str) -> SourceBinding {
    SourceBinding {
        receipt_id: format!(
            "obs-input-v1:{}",
            core_prov::content_hash(node.id.as_bytes())
        ),
        ..node_binding(node, repo, "observed")
    }
}

#[test]
fn exact_v2_store_migrates_to_empty_observation_tables() {
    // AC-1047 (ADR-0079): an exact v2 store keeps its facts and bindings and
    // gains empty observed binding tables in one transaction; a v2 store
    // with an extra object is rejected.
    let directory = tempfile::tempdir().unwrap();
    for extra in [
        None,
        Some("CREATE INDEX source_binding_extra ON source_node_bindings(receipt_id)"),
    ] {
        let path = directory.path().join(format!("v2-{}.db", extra.is_some()));
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}') STRICT;
                     CREATE TABLE edges (src TEXT NOT NULL REFERENCES nodes(id), dst TEXT NOT NULL REFERENCES nodes(id), label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (src,dst,label)) STRICT;
                     INSERT INTO nodes(id,label) VALUES ('retained','Symbol');",
                )
                .unwrap();
            for (_, _, sql) in SCHEMA_V2 {
                connection.execute_batch(sql).unwrap();
            }
            connection
                .execute(
                    "INSERT INTO source_binding_meta(singleton, version) VALUES (1, 2)",
                    [],
                )
                .unwrap();
            if let Some(extra) = extra {
                connection.execute_batch(extra).unwrap();
            }
            connection
                .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)
                .unwrap();
        }
        let opened = SqliteGraphStore::open(&path);
        if extra.is_some() {
            assert!(matches!(
                opened,
                Err(GraphError::SourceBinding("incompatible association schema"))
            ));
            continue;
        }
        let migrated = opened.unwrap();
        let version: i64 = migrated
            .conn
            .query_row("SELECT version FROM source_binding_meta", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(migrated.fact_counts().unwrap().0, 1);
        assert!(
            migrated
                .source_observations_for_repo("repo/a")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn observed_bindings_publish_beside_primary_ones_and_follow_their_facts() {
    // AC-1047 (ADR-0079): a fact can hold a primary and an observed binding
    // at once; each role is replaced per repo with the publication, checked
    // against the final fact, never taken over from another repo, and
    // dropped when its fact changes.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, ae, b, _) = seed(&mut store);
    let observed_a = observation(&node("a"), "repo/a");
    let snapshot = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                "repo/a",
                &[a.clone(), ae.clone()],
                std::slice::from_ref(&observed_a),
                None,
                SourceOmissions::NONE,
            )
            .unwrap()
    );
    assert_eq!(
        store.source_bindings_for_repo("repo/a").unwrap(),
        vec![a.clone(), ae.clone()]
    );
    assert_eq!(
        store.source_observations_for_repo("repo/a").unwrap(),
        vec![observed_a.clone()]
    );
    assert_eq!(
        store
            .current_source_observation(&FactKey::from_node(&node("a")))
            .unwrap(),
        Some(observed_a.clone())
    );
    // Another repo's fact, a stale digest and a duplicate are refused.
    let current = store.read_snapshot().unwrap();
    let takeover = observation(&node("b"), "repo/a");
    let mut stale = observation(&node("a"), "repo/a");
    stale.emitted_fact_digest = node_digest(&node("changed")).unwrap();
    for observations in [
        vec![observed_a.clone(), observed_a.clone()],
        vec![observation(&node("b"), "repo/b")],
        vec![stale],
    ] {
        assert!(
            store
                .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                    &current,
                    &GraphPatch::default(),
                    "repo/a",
                    &[a.clone(), ae.clone()],
                    &observations,
                    None,
                    SourceOmissions::NONE,
                )
                .is_err()
        );
    }
    // repo/b's observation of b is its own; repo/a cannot take it over.
    let current = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                &current,
                &GraphPatch::default(),
                "repo/b",
                std::slice::from_ref(&b),
                &[observation(&node("b"), "repo/b")],
                None,
                SourceOmissions::NONE,
            )
            .unwrap()
    );
    let current = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                &current,
                &GraphPatch::default(),
                "repo/a",
                &[a.clone(), ae.clone()],
                &[takeover],
                None,
                SourceOmissions::NONE,
            )
            .is_err()
    );
    // A publication without observations removes this repo's only.
    let current = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &current,
                &GraphPatch::default(),
                "repo/a",
                &[a.clone(), ae],
            )
            .unwrap()
    );
    assert!(
        store
            .source_observations_for_repo("repo/a")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.source_observations_for_repo("repo/b").unwrap().len(),
        1
    );
    // A changed fact drops its observed binding, as it does the primary one.
    let mut changed = node("b");
    changed.props["name"] = serde_json::json!("changed");
    store.put_node(&changed).unwrap();
    assert!(
        store
            .source_observations_for_repo("repo/b")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .source_bindings_for_repo("repo/b")
            .unwrap()
            .iter()
            .all(|binding| binding.fact != FactKey::from_node(&node("b")))
    );
}

#[test]
fn an_observation_only_change_advances_the_revision() {
    // AC-1047: observed rows are not generation-watched, so publication bumps
    // the revision itself when they change and the facts do not.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a")).unwrap();
    let before = store.read_revision().unwrap();
    let snapshot = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                "repo/a",
                &[],
                &[observation(&node("a"), "repo/a")],
                None,
                SourceOmissions::NONE,
            )
            .unwrap()
    );
    let after = store.read_revision().unwrap();
    assert!(after.generation > before.generation);
}
