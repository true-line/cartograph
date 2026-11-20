//! AC-0931 (T-0931): the host compiles an area's briefing from the graph
//! and the overlay current for it.
use super::*;
use core_graph::overlay::{BaseRef, OverlayFact, OverlayRecipe, OverlayStore};
use core_graph::{Edge, GraphStore, Node, SnapshotReadLimits};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn subsystem(id: &str, name: &str, file: &str) -> OverlayFact {
    let prov = core_prov::Provenance::new(
        core_prov::Tier::Semantic,
        core_prov::ConfidenceTier::InferredWeak,
        vec![core_prov::EvidenceRef {
            repo: "local/fixture".into(),
            path: "app/alias.py".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "fixed".into(),
        }],
        "t2.knowledge.subsystems",
        id.as_bytes(),
    )
    .unwrap();
    OverlayFact {
        item: OverlayItem::Node(Node {
            id: id.into(),
            label: "Subsystem".into(),
            props: json!({"name": name, "kind": "cluster", "files": 1, "prov": prov}),
        }),
        cites: [BaseRef::Node(file.into())].into(),
    }
}

/// Two files with one symbol each and a call from the first to the second,
/// and an overlay current for them with one area per file.
fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let mut state = crate::registered_source_tests::app_state(dir.path());
    let base = {
        let mut graph = state.graph.lock().unwrap();
        for (id, label) in [
            ("file:alias", "File"),
            ("sym:alias", "Symbol"),
            ("file:mailbox", "File"),
            ("sym:mailbox", "Symbol"),
        ] {
            graph
                .put_node(&Node {
                    id: id.into(),
                    label: label.into(),
                    props: json!({}),
                })
                .unwrap();
        }
        for (src, label, dst) in [
            ("sym:alias", "DEFINED_IN", "file:alias"),
            ("sym:mailbox", "DEFINED_IN", "file:mailbox"),
            ("sym:alias", "CALLS", "sym:mailbox"),
        ] {
            graph
                .put_edge(&Edge {
                    src: src.into(),
                    dst: dst.into(),
                    label: label.into(),
                    props: json!({}),
                })
                .unwrap();
        }
        graph
            .read_snapshot_with_revision_bounded(SnapshotReadLimits::investigation())
            .unwrap()
    };
    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay
        .bind_instance(&base.revision.graph_instance)
        .unwrap();
    overlay
        .publish(
            &base,
            OverlayRecipe {
                recipe: "host-fixture@1".into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            vec![
                subsystem("subsystem:cluster:alias", "Alias", "file:alias"),
                subsystem("subsystem:cluster:mailbox", "Mailbox", "file:mailbox"),
            ],
        )
        .unwrap();
    state.knowledge_overlay = Some(Arc::new(Mutex::new(overlay)));
    (dir, state)
}

#[test]
fn an_areas_briefing_is_compiled_from_the_current_overlay() {
    let (_dir, state) = state();
    let one = compile(&state, Some("subsystem:cluster:alias")).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!((one[0].name.as_str(), one[0].files), ("Alias", 1));
    assert!(one[0].text.starts_with("# Area briefing: Alias\n"));
    assert!(one[0].text.contains("- Calls into: Mailbox.\n"));
    // This graph recovers no capability, so the specification lists none.
    assert!(one[0].sections.is_empty());
    assert!(
        one[0]
            .text
            .contains("The product specification lists nothing under this area.")
    );
    let all = compile(&state, None).unwrap();
    assert_eq!(
        all.iter()
            .map(|view| view.name.as_str())
            .collect::<Vec<_>>(),
        ["Alias", "Mailbox"]
    );
    assert_eq!(all[0], one[0]);
    assert!(all[1].text.contains("- Called from: Alias.\n"));
}

#[test]
fn an_unknown_area_or_an_overlay_that_is_not_current_has_no_briefing() {
    let (_dir, mut state) = state();
    assert_eq!(
        compile(&state, Some("subsystem:cluster:missing")),
        Err("area_not_found".into())
    );
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "file:later".into(),
            label: "File".into(),
            props: json!({}),
        })
        .unwrap();
    assert_eq!(
        compile(&state, Some("subsystem:cluster:alias")),
        Err("area_not_found".into())
    );
    state.knowledge_overlay = None;
    assert_eq!(compile(&state, None), Err("area_not_found".into()));
}
