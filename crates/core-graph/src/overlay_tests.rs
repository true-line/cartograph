use super::identity as identity_of;
use super::*;
use crate::{GraphStore, SnapshotReadLimits, SqliteGraphStore};
use core_prov::{ConfidenceTier, Tier};
use serde_json::json;

fn prov(tier: Tier, confidence: ConfidenceTier) -> serde_json::Value {
    serde_json::to_value(core_prov::Provenance {
        tier,
        confidence_tier: confidence,
        evidence: vec![],
        extractor_id: "knowledge-rules".into(),
        content_hash: core_prov::content_hash(b"fact"),
        method: Default::default(),
        basis: None,
    })
    .unwrap()
}

/// Facts carry semantic provenance; base fixtures share the shape.
fn node(id: &str, label: &str) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({ "name": id, "prov": prov(Tier::Semantic, ConfidenceTier::InferredStrong) }),
    }
}

fn edge(src: &str, dst: &str, label: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: json!({ "prov": prov(Tier::Agentic, ConfidenceTier::InferredWeak) }),
    }
}

fn with_props(mut node: Node, props: serde_json::Value) -> Node {
    node.props = props;
    node
}

/// A base graph with two symbols and a call between them.
fn base() -> SqliteGraphStore {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    graph.put_node(&node("sym:a", "Symbol")).unwrap();
    graph.put_node(&node("sym:b", "Symbol")).unwrap();
    graph.put_edge(&edge("sym:a", "sym:b", "CALLS")).unwrap();
    graph
}

fn snapshot(graph: &SqliteGraphStore) -> RevisionedSnapshot {
    graph
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap()
}

fn store(graph: &SqliteGraphStore) -> OverlayStore {
    let mut store = OverlayStore::open_in_memory().unwrap();
    store.bind_instance(&graph.instance_id().unwrap()).unwrap();
    store
}

fn recipe() -> OverlayRecipe {
    OverlayRecipe {
        recipe: "knowledge-rules@1".into(),
        embedding_namespace: None,
        provider_ran: false,
    }
}

fn cites(refs: &[BaseRef]) -> BTreeSet<BaseRef> {
    refs.iter().cloned().collect()
}

/// A `Capability` citing both symbols and a `REALIZES` edge to one of them,
/// citing the base call.
fn facts() -> Vec<OverlayFact> {
    vec![
        OverlayFact {
            item: OverlayItem::Node(node("cap:checkout", "Capability")),
            cites: cites(&[BaseRef::Node("sym:a".into()), BaseRef::Node("sym:b".into())]),
        },
        OverlayFact {
            item: OverlayItem::Edge(edge("sym:a", "cap:checkout", "REALIZES")),
            cites: cites(&[BaseRef::Edge(
                "sym:a".into(),
                "sym:b".into(),
                "CALLS".into(),
            )]),
        },
    ]
}

fn current(read: OverlayRead) -> (OverlayIdentity, OverlayBasis, Vec<OverlayFact>) {
    match read {
        OverlayRead::Current {
            identity,
            basis,
            facts,
        } => (identity, basis, facts),
        other => panic!("expected a current overlay, got {other:?}"),
    }
}

// AC-0498: a published overlay reads back at its base revision with a
// content identity independent of input order, and the base graph, its
// generation and its facts are untouched (#1440).
#[test]
fn published_overlay_reads_back_without_touching_the_base() {
    let graph = base();
    let before = snapshot(&graph);
    let mut store = store(&graph);
    assert_eq!(store.read(&before.revision).unwrap(), OverlayRead::Absent);

    let identity = store.publish(&before, recipe(), facts()).unwrap();
    let mut reversed = facts();
    reversed.reverse();
    let mut other = self::store(&graph);
    assert_eq!(
        other.publish(&before, recipe(), reversed).unwrap(),
        identity
    );

    let (read_identity, basis, read_facts) = current(store.read(&before.revision).unwrap());
    assert_eq!(read_identity, identity);
    assert_eq!(basis.base, before.revision);
    assert_eq!(basis.recipe, recipe());
    let mut expected = facts();
    expected.reverse(); // key order: edges ("e…") before nodes ("n…")
    assert_eq!(read_facts, expected);
    assert_eq!(snapshot(&graph), before);

    // A different recipe or provider phase is a different overlay.
    let mut with_provider = recipe();
    with_provider.provider_ran = true;
    assert_ne!(
        other.publish(&before, with_provider, facts()).unwrap(),
        identity
    );
}

