//! Manual deterministic showcase run (#721): recover local checkouts through the
//! production ingest path, run the knowledge phase's deterministic producers as
//! the desktop does after an ingest (#1443), and write the official spec bundle
//! through the desktop export path plus the recovered endpoint inventory and
//! the Subsystem candidates the phase published (#260) with the briefing of
//! each (#2055). No T2
//! embeddings, no provider calls, no network. A second, separate run asks
//! the topic experts fixed questions and does call a provider (#2057).
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
    let mut state = crate::registered_source_tests::app_state(&dirs[0]);
    // T2 off, so the post-ingest background phase the stores below enable
    // never probes a provider: no network and no embeddings, as this run
    // promises, whether or not a local model is installed.
    state
        .settings
        .lock()
        .unwrap()
        .set_enabled("T2", false)
        .unwrap();
    // The knowledge overlay and embedding cache the desktop opens at startup,
    // bound to this profile's graph, so the knowledge phase can publish.
    let instance = state.graph.lock().unwrap().instance_id().unwrap();
    let cache = semantic::embedding_cache::EmbeddingCache::open_disposable(&dirs[0]).unwrap();
    let mut overlay = core_graph::overlay::OverlayStore::open_disposable(&dirs[0]).unwrap();
    overlay.bind_instance(&instance).unwrap();
    state.embedding_cache = Some(std::sync::Arc::new(std::sync::Mutex::new(cache)));
    state.knowledge_overlay = Some(std::sync::Arc::new(std::sync::Mutex::new(overlay)));
    app.manage(state);
    app.manage(crate::PreflightRuns::default());
    app
}

