//! Existing captured-profile preparation only. No provider implementation or dispatch.
use super::super::vendure;
use super::*;
use agents::{TaskFactKey, investigation::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct Selection {
    pub fact: TaskFactKey,
    pub graph_snapshot_id: String,
    pub receipt_id: String,
    pub ordinal: usize,
}

/// AC-1436 (#2524): two selections naming the same fact would stage the same
/// source obligation twice and silently shrink the distinct read set.
fn repeats_a_fact(selections: &[Selection]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    selections
        .iter()
        .any(|selection| !seen.insert(&selection.fact))
}

/// A fact left out of a rule v3 list, with the reason it would not stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(in super::super) struct Excluded {
    pub fact: TaskFactKey,
    pub reason: String,
}

/// AC-1438 (#2523): rule v3 that stages only what resolves now. Each candidate
/// (owners in ranking order, then the rule v2 fill) goes through `current`,
/// which returns the selection as the profile binds it today (a fill entry
/// from an earlier profile gets the current snapshot and receipt) or the
/// typed reason it cannot be inspected. A candidate that cannot is excluded
/// with that reason and the next one takes its place, so one unresolvable
/// fact never refuses the whole run at staging.
/// AC-1436 (#2524) dedupes by fact identity, owners first, filled to `max`.
pub(in super::super) fn complete_rule_v3_current(
    owners: Vec<Selection>,
    fill: Vec<Selection>,
    max: usize,
    current: impl Fn(&Selection) -> Result<Selection, String>,
) -> (Vec<Selection>, Vec<Excluded>) {
    let mut taken = std::collections::BTreeSet::new();
    let mut selections = Vec::new();
    let mut excluded = Vec::new();
    for candidate in owners.into_iter().chain(fill) {
        if selections.len() == max {
            break;
        }
        if !taken.insert(candidate.fact.clone()) {
            continue;
        }
        match current(&candidate) {
            Ok(selection) => selections.push(selection),
            Err(reason) => excluded.push(Excluded {
                fact: candidate.fact,
                reason,
            }),
        }
    }
    (selections, excluded)
}

#[test]
fn rule_v3_current_excludes_unresolvable_facts_with_a_reason_and_refills() {
    let selection = |id: &str, snapshot: &str| Selection {
        fact: TaskFactKey::Node { id: id.into() },
        graph_snapshot_id: snapshot.into(),
        receipt_id: format!("receipt-{id}"),
        ordinal: 0,
    };
    let owners = vec![
        selection("a", "context-v1:now"),
        selection("b", "context-v1:now"),
    ];
    let fill = vec![
        selection("a", "context-v1:old"),
        selection("c", "context-v1:old"),
        selection("d", "context-v1:old"),
    ];
    // `b` no longer resolves; the fill's stale snapshot ids are refreshed.
    let current = |candidate: &Selection| match &candidate.fact {
        TaskFactKey::Node { id } if id == "b" => Err("Stale".to_owned()),
        _ => Ok(Selection {
            graph_snapshot_id: "context-v1:now".into(),
            ..candidate.clone()
        }),
    };
    let (staged, excluded) = complete_rule_v3_current(owners, fill, 3, current);
    assert_eq!(
        staged,
        [
            selection("a", "context-v1:now"),
            selection("c", "context-v1:now"),
            selection("d", "context-v1:now")
        ]
    );
    assert_eq!(
        excluded,
        [Excluded {
            fact: TaskFactKey::Node { id: "b".into() },
            reason: "Stale".into()
        }]
    );
    assert!(!repeats_a_fact(&staged));
}