// AC-0498: once the base changes, the overlay derived from the old revision
// is stale and none of its facts are returned (#1440).
#[test]
fn overlay_from_an_older_base_revision_is_stale() {
    let mut graph = base();
    let before = snapshot(&graph);
    let mut store = store(&graph);
    store.publish(&before, recipe(), facts()).unwrap();
    graph.put_node(&node("sym:c", "Symbol")).unwrap();
    let after = snapshot(&graph);
    assert_ne!(after.revision, before.revision);
    assert_eq!(
        store.read(&after.revision).unwrap(),
        OverlayRead::Stale {
            base: before.revision.clone()
        }
    );
    // Republishing at the new revision makes it current again.
    store.publish(&after, recipe(), facts()).unwrap();
    current(store.read(&after.revision).unwrap());
}

// AC-0498: every admission rule rejects the whole publication and keeps the
// previous overlay exactly (#1440).
#[test]
fn rejected_publications_keep_the_previous_overlay() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let identity = store.publish(&base, recipe(), facts()).unwrap();
    let fact = |item: OverlayItem, refs: &[BaseRef]| OverlayFact {
        item,
        cites: cites(refs),
    };
    let cite_a = [BaseRef::Node("sym:a".into())];
    let cases: Vec<(&str, OverlayRecipe, Vec<OverlayFact>)> = vec![
        (
            "shadows the base",
            recipe(),
            vec![fact(
                OverlayItem::Node(node("sym:a", "Capability")),
                &cite_a,
            )],
        ),
        (
            "edge endpoint",
            recipe(),
            vec![fact(
                OverlayItem::Edge(edge("sym:a", "cap:none", "REALIZES")),
                &cite_a,
            )],
        ),
        (
            "uncited",
            recipe(),
            vec![fact(OverlayItem::Node(node("cap:x", "Capability")), &[])],
        ),
        (
            "missing node citation",
            recipe(),
            vec![fact(
                OverlayItem::Node(node("cap:x", "Capability")),
                &[BaseRef::Node("sym:gone".into())],
            )],
        ),
        (
            "missing edge citation",
            recipe(),
            vec![fact(
                OverlayItem::Node(node("cap:x", "Capability")),
                &[BaseRef::Edge(
                    "sym:b".into(),
                    "sym:a".into(),
                    "CALLS".into(),
                )],
            )],
        ),
        (
            "duplicate",
            recipe(),
            vec![
                fact(OverlayItem::Node(node("cap:x", "Capability")), &cite_a),
                fact(OverlayItem::Node(node("cap:x", "Actor")), &cite_a),
            ],
        ),
        (
            "no provenance",
            recipe(),
            vec![fact(
                OverlayItem::Node(with_props(node("cap:x", "Capability"), json!({}))),
                &cite_a,
            )],
        ),
        (
            "malformed provenance",
            recipe(),
            vec![fact(
                OverlayItem::Node(with_props(
                    node("cap:x", "Capability"),
                    json!({ "prov": { "tier": "Semantic" } }),
                )),
                &cite_a,
            )],
        ),
        (
            "confidence above the tier ceiling",
            recipe(),
            vec![fact(
                OverlayItem::Node(with_props(
                    node("cap:x", "Capability"),
                    json!({ "prov": prov(Tier::Semantic, ConfidenceTier::Confirmed) }),
                )),
                &cite_a,
            )],
        ),
        (
            "base tier",
            recipe(),
            vec![fact(
                OverlayItem::Edge(Edge {
                    props: json!({ "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed) }),
                    ..edge("sym:a", "sym:b", "REALIZES")
                }),
                &cite_a,
            )],
        ),
        (
            "empty recipe",
            OverlayRecipe {
                recipe: String::new(),
                ..recipe()
            },
            facts(),
        ),
        (
            "empty namespace",
            OverlayRecipe {
                embedding_namespace: Some(String::new()),
                ..recipe()
            },
            facts(),
        ),
    ];
    for (name, recipe, facts) in cases {
        assert!(
            matches!(
                store.publish(&base, recipe, facts),
                Err(OverlayError::Rejected(_))
            ),
            "{name}"
        );
        assert_eq!(
            current(store.read(&base.revision).unwrap()).0,
            identity,
            "{name}"
        );
    }

    // A snapshot of another graph instance is refused.
    let foreign = snapshot(&self::base());
    assert!(matches!(
        store.publish(&foreign, recipe(), facts()),
        Err(OverlayError::Rejected(_))
    ));
}

// AC-0498: invalidation and a new graph instance discard the overlay; the
// same instance keeps it (#1440).
#[test]
fn invalidation_and_a_new_instance_discard_the_overlay() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    store.publish(&base, recipe(), facts()).unwrap();
    store.bind_instance(&graph.instance_id().unwrap()).unwrap();
    current(store.read(&base.revision).unwrap());
    store.invalidate().unwrap();
    assert_eq!(store.read(&base.revision).unwrap(), OverlayRead::Absent);

    store.publish(&base, recipe(), facts()).unwrap();
    let other = self::base();
    store.bind_instance(&other.instance_id().unwrap()).unwrap();
    assert_eq!(store.read(&base.revision).unwrap(), OverlayRead::Absent);
    let count: i64 = store
        .connection
        .query_row("SELECT count(*) FROM overlay_facts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

// AC-0498: a reopened overlay reads back; tampered rows or foreign schema
// objects are refused rather than trusted (#1440).
#[test]
fn reopened_overlay_validates_schema_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("knowledge-overlay.sqlite");
    let graph = base();
    let base = snapshot(&graph);
    let identity = {
        let mut store = OverlayStore::open(&path).unwrap();
        store.bind_instance(&graph.instance_id().unwrap()).unwrap();
        store.publish(&base, recipe(), facts()).unwrap()
    };
    let store = OverlayStore::open(&path).unwrap();
    assert_eq!(current(store.read(&base.revision).unwrap()).0, identity);

    // A changed fact no longer reproduces the stored identity.
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET fact = replace(fact, 'Capability', 'Actor')",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.read(&base.revision),
        Err(OverlayError::Schema(_))
    ));
    // A key that disagrees with its fact is refused even when the identity
    // is coherently recomputed over the tampered rows.
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET fact = replace(fact, 'Actor', 'Capability')",
            [],
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET key = replace(key, 'checkout', 'cart')",
            [],
        )
        .unwrap();
    let (basis, rows): (OverlayBasis, Vec<(String, String)>) = {
        let basis: String = store
            .connection
            .query_row("SELECT basis FROM overlay_meta", [], |r| r.get(0))
            .unwrap();
        let rows = store
            .connection
            .prepare("SELECT key, fact FROM overlay_facts ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        (serde_json::from_str(&basis).unwrap(), rows)
    };
    store
        .connection
        .execute(
            "UPDATE overlay_meta SET identity = ?1",
            [identity_of(&basis, &rows).unwrap().0],
        )
        .unwrap();
    assert!(matches!(
        store.read(&base.revision),
        Err(OverlayError::Schema("overlay fact key"))
    ));
    drop(store);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE extra (x INTEGER)")
        .unwrap();
    drop(connection);
    assert!(matches!(
        OverlayStore::open(&path),
        Err(OverlayError::Schema(_))
    ));

    let unversioned = dir.path().join("foreign.sqlite");
    Connection::open(&unversioned)
        .unwrap()
        .execute_batch("CREATE TABLE nodes (id TEXT)")
        .unwrap();
    assert!(matches!(
        OverlayStore::open(&unversioned),
        Err(OverlayError::Schema(_))
    ));
}

