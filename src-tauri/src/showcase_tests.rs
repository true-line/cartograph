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
fn showcase_app(profile: &Path) -> crate::test_app::TestApp {
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
    let app = crate::test_app::TestApp::new(tauri::test::mock_builder().build(context).unwrap());
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
fn showcase_output() -> (PathBuf, crate::test_app::TestApp) {
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
        // #1582: the complete bundle as the bundle file Export bundle writes
        // (ADR-0086), its artifact and assertion tables spooled to temp
        // files as the host does, so no table-sized buffer is held.
        write_showcase_bundle_file(&bundle, &root.join("bundle.json"));
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
    let provider =
        crate::egress::escalation_provider(&mode, llm::OllamaProvider::DEFAULT_PORT).unwrap();
    let choices = areas(&state).unwrap();
    let mut records = Vec::new();
    let mut report = String::from("# Topic expert answers\n");
    for AreaQuestion { area, question } in &questions {
        let _ = write!(report, "\n## {area}: {question}\n\n");
        let Some(choice) = crate::area_expert::named(&choices, area) else {
            report.push_str("No such area in this run.\n");
            records.push(serde_json::json!({
                "area": area, "question": question, "error": "area_not_found",
            }));
            continue;
        };
        if &choice.name != area {
            let _ = writeln!(report, "Asked of the area {}.\n", choice.name);
        }
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
            ask(
                &state,
                provider.as_ref(),
                &prepared,
                Some(hash),
                &|| CallDirective::Continue,
                None,
            )
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

/// A third manual run (#2083, AC-0980, AC-0981): draft D1 1.5 through the
/// gated `draft-rules@1` workflow on a local model, one draft per area, and
/// write the staged drafts and what the run did.
/// `CARTOGRAPH_SHOWCASE_LOCAL_URL` is a loopback Ollama endpoint (a
/// forwarder may stand in for a LAN host) and
/// `CARTOGRAPH_SHOWCASE_LOCAL_MODEL` the completion model. Nothing leaves
/// the loopback interface from this process.
#[test]
#[ignore = "manual gated drafting run over local checkouts; calls a local model"]
fn showcase_compose_rules_gated() {
    use crate::section_compose::compose_gated_by_area;
    use llm::bounded::CallDirective;
    use std::fmt::Write as _;
    let url = std::env::var("CARTOGRAPH_SHOWCASE_LOCAL_URL")
        .unwrap_or_else(|_| llm::OllamaProvider::DEFAULT_URL.into());
    let model = required("CARTOGRAPH_SHOWCASE_LOCAL_MODEL");
    let (output, app) = showcase_output();
    recover(&app, &output);
    let state = app.state::<AppState>();
    let provider = llm::OllamaProvider::with_models(
        &url,
        llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
        model.clone(),
        std::time::Duration::from_secs(600),
    )
    .expect("a loopback endpoint");
    let (_, job) = start_job(&state, "section-compose-gated:D1:1.5:system:local").unwrap();
    let started = std::time::Instant::now();
    let drafts = compose_gated_by_area(
        &state,
        &provider,
        (spec::CATALOGUE_VERSION, "D1", "1.5"),
        &|| CallDirective::Continue,
        job.id(),
    )
    .expect("the gated run completes");
    let seconds = started.elapsed().as_secs();
    let store = state.drafts.lock().unwrap();
    let mut report = format!("# D1 1.5 drafted through draft-rules@1 on {model}\n\n{seconds} s.\n");
    let mut written = Vec::new();
    for draft in &drafts {
        let _ = write!(
            report,
            "\n## {}\n\n{}\n\n",
            draft.area.as_deref().unwrap_or("Rules outside an area"),
            serde_json::to_string(&draft.outcome).unwrap()
        );
        let claims = match &draft.outcome.outcome {
            crate::investigations::section_drafts::DraftingOutcome::Staged { draft_id } => store
                .get(draft_id)
                .unwrap()
                .map(|stored| stored.draft.claims),
            _ => None,
        };
        for claim in claims.iter().flatten() {
            let _ = writeln!(
                report,
                "- {} ({})",
                claim.statement,
                claim.citations.join(", ")
            );
        }
        written.push(serde_json::json!({
            "area": draft.area, "scope": draft.scope, "outcome": draft.outcome, "claims": claims,
        }));
    }
    std::fs::write(
        output.join("gated-rules.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "model": model, "seconds": seconds, "drafts": written,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("gated-rules.md"), report).unwrap();
}

/// The W13 content-acceptance pilot (#1748, AC-0670): draft D1 1.2 and 1.3
/// through `section-composer@1` and 1.5 through the gated `draft-rules@1`
/// workflow, all at the system scope, on a local OpenAI-compatible server
/// (vLLM), as the Workbench's "Draft this section" does.
/// `CARTOGRAPH_SHOWCASE_OPENAI_URL` is the loopback base (`{url}v1/`) and
/// `CARTOGRAPH_SHOWCASE_OPENAI_MODEL` the model.
///
/// The run writes the staged drafts to `staged.json` and `pilot.json` (per
/// section: outcome, seconds, the job's reported usage, the role and its
/// definition fingerprint), then waits for the reviewer's decisions: a file
/// `decisions.json` in the output directory, `[{"draft_id", "decision":
/// "accepted" | "rejected" | "returned", "reviewer", "note", "remove"}]`
/// (`remove`, optional: the claim numbers an accepted draft is accepted
/// without, #1748), written by
/// the person the pilot names after reading the drafts. Nothing is accepted
/// without that file, a draft it does not name stays pending, and the wait
/// ends after `CARTOGRAPH_W13_DECISION_WAIT_SECS` (default 6 hours) with no
/// export. Each decision is recorded through the Workbench's review path,
/// its note naming the reviewer, and the `best-effort/` bundle is written
/// with the curated read. Nothing is scored here.
#[test]
#[ignore = "manual W13 pilot over local checkouts; calls a local model"]
fn showcase_w13_pilot() {
    use crate::section_compose::{compose_gated_by_area, run_blocking};
    use agents::draft_store::DraftDecision;
    use llm::bounded::CallDirective;
    let url = std::env::var("CARTOGRAPH_SHOWCASE_OPENAI_URL")
        .unwrap_or_else(|_| llm::openai_compatible::OpenAiCompatibleProvider::DEFAULT_URL.into());
    let model = required("CARTOGRAPH_SHOWCASE_OPENAI_MODEL");
    let wait = std::env::var("CARTOGRAPH_W13_DECISION_WAIT_SECS")
        .map(|secs| secs.parse::<u64>().expect("seconds"))
        .unwrap_or(6 * 3600);
    let provider = llm::openai_compatible::OpenAiCompatibleProvider::new(
        &url,
        model.clone(),
        std::time::Duration::from_secs(600),
    )
    .expect("a loopback endpoint")
    .with_thinking_disabled();
    let (output, app) = showcase_output();
    let started = std::time::Instant::now();
    recover(&app, &output);
    let ingest_seconds = started.elapsed().as_secs();
    let state = app.state::<AppState>();
    let usage_of = |job: i64| {
        let jobs = state.jobs.lock().unwrap();
        jobs.list_usage()
            .unwrap()
            .into_iter()
            .filter(|usage| usage.job_id == job)
            .map(|usage| serde_json::to_value(usage).unwrap())
            .collect::<Vec<_>>()
    };
    // As the desktop's `finish_job`: a run that errored fails its job.
    let finish = |job: &JobExecution, drafts: &[String], error: Option<&String>| {
        let mut jobs = state.jobs.lock().unwrap();
        match error {
            Some(error) => jobs.fail_execution(job, error).unwrap(),
            None => jobs.finish_execution(job, drafts).unwrap(),
        };
    };
    let mut sections = Vec::new();
    let mut staged: Vec<String> = Vec::new();
    for section in ["1.2", "1.3"] {
        let (_, job) = start_job(
            &state,
            &format!("section-compose:D1:{section}:system:local"),
        )
        .unwrap();
        let begun = std::time::Instant::now();
        let outcome = run_blocking(
            &state,
            &job,
            &provider,
            (spec::CATALOGUE_VERSION, "D1", section, spec::SYSTEM_SCOPE),
            None,
        );
        let seconds = begun.elapsed().as_secs();
        let drafts: Vec<String> = match &outcome {
            Ok(outcome) => match &outcome.outcome {
                crate::investigations::section_drafts::DraftingOutcome::Staged { draft_id } => {
                    vec![draft_id.clone()]
                }
                _ => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        finish(&job, &drafts, outcome.as_ref().err());
        staged.extend(drafts.iter().cloned());
        sections.push(serde_json::json!({
            "section": section, "role": agents::section_compose::SECTION_COMPOSER_ROLE,
            "role_definition": agents::section_compose::definition_fingerprint(),
            "seconds": seconds, "job": job.id(), "usage": usage_of(job.id()),
            "outcome": outcome.as_ref().ok(), "error": outcome.as_ref().err(),
            "drafts": drafts,
        }));
    }
    // 1.5 on the gated workflow; a run that stages nothing is tried once more.
    for attempt in 1..=2 {
        let (_, job) = start_job(&state, "section-compose-gated:D1:1.5:system:local").unwrap();
        let begun = std::time::Instant::now();
        let result = compose_gated_by_area(
            &state,
            &provider,
            (spec::CATALOGUE_VERSION, "D1", "1.5"),
            &|| CallDirective::Continue,
            job.id(),
        );
        let seconds = begun.elapsed().as_secs();
        let drafts: Vec<String> = result
            .iter()
            .flatten()
            .filter_map(|draft| match &draft.outcome.outcome {
                crate::investigations::section_drafts::DraftingOutcome::Staged { draft_id } => {
                    Some(draft_id.clone())
                }
                _ => None,
            })
            .collect();
        finish(&job, &drafts, result.as_ref().err());
        staged.extend(drafts.iter().cloned());
        sections.push(serde_json::json!({
            "section": "1.5", "attempt": attempt, "role": agents::gated_compose::DRAFT_RULES_ROLE,
            "role_definition": agents::gated_compose::definition_fingerprint(),
            "seconds": seconds, "job": job.id(), "usage": usage_of(job.id()),
            "outcome": result.as_ref().ok(), "error": result.as_ref().err(),
            "drafts": drafts,
        }));
        if !drafts.is_empty() {
            break;
        }
    }
    let write_pilot = |sections: &[serde_json::Value]| {
        std::fs::write(
            output.join("pilot.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "model": model, "endpoint": url,
                "cartograph_version": env!("CARGO_PKG_VERSION"),
                "ingest_seconds": ingest_seconds,
                "total_seconds": started.elapsed().as_secs(),
                "sections": sections,
            }))
            .unwrap(),
        )
        .unwrap();
    };
    write_pilot(&sections);
    let stored: Vec<_> = {
        let store = state.drafts.lock().unwrap();
        staged
            .iter()
            .map(|draft_id| {
                store
                    .get(draft_id)
                    .unwrap()
                    .expect("a staged draft is stored")
            })
            .collect()
    };
    std::fs::write(
        output.join("staged.json"),
        serde_json::to_vec_pretty(&stored).unwrap(),
    )
    .unwrap();
    // The reviewer's decisions, made after reading `staged.json`.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Decision {
        draft_id: String,
        decision: DraftDecision,
        reviewer: String,
        note: Option<String>,
        #[serde(default)]
        remove: std::collections::BTreeSet<usize>,
    }
    let decisions_path = output.join("decisions.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    eprintln!("waiting for {}", decisions_path.display());
    while !decisions_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "no decisions.json within {wait} s; nothing was reviewed or exported"
        );
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    // A file still being written is read again.
    let decisions: Vec<Decision> = loop {
        match serde_json::from_slice(&std::fs::read(&decisions_path).unwrap()) {
            Ok(decisions) => break decisions,
            Err(error) if std::time::Instant::now() < deadline => {
                eprintln!("decisions.json does not parse yet: {error}");
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
            Err(error) => panic!("decisions.json does not parse: {error}"),
        }
    };
    let mut reviewed = Vec::new();
    {
        let mut store = state.drafts.lock().unwrap();
        for decision in &decisions {
            assert!(
                staged.contains(&decision.draft_id),
                "decision for a draft this run did not stage: {}",
                decision.draft_id
            );
            assert!(
                !decision.reviewer.trim().is_empty(),
                "a decision names its reviewer"
            );
            let current = store
                .get(&decision.draft_id)
                .unwrap()
                .expect("a staged draft is stored");
            let note = match decision.note.as_deref().map(str::trim) {
                Some(note) if !note.is_empty() => format!("{}: {note}", decision.reviewer),
                _ => decision.reviewer.clone(),
            };
            assert!(
                decision.remove.is_empty() || decision.decision == DraftDecision::Accepted,
                "only an accepted draft is accepted without claims"
            );
            let recorded = if decision.remove.is_empty() {
                store.review(
                    &decision.draft_id,
                    current.review_revision,
                    decision.decision,
                    Some(&note),
                )
            } else {
                // The draft is returned and its successor, the other claims
                // unchanged, accepted (AC-1607).
                store.accept_without(
                    &decision.draft_id,
                    current.review_revision,
                    &decision.remove,
                    Some(&note),
                )
            }
            .unwrap();
            reviewed.push(serde_json::to_value(&recorded).unwrap());
        }
    }
    std::fs::write(
        output.join("drafts.json"),
        serde_json::to_vec_pretty(&reviewed).unwrap(),
    )
    .unwrap();
    // The desktop export path, curated read included.
    let bundle = crate::compile_spec_bundle(
        crate::export_inputs(&state).unwrap(),
        spec::ExportMode::BestEffort,
    )
    .unwrap();
    let root = output.join("best-effort");
    std::fs::create_dir(&root).unwrap();
    for artifact in &bundle.artifacts {
        std::fs::write(root.join(&artifact.file_name), &artifact.content).unwrap();
    }
    write_pilot(&sections);
}

/// A fourth manual run (#258): the yield-only T2 pass at full scale.
/// It recovers the sources, then runs the desktop's T2 preview path twice on
/// a local embedding model with no labelled pairs and no reviewed evidence,
/// so every family stays calibration-only and nothing is admitted (ADR-0038).
/// It writes what the pass would stage: hop and candidate counts per family,
/// the similarity bands of the staged proposals, the cold and warm cache
/// statistics with their wall times, and one line per proposal with both
/// texts as the input for drafting labels.
/// `CARTOGRAPH_SHOWCASE_LOCAL_URL` is a loopback Ollama endpoint (a forwarder
/// may stand in for a LAN host) and `CARTOGRAPH_SHOWCASE_EMBED_MODEL` the
/// embedding model, by default the M7 default.
#[test]
#[ignore = "manual T2 yield run over local checkouts; calls a local embedding model"]
fn showcase_t2_yield() {
    use std::collections::BTreeMap;
    let url = std::env::var("CARTOGRAPH_SHOWCASE_LOCAL_URL")
        .unwrap_or_else(|_| llm::OllamaProvider::DEFAULT_URL.into());
    let model = std::env::var("CARTOGRAPH_SHOWCASE_EMBED_MODEL")
        .unwrap_or_else(|_| llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL.into());
    let (output, app) = showcase_output();
    let started = std::time::Instant::now();
    recover(&app, &output);
    let ingest_seconds = started.elapsed().as_secs();
    let state = app.state::<AppState>();
    let provider =
        llm::OllamaProvider::new(&url, model.clone(), std::time::Duration::from_secs(600))
            .expect("a loopback endpoint");
    // The graph slice the desktop's `semantic_preview` reads.
    let (nodes, edges) = {
        let graph = state.graph.lock().unwrap();
        let mut labels = flowtracer::FLOW_NODE_LABELS.to_vec();
        labels.push("Resource");
        read_filtered_graph(&*graph, &labels, flowtracer::FLOW_EDGE_LABELS).unwrap()
    };
    let cache = state.embedding_cache.clone().unwrap();
    let pass = || {
        let lease = {
            let graph = state.graph.lock().unwrap();
            cache
                .lock()
                .unwrap()
                .bind_instance(&graph.instance_id().unwrap())
                .unwrap()
        };
        let started = std::time::Instant::now();
        let preview = build_reviewed_semantic_preview(
            &provider,
            Arc::clone(&cache),
            lease,
            &nodes,
            &edges,
            &[],
            1.0,
            None,
        )
        .expect("the T2 pass completes");
        (preview, started.elapsed().as_secs_f64())
    };
    let (cold, cold_seconds) = pass();
    let (warm, warm_seconds) = pass();
    assert!(cold.approved.is_empty() && warm.approved.is_empty());

    let family =
        |key: &semantic::FamilyKey| format!("{:?}/{}", key.kind(), key.subtype().unwrap_or("null"));
    let (hops, candidates) = semantic::graph_inputs(&nodes, &edges);
    let mut per_family: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
    for hop in &hops {
        let entry = per_family.entry(family(&hop.family())).or_default();
        let Some(exclusion) = hop.exclusion else {
            *entry.entry("hops").or_default() += 1;
            continue;
        };
        *entry.entry("hops_excluded").or_default() += 1;
        // Excluded hops by reason (AC-0285 dispatch, AC-1513 name collision).
        let reason = match exclusion {
            semantic::HopExclusion::DynamicDispatch => "hops_excluded_dynamic_dispatch",
            semantic::HopExclusion::NameCollision => "hops_excluded_name_collision",
        };
        *entry.entry(reason).or_default() += 1;
    }
    for candidate in &candidates {
        *per_family
            .entry(family(&candidate.family()))
            .or_default()
            .entry("candidates")
            .or_default() += 1;
    }
    // The quality bands #254 measured on a VSCode subtree.
    const BANDS: [(&str, f32); 5] = [
        ("ge_0_95", 0.95),
        ("0_90_to_0_95", 0.90),
        ("0_80_to_0_90", 0.80),
        ("0_70_to_0_80", 0.70),
        ("lt_0_70", f32::NEG_INFINITY),
    ];
    let mut bands: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
    let mut proposed_gaps = std::collections::BTreeSet::new();
    for proposal in &cold.proposals {
        let band = BANDS
            .iter()
            .find(|(_, floor)| proposal.similarity >= *floor)
            .map(|(name, _)| *name)
            .unwrap();
        *bands
            .entry(family(proposal.family()))
            .or_default()
            .entry(band)
            .or_default() += 1;
        proposed_gaps.insert(proposal.gap_id.as_str());
    }
    std::fs::write(
        output.join("t2-yield.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "provider": cold.provider,
            "model": model,
            "ingest_and_knowledge_seconds": ingest_seconds,
            "hops": hops.len(),
            "ann_hops": hops.iter().filter(|hop| hop.is_ann_candidate()).count(),
            "candidates": candidates.len(),
            "families": per_family,
            "proposals": cold.proposals.len(),
            "gaps_with_a_proposal": proposed_gaps.len(),
            "similarity_bands": bands,
            "admitted": cold.approved.len(),
            "evaluation": cold.evaluation,
            "cold": { "seconds": cold_seconds, "cache": cold.embedding_cache },
            "warm": { "seconds": warm_seconds, "cache": warm.embedding_cache },
        }))
        .unwrap(),
    )
    .unwrap();

    // One line per staged proposal, both texts beside the ids, for drafting
    // labels. Labels are drafted from source, never from this similarity.
    let queries: BTreeMap<&str, &semantic::UnresolvedHop> =
        hops.iter().map(|hop| (hop.gap_id.as_str(), hop)).collect();
    let targets: BTreeMap<&str, &str> = candidates
        .iter()
        .map(|candidate| (candidate.node_id.as_str(), candidate.text.as_str()))
        .collect();
    let mut writer =
        std::io::BufWriter::new(std::fs::File::create(output.join("t2-proposals.jsonl")).unwrap());
    for proposal in &cold.proposals {
        let hop = queries.get(proposal.gap_id.as_str());
        serde_json::to_writer(
            &mut writer,
            &serde_json::json!({
                "family": family(proposal.family()),
                "gap_id": proposal.gap_id,
                "source_id": proposal.source_id,
                "target_id": proposal.target_id,
                "similarity": proposal.similarity,
                "query": hop.map(|hop| hop.query.as_str()),
                "query_evidence": hop.map(|hop| &hop.evidence),
                "candidate": targets.get(proposal.target_id.as_str()),
            }),
        )
        .unwrap();
        writer.write_all(b"\n").unwrap();
    }
    writer.flush().unwrap();
}

/// The bundle file of `bundle` at `path`, through [`spec::bundle_file::BundleFileV2`]
/// with its tables spooled to temp files (#1582).
fn write_showcase_bundle_file(bundle: &spec::SpecBundle, path: &Path) {
    use spec::bundle_file::BundleFileV2;
    use std::io::{BufWriter, Seek, SeekFrom};
    let spool = || BufWriter::new(tempfile::tempfile().unwrap());
    let rewound = |spool: BufWriter<std::fs::File>| {
        let mut file = spool.into_inner().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    };
    let (mut artifacts, mut assertions) = (spool(), spool());
    let mut writer = BundleFileV2::new();
    for artifact in &bundle.artifacts {
        writer
            .push(artifact, &mut artifacts, &mut assertions)
            .unwrap();
    }
    let rendered = writer.rendered_from();
    let head = spec::SpecBundle {
        mode: bundle.mode,
        artifacts: Vec::new(),
        assertion_count: bundle.assertion_count,
        gap_count: bundle.gap_count,
        drift_count: bundle.drift_count,
        security_count: bundle.security_count,
        graph_basis: bundle.graph_basis.clone(),
        curated_basis: bundle.curated_basis.clone(),
    };
    let mut out = BufWriter::new(std::fs::File::create(path).unwrap());
    writer
        .finish(
            &head,
            &mut out,
            &mut rewound(artifacts),
            &mut rewound(assertions),
            |out| {
                for (i, artifact) in bundle.artifacts.iter().enumerate() {
                    match &rendered[i] {
                        Some(from) => {
                            BundleFileV2::rendered_entry(out, i, &artifact.file_name, from)?
                        }
                        None => {
                            BundleFileV2::file_entry(out, i, &artifact.file_name, b"")?;
                            serde_json::to_writer(&mut *out, &artifact.content)?;
                        }
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    out.flush().unwrap();
}
