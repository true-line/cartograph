//! The attested pilot (AC-1083, ADR-0090 slice 7, #2463): on SimpleLogin, a
//! Gap answered by an agent and confirmed by the engineer is printed in D1
//! `best-effort` as an attested claim naming the agent's run and the
//! engineer, is left out of `verified-only` and counted there, and the
//! graph content hash and the Gap count do not change.
//!
//! The run waits on files the operator writes into the output directory
//! after reading the ones before them, as the W13 pilot does:
//!
//! 1. `gaps.json` (written): every open Gap, with the hash and counts in
//!    `before.json`.
//! 2. `request.json` (read): the analyst's start request, unless
//!    `CARTOGRAPH_PILOT_INVESTIGATION` names a completed run of a copied
//!    profile. Writing it is the operator's consent to the run's steps.
//! 3. `findings.json` (written): the run and its findings.
//! 4. `answer.json` (read): `{"finding_id", "gap_id", "slot"}`, the finding
//!    the operator proposes as the agent's answer. The engineer is recorded
//!    as directing it, and then confirms it from the Gaps register.
//! 5. `staged.json` (written): the drafts of the sections composed after.
//! 6. `decisions.json` (read): the reviewer's decisions, as in W13.
//! 7. `verified-only/`, `best-effort/`, `after.json` and `pilot.json`
//!    (written). The pass bar's checks are in `pilot.json`; the run fails
//!    after writing it when one does not hold.
//!
//! A copied profile is used only with its completed run named, since its
//! saved local provider settings would bypass the provider set here.
use super::*;
use agents::attested::{AttestationSource, EngineerVerb, Standing};
use agents::investigation::{InvestigationStatus, StartInvestigationRequest};

