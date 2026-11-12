//! Manual deterministic showcase run (#721): recover local checkouts through the
//! production ingest path and write the official spec bundle plus the recovered
//! endpoint inventory. No T2 embeddings, no provider calls, no network.
use crate::*;
use std::io::Write;
use std::path::{Path, PathBuf};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

/// An isolated profile under the output directory, so a run never touches the
/// user's app data and every database it produced stays inspectable.
fn showcase_app(profile: &Path) -> tauri::App<tauri::test::MockRuntime> {
    let dirs = ["data", "config", "local-data", "cache", "logs"].map(|name| profile.join(name));
    for dir in &dirs {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = "com.qwtm.cartograph.showcase".into();
    context.config_mut().app.app_directories_override = Some(
        serde_json::from_value(serde_json::json!({
            "data": dirs[0], "config": dirs[1], "localData": dirs[2], "cache": dirs[3], "log": dirs[4],
        }))
        .unwrap(),
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    assert_eq!(app.path().app_data_dir().unwrap(), dirs[0]);
    app.manage(crate::registered_source_tests::app_state(&dirs[0]));
    app.manage(crate::PreflightRuns::default());
    app
}

#[test]
#[ignore = "manual showcase run over local checkouts; no provider calls"]
fn showcase_recover_and_export() {
    let output = PathBuf::from(required("CARTOGRAPH_SHOWCASE_OUTPUT"));
    assert!(output.is_absolute(), "output must be absolute");
    std::fs::create_dir(&output).expect("output directory must be new");
    let app = showcase_app(&output.join("profile"));
    let state = app.state::<AppState>();

    // A system manifest ingests every repo it declares through the
    // production `add_system` path, so cross-repo binding runs (#737).
    let summaries = if let Ok(manifest) = std::env::var("CARTOGRAPH_SHOWCASE_MANIFEST") {
        let summary = add_system_blocking(manifest.clone(), app.handle().clone())
            .unwrap_or_else(|error| panic!("system {manifest} failed: {error}"));
        vec![serde_json::json!({ "manifest": manifest, "summary": summary })]
    } else {
        let sources: Vec<PathBuf> = required("CARTOGRAPH_SHOWCASE_SOURCES")
            .split(':')
            .map(PathBuf::from)
            .collect();
        let mut summaries = Vec::new();
        for source in &sources {
            let registered = register_local_source(&state, source).unwrap();
            let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
            let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
            let summary = run_ingest(&registered, &operation, &execution, app.handle(), &state)
                .unwrap_or_else(|error| panic!("ingest {} failed: {error}", source.display()));
            summaries.push(serde_json::json!({
                "source": source, "repo": registered.repo_key, "summary": summary,
            }));
        }
        summaries
    };
    std::fs::write(
        output.join("ingest.json"),
        serde_json::to_vec_pretty(&summaries).unwrap(),
    )
    .unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    for (mode, dir) in [
        (spec::ExportMode::VerifiedOnly, "verified-only"),
        (spec::ExportMode::BestEffort, "best-effort"),
    ] {
        let bundle = build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let root = output.join(dir);
        std::fs::create_dir(&root).unwrap();
        for artifact in &bundle.artifacts {
            std::fs::write(root.join(&artifact.file_name), &artifact.content).unwrap();
        }
        // #1582: keep the complete diagnostic bundle, but avoid whitespace
        // expansion and a second bundle-sized serialization buffer.
        let mut writer =
            std::io::BufWriter::new(std::fs::File::create(root.join("bundle.json")).unwrap());
        serde_json::to_writer(&mut writer, &bundle).unwrap();
        writer.flush().unwrap();
    }

    let (nodes, edges) = graph.read_snapshot().unwrap();
    // Client calls and where they landed: an endpoint, or a Gap with its reason.
    let reasons: std::collections::BTreeMap<&str, &serde_json::Value> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| (node.id.as_str(), &node.props["reason"]))
        .collect();
    let fetches: Vec<_> = edges
        .iter()
        .filter(|edge| edge.label == "FETCHES")
        .map(|edge| {
            serde_json::json!({
                "src": edge.src,
                "dst": edge.dst,
                "method": edge.props["method"],
                "entry": edge.props["join"]["entry"],
                "binding": edge.props["binding"],
                "reason": reasons.get(edge.dst.as_str()),
            })
        })
        .collect();
    std::fs::write(
        output.join("fetches.json"),
        serde_json::to_vec_pretty(&fetches).unwrap(),
    )
    .unwrap();
    let endpoints: Vec<_> = nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| serde_json::json!({ "id": node.id, "props": node.props }))
        .collect();
    std::fs::write(
        output.join("endpoints.json"),
        serde_json::to_vec_pretty(&endpoints).unwrap(),
    )
    .unwrap();
    let mut labels = std::collections::BTreeMap::<&str, usize>::new();
    for node in &nodes {
        *labels.entry(node.label.as_str()).or_default() += 1;
    }
    std::fs::write(
        output.join("labels.json"),
        serde_json::to_vec_pretty(&labels).unwrap(),
    )
    .unwrap();
}