#[test]
fn rule_v3_fill_skips_taken_facts_and_staging_refuses_repeats() {
    let selection = |id: &str, ordinal: usize| Selection {
        fact: TaskFactKey::Node { id: id.into() },
        graph_snapshot_id: "context-v1:g".into(),
        receipt_id: format!("receipt-{id}"),
        ordinal,
    };
    let owners = vec![selection("a", 0), selection("b", 0)];
    // The fill repeats both owners (one at another ordinal) and itself.
    let fill = vec![
        selection("b", 0),
        selection("c", 0),
        selection("a", 1),
        selection("c", 0),
        selection("d", 0),
        selection("e", 0),
    ];
    let as_is = |candidate: &Selection| Ok(candidate.clone());
    let complete = complete_rule_v3_current(owners.clone(), fill.clone(), 4, as_is).0;
    let ids: Vec<_> = complete
        .iter()
        .map(|s| match &s.fact {
            TaskFactKey::Node { id } => id.as_str(),
            TaskFactKey::Edge { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
    assert!(!repeats_a_fact(&complete));
    // Owners alone already at the cap take nothing from the fill.
    assert_eq!(
        complete_rule_v3_current(owners.clone(), fill, 2, as_is).0,
        owners
    );
    // The canonical field order is the struct order, whatever the input order.
    let reordered: Selection = serde_json::from_str(
        r#"{"ordinal":0,"receipt_id":"receipt-a","graph_snapshot_id":"context-v1:g","fact":{"id":"a","kind":"node"}}"#,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&reordered).unwrap(),
        r#"{"fact":{"kind":"node","id":"a"},"graph_snapshot_id":"context-v1:g","receipt_id":"receipt-a","ordinal":0}"#
    );
    assert!(repeats_a_fact(&[selection("a", 0), selection("a", 1)]));
    assert!(!repeats_a_fact(&[selection("a", 0), selection("b", 0)]));
}
/// Supplied independently by the caller, never recovered as expected authority
/// from the adjacent frozen output. The preparation SHA256 binds the complete
/// input/profile/config/source/capture/graph/coverage receipt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct Definition {
    pub binding: PreparationBinding,
    /// Out-of-band caller authority, intentionally absent from frozen definition bytes.
    /// None permits fresh staging only; every reopen needs the independently retained pin.
    #[serde(skip)]
    pub start_sha256: Option<String>,
    pub preparation_sha256: String,
    pub provider: InvestigationProvider,
    pub selections: Vec<Selection>,
    pub question: String,
    pub request_nonce: String,
    pub scope: InvestigationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_limits: Option<RunLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_policy: Option<GenerationPolicy>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct RunLimits {
    pub active_seconds: u64,
    pub request_timeout_ms: u64,
}
fn json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, 128 * 1024)
        .map_err(|error| error.to_string())
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    vendure::read_preparation_artifact(path, 128 * 1024)
}
fn write(directory: &Path, name: &str, value: &impl Serialize) -> Result<(), String> {
    // Existing bounded, exclusive, fsync writer; no replacement of frozen bytes.
    vendure::write_preparation_artifact(&directory.join(name), value)?;
    sync_directory(directory)
}
pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(any(unix, windows))]
    {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Directory opens need backup semantics; FlushFileBuffers also
            // requires GENERIC_WRITE, matching job_execution's directory sync.
            options
                .write(true)
                .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
        }
        let directory = options.open(path).map_err(|error| error.to_string())?;
        if !directory
            .metadata()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            return Err("capture sync target must be a directory".into());
        }
        directory.sync_all().map_err(|error| error.to_string())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err("capture directory synchronization is unsupported on this platform".into())
    }
}

#[test]
fn captured_preparation_directory_sync_requires_directory_and_preserves_errors() {
    let directory = tempfile::tempdir().unwrap();
    sync_directory(directory.path()).unwrap();
    let file = directory.path().join("artifact.json");
    std::fs::write(&file, b"{}").unwrap();
    assert!(
        sync_directory(&file)
            .unwrap_err()
            .contains("must be a directory")
    );
    assert!(sync_directory(&directory.path().join("missing")).is_err());
}