/// Recompute the stored identity over the current rows, as a coherent
/// local edit would.
fn reseal(store: &OverlayStore) {
    let basis: String = store
        .connection
        .query_row("SELECT basis FROM overlay_meta", [], |r| r.get(0))
        .unwrap();
    let rows: Vec<(String, String)> = store
        .connection
        .prepare("SELECT key, fact FROM overlay_facts ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let basis: OverlayBasis = serde_json::from_str(&basis).unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_meta SET identity = ?1",
            [identity_of(&basis, &rows).unwrap().0],
        )
        .unwrap();
}

// AC-0498: a read measures stored rows before loading them and refuses an
// overlay past the publication bounds or a fact without valid provenance,
// even when its identity was coherently recomputed (#1440).
#[test]
fn reads_refuse_rows_past_bounds_or_without_provenance() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    store.publish(&base, recipe(), facts()).unwrap();
    let bounds = |store: &OverlayStore, facts: usize, bytes: usize| {
        store.read_within(&base.revision, facts, bytes)
    };
    let fact_bytes: i64 = store
        .connection
        .query_row("SELECT sum(length(fact)) FROM overlay_facts", [], |r| {
            r.get(0)
        })
        .unwrap();
    let fact_bytes = fact_bytes as usize;
    current(bounds(&store, 2, fact_bytes).unwrap());
    for (facts, bytes) in [(1, fact_bytes), (2, fact_bytes - 1)] {
        assert!(matches!(
            bounds(&store, facts, bytes),
            Err(OverlayError::Schema("overlay rows exceed their bounds"))
        ));
    }
    // Keys are bounded too: a padded key fails the measure, not the load.
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET key = key || ?1 WHERE key LIKE 'n%'",
            ["x".repeat(fact_bytes)],
        )
        .unwrap();
    assert!(matches!(
        bounds(&store, 2, fact_bytes),
        Err(OverlayError::Schema("overlay rows exceed their bounds"))
    ));

    store.publish(&base, recipe(), facts()).unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET fact = replace(fact, '\"InferredStrong\"', '\"Confirmed\"')",
            [],
        )
        .unwrap();
    reseal(&store);
    assert!(matches!(
        store.read(&base.revision),
        Err(OverlayError::Schema("overlay fact shape"))
    ));

    store.publish(&base, recipe(), facts()).unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET fact = json_set(fact, '$.cites', json('[]'))",
            [],
        )
        .unwrap();
    reseal(&store);
    assert!(matches!(
        store.read(&base.revision),
        Err(OverlayError::Schema("overlay fact shape"))
    ));

    store.publish(&base, recipe(), facts()).unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_meta SET basis = ?1",
            [" ".repeat(MAX_BASIS_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        store.read(&base.revision),
        Err(OverlayError::Schema("overlay basis bytes"))
    ));
}