/// Wait for the operator's file `name` in `output` and parse it. A file
/// still being written is read again.
fn await_file<T: serde::de::DeserializeOwned>(output: &Path, name: &str, wait: u64) -> T {
    let path = output.join(name);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    eprintln!("waiting for {}", path.display());
    loop {
        if let Ok(bytes) = std::fs::read(&path) {
            match serde_json::from_slice(&bytes) {
                Ok(value) => return value,
                Err(error) => eprintln!("{name} does not parse yet: {error}"),
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no usable {name} within {wait} s"
        );
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

fn write_json(output: &Path, name: &str, value: &serde_json::Value) {
    std::fs::write(output.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// Copy the profile tree `from` to the new directory `to`, so the original
/// is never opened.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The graph content hash, the register's Gap count and the open Gaps.
fn measure(state: &AppState) -> (serde_json::Value, Vec<serde_json::Value>) {
    let graph = state.graph.lock().unwrap();
    let (nodes, edges) = graph.read_snapshot().unwrap();
    let register = summarize_register(&nodes, &edges, 0, 0);
    let gaps: Vec<serde_json::Value> = nodes
        .iter()
        .filter(|node| context_hub::curated::is_gap(node))
        .map(|node| {
            serde_json::json!({
                "id": node.id, "label": node.label,
                "name": node.props.get("name"), "reason": node.props.get("reason"),
            })
        })
        .collect();
    (
        serde_json::json!({
            "graph_content_hash": spec::graph_content_hash(&nodes, &edges),
            "register_gaps": register.gaps,
            "open_gaps": gaps.len(),
        }),
        gaps,
    )
}

/// Start the analyst's run and wait for it to end, approving each step it
/// asks consent for: the operator consented by writing `request.json`.
fn run_analyst(
    app: &tauri::App<tauri::test::MockRuntime>,
    request: StartInvestigationRequest,
    wait: u64,
) -> String {
    let state = app.state::<AppState>();
    let summary =
        crate::investigations::commands::start_for_test(request, app.handle().clone()).unwrap();
    let id = summary.investigation_id;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        let status = {
            let jobs = state.jobs.lock().unwrap();
            jobs.investigation(&id).unwrap().summary.status
        };
        match status {
            InvestigationStatus::Completed => return id,
            InvestigationStatus::AwaitingConsent => {
                if let Some(consent) =
                    crate::investigations::commands::pending(&state, &id).unwrap()
                {
                    crate::investigations::commands::consent_decision(
                        app.handle(),
                        &id,
                        &consent.step_id,
                        consent.revision,
                        &consent.preview.payload_hash,
                        true,
                    )
                    .unwrap();
                }
            }
            InvestigationStatus::Failed
            | InvestigationStatus::Cancelled
            | InvestigationStatus::Interrupted
            | InvestigationStatus::OutcomeUnknown => {
                panic!("the analyst's run {id} ended {status:?}")
            }
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the analyst's run {id} did not end within {wait} s"
        );
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    finding_id: String,
    gap_id: String,
    slot: String,
}

/// The manual attested pilot (AC-1083). `CARTOGRAPH_SHOWCASE_OUTPUT` is a new
/// directory. Either `CARTOGRAPH_PILOT_PROFILE` (a recovered profile, copied
/// into the output) or the showcase sources are recovered.
/// `CARTOGRAPH_SHOWCASE_OPENAI_URL` and `CARTOGRAPH_SHOWCASE_OPENAI_MODEL`
/// name the local OpenAI-compatible server the analyst's run (unless it
/// names a runtime of its own) and the drafting use.
/// `CARTOGRAPH_PILOT_SECTIONS` lists the D1 sections drafted after the
/// confirmation (default `1.5`, on the gated workflow), and
/// `CARTOGRAPH_PILOT_ENGINEER` is the engineer's display name, which stays
/// in the profile (default `Pilot Engineer`).
#[test]
#[ignore = "manual attested pilot over local checkouts; calls a local model"]
fn showcase_attested_pilot() {
    use crate::section_compose::{compose_gated_by_area, run_blocking};
    use llm::bounded::CallDirective;
    let url = std::env::var("CARTOGRAPH_SHOWCASE_OPENAI_URL")
        .unwrap_or_else(|_| llm::openai_compatible::OpenAiCompatibleProvider::DEFAULT_URL.into());
    let model = required("CARTOGRAPH_SHOWCASE_OPENAI_MODEL");
    let wait = std::env::var("CARTOGRAPH_PILOT_WAIT_SECS")
        .map(|secs| secs.parse::<u64>().expect("seconds"))
        .unwrap_or(6 * 3600);
    let engineer_name =
        std::env::var("CARTOGRAPH_PILOT_ENGINEER").unwrap_or_else(|_| "Pilot Engineer".into());
    let sections: Vec<String> = std::env::var("CARTOGRAPH_PILOT_SECTIONS")
        .unwrap_or_else(|_| "1.5".into())
        .split(',')
        .map(|section| section.trim().to_owned())
        .collect();
    let provider = llm::openai_compatible::OpenAiCompatibleProvider::new(
        &url,
        model.clone(),
        std::time::Duration::from_secs(600),
    )
    .expect("a loopback endpoint")
    .with_thinking_disabled();

    let output = PathBuf::from(required("CARTOGRAPH_SHOWCASE_OUTPUT"));
    assert!(output.is_absolute(), "output must be absolute");
    std::fs::create_dir(&output).expect("output directory must be new");
    let copied = std::env::var("CARTOGRAPH_PILOT_PROFILE").ok();
    if let Some(profile) = &copied {
        copy_tree(Path::new(profile), &output.join("profile"));
    }
    let app = showcase_app(&output.join("profile"));
    if copied.is_none() {
        recover(&app, &output);
    }
    let state = app.state::<AppState>();
    let (before, gaps) = measure(&state);
    write_json(&output, "before.json", &before);
    write_json(&output, "gaps.json", &serde_json::json!(gaps));

    // The agent's run: a completed run of the copied profile, or a new one.
    let investigation = match std::env::var("CARTOGRAPH_PILOT_INVESTIGATION") {
        Ok(id) => id,
        Err(_) => {
            // A copied profile's saved local model, context window or port
            // would build its own provider past the one set here.
            assert!(
                copied.is_none(),
                "with CARTOGRAPH_PILOT_PROFILE, name its completed run in CARTOGRAPH_PILOT_INVESTIGATION"
            );
            let request: StartInvestigationRequest = await_file(&output, "request.json", wait);
            state.investigations.set_test_provider(
                request.provider_mode,
                std::sync::Arc::new(
                    llm::openai_compatible::OpenAiCompatibleProvider::new(
                        &url,
                        model.clone(),
                        std::time::Duration::from_secs(600),
                    )
                    .expect("a loopback endpoint")
                    .with_thinking_disabled(),
                ),
            );
            run_analyst(&app, request, wait)
        }
    };
    let (detail, result) = {
        let jobs = state.jobs.lock().unwrap();
        (
            jobs.investigation(&investigation).unwrap(),
            jobs.investigation_result(&investigation).unwrap(),
        )
    };
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    write_json(
        &output,
        "findings.json",
        &serde_json::json!({ "investigation": detail, "result": result }),
    );

    // The engineer, then the agent's answer the engineer chose, confirmed.
    let engineer = match crate::attested_capture::set_engineer(&state, &engineer_name, None) {
        Ok(engineer) => engineer,
        Err(_) => {
            let drafts = state.drafts.lock().unwrap();
            drafts.attested().engineer().unwrap().expect("one engineer")
        }
    };
    let answer: Answer = await_file(&output, "answer.json", wait);
    let proposed = crate::attested_capture::propose_from_finding(
        &state,
        &investigation,
        &answer.finding_id,
        &answer.gap_id,
        &answer.slot,
        true,
    )
    .unwrap_or_else(|refusal| panic!("the proposal was refused: {refusal}"));
    assert_eq!(proposed.standing, Standing::AgentAttestedUnconfirmed);
    let confirmed = crate::attested_capture::engineer_attest(
        &state,
        &proposed.record_id,
        proposed.attestation_count(),
        EngineerVerb::Confirmed,
        AttestationSource::GapsRegister,
        None,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            }),
    )
    .unwrap();
    assert_eq!(confirmed.standing, Standing::EngineerBacked);
    let record = serde_json::to_value(
        crate::attested_capture::attested_views(&state, 50, None)
            .unwrap()
            .items
            .into_iter()
            .find(|view| view.record_id == confirmed.record_id)
            .expect("the record is listed"),
    )
    .unwrap();
    write_json(&output, "record.json", &record);

    // The sections drafted after the confirmation, so a composition is
    // handed the record with the Gap among its facts.
    let mut staged: Vec<String> = Vec::new();
    let mut runs = Vec::new();
    for section in &sections {
        let kind = if section == "1.5" {
            "section-compose-gated:D1:1.5:system:local".to_owned()
        } else {
            format!("section-compose:D1:{section}:system:local")
        };
        let (_, job) = start_job(&state, &kind).unwrap();
        let drafts: Vec<String> = if section == "1.5" {
            compose_gated_by_area(
                &state,
                &provider,
                (spec::CATALOGUE_VERSION, "D1", "1.5"),
                &|| CallDirective::Continue,
                job.id(),
            )
            .expect("the gated run completes")
            .into_iter()
            .filter_map(|draft| match draft.outcome.outcome {
                crate::investigations::section_drafts::DraftingOutcome::Staged { draft_id } => {
                    Some(draft_id)
                }
                _ => None,
            })
            .collect()
        } else {
            let outcome = run_blocking(
                &state,
                &job,
                &provider,
                (spec::CATALOGUE_VERSION, "D1", section, spec::SYSTEM_SCOPE),
                None,
            )
            .expect("the drafting run completes");
            match outcome.outcome {
                crate::investigations::section_drafts::DraftingOutcome::Staged { draft_id } => {
                    vec![draft_id]
                }
                _ => Vec::new(),
            }
        };
        state
            .jobs
            .lock()
            .unwrap()
            .finish_execution(&job, &drafts)
            .unwrap();
        runs.push(serde_json::json!({ "section": section, "drafts": drafts }));
        staged.extend(drafts);
    }
    let stored: Vec<_> = {
        let store = state.drafts.lock().unwrap();
        staged
            .iter()
            .map(|id| store.get(id).unwrap().expect("a staged draft is stored"))
            .collect()
    };
    let citation = confirmed.citation_id.clone();
    let drafted_citing = stored
        .iter()
        .flat_map(|stored| &stored.draft.claims)
        .filter(|claim| claim.citations.contains(&citation))
        .count();
    write_json(&output, "staged.json", &serde_json::json!(stored));
    let reviewed = await_decisions(&state, &output, &staged, wait);
    write_json(&output, "drafts.json", &serde_json::json!(reviewed));

    // Both exports, through the desktop export path with the curated read.
    let mut printed = std::collections::BTreeMap::new();
    for (mode, dir) in [
        (spec::ExportMode::VerifiedOnly, "verified-only"),
        (spec::ExportMode::BestEffort, "best-effort"),
    ] {
        let bundle =
            crate::compile_spec_bundle(crate::export_inputs(&state).unwrap(), mode).unwrap();
        let root = output.join(dir);
        std::fs::create_dir(&root).unwrap();
        let mut text = String::new();
        for artifact in &bundle.artifacts {
            std::fs::write(root.join(&artifact.file_name), &artifact.content).unwrap();
            text.push_str(&artifact.content);
        }
        printed.insert(dir, text);
    }
    let (after, _) = measure(&state);
    write_json(&output, "after.json", &after);

    let best = &printed["best-effort"];
    let verified = &printed["verified-only"];
    let checks = serde_json::json!({
        "drafted_claims_citing_the_record": drafted_citing,
        "this_run_drafted_a_claim_citing_the_record": drafted_citing > 0,
        "best_effort_prints_the_record": best.contains(&citation),
        "best_effort_names_the_engineer": best.contains(&engineer.display_name),
        "best_effort_names_the_agent_run": best.contains(&investigation),
        "verified_only_leaves_it_out": !verified.contains(&citation),
        "verified_only_counts_it": verified.contains("cite an attested record"),
        "graph_content_hash_unchanged": before["graph_content_hash"] == after["graph_content_hash"],
        "register_gaps_unchanged": before["register_gaps"] == after["register_gaps"],
        "open_gaps_unchanged": before["open_gaps"] == after["open_gaps"],
    });
    write_json(
        &output,
        "pilot.json",
        &serde_json::json!({
            "model": model, "endpoint": url,
            "cartograph_version": env!("CARGO_PKG_VERSION"),
            "investigation": investigation, "record": record, "sections": runs,
            "before": before, "after": after, "checks": checks,
        }),
    );
    let failed: Vec<&String> = checks
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, value)| value.as_bool() == Some(false))
        .map(|(name, _)| name)
        .collect();
    assert!(failed.is_empty(), "pilot checks failed: {failed:?}");
}
