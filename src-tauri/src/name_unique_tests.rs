//! T0.5 exact-name fills as ingest publishes them (#257, AC-0270).
use crate::{OperationFacts, load_into_graph, stitch_name_unique, summarize_register};
use core_graph::{GraphStore, SqliteGraphStore};

const ID: adapters_lang_ts::SourceId<'static> = adapters_lang_ts::SourceId {
    repo: "local",
    commit: "workdir",
};

fn load(store: &mut SqliteGraphStore, dir: &std::path::Path) {
    let extraction = adapters_lang_ts::extract_dir(dir, &ID).unwrap();
    load_into_graph(store, &extraction, "local", dir, "workdir").unwrap();
}

fn fills(store: &SqliteGraphStore) -> Vec<(String, String)> {
    store
        .edges_with_labels(&["CALLS"])
        .unwrap()
        .into_iter()
        .filter(|edge| edge.props["prov"]["extractor_id"] == semantic::name_unique::EXTRACTOR_ID)
        .map(|edge| (edge.src, edge.dst))
        .collect()
}

#[test]
fn name_unique_fills_are_recomputed_on_every_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    // `./index` re-exports, so the import proof finds no `isObservable` in
    // index.ts and the call stays a Gap; the name is unique tree-wide.
    std::fs::write(
        dir.path().join("src/main.ts"),
        "import { isObservable } from './index';\nexport function main(x) {\n  return isObservable(x);\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/index.ts"), "export * from './obs';\n").unwrap();
    std::fs::write(
        dir.path().join("src/obs.ts"),
        "export function isObservable(x) {\n  return !!x;\n}\n",
    )
    .unwrap();
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    load(&mut store, dir.path());
    let (nodes, edges) = store.read_snapshot().unwrap();
    let before = summarize_register(&nodes, &edges, 0, 0);
    assert_eq!(semantic::graph_inputs(&nodes, &edges).0.len(), 1);

    let plan = stitch_name_unique(&mut store, &mut OperationFacts::default()).unwrap();
    assert_eq!(plan.filled, 1);
    assert_eq!(
        fills(&store),
        [(
            "sym:local@src/main.ts#main".to_string(),
            "sym:local@src/obs.ts#isObservable".to_string()
        )]
    );
    let (nodes, edges) = store.read_snapshot().unwrap();
    let filled = summarize_register(&nodes, &edges, 0, 0);
    // The Gap stays open in the register; only T2's workload shrinks.
    assert_eq!(filled.gaps, before.gaps);
    assert_eq!(filled.gaps_with_inferred_fill, 1);
    assert!(semantic::graph_inputs(&nodes, &edges).0.is_empty());

    // Re-stitching the same graph changes nothing.
    stitch_name_unique(&mut store, &mut OperationFacts::default()).unwrap();
    assert_eq!(store.read_snapshot().unwrap(), (nodes, edges));

    // A second same-name Symbol arrives: the next load retracts the fill
    // and the hop is T2 work again.
    std::fs::write(
        dir.path().join("src/legacy.ts"),
        "export function isObservable(y) {\n  return y !== undefined;\n}\n",
    )
    .unwrap();
    load(&mut store, dir.path());
    let plan = stitch_name_unique(&mut store, &mut OperationFacts::default()).unwrap();
    assert_eq!(plan.filled, 0);
    assert!(fills(&store).is_empty());
    let (nodes, edges) = store.read_snapshot().unwrap();
    assert_eq!(
        summarize_register(&nodes, &edges, 0, 0).gaps_with_inferred_fill,
        0
    );
    assert_eq!(semantic::graph_inputs(&nodes, &edges).0.len(), 1);
}