// AC-0498: the host's disposable overlay resets a database that fails
// validation instead of refusing startup, and refuses a linked file (#1440).
#[test]
fn disposable_overlay_resets_invalid_state_and_refuses_links() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("knowledge-overlay.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE foreign_rows (x INTEGER); INSERT INTO foreign_rows VALUES (1);",
        )
        .unwrap();
    let graph = base();
    let base = snapshot(&graph);
    let mut store = OverlayStore::open_disposable(dir.path()).unwrap();
    store.bind_instance(&graph.instance_id().unwrap()).unwrap();
    assert_eq!(store.read(&base.revision).unwrap(), OverlayRead::Absent);
    store.publish(&base, recipe(), facts()).unwrap();
    drop(store);
    let store = OverlayStore::open_disposable(dir.path()).unwrap();
    current(store.read(&base.revision).unwrap());
    drop(store);

    #[cfg(unix)]
    {
        let linked = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&path, linked.path().join("knowledge-overlay.db")).unwrap();
        assert!(matches!(
            OverlayStore::open_disposable(linked.path()),
            Err(OverlayError::Rejected(_))
        ));
    }
}

// AC-0498: wired into the graph's reset hook, as the host does, graph clear
// and retained-source invalidation discard the overlay first (#1440).
#[test]
fn graph_reset_hook_discards_the_overlay() {
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let overlay = Arc::new(Mutex::new(
        OverlayStore::open_disposable(dir.path()).unwrap(),
    ));
    let invalidator = Arc::clone(&overlay);
    let mut graph = SqliteGraphStore::open_with_reset_hook(
        dir.path().join("graph.db"),
        Box::new(move || {
            invalidator
                .lock()
                .unwrap()
                .invalidate()
                .map_err(|e| e.to_string())
        }),
    )
    .unwrap();
    graph.put_node(&node("sym:a", "Symbol")).unwrap();
    graph.put_node(&node("sym:b", "Symbol")).unwrap();
    graph.put_edge(&edge("sym:a", "sym:b", "CALLS")).unwrap();
    overlay
        .lock()
        .unwrap()
        .bind_instance(&graph.instance_id().unwrap())
        .unwrap();
    let publish = |graph: &SqliteGraphStore| {
        let base = snapshot(graph);
        let mut overlay = overlay.lock().unwrap();
        overlay.publish(&base, recipe(), facts()).unwrap();
        current(overlay.read(&base.revision).unwrap());
        base
    };
    let base = publish(&graph);
    graph.invalidate_derived_state().unwrap();
    assert_eq!(
        overlay.lock().unwrap().read(&base.revision).unwrap(),
        OverlayRead::Absent
    );

    publish(&graph);
    graph.clear().unwrap();
    let cleared = graph.read_revision().unwrap();
    assert_eq!(
        overlay.lock().unwrap().read(&cleared).unwrap(),
        OverlayRead::Absent
    );
    let count: i64 = overlay
        .lock()
        .unwrap()
        .connection
        .query_row("SELECT count(*) FROM overlay_facts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

// AC-0505: independent old encoding oracle, not the shared streaming helper.
#[test]
fn capability_read_identity_matches_legacy_encoding() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let mut input = facts();
    input[0].cites.insert(BaseRef::Node("sym:a".into()));
    if let OverlayItem::Node(n) = &mut input[0].item {
        n.id = "cap:é\"\\\n".into();
    }
    // Keep edge endpoints valid after changing the node identity.
    if let OverlayItem::Edge(e) = &mut input[1].item {
        e.dst = "cap:é\"\\\n".into();
    }
    let published = store.publish(&base, recipe(), input).unwrap();
    let (identity, basis, selected) = current(
        store
            .read_capabilities_bounded(&base.revision, 1, MAX_BYTES)
            .unwrap(),
    );
    let rows: Vec<(String, String)> = store
        .connection
        .prepare("SELECT key, fact FROM overlay_facts ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut old = Vec::new();
    let basis_json = serde_json::to_string(&basis).unwrap();
    for bytes in std::iter::once(b"cartograph-knowledge-overlay@1".as_slice())
        .chain(std::iter::once(basis_json.as_bytes()))
        .chain(
            rows.iter()
                .flat_map(|(key, json)| [key.as_bytes(), json.as_bytes()]),
        )
    {
        old.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        old.extend_from_slice(bytes);
    }
    assert_eq!(identity.0, core_prov::content_hash(&old));
    assert_eq!(identity, published);
    assert_eq!(current(store.read(&base.revision).unwrap()).0, identity);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].cites.len(), 2);
    assert_eq!(snapshot(&graph).graph, base.graph);
    assert_eq!(graph.read_revision().unwrap(), base.revision);
}