/// Fresh output starts exactly one queued task. Existing output can only reopen
/// the exact already-started task, including a committed start whose ack was lost.
/// A crash between task commit and its synced start receipt requires explicit
/// recovery; this path refuses that ambiguous window without creating a task.
pub(in super::super) fn prepare_bound(
    config: &Path,
    preparation: &Path,
    definition_path: &Path,
    output: &Path,
    expected: &Definition,
    policy: vendure::InputPolicy,
) -> Result<serde_json::Value, String> {
    bound(
        [config, preparation, definition_path, output],
        expected,
        policy,
        true,
        |_, _, _, manifest, _| Ok(manifest.clone()),
    )
}
pub(in super::super) fn reopen_bound<T>(
    paths: [&Path; 4],
    expected: &Definition,
    policy: vendure::InputPolicy,
    use_task: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &StagedObligationRequest,
        &str,
        &serde_json::Value,
        &Path,
    ) -> Result<T, String>,
) -> Result<T, String> {
    bound(paths, expected, policy, false, use_task)
}
fn bound<T>(
    paths: [&Path; 4],
    expected: &Definition,
    policy: vendure::InputPolicy,
    preparing: bool,
    use_task: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &StagedObligationRequest,
        &str,
        &serde_json::Value,
        &Path,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let validated = validate_bound(paths, expected, preparing)?;
    bound_with_scope(
        paths, expected, policy, preparing, None, validated, use_task,
    )
}

pub(in super::super) struct BoundDefinition<'a> {
    definition: Vec<u8>,
    specialist: &'a str,
}
fn validate_bound<'a>(
    paths: [&Path; 4],
    expected: &'a Definition,
    preparing: bool,
) -> Result<BoundDefinition<'a>, String> {
    let [_, _, definition_path, output] = paths;
    if !preparing && !output.is_dir() {
        return Err("Captured output missing.".into());
    }
    let specialist = expected
        .specialist_id
        .as_deref()
        .unwrap_or("domain-analyst@10");
    captured_analyst_lane(
        specialist,
        expected.generation_policy.is_some(),
        expected.provider.mode,
    )?;
    if let Some(limits) = expected.run_limits {
        let minimum = llm::bounded::CompletionLimits::default()
            .values()
            .request_timeout_ms;
        if INVESTIGATION_V2_POLICY
            .with_active_seconds(limits.active_seconds)
            .is_none()
            || !(minimum..=llm::bounded::MAX_REQUEST_TIMEOUT_MS)
                .contains(&limits.request_timeout_ms)
        {
            return Err("invalid frozen run limits".into());
        }
    }
    capture::validate_binding(&expected.binding)?;
    let definition = json(expected)?;
    if read(definition_path)? != definition
        || expected.selections.is_empty()
        || expected.selections.len() > 64
        || repeats_a_fact(&expected.selections)
        || !expected.provider.available
        || (expected.provider.mode == InvestigationProviderMode::Cloud
            && super::cloud_run::pinned(&expected.provider).is_err())
    {
        return Err("independent capture definition required".into());
    }
    Ok(BoundDefinition {
        definition,
        specialist,
    })
}

/// The captured analyst identities and the local finish lane (#1335: @28
/// takes @27's lane; the frozen #598 negative cases stay pinned to @27).
fn captured_analyst_lane(
    specialist: &str,
    local_finish: bool,
    mode: InvestigationProviderMode,
) -> Result<(), String> {
    if !matches!(
        specialist,
        "domain-analyst@10"
            | "domain-analyst@11"
            | "domain-analyst@12"
            | "domain-analyst@13"
            | "domain-analyst@14"
            | "domain-analyst@15"
            | "domain-analyst@17"
            | "domain-analyst@18"
            | "domain-analyst@19"
            | "domain-analyst@20"
            | "domain-analyst@21"
            | "domain-analyst@22"
            | "domain-analyst@23"
            | "domain-analyst@24"
            | "domain-analyst@25"
            | "domain-analyst@26"
            | "domain-analyst@27"
            | "domain-analyst@28"
            | "domain-analyst@29"
    ) {
        return Err("unsupported captured analyst specialist".into());
    }
    // ADR-0085: @29's tool-calling lane is local until the Claude lane lands.
    if specialist == "domain-analyst@29" && mode != InvestigationProviderMode::Local {
        return Err("tool-calling analyst is local only".into());
    }
    if local_finish
        && (!matches!(
            specialist,
            "domain-analyst@26" | "domain-analyst@27" | "domain-analyst@28" | "domain-analyst@29"
        ) || mode != InvestigationProviderMode::Local)
    {
        return Err("invalid local finish policy lane".into());
    }
    Ok(())
}