/// The Subsystem candidates the knowledge phase published (#260), as JSON
/// and as a list a reader can judge: each group's name, its terms and the
/// files it holds, largest first.
fn write_subsystems(state: &AppState, output: &Path) {
    use core_graph::overlay::{BaseRef, OverlayItem, OverlayRead};
    let revision = state.graph.lock().unwrap().read_revision().unwrap();
    let overlay = state.knowledge_overlay.as_ref().unwrap().lock().unwrap();
    let OverlayRead::Current { facts, .. } = overlay.read(&revision).unwrap() else {
        panic!("the knowledge phase published no overlay for this base");
    };
    let mut groups: Vec<_> = facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "Subsystem" => {
                let files: Vec<&str> = fact
                    .cites
                    .iter()
                    .filter_map(|cite| match cite {
                        BaseRef::Node(id) => id.split_once('@').map(|(_, path)| path),
                        BaseRef::Edge(..) => None,
                    })
                    .collect();
                Some((node, files))
            }
            _ => None,
        })
        .collect();
    groups.sort_by(|a, b| {
        (a.0.props["kind"] == "shared")
            .cmp(&(b.0.props["kind"] == "shared"))
            .then_with(|| b.1.len().cmp(&a.1.len()))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    let json: Vec<_> = groups
        .iter()
        .map(|(node, files)| {
            serde_json::json!({
                "id": node.id, "name": node.props["name"], "kind": node.props["kind"],
                "terms": node.props["terms"], "files": files,
            })
        })
        .collect();
    std::fs::write(
        output.join("subsystems.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
    let mut text = String::from("# Subsystems\n");
    for (node, files) in &groups {
        let terms: Vec<&str> = node.props["terms"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|term| term.as_str())
            .collect();
        text.push_str(&format!(
            "\n## {} ({} files)\n",
            node.props["name"].as_str().unwrap_or_default(),
            files.len()
        ));
        if !terms.is_empty() {
            text.push_str(&format!("\nTerms: {}\n", terms.join(", ")));
        }
        text.push('\n');
        for file in files {
            text.push_str(&format!("- `{file}`\n"));
        }
    }
    std::fs::write(output.join("subsystems.md"), text).unwrap();
}

/// The briefing a topic expert would start from for each area (#2055), one
/// file per area under `briefings/`, and their sizes in `briefings.json`.
fn write_briefings(state: &AppState, output: &Path) {
    let briefings = crate::area_briefing::compile(state, None).unwrap_or_default();
    let dir = output.join("briefings");
    std::fs::create_dir(&dir).unwrap();
    let mut index = Vec::new();
    for (at, view) in briefings.iter().enumerate() {
        let slug: String = view
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let file = format!("{at:02}-{slug}.md");
        std::fs::write(dir.join(&file), &view.text).unwrap();
        index.push(serde_json::json!({
            "area": view.area, "name": view.name, "files": view.files, "file": file,
            "bytes": view.text.len(), "sections": view.sections, "left_out": view.left_out,
        }));
    }
    std::fs::write(
        output.join("briefings.json"),
        serde_json::to_vec_pretty(&index).unwrap(),
    )
    .unwrap();
}

/// A new output directory and the isolated app that writes into it.
fn showcase_output() -> (PathBuf, tauri::App<tauri::test::MockRuntime>) {
    let output = PathBuf::from(required("CARTOGRAPH_SHOWCASE_OUTPUT"));
    assert!(output.is_absolute(), "output must be absolute");
    std::fs::create_dir(&output).expect("output directory must be new");
    let app = showcase_app(&output.join("profile"));
    (output, app)
}

/// Ingest the sources and run the knowledge phase's deterministic producers.
fn recover(app: &tauri::App<tauri::test::MockRuntime>, output: &Path) {
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

    // The knowledge phase the desktop starts after every ingest, run inline
    // with T2 off: the deterministic producers publish Capabilities, Actors,
    // BusinessFlows, DomainEntities and Subsystems into the overlay; nothing
    // is embedded.
    let knowledge = crate::knowledge_phase::run_for_setting(
        &state.graph,
        state.embedding_cache.as_ref().unwrap(),
        state.knowledge_overlay.as_ref().unwrap(),
        false,
        || None,
    )
    .unwrap();
    std::fs::write(
        output.join("knowledge.json"),
        serde_json::to_vec_pretty(&knowledge).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "manual showcase run over local checkouts; no provider calls"]
fn showcase_recover_and_export() {
    let (output, app) = showcase_output();
    recover(&app, &output);
    let state = app.state::<AppState>();

    write_subsystems(&state, &output);
    write_briefings(&state, &output);

    for (mode, dir) in [
        (spec::ExportMode::VerifiedOnly, "verified-only"),
        (spec::ExportMode::BestEffort, "best-effort"),
    ] {
        // The desktop export path: graph and decisions under their locks, then
        // the overlay current for that revision (#1443).
        let inputs = crate::read_spec_locked(&state)
            .unwrap()
            .inputs(Some(&crate::curated::CuratedReview::default()));
        let bundle = crate::compile_spec_bundle(inputs, mode).unwrap();
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
        // #2049: the structured views the Workbench draws, for screenshots
        // of the app's diagrams from the same run.
        let preview = crate::export_spec_with_views_blocking(mode, &state).unwrap();
        std::fs::write(
            root.join("views.json"),
            serde_json::to_vec(&preview.views).unwrap(),
        )
        .unwrap();
    }

    let graph = state.graph.lock().unwrap();
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

/// One fixed question for the topic expert of the area named `area`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaQuestion {
    area: String,
    question: String,
}

/// Manual topic expert run (#2057): recover the checkouts as the showcase
/// does, then ask `topic-expert@1` each question of the JSON file
/// `CARTOGRAPH_SHOWCASE_QUESTIONS` (`[{"area": "<name>", "question": ...}]`)
/// through the production ask path, and write every answer as it is, with
/// the usage the provider reported, to `answers.json` and `answers.md`.
///
/// This run calls a provider: `CARTOGRAPH_SHOWCASE_ASK_MODE` is `cloud`
/// (default; needs the Anthropic key) or `local`. Setting the questions
/// file is the operator's consent to send each payload: the run approves
/// the exact payload hash the preview shows, as the consent dialog would.
#[test]
#[ignore = "manual topic expert run over local checkouts; calls a provider"]
fn showcase_ask_areas() {
    use crate::area_expert::{Asked, areas, ask, prepare};
    use llm::bounded::CallDirective;
    use std::fmt::Write as _;

    let questions: Vec<AreaQuestion> = serde_json::from_slice(
        &std::fs::read(required("CARTOGRAPH_SHOWCASE_QUESTIONS")).expect("questions file"),
    )
    .expect("questions are [{area, question}]");
    let mode = std::env::var("CARTOGRAPH_SHOWCASE_ASK_MODE").unwrap_or_else(|_| "cloud".into());
    let (output, app) = showcase_output();
    recover(&app, &output);
    let state = app.state::<AppState>();
    write_briefings(&state, &output);
    if mode == "cloud" {
        let mut settings = state.settings.lock().unwrap();
        settings.set_enabled("T3", true).unwrap();
        settings.set_provider("T3", "cloud").unwrap();
        settings
            .grant_consent("T3", r#"{"provider":"showcase-ask","network":true}"#)
            .unwrap();
    }
    let provider = crate::egress::escalation_provider(&mode).unwrap();
    let choices = areas(&state).unwrap();
    let mut records = Vec::new();
    let mut report = String::from("# Topic expert answers\n");
    for AreaQuestion { area, question } in &questions {
        let _ = write!(report, "\n## {area}: {question}\n\n");
        let Some(choice) = choices.iter().find(|choice| &choice.name == area) else {
            report.push_str("No such area in this run.\n");
            records.push(serde_json::json!({
                "area": area, "question": question, "error": "area_not_found",
            }));
            continue;
        };
        let asked = prepare(&state, &choice.area, question, &[]).and_then(|prepared| {
            let hash = crate::section_compose::firewall(&state)?
                .prepare_bounded(
                    provider.as_ref(),
                    prepared.action(),
                    &crate::section_compose::limits()?,
                )
                .map_err(|error| error.to_string())?
                .preview()
                .egress
                .payload_hash
                .clone();
            ask(&state, provider.as_ref(), &prepared, Some(hash), &|| {
                CallDirective::Continue
            })
        });
        match asked {
            Err(error) => {
                let _ = writeln!(report, "The call failed: {error}");
                records.push(serde_json::json!({
                    "area": area, "question": question, "error": error,
                }));
            }
            Ok(Asked { answer, usage }) => {
                match &answer {
                    Err(reason) => {
                        let _ = writeln!(report, "No answer: `{reason}`.");
                    }
                    Ok(answer) => {
                        for item in &answer.statements {
                            let _ = writeln!(
                                report,
                                "- {} ({:?}; {})",
                                item.statement,
                                item.basis,
                                item.citations.join(", ")
                            );
                        }
                        for item in &answer.not_known {
                            let _ = writeln!(report, "- Not in the briefing: {item}");
                        }
                        if !answer.ask_instead.is_empty() {
                            let _ = writeln!(
                                report,
                                "- Ask instead: {}",
                                answer.ask_instead.join(", ")
                            );
                        }
                        if answer.left_out > 0 {
                            let _ = writeln!(
                                report,
                                "- {} statements left out: no citation the briefing prints.",
                                answer.left_out
                            );
                        }
                    }
                }
                let _ = writeln!(
                    report,
                    "\nUsage ({}): {:?} input and {:?} output tokens, {} request bytes.",
                    usage.model, usage.input_tokens, usage.output_tokens, usage.request_bytes
                );
                records.push(serde_json::json!({
                    "area": area, "question": question, "briefing_bytes": choice.briefing_bytes,
                    "answer": answer.as_ref().ok(), "no_answer": answer.as_ref().err(),
                    "usage": usage,
                }));
            }
        }
    }
    std::fs::write(
        output.join("answers.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("answers.md"), report).unwrap();
}