// AC-0505: exact retained logical JSON bytes/count; limits never return partial facts.
#[test]
fn capability_read_selected_boundaries_and_state_parity() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let read =
        |s: &OverlayStore, count, bytes| s.read_capabilities_bounded(&base.revision, count, bytes);
    assert_eq!(read(&store, 0, 0).unwrap(), OverlayRead::Absent);
    let input = facts();
    let bytes = serde_json::to_string(&input[0]).unwrap().len();
    store.publish(&base, recipe(), input.clone()).unwrap();
    assert_eq!(
        current(read(&store, 1, bytes).unwrap()).2,
        vec![input[0].clone()]
    );
    for (count, cap) in [(0, bytes), (1, bytes - 1)] {
        assert!(matches!(
            read(&store, count, cap),
            Err(OverlayError::SelectionLimit)
        ));
    }
    let mut two = input.clone();
    let mut second = input[0].clone();
    if let OverlayItem::Node(n) = &mut second.item {
        n.id = "cap:second".into();
    }
    let second_bytes = serde_json::to_string(&second).unwrap().len();
    two.push(second);
    store.publish(&base, recipe(), two).unwrap();
    assert_eq!(
        current(read(&store, 2, bytes + second_bytes).unwrap())
            .2
            .len(),
        2
    );
    assert!(matches!(
        read(&store, 1, MAX_BYTES),
        Err(OverlayError::SelectionLimit)
    ));
    assert!(matches!(
        read(&store, 2, bytes + second_bytes - 1),
        Err(OverlayError::SelectionLimit)
    ));
    store
        .publish(
            &base,
            recipe(),
            vec![
                input[1].clone(),
                OverlayFact {
                    item: OverlayItem::Node(node("cap:checkout", "Actor")),
                    cites: input[0].cites.clone(),
                },
            ],
        )
        .unwrap();
    assert!(current(read(&store, 0, 0).unwrap()).2.is_empty());
    let mut other = base.revision.clone();
    other.graph_instance = "different-instance".into();
    assert_eq!(
        store.read_capabilities_bounded(&other, 0, 0).unwrap(),
        store.read(&other).unwrap()
    );
    assert_eq!(snapshot(&graph).graph, base.graph);
    assert_eq!(graph.read_revision().unwrap(), base.revision);
}