#[test]
fn captured_analyst_lane_admits_analyst28_like_analyst27() {
    // AC-0835 (#1335): acceptance preparation accepts @28 exactly as @27.
    use InvestigationProviderMode::{Cloud, Local};
    for role in [
        "domain-analyst@26",
        "domain-analyst@27",
        "domain-analyst@28",
    ] {
        captured_analyst_lane(role, false, Cloud).unwrap();
        captured_analyst_lane(role, true, Local).unwrap();
        assert!(captured_analyst_lane(role, true, Cloud).is_err());
    }
    captured_analyst_lane("domain-analyst@25", false, Local).unwrap();
    assert!(captured_analyst_lane("domain-analyst@25", true, Local).is_err());
    // AC-1429 (ADR-0085): @29 runs locally, with or without the local finish lane.
    captured_analyst_lane("domain-analyst@29", false, Local).unwrap();
    captured_analyst_lane("domain-analyst@29", true, Local).unwrap();
    assert!(captured_analyst_lane("domain-analyst@29", false, Cloud).is_err());
    for role in [
        "domain-analyst@30",
        "domain-analyst@31",
        "domain-analyst@16",
        "evidence-auditor@14",
    ] {
        assert!(captured_analyst_lane(role, false, Local).is_err(), "{role}");
    }
}

/// No external callback enters this concrete fixture owner.
pub(in super::super) fn prepare_fixture_bound(
    paths: [&Path; 4],
    expected: &Definition,
) -> Result<serde_json::Value, String> {
    let validated = validate_bound(paths, expected, true)?;
    let basis = vendure::reopen_basis(
        paths[0],
        paths[1],
        paths[2],
        paths[3],
        vendure::InputPolicy::OfflineFixture,
    )?;
    let owner = vendure::PreparationApp::for_reopen(&basis)?;
    owner.bound(&basis, paths, expected, validated)
}

pub(in super::super) fn prepare_fixture_on_app(
    app: &tauri::App<tauri::test::MockRuntime>,
    basis: &vendure::ReopenPreparation<'_>,
    paths: [&Path; 4],
    expected: &Definition,
    validated: BoundDefinition<'_>,
) -> Result<serde_json::Value, String> {
    bound_with_scope(
        paths,
        expected,
        vendure::InputPolicy::OfflineFixture,
        true,
        Some((basis, app)),
        validated,
        |_, _, _, manifest, _| Ok(manifest.clone()),
    )
}

