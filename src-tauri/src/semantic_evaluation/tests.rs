use super::*;
use serde_json::json;

fn captured_graph() -> (
    tempfile::TempDir,
    AppState,
    Vec<Node>,
    Vec<Edge>,
    CorpusSelector,
) {
    captured_graph_with_source(
        "import { processOrder } from './missing';\nexport function route(value: string) { return processOrder(value); }\nexport function target(value: string) { return value; }\n",
    )
}
fn captured_graph_with_source(
    text: &str,
) -> (
    tempfile::TempDir,
    AppState,
    Vec<Node>,
    Vec<Edge>,
    CorpusSelector,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("calls.ts"), text).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Host fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "calls.ts"]);
    git(&["commit", "-qm", "synthetic host source"]);
    let revision = git(&["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(revision.len(), 40);
    assert!(git(&["status", "--porcelain"]).is_empty());
    assert_eq!(git(&["show", "HEAD:calls.ts"]), text);
    let state = crate::registered_source_tests::app_state(dir.path());
    let registered = state.sources.lock().unwrap().register_local(&root).unwrap();
    let input = state
        .primary_sources
        .prepare(&registered, &root, &[])
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    assert_eq!(capture.file("calls.ts").unwrap().bytes(), text.as_bytes());
    assert!(git(&["status", "--porcelain"]).is_empty());
    let (extraction, receipts, _) = adapters_lang_ts::captured::extract_captured_dir(
        &root,
        &adapters_lang_ts::SourceId {
            repo: &registered.repo_key,
            commit: &revision,
        },
        capture,
        crate::Registries::builtin(),
        &mut |_| {},
    )
    .unwrap();
    state
        .primary_sources
        .persist(&input, &registered, &receipts)
        .unwrap();
    let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
    crate::load_into_graph_with_bindings(
        &mut state.graph.lock().unwrap(),
        &extraction,
        &registered.repo_key,
        &root,
        &revision,
        &bindings,
        None,
    )
    .unwrap();
    let capture_id = capture.id().to_owned();
    drop(input);
    let graph = state.graph.lock().unwrap();
    let mut labels = flowtracer::FLOW_NODE_LABELS.to_vec();
    labels.push("Resource");
    let (nodes, edges) =
        crate::read_filtered_graph(&*graph, &labels, flowtracer::FLOW_EDGE_LABELS).unwrap();
    drop(graph);
    let selector = CorpusSelector {
        corpus_id: "actual-captured-corpus".into(),
        repo_id: registered.repo_key,
        revision,
        capture_id,
    };
    (dir, state, nodes, edges, selector)
}
// AC-0274/0277: ordinary captured extraction anchors an unresolved call on
// the actual capture, not on per-fact receipts. (The Gap and File nodes carry
// the producer's own receipts since AC-1019, and the Gap's CALLS edge its
// own since AC-1096; observation never invents one.)
#[test]
fn host_corpus_observation_uses_actual_capture_without_per_fact_receipts() {
    let (_dir, state, nodes, edges, selector) = captured_graph();
    let graph = state.graph.lock().unwrap();
    let (hops, _) = semantic::graph_inputs(&nodes, &edges);
    assert!(!hops.is_empty());
    let call = edges
        .iter()
        .find(|edge| edge.dst == hops[0].gap_id)
        .unwrap();
    assert!(
        graph
            .current_source_binding(&core_graph::source::FactKey::from_edge(call))
            .unwrap()
            .is_some()
    );
    let observed = source::observe(
        &state,
        &graph,
        &nodes,
        &edges,
        std::slice::from_ref(&selector),
    )
    .unwrap();
    assert_eq!(observed.observed.len(), 1);
    assert_eq!(observed.observed[0].graph_snapshot_hash.len(), 64);
    let mut changed = nodes.clone();
    changed[0].props["mutation"] = json!(true);
    let different = source::observe(
        &state,
        &graph,
        &changed,
        &edges,
        std::slice::from_ref(&selector),
    )
    .unwrap();
    assert_ne!(
        observed.observed[0].graph_snapshot_hash,
        different.observed[0].graph_snapshot_hash
    );
    let mut wrong = selector.clone();
    wrong.revision = "other-revision".into();
    assert!(source::observe(&state, &graph, &nodes, &edges, &[wrong]).is_err());
    let mut wrong = selector;
    wrong.capture_id = "capture-v1:unavailable".into();
    assert!(source::observe(&state, &graph, &nodes, &edges, &[wrong]).is_err());
    assert!(source::observe(&state, &graph, &nodes, &edges, &[]).is_err());
}
// AC-0271/0274/0277: query bytes and the complete real family candidate
// catalogue must agree, even if a dataset is internally self-consistent.
#[test]
fn host_dataset_case_rejects_forged_query_candidate_text_and_subset() {
    let (_dir, _state, nodes, edges, _) = captured_graph();
    let (hops, candidates) = semantic::graph_inputs(&nodes, &edges);
    let hop = &hops[0];
    let case:semantic::evaluation::fixtures::CaseInput=serde_json::from_value(json!({
        "case_id":"actual-case","corpus_id":"actual-captured-corpus","family":hop.family(),
        "hop":{"gap_id":hop.gap_id,"source_id":hop.source_id,"edge_label":hop.edge_label,"query":hop.query,"query_hash":sha256(hop.query.as_bytes()),"evidence":hop.evidence},
        "source_ids":["actual-source"],"candidate_set_hash":"a".repeat(64),
        "candidates":candidates.iter().filter(|c|c.family()==hop.family()).map(|c|json!({"node_id":c.node_id,"family":c.family(),"text":c.text,"text_hash":sha256(c.text.as_bytes()),"evidence":c.evidence,"source_ids":["actual-source"]})).collect::<Vec<_>>()
    })).unwrap();
    assert!(!case.candidates.is_empty());
    source::validate_case(&case, &hops, &candidates).unwrap();
    let mut wrong = case.clone();
    wrong.hop.query.push_str(" fabricated");
    assert!(source::validate_case(&wrong, &hops, &candidates).is_err());
    let mut wrong = case.clone();
    wrong.candidates.pop();
    assert!(source::validate_case(&wrong, &hops, &candidates).is_err());
    let mut wrong = case;
    wrong.candidates[0].text.push_str(" fabricated");
    assert!(source::validate_case(&wrong, &hops, &candidates).is_err());
}
// AC-0277: absent configuration is honest absence; configuration I/O never
// follows linked files/directories or silently accepts a changed review anchor.
#[test]
fn host_evaluation_store_rejects_links_nonregular_files_and_config_changes() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Store::open(dir.path()).unwrap().is_none());
    let root = dir.path().join("semantic-evaluation");
    std::fs::create_dir_all(root.join("dataset")).unwrap();
    for name in ["review-anchor.json", "policy.json"] {
        std::fs::write(root.join(name), "{}").unwrap();
    }
    let store = Store::open(dir.path()).unwrap().unwrap();
    store.verify().unwrap();
    std::fs::write(root.join("review-anchor.json"), "{\"changed\":true}").unwrap();
    assert!(store.verify().is_err());
    std::fs::remove_file(root.join("policy.json")).unwrap();
    std::fs::create_dir(root.join("policy.json")).unwrap();
    assert!(Store::open(dir.path()).is_err());
    #[cfg(unix)]
    {
        std::fs::remove_dir(root.join("policy.json")).unwrap();
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("policy.json")).unwrap();
        assert!(Store::open(dir.path()).is_err());
        std::fs::remove_file(root.join("policy.json")).unwrap();
        std::fs::hard_link(&outside, root.join("policy.json")).unwrap();
        assert!(Store::open(dir.path()).is_err());
    }
}

mod positive;