// AC-0505: all rows are identity/shape checked, even after selection overflow.
#[test]
fn capability_read_rejects_unselected_corruption_before_limit() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let mut input = facts();
    input.push(OverlayFact {
        item: OverlayItem::Node(node("zz:actor", "Actor")),
        cites: input[0].cites.clone(),
    });
    for (assignment, resealed) in [
        ("fact = replace(fact, 'Actor', 'Other')", false),
        ("key = key || 'wrong'", true),
        ("fact = json_set(fact, '$.cites', json('[]'))", true),
        ("fact = json_remove(fact, '$.item.Node.props.prov')", true),
        (
            "fact = json_set(fact, '$.item.Node.props.prov.confidence_tier', 'Confirmed')",
            true,
        ),
        ("fact = '{'", true),
    ] {
        store.publish(&base, recipe(), input.clone()).unwrap();
        store
            .connection
            .execute(
                &format!("UPDATE overlay_facts SET {assignment} WHERE fact LIKE '%zz:actor%'"),
                [],
            )
            .unwrap();
        if resealed {
            reseal(&store);
        }
        for count in [0, 1] {
            let result = store.read_capabilities_bounded(&base.revision, count, MAX_BYTES);
            assert!(
                matches!(
                    result,
                    Err(OverlayError::Schema(_) | OverlayError::Encoding(_))
                ),
                "{assignment}: {result:?}"
            );
        }
    }
}