#[allow(clippy::too_many_arguments)]
fn reopen_with_scope<T>(
    scoped: Option<(
        &vendure::ReopenPreparation<'_>,
        &tauri::App<tauri::test::MockRuntime>,
    )>,
    config: &Path,
    preparation: &Path,
    proposal: &Path,
    output: &Path,
    policy: vendure::InputPolicy,
    use_preparation: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &vendure::report::GraphPreparation,
        &serde_json::Value,
        &[u8],
    ) -> Result<T, String>,
) -> Result<T, String> {
    match scoped {
        Some((basis, app)) => vendure::use_reopen_app(basis, app, use_preparation),
        None => vendure::reopen_preparation(
            config,
            preparation,
            proposal,
            output,
            policy,
            use_preparation,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn bound_with_scope<T>(
    paths: [&Path; 4],
    expected: &Definition,
    policy: vendure::InputPolicy,
    preparing: bool,
    scoped: Option<(
        &vendure::ReopenPreparation<'_>,
        &tauri::App<tauri::test::MockRuntime>,
    )>,
    validated: BoundDefinition<'_>,
    use_task: impl FnOnce(
        &tauri::App<tauri::test::MockRuntime>,
        &StagedObligationRequest,
        &str,
        &serde_json::Value,
        &Path,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let [config, preparation, definition_path, output] = paths;
    let BoundDefinition {
        definition,
        specialist,
    } = validated;
    let mut unsealed_manifest = None;
    let result = reopen_with_scope(
        scoped,
        config,
        preparation,
        definition_path,
        output,
        policy,
        |app, graph, saved, receipt| {
            if sha(receipt) != expected.preparation_sha256 {
                return Err("independent preparation digest mismatch".into());
            }
            if let vendure::InputPolicy::NegativeCases598(case) = policy {
                super::negative_cases::validate_graph(case, expected, graph)?;
            }
            let state = app.state::<crate::AppState>();
            let inventory = crate::investigations::completion_choices::control_inventory(
                app.handle(),
                &state,
                &expected.scope,
            )?;
            let context = match &inventory {
                Obligation::Inventory { target, .. } => target.context_id.clone(),
                _ => return Err("production inventory required".into()),
            };
            let mut obligations = vec![inventory];
            let mut guards = Vec::new();
            for selection in &expected.selections {
                let (target, guard) = crate::investigations::inspection::resolve(
                    &state,
                    &selection.fact,
                    &selection.graph_snapshot_id,
                    &selection.receipt_id,
                    selection.ordinal,
                )?;
                if target.registered_source_id != graph.source_id
                    || target.captured.file.capture_id != graph.capture_id
                {
                    return Err("selection outside prepared capture".into());
                }
                let mut obligation = Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(target),
                };
                let identity = obligation.content_id().map_err(|e| e.to_string())?;
                if let Obligation::Source { obligation_id, .. } = &mut obligation {
                    *obligation_id = identity;
                }
                obligations.push(obligation);
                guards.push(guard);
            }
            let request = StagedObligationRequest {
                schema_version: 7,
                specialist_id: specialist.into(),
                request_nonce: expected.request_nonce.clone(),
                question: expected.question.clone(),
                scope: expected.scope.clone(),
                obligations: ObligationEnvelope {
                    schema_version: 2,
                    contract: "investigation-completion-obligations@2".into(),
                    obligations,
                },
                inspection_requirements: None,
                provider_mode: expected.provider.mode,
                limit_profile: INVESTIGATION_V2_POLICY.profile().into(),
                expected_graph_revision: Some(graph.graph_snapshot_id.clone()),
                expected_context_id: context,
                conversation_id: None,
                parent_id: None,
            }
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .map_err(|e| e.to_string())?;
            crate::investigations::completion_choices::validate_request(
                app.handle(),
                &state,
                &request,
            )?;
            let wire = json(&request)?;
            let mut jobs = state.jobs.lock().map_err(|_| "jobs unavailable")?;
            if !output.exists()
                && jobs
                    .existing_investigation_completion(&request)
                    .map_err(|e| e.to_string())?
                    .is_some()
            {
                return Err("fresh capture nonce already consumed".into());
            }
            if preparing && !output.exists() && expected.start_sha256.is_some() {
                return Err("Fresh start cannot reuse a prior start pin".into());
            }
            let fresh = if !preparing {
                false
            } else {
                match std::fs::create_dir(output) {
                    Ok(()) => {
                        sync_directory(output.parent().ok_or("missing capture output parent")?)?;
                        true
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
                    Err(_) => return Err("capture output unavailable".into()),
                }
            };
            if fresh {
                write(output, "definition.json", expected)?;
                write(output, "analyst-request.json", &request)?;
            } else if read(&output.join("definition.json"))? != definition
                || read(&output.join("analyst-request.json"))? != wire
            {
                return Err("frozen capture authority differs".into());
            }
            if read(definition_path)? != definition {
                return Err("caller definition changed".into());
            }
            let (task, elapsed) = if fresh {
                let clock = std::time::Instant::now();
                let started = if expected.generation_policy.is_some() {
                    jobs.start_local_finish_investigation(
                        &request,
                        Some(&expected.provider),
                        expected
                            .run_limits
                            .map(|v| (v.active_seconds, v.request_timeout_ms)),
                    )
                } else {
                    match expected.run_limits {
                        Some(limits) => jobs.start_configured_next_prompt_investigation(
                            &request,
                            Some(&expected.provider),
                            limits.active_seconds,
                            limits.request_timeout_ms,
                        ),
                        None => {
                            jobs.start_next_prompt_investigation(&request, Some(&expected.provider))
                        }
                    }
                }
                .map_err(|e| e.to_string())?;
                let elapsed = clock.elapsed();
                if !started.created {
                    return Err("fresh capture nonce already consumed".into());
                }
                (started.detail, Some(elapsed))
            } else {
                (
                    (if expected.generation_policy.is_some() {
                        jobs.existing_local_finish_investigation(&request)
                    } else {
                        jobs.existing_investigation_completion(&request)
                    })
                    .map_err(|e| e.to_string())?
                    .ok_or("frozen start absent; explicit recovery required")?,
                    None,
                )
            };
            let saved_limits = jobs
                .captured_run_limits(&task.summary.investigation_id)
                .map_err(|e| e.to_string())?;
            if task.limits.generation_policy != expected.generation_policy {
                return Err("frozen generation policy differs".into());
            }
            if saved_limits
                != expected
                    .run_limits
                    .map(|v| (v.active_seconds, v.request_timeout_ms))
                || expected
                    .run_limits
                    .is_some_and(|v| task.limits.active_seconds != v.active_seconds)
            {
                return Err("frozen run limits differ from saved task".into());
            }
            let start = serde_json::json!({"definition_sha256":sha(&definition), "request_sha256":sha(&wire),
                "intent_hash":request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
                "analyst_id":task.summary.investigation_id});
            super::start_timing::seal_or_validate(
                output,
                &start,
                "analyst",
                &sha(&wire),
                elapsed,
                expected.start_sha256.as_deref(),
            )?;
            if task.provider != expected.provider
                || (preparing
                    && (task.summary.status != InvestigationStatus::Queued
                        || task.usage.model_invocations != 0))
            {
                return Err("stored capture task differs or was executed".into());
            }
            let mut manifest = serde_json::json!({"schema_version":1,"status":"captured_analyst_preparation_only",
            "definition_sha256":sha(&definition),"preparation_sha256":expected.preparation_sha256,
            "selection_sha256":sha(&json(&expected.selections)?),"question_sha256":sha(expected.question.as_bytes()),
            "source_id":graph.source_id,"capture_id":graph.capture_id,"graph_snapshot_id":graph.graph_snapshot_id,
            "profile":saved["profile"],"native_config_sha256":saved["native_config_sha256"],
            "request_sha256":sha(&wire),"intent_hash":request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
            "analyst_id":task.summary.investigation_id,"conversation_id":task.summary.conversation_id,
            "provider_calls":0,"acceptance_passed":false});
            if let Some(limits) = expected.run_limits {
                manifest["run_limits"] = serde_json::to_value(limits).map_err(|e| e.to_string())?;
            }
            let path = output.join("captured-analyst.json");
            if path.exists() && read(&path)? != json(&manifest)? {
                return Err("capture manifest differs".into());
            }
            if !path.exists() {
                if !preparing {
                    return Err("Captured manifest missing.".into());
                }
                unsealed_manifest = Some(manifest.clone());
            }
            drop(jobs);
            let app_data = Path::new(
                saved["profile"]["app"]["appDirectoriesOverride"]["data"]
                    .as_str()
                    .ok_or("validated profile data missing")?,
            );
            use_task(
                app,
                &request,
                &task.summary.investigation_id,
                &manifest,
                app_data,
            )
        },
    )?;
    if read(definition_path)? != definition {
        return Err("caller definition changed".into());
    }
    if let Some(manifest) = unsealed_manifest {
        write(output, "captured-analyst.json", &manifest)?;
    }
    Ok(result)
}
#[test]
#[ignore = "manual existing Vendure profile preparation only; no provider"]
fn vendure_prepare_captured_analyst() {
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let definition_path = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let mut expected: Definition = agents::investigation::decode_record(
        std::str::from_utf8(&read(&definition_path).unwrap()).unwrap(),
    )
    .unwrap();
    expected.start_sha256 = std::env::var("CARTOGRAPH_VENDURE_ANALYST_START_SHA256").ok();
    prepare_bound(
        &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
        &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
        &definition_path,
        &path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT"),
        &expected,
        vendure::InputPolicy::FrozenVendure,
    )
    .unwrap();
}