// AC-0505: bound SQLite-owned row bytes before decoding or copying them.
#[test]
fn capability_read_refuses_oversized_single_row_before_decode() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    store.publish(&base, recipe(), facts()).unwrap();
    store
        .connection
        .execute(
            "UPDATE overlay_facts SET key = ?1 WHERE key LIKE 'e%'",
            ["x".repeat(MAX_FACT_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        store.read_capabilities_bounded(&base.revision, 0, 0),
        Err(OverlayError::Schema("overlay row bytes"))
    ));
}

#[test]
fn capability_workspace_covers_real_selection_large_unselected_row_and_metadata() {
    use std::mem::size_of;
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let mut input = facts();
    let mut actor = node("zz:actor", "Actor");
    actor.props["padding"] = json!("x".repeat(MAX_FACT_BYTES / 2));
    actor.props["nested"] = json!([{"value":"é\n"}]);
    input.push(OverlayFact {
        item: OverlayItem::Node(actor),
        cites: input[0].cites.clone(),
    });
    let selected_bytes = serde_json::to_string(&input[0]).unwrap().len();
    let bound = capability_read_workspace(1, selected_bytes).unwrap();
    store.publish(&base, recipe(), input.clone()).unwrap();
    let (identity, basis, selected) = current(
        store
            .read_capabilities_bounded(&base.revision, 1, selected_bytes)
            .unwrap(),
    );
    assert_eq!(selected, vec![input[0].clone()]);
    let citation_slots = selected[0].cites.len() * (size_of::<BaseRef>() + 2 * size_of::<usize>());
    assert!(
        bound.retained_selection_bytes
            >= selected_bytes + size_of::<OverlayFact>() + citation_slots
    );
    // Independently decode the actual unrelated row, whose size is not selected_bytes.
    let raw = serde_json::to_string(&input[2]).unwrap();
    let decoded: OverlayFact = serde_json::from_str(&raw).unwrap();
    let OverlayItem::Node(node) = &decoded.item else {
        unreachable!()
    };
    let p = core_prov::Provenance::deserialize(&node.props["prov"]).unwrap();
    let known_row = raw.len()
        + decoded.key().len()
        + size_of::<OverlayFact>()
        + serde_json::to_string(&p).unwrap().len()
        + citation_slots;
    assert!(raw.len() > selected_bytes);
    assert!(bound.row_scratch_bytes >= known_row);
    let encoded = serde_json::to_string(&basis).unwrap();
    let payload = basis.base.graph_instance.len()
        + basis.recipe.recipe.len()
        + basis
            .recipe
            .embedding_namespace
            .as_ref()
            .map_or(0, String::len);
    assert!(bound.retained_metadata_bytes >= payload + identity.0.len() + size_of::<OverlayRead>());
    assert!(bound.metadata_peak_bytes >= encoded.len() * 2 + payload + size_of::<IdentityHash>());
    assert!(matches!(
        store.read_capabilities_bounded(&base.revision, 0, 0),
        Err(OverlayError::SelectionLimit)
    ));
    assert_eq!(graph.read_revision().unwrap(), base.revision);
}

#[test]
fn capability_workspace_admits_stale_revision_strings_up_to_the_stored_basis_cap() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    store.publish(&base, recipe(), facts()).unwrap();
    let (_, mut basis, _) = current(store.read(&base.revision).unwrap());
    // The reader deserializes stale metadata without imposing publication names
    // or graph-instance shape. Its returned string can be much longer than 32.
    basis.base.graph_instance = "x".repeat(MAX_BASIS_BYTES / 2);
    basis.recipe.embedding_namespace = Some("\"\\\n".repeat(512));
    let raw = serde_json::to_string(&basis).unwrap();
    assert!(raw.len() <= MAX_BASIS_BYTES);
    store
        .connection
        .execute("UPDATE overlay_meta SET basis = ?1", [&raw])
        .unwrap();
    let read = store
        .read_capabilities_bounded(&base.revision, 0, 0)
        .unwrap();
    let OverlayRead::Stale { base: stale } = &read else {
        panic!("stale metadata")
    };
    assert_eq!(stale, &basis.base);
    let bound = capability_read_workspace(0, 0).unwrap();
    assert!(
        bound.retained_metadata_bytes >= stale.graph_instance.len() + std::mem::size_of_val(&read)
    );
    let canonical = serde_json::to_string(&basis).unwrap();
    assert!(bound.metadata_peak_bytes >= raw.len() + canonical.len() + stale.graph_instance.len());
}

fn with_actor_and_other() -> Vec<OverlayFact> {
    let mut input = facts();
    for (id, label) in [("actor:owner", "Actor"), ("zz:other", "Other")] {
        input.push(OverlayFact {
            item: OverlayItem::Node(node(id, label)),
            cites: input[0].cites.clone(),
        });
    }
    input
}

// AC-0533: Capability and Actor nodes share one count and byte charge, in key
// order; limits never return partial facts and the Capability read is unchanged.
#[test]
fn combined_read_charges_capabilities_and_actors_together() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let read = |s: &OverlayStore, count, bytes| {
        s.read_capabilities_and_actors_bounded(&base.revision, count, bytes)
    };
    assert_eq!(read(&store, 0, 0).unwrap(), OverlayRead::Absent);
    let input = with_actor_and_other();
    store.publish(&base, recipe(), input.clone()).unwrap();
    let mut expected = vec![input[0].clone(), input[2].clone()];
    expected.sort_by_key(|a| a.key());
    let bytes: usize = expected
        .iter()
        .map(|f| serde_json::to_string(f).unwrap().len())
        .sum();
    let (identity, basis, selected) = current(read(&store, 2, bytes).unwrap());
    assert_eq!(selected, expected);
    for (count, cap) in [(1, MAX_BYTES), (2, bytes - 1), (0, 0)] {
        assert!(matches!(
            read(&store, count, cap),
            Err(OverlayError::SelectionLimit)
        ));
    }
    let capability_bytes = serde_json::to_string(&input[0]).unwrap().len();
    assert_eq!(
        store
            .read_capabilities_bounded(&base.revision, 1, capability_bytes)
            .unwrap(),
        OverlayRead::Current {
            identity,
            basis,
            facts: vec![input[0].clone()],
        }
    );
    let mut other = base.revision.clone();
    other.graph_instance = "different-instance".into();
    assert_eq!(
        store
            .read_capabilities_and_actors_bounded(&other, 0, 0)
            .unwrap(),
        store.read(&other).unwrap()
    );
    assert_eq!(snapshot(&graph).graph, base.graph);
    assert_eq!(graph.read_revision().unwrap(), base.revision);
}

// AC-0533: corruption in any row wins over a selection overflow, as for the
// Capability read.
#[test]
fn combined_read_rejects_corruption_after_overflow() {
    let graph = base();
    let base = snapshot(&graph);
    let mut store = store(&graph);
    let input = with_actor_and_other();
    for target in ["actor:owner", "zz:other"] {
        for (assignment, resealed) in [
            ("key = key || 'wrong'", true),
            ("fact = json_set(fact, '$.cites', json('[]'))", true),
            ("fact = json_remove(fact, '$.item.Node.props.prov')", true),
            ("fact = '{'", true),
            ("fact = replace(fact, 'name', 'nome')", false),
        ] {
            store.publish(&base, recipe(), input.clone()).unwrap();
            store
                .connection
                .execute(
                    &format!("UPDATE overlay_facts SET {assignment} WHERE fact LIKE '%{target}%'"),
                    [],
                )
                .unwrap();
            if resealed {
                reseal(&store);
            }
            for count in [0, 1, 2] {
                let result =
                    store.read_capabilities_and_actors_bounded(&base.revision, count, MAX_BYTES);
                assert!(
                    matches!(
                        result,
                        Err(OverlayError::Schema(_) | OverlayError::Encoding(_))
                    ),
                    "{target} {assignment}: {result:?}"
                );
            }
        }
    }
}
