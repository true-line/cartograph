//! Captured child staging only; local review bytes bind content, never authenticate a reviewer.
use super::super::completion::{control, execution};
use super::{PreparationBinding, analyst};
use agents::investigation::*;
use core_prov::content_hash;
use llm::LlmProvider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};
use tauri::Manager;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    pub binding: PreparationBinding,
    /// Out-of-band caller authority, intentionally absent from frozen definition bytes.
    /// None permits fresh staging only; every reopen needs the independently retained pin.
    #[serde(skip)]
    pub start_sha256: Option<String>,
    /// Omitted only for historical supported-parent definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_completion_outcome: Option<String>,
    pub parent_revision: u64,
    pub parent_result_id: String,
    pub parent_result_hash: String,
    pub parent_receipt_sha256: String,
    pub review_issue: u64,
    pub rubric_sha256: String,
    pub review_sha256: String,
    pub request: StagedObligationRequest,
    pub specialist: InvestigationSpecialist,
    pub provider: InvestigationProvider,
}
fn parent_outcome_matches(fresh: &Value, declared: Option<&str>) -> bool {
    let actual = fresh["detail"]["completion_outcome"].as_str();
    match (actual, declared) {
        (Some("supported"), None | Some("supported")) => true,
        (
            Some("findings_withheld_after_correction"),
            Some("findings_withheld_after_correction"),
        ) => {
            fresh["result"]["completion"]["outcome"] == "findings_withheld_after_correction"
                && fresh["result"]["completion"]["withholding"]["disposition"]
                    == "findings_withheld_after_correction"
                && fresh["result"]["findings"]
                    .as_array()
                    .is_some_and(|f| !f.is_empty())
        }
        _ => false,
    }
}
fn sha(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    super::super::vendure::read_preparation_artifact(path, 2 * 1024 * 1024)
}
fn wire(value: &impl Serialize) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, 2 * 1024 * 1024).map_err(|e| e.to_string())
}
pub(super) struct Artifacts<'a> {
    pub parent_output: &'a Path,
    pub output: &'a Path,
    pub definition_path: &'a Path,
    pub review: &'a [u8],
}
/// Caller obtains `parent` only inside the existing captured `reopen_bound` callback.
/// The independently supplied definition and fresh external verification are prerequisites.
pub(super) fn stage(
    parent: &analyst::Authority<'_>,
    artifacts: &Artifacts<'_>,
    expected: &Definition,
    preparing: bool,
) -> Result<String, String> {
    let Artifacts {
        parent_output,
        output,
        definition_path,
        review,
    } = *artifacts;
    let definition = wire(expected)?;
    if read(definition_path)? != definition {
        return Err("Auditor definition changed.".into());
    }
    let receipt = read(&parent_output.join("analyst-receipt.json"))?;
    // Every check below reads the reviewed receipt; the fresh production read
    // after them must reproduce it byte for byte, so each holds for the saved
    // parent too. Cheap mismatches are refused before that full read.
    let saved: Value = serde_json::from_slice(&receipt).map_err(|e| e.to_string())?;
    if sha(&receipt) != expected.parent_receipt_sha256
        || saved["detail"]["revision"] != expected.parent_revision
        || saved["detail"]["status"] != "completed"
        || !parent_outcome_matches(&saved, expected.parent_completion_outcome.as_deref())
        || saved["detail"]["obligation_status"] != "satisfied"
        || saved["result"]["result_id"] != expected.parent_result_id
    {
        return Err("Reviewed saved parent differs or is not supported.".into());
    }
    let result: InvestigationResult =
        serde_json::from_value(saved["result"].clone()).map_err(|e| e.to_string())?;
    if content_hash(&wire(&result)?) != expected.parent_result_hash
        || sha(review) != expected.review_sha256
    {
        return Err("Parent result or independent review changed.".into());
    }
    control::validate_review_attestation(
        [
            &expected.binding.protocol_hash,
            &expected.binding.run_id,
            &expected.parent_receipt_sha256,
            &expected.rubric_sha256,
        ],
        expected.review_issue,
        review,
    )?;
    let request = &expected.request;
    let policy = &INVESTIGATION_V2_POLICY;
    let normalized = request
        .normalized_with_policy(policy)
        .map_err(|e| e.to_string())?;
    let sources = request
        .obligations
        .sources_with_policy(policy)
        .map_err(|e| e.to_string())?;
    let conversation = request
        .conversation_id
        .as_deref()
        .ok_or("Missing conversation")?;
    if expected.review_issue == 0
        || expected.binding != *parent.binding
        || wire(&normalized)? != wire(request)?
        || !matches!(
            request.specialist_id.as_str(),
            "evidence-auditor@11"
                | "evidence-auditor@12"
                | "evidence-auditor@13"
                | "evidence-auditor@14"
        )
        || !request
            .obligations
            .obligations
            .iter()
            .any(|o| matches!(o, obligations::Obligation::Inventory { .. }))
        || sources.is_empty()
        || expected.specialist
            != (if request.specialist_id == "evidence-auditor@14" {
                SpecialistId::EvidenceAuditorV14
            } else if request.specialist_id == "evidence-auditor@13" {
                SpecialistId::EvidenceAuditorV13
            } else if request.specialist_id == "evidence-auditor@12" {
                SpecialistId::EvidenceAuditorV12
            } else {
                SpecialistId::EvidenceAuditorV11
            })
            .definition()
            .map_err(|e| e.to_string())?
        || request.parent_id.as_deref() != Some(parent.analyst_id)
        || Some(conversation) != saved["detail"]["conversation_id"].as_str()
        || request.scope != parent.request.scope
        || request.expected_graph_revision != parent.request.expected_graph_revision
        || request.expected_context_id != parent.request.expected_context_id
        || request.provider_mode != expected.provider.mode
        || !expected.provider.available
    {
        return Err("Auditor parent, scope, definition or provider differs.".into());
    }
    if receipt != analyst::bytes(&analyst::collect_bound(parent)?)? {
        return Err("Reviewed saved parent differs or is not supported.".into());
    }
    if expected.provider.mode == InvestigationProviderMode::Cloud {
        super::cloud_run::pinned(&expected.provider)?;
    }
    let state = parent.app.state::<crate::AppState>();
    crate::investigations::completion_choices::validate_request(
        parent.app.handle(),
        &state,
        request,
    )?;
    let mut guards = vec![];
    for source in sources {
        let (actual, guard) = crate::investigations::inspection::resolve(
            &state,
            &source.fact,
            &source.graph_snapshot_id,
            &source.receipt_id,
            source.receipt_inventory_index as usize,
        )?;
        if actual != source {
            return Err("Independent auditor source changed.".into());
        }
        guards.push(guard);
    }
    let mut jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
    // Production reader preserves exact status/uncertainty under the fresh child history policy.
    jobs.investigation_next_parent_history_with_policy(
        parent.analyst_id,
        conversation,
        &INVESTIGATION_V2_POLICY
            .with_parent_history_32768()
            .ok_or("History policy unavailable")?,
    )
    .map_err(|e| e.to_string())?;
    let fresh_output = preparing && !output.exists();
    if fresh_output {
        if expected.start_sha256.is_some() {
            return Err("Fresh start cannot reuse a prior start pin".into());
        }
        if jobs
            .existing_investigation_completion(request)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("Auditor nonce consumed.".into());
        }
        std::fs::create_dir(output).map_err(|e| e.to_string())?;
        super::vendure_prepare::sync_directory(output.parent().ok_or("Missing output parent")?)?;
        analyst::exclusive(&output.join("definition.json"), &definition)?;
        analyst::exclusive(&output.join("review-attestation.json"), review)?;
    } else if read(&output.join("definition.json"))? != definition
        || read(&output.join("review-attestation.json"))? != review
    {
        return Err("Frozen auditor definition changed.".into());
    }
    let (detail, elapsed) = if fresh_output {
        let clock = std::time::Instant::now();
        let started = jobs
            .start_next_prompt_investigation(request, Some(&expected.provider))
            .map_err(|e| e.to_string())?;
        let elapsed = clock.elapsed();
        if !started.created {
            return Err("Auditor nonce consumed.".into());
        }
        (started.detail, Some(elapsed))
    } else {
        (
            jobs.existing_investigation_completion(request)
                .map_err(|e| e.to_string())?
                .ok_or("Auditor start absent; explicit recovery required")?,
            None,
        )
    };
    let id = detail.summary.investigation_id;
    let start = json!({"definition_sha256":sha(&definition),"auditor_id":id});
    super::start_timing::seal_or_validate(
        output,
        &start,
        "auditor",
        &sha(&wire(request)?),
        elapsed,
        expected.start_sha256.as_deref(),
    )?;
    if detail.provider != expected.provider
        || (preparing
            && (detail.summary.status != InvestigationStatus::Queued
                || detail.usage.model_invocations != 0))
    {
        return Err("Auditor task already executed or rebound.".into());
    }
    Ok(id)
}

fn authority<'a>(
    parent: &analyst::Authority<'a>,
    expected: &'a Definition,
    id: &'a str,
    hash: &'a str,
) -> analyst::Authority<'a> {
    analyst::Authority {
        app: parent.app,
        app_data: parent.app_data,
        analyst_id: id,
        request: &expected.request,
        binding: &expected.binding,
        manifest_hash: hash,
        provider: &expected.provider,
        run_limits: None,
    }
}
/// Explicit dispatch only after the caller freshly authenticates review evidence externally.
/// Reopen uses the same staged authority but has no provider or dispatch callback.
pub(super) fn execute(
    parent: &analyst::Authority<'_>,
    expected: &Definition,
    artifacts: &Artifacts<'_>,
    provider: Arc<dyn LlmProvider>,
) -> Result<Value, String> {
    execute_with_runner(
        parent,
        expected,
        artifacts,
        provider,
        |output, authority, observed| analyst::execute_role(output, authority, observed, "auditor"),
    )
}
fn execute_with_runner(
    parent: &analyst::Authority<'_>,
    expected: &Definition,
    artifacts: &Artifacts<'_>,
    provider: Arc<dyn LlmProvider>,
    run: impl FnOnce(&Path, &analyst::Authority<'_>, Arc<dyn LlmProvider>) -> Result<(), String>,
) -> Result<Value, String> {
    let id = stage(parent, artifacts, expected, false)?;
    let output = artifacts.output;
    let observed = Arc::new(execution::ObservedProvider::next(
        provider,
        expected.request.expected_context_id.clone(),
    ));
    let hash = content_hash(&wire(expected)?);
    run(
        output,
        &authority(parent, expected, &id, &hash),
        observed.clone(),
    )?;
    analyst::exclusive(
        &output.join("auditor-observations.json"),
        &wire(&observed.observations()?)?,
    )?;
    let receipt = collect(parent, expected, &id, output)?;
    analyst::exclusive(
        &output.join("auditor-receipt.json"),
        &analyst::bytes(&receipt)?,
    )?;
    Ok(receipt)
}
fn collect(
    parent: &analyst::Authority<'_>,
    expected: &Definition,
    id: &str,
    output: &Path,
) -> Result<Value, String> {
    if read(&output.join("definition.json"))? != wire(expected)? {
        return Err("Frozen auditor definition changed.".into());
    }
    let hash = content_hash(&wire(expected)?);
    let authority = authority(parent, expected, id, &hash);
    let mut receipt = analyst::collect_role(&authority, "auditor")?;
    let observations: Vec<execution::InvocationObservation> =
        serde_json::from_slice(&read(&output.join("auditor-observations.json"))?)
            .map_err(|e| e.to_string())?;
    let db = parent.app_data.join("state.db");
    let steps = execution::rows(
        &db,
        id,
        "investigation_invocations",
        INVESTIGATION_V2_POLICY.limits().model_invocations as usize,
        2 * 1024 * 1024,
    )?;
    let ledgers = execution::rows(
        &db,
        id,
        "investigation_inputs",
        INVESTIGATION_V2_POLICY.journal_events() as usize,
        2 * 1024 * 1024,
    )?;
    let detail: InvestigationDetail =
        serde_json::from_value(receipt["detail"].clone()).map_err(|e| e.to_string())?;
    let policy =
        crate::jobs::investigations::policy_for_detail(&detail).map_err(|e| e.to_string())?;
    let (matching, bound, reviewable) = execution::sealed_checks_with_policy(
        &expected.request,
        &steps,
        &ledgers,
        &observations,
        &policy,
    )?;
    let parent_exact = !observations.is_empty()
        && observations.iter().all(|o| {
            o.parent_result_hashes.len() == 1
                && o.parent_result_hashes.get(&expected.parent_result_id)
                    == Some(&expected.parent_result_hash)
        });
    receipt["observations"] = serde_json::to_value(&observations).map_err(|e| e.to_string())?;
    receipt["mechanical_checks"] = json!({"payload_disclosure_match":matching,"request_context_bound":bound,
        "representation_reviewable":reviewable,"exact_saved_parent":parent_exact,
        "own_source_observed":observations.iter().any(|o|!o.source_hashes.is_empty()),
        "own_inventory_observed":observations.iter().any(|o|!o.inventory_hashes.is_empty())});
    receipt["auditor"] = json!("independent_semantic_review_pending");
    receipt["definition_sha256"] = json!(sha(&wire(expected)?));
    if let Some(outcome) = &expected.parent_completion_outcome {
        receipt["parent_completion_outcome"] = json!(outcome);
    }
    Ok(receipt)
}
pub(super) fn reopen(
    parent: &analyst::Authority<'_>,
    expected: &Definition,
    artifacts: &Artifacts<'_>,
) -> Result<Value, String> {
    let id = stage(parent, artifacts, expected, false)?;
    let output = artifacts.output;
    let fresh = collect(parent, expected, &id, output)?;
    let invocations = fresh["invocations"]
        .as_array()
        .ok_or("Auditor invocation journal missing.")?;
    let observations = fresh["observations"]
        .as_array()
        .ok_or("Auditor observations missing.")?;
    // Quality/acquisition checks may honestly fail for incomplete outcomes. Binding cannot.
    // Before any invocation, there is no supplied payload or parent observation to bind.
    if invocations.len() != observations.len()
        || (!invocations.is_empty()
            && [
                "payload_disclosure_match",
                "request_context_bound",
                "exact_saved_parent",
            ]
            .iter()
            .any(|key| fresh["mechanical_checks"][*key] != true))
    {
        return Err("Auditor observation binding changed.".into());
    }
    if read(&output.join("auditor-receipt.json"))? != analyst::bytes(&fresh)? {
        return Err("Auditor durable receipt changed.".into());
    }
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigations::worker::tests::completion_tests::{
        expanded_auditor_scripted_provider, expanded_scripted_provider,
    };

    use crate::investigations::worker::tests::{Mode, Scripted};
    #[derive(Clone, Copy)]
    enum Scenario {
        Stage,
        CloudStage,
        CloudComplete,
        WithheldCloud,
        Withheld,
        Supported,
        Unknown,
        Refused,
    }

    // AC-0466: synthetic authentication-shaped bytes prove content gates only.
    #[test]
    fn captured_auditor_preparation_binds_saved_parent_and_stages_without_dispatch() {
        exercise_auditor(Scenario::Stage);
    }
    // AC-0463: actual child inputs contain independently acquired evidence and saved T3 history.
    #[test]
    fn captured_auditor_stages_once_observes_saved_parent_and_reopens() {
        for scenario in [Scenario::Supported, Scenario::Unknown] {
            exercise_auditor(scenario);
        }
    }
    // AC-0463: unacquired evidence and refused finish remain honest, reopenable outcomes.
    #[test]
    fn captured_auditor_refused_observations_reopen_and_reject_paired_rewrites() {
        exercise_auditor(Scenario::Refused);
    }
    // AC-0577: real saved-parent staging, zero dispatch, exact hosted policy recovery.
    #[test]
    fn captured_cloud_auditor_stages_pinned_policy_without_dispatch() {
        exercise_auditor(Scenario::CloudStage);
    }
    // AC-0577: never relabel withheld or accept an unknown/empty disposition.
    #[test]
    fn captured_auditor_withheld_parent_requires_explicit_matching_disposition() {
        let mut receipt = json!({"detail":{"completion_outcome":"findings_withheld_after_correction"},
            "result":{"findings":[{"finding_id":"saved"}],"completion":{
                "outcome":"findings_withheld_after_correction",
                "withholding":{"disposition":"findings_withheld_after_correction"}}}});
        let outcome = Some("findings_withheld_after_correction");
        assert!(parent_outcome_matches(&receipt, outcome));
        for declared in [None, Some("supported"), Some("insufficient_evidence")] {
            assert!(!parent_outcome_matches(&receipt, declared));
        }
        receipt["result"]["completion"]["withholding"]["disposition"] = json!("supported");
        assert!(!parent_outcome_matches(&receipt, outcome));
        receipt["result"]["completion"]["withholding"]["disposition"] =
            json!("findings_withheld_after_correction");
        receipt["result"]["findings"] = json!([]);
        assert!(!parent_outcome_matches(&receipt, outcome));
        for actual in ["failed", "insufficient_evidence", "unknown"] {
            receipt["detail"]["completion_outcome"] = json!(actual);
            assert!(!parent_outcome_matches(&receipt, Some(actual)));
        }
    }
    // AC-0577: durable production withholding, bound history and child receipt/reopen.
    #[test]
    fn captured_auditor_withheld_parent_history_and_receipt_reopen_without_relabelling() {
        exercise_auditor(Scenario::Withheld);
    }
    // AC-0582: finish/publication and captured receipt replay on both effective policies.
    #[test]
    fn captured_hosted_auditor_large_history_finishes_and_receipt_reopens() {
        exercise_auditor(Scenario::CloudComplete);
    }
    #[test]
    fn captured_auditor12_large_history_finishes_and_receipt_reopens() {
        exercise_auditor_role(Scenario::CloudComplete, SpecialistId::EvidenceAuditorV12);
    }
    fn exercise_auditor(scenario: Scenario) {
        exercise_auditor_role(scenario, SpecialistId::EvidenceAuditorV11);
    }
    #[path = "auditor13_tests.rs"]
    mod auditor13;
    fn exercise_auditor_role(scenario: Scenario, role: SpecialistId) {
        exercise_auditor_configured(scenario, role, None);
    }
    fn exercise_auditor_configured(
        scenario: Scenario,
        role: SpecialistId,
        rejection: Option<auditor13::Rejection>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let provider = if matches!(scenario, Scenario::CloudComplete) {
            crate::investigations::worker::tests::completion_tests::expanded_large_scripted_provider(
            )
        } else if matches!(scenario, Scenario::Withheld | Scenario::WithheldCloud) {
            crate::investigations::worker::tests::completion_tests::expanded_withheld_scripted_provider()
        } else {
            expanded_scripted_provider(true)
        };
        let descriptor = crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Local,
        )
        .unwrap();
        let mut prepared = super::super::prepare(
            directory.path(),
            &descriptor,
            super::super::tests::binding(),
        )
        .unwrap();
        if matches!(scenario, Scenario::Withheld | Scenario::WithheldCloud) {
            prepared.analyst_request.specialist_id = "domain-analyst@25".into();
            prepared.analyst_request.request_nonce = "withheld-parent".into();
            prepared.analyst_request.conversation_id = Some(prepared.conversation_id.clone());
            let state = prepared.app.state::<crate::AppState>();
            let start = state
                .jobs
                .lock()
                .unwrap()
                .start_next_prompt_investigation(&prepared.analyst_request, Some(&descriptor))
                .unwrap();
            prepared.analyst_id = start.detail.summary.investigation_id;
            prepared.auditor_request.parent_id = Some(prepared.analyst_id.clone());
        }
        let parent = analyst::Authority::synthetic(&prepared);
        let receipt = analyst::execute_bound(directory.path(), &parent, provider).unwrap();
        let result: InvestigationResult =
            serde_json::from_value(receipt["result"].clone()).unwrap();
        let mut request = prepared.auditor_request.clone();
        request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
        let review_binding = json!({"schema":"cartograph-v7-semantic-review-binding@1",
            "protocol_sha256":parent.binding.protocol_hash,"run_id":parent.binding.run_id,
            "role":"analyst","receipt_sha256":sha(&read(&directory.path().join("analyst-receipt.json")).unwrap()),"rubric_sha256":"d".repeat(64)});
        let mut review_body = review_binding.clone();
        review_body["schema"] = json!("cartograph-v7-semantic-review@1");
        review_body["verdict"] = json!("pass");
        review_body["reasons"] = json!(["Synthetic content gate only."]);
        let review = wire(&json!({"schema":"cartograph-v7-semantic-review-attestation@1","binding":review_binding,
            "review":review_body,"comment":{"database_id":123,"url":"https://github.com/true-line/cartograph/issues/598#issuecomment-123",
                "created_at":"2026-10-29T00:00:00Z","body_sha256":"e".repeat(64)}})).unwrap();
        let mut expected = Definition {
            start_sha256: None,
            binding: parent.binding.clone(),
            parent_completion_outcome: if matches!(
                scenario,
                Scenario::Withheld | Scenario::WithheldCloud
            ) {
                assert_eq!(
                    receipt["detail"]["completion_outcome"],
                    "findings_withheld_after_correction"
                );
                Some("findings_withheld_after_correction".into())
            } else {
                None
            },
            parent_revision: receipt["detail"]["revision"].as_u64().unwrap(),
            parent_result_id: result.result_id.clone(),
            parent_result_hash: content_hash(&wire(&result).unwrap()),
            parent_receipt_sha256: review_binding["receipt_sha256"].as_str().unwrap().into(),
            review_issue: 598,
            rubric_sha256: "d".repeat(64),
            review_sha256: sha(&review),
            request,
            specialist: role.definition().unwrap(),
            provider: descriptor.clone(),
        };
        if matches!(
            scenario,
            Scenario::CloudStage | Scenario::CloudComplete | Scenario::WithheldCloud
        ) {
            expected.provider = InvestigationProvider {
                mode: InvestigationProviderMode::Cloud,
                provider_id: "openai:gpt-6.1-sol".into(),
                model: "gpt-6.1-sol".into(),
                endpoint: llm::openai::API_URL.into(),
                protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
                ..descriptor.clone()
            };
            expected.request.provider_mode = InvestigationProviderMode::Cloud;
        }
        let definition_path = directory.path().join("auditor-definition.json");
        std::fs::write(&definition_path, wire(&expected).unwrap()).unwrap();
        let output = directory.path().join("auditor");
        let artifacts = Artifacts {
            parent_output: directory.path(),
            output: &output,
            definition_path: &definition_path,
            review: &review,
        };
        for field in [
            "parent_revision",
            "parent_completion_outcome",
            "parent_result_hash",
            "parent_receipt_sha256",
            "review_sha256",
            "request",
        ] {
            let mut changed = serde_json::to_value(&expected).unwrap();
            match field {
                "parent_revision" => changed[field] = json!(expected.parent_revision + 1),
                "request" => changed[field]["parent_id"] = json!("investigation:foreign"),
                _ => changed[field] = json!("f".repeat(64)),
            }
            let changed: Definition = serde_json::from_value(changed).unwrap();
            std::fs::write(&definition_path, wire(&changed).unwrap()).unwrap();
            assert!(
                stage(&parent, &artifacts, &changed, true).is_err(),
                "{field}"
            );
            assert!(!output.exists());
        }
        let mut failed_review: Value = serde_json::from_slice(&review).unwrap();
        failed_review["review"]["verdict"] = json!("fail");
        let failed_review = wire(&failed_review).unwrap();
        let mut failed = expected.clone();
        failed.review_sha256 = sha(&failed_review);
        std::fs::write(&definition_path, wire(&failed).unwrap()).unwrap();
        let failed_artifacts = Artifacts {
            review: &failed_review,
            ..artifacts
        };
        assert!(stage(&parent, &failed_artifacts, &failed, true).is_err());
        assert!(!output.exists());
        if matches!(
            scenario,
            Scenario::CloudStage | Scenario::CloudComplete | Scenario::WithheldCloud
        ) {
            for mode_mismatch in [false, true] {
                let mut changed = expected.clone();
                if mode_mismatch {
                    changed.request.provider_mode = InvestigationProviderMode::Local;
                } else {
                    changed.provider.endpoint =
                        "https://example.invalid/v1/chat/completions".into();
                }
                std::fs::write(&definition_path, wire(&changed).unwrap()).unwrap();
                assert!(stage(&parent, &artifacts, &changed, true).is_err());
                assert!(!output.exists());
            }
        }
        std::fs::write(&definition_path, wire(&expected).unwrap()).unwrap();
        let id = stage(&parent, &artifacts, &expected, true).unwrap();
        if matches!(scenario, Scenario::Withheld | Scenario::WithheldCloud) {
            let state = parent.app.state::<crate::AppState>();
            let history = state
                .jobs
                .lock()
                .unwrap()
                .investigation_next_parent_history(
                    parent.analyst_id,
                    expected.request.conversation_id.as_deref().unwrap(),
                )
                .unwrap();
            assert!(history.history()[0].contains("findings_withheld_after_correction"));
            assert!(!history.history()[0].contains("MissingStrategy controls"));
        }
        expected.start_sha256 = Some(sha(&read(&output.join("start.json")).unwrap()));
        // AC-0463 (#1626): real auditor staging and reopen preserve timing.
        let timing_bytes = read(&output.join("start-latency.json")).unwrap();
        let timing: Value = serde_json::from_slice(&timing_bytes).unwrap();
        assert_eq!(timing["role"], "auditor");
        assert_eq!(
            timing["boundary"],
            "production_start_call_to_returned_task_id"
        );
        assert_eq!(
            timing["request_sha256"],
            sha(&wire(&expected.request).unwrap())
        );
        assert!(timing["elapsed_ns"].as_u64().is_some());
        assert_eq!(stage(&parent, &artifacts, &expected, true).unwrap(), id);
        assert_eq!(
            read(&output.join("start-latency.json")).unwrap(),
            timing_bytes
        );
        let db = prepared.app_data.join("state.db");
        let read_task = || {
            crate::jobs::JobStore::open(&db)
                .unwrap()
                .investigation(&id)
                .unwrap()
        };
        let first = read_task();
        assert_eq!(first.summary.status, InvestigationStatus::Queued);
        assert_eq!(first.summary.specialist_id, role);
        assert_eq!(
            first.summary.parent_id.as_deref(),
            Some(prepared.analyst_id.as_str())
        );
        assert_eq!(first.usage, InvestigationUsage::default());
        assert_eq!(read_task(), first);
        if matches!(
            scenario,
            Scenario::CloudStage | Scenario::CloudComplete | Scenario::WithheldCloud
        ) {
            let policy = crate::jobs::investigations::policy_for_detail(&first).unwrap();
            assert_eq!(policy.limits().generated_tokens_per_invocation, 16384);
            assert_eq!(policy.limits().generated_token_reservations, 1048576);
            assert_eq!(first.limits, policy.limits());
            assert!(!output.join("auditor-attempt.json").exists());
            assert!(!output.join("auditor-consent-log.jsonl").exists());
        }
        if !matches!(scenario, Scenario::Stage | Scenario::CloudStage) {
            let unknown = matches!(scenario, Scenario::Unknown);
            let refused = matches!(scenario, Scenario::Refused);
            let raw: Arc<dyn LlmProvider> = if matches!(
                scenario,
                Scenario::CloudComplete | Scenario::WithheldCloud
            ) {
                crate::investigations::worker::tests::completion_tests::full_hosted_auditor_provider(
                    matches!(scenario, Scenario::CloudComplete),
                )
            } else if unknown {
                Arc::new(Scripted::new(Mode::Unknown))
            } else if refused {
                expanded_scripted_provider(false)
            } else {
                expanded_auditor_scripted_provider()
            };
            let raw = if let Some(rejection) = rejection {
                auditor13::provider(raw, &result, rejection)
            } else {
                raw
            };
            let provider = Arc::new(execution::ObservedProvider::next(
                raw,
                expected.request.expected_context_id.clone(),
            ));
            let actual = if matches!(scenario, Scenario::CloudComplete | Scenario::WithheldCloud) {
                if matches!(scenario, Scenario::CloudComplete) {
                    assert!(first.limits.history_policy.is_some());
                }
                execute_with_runner(
                    &parent,
                    &expected,
                    &artifacts,
                    provider.clone(),
                    |output, authority, observed| {
                        let authorization = super::super::cloud_run::authorization(
                            authority.provider,
                            Some(authority.provider.provider_id.as_str()),
                            Some("32"),
                        )?;
                        let consent = super::super::cloud_run::Consent::grant(
                            authority.app.handle(),
                            authority.provider,
                            &authorization,
                            &output.join("auditor-consent-log.jsonl"),
                        )?;
                        analyst::execute_role_with_granted_consent(
                            output,
                            authority,
                            observed,
                            "auditor",
                            analyst::BoundaryClock::Real,
                            Some(consent),
                        )
                    },
                )
                .unwrap()
            } else {
                execute(&parent, &expected, &artifacts, provider.clone()).unwrap()
            };
            if let Some(rejection) = rejection {
                auditor13::verify(&actual, provider.as_ref(), role, rejection);
                let responses: Vec<crate::jobs::investigations::InvestigationResponseMeta> =
                    execution::rows(&db, &id, "investigation_responses", 64, 4096).unwrap();
                assert_eq!(responses.len(), provider.observations().unwrap().len());
                let last = responses.last().unwrap();
                let observed =
                    serde_json::to_value(provider.observations().unwrap().last().unwrap()).unwrap();
                assert_eq!(observed["response"]["response_hash"], last.response_hash);
                assert_eq!(last.reported_output_tokens, Some(3));
                let safe = serde_json::to_value(last).unwrap();
                assert!(safe.get("text").is_none() && safe.get("reasoning").is_none());
                assert_eq!(reopen(&parent, &expected, &artifacts).unwrap(), actual);
                assert!(execute(&parent, &expected, &artifacts, provider.clone()).is_err());
                assert_eq!(reopen(&parent, &expected, &artifacts).unwrap(), actual);
                return;
            }
            assert_eq!(actual["role"], "auditor");
            if matches!(scenario, Scenario::CloudComplete | Scenario::WithheldCloud) {
                assert_eq!(
                    actual["detail"]["obligation_status"],
                    "satisfied",
                    "status={} error={} usage={}",
                    actual["detail"]["status"],
                    actual["detail"]["error"],
                    actual["detail"]["usage"]
                );
                assert!(actual["detail"]["terminal_diagnostic"].is_null());
                let lines = read(&output.join("auditor-consent-log.jsonl")).unwrap();
                let events: Vec<Value> = std::str::from_utf8(&lines)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert!(events.iter().any(|e| e["event"] == "tier_consent_granted"));
                assert!(events.iter().any(|e| e["event"] == "tier_reverted_local"));
            }
            if matches!(scenario, Scenario::Withheld | Scenario::WithheldCloud) {
                assert_eq!(
                    actual["parent_completion_outcome"],
                    "findings_withheld_after_correction"
                );
            } else {
                assert!(actual.get("parent_completion_outcome").is_none());
            }
            assert_eq!(
                actual["detail"]["status"],
                if unknown {
                    "outcome_unknown"
                } else if refused {
                    "failed"
                } else {
                    "completed"
                },
                "{}",
                actual["detail"]["error"]
            );
            if unknown || refused {
                assert!(actual["result"].is_null());
                if refused {
                    assert_eq!(actual["finish_refusal_count"], 2);
                }
                assert_eq!(actual["mechanical_checks"]["own_source_observed"], false);
                assert_eq!(actual["mechanical_checks"]["own_inventory_observed"], false);
                assert_eq!(actual["mechanical_checks"]["exact_saved_parent"], true);
            } else {
                assert_eq!(actual["detail"]["completion_outcome"], "supported");
                assert_eq!(actual["result"]["knowledge_completeness"], "partial");
                for check in actual["mechanical_checks"].as_object().unwrap().values() {
                    assert_eq!(check, true);
                }
            }
            assert_eq!(
                actual["detail"]["specialist_id"],
                serde_json::to_value(role).unwrap()
            );
            assert_eq!(actual["detail"]["parent_id"], prepared.analyst_id);
            let observations = provider.observations().unwrap();
            assert_eq!(
                observations.len(),
                if unknown {
                    1
                } else if refused {
                    2
                } else {
                    4
                }
            );
            assert!(observations.iter().all(|o| o.history_hash.is_some()
                && o.parent_result_hashes.get(&expected.parent_result_id)
                    == Some(&expected.parent_result_hash)));
            if !unknown && !refused {
                assert!(observations.iter().any(|o| !o.source_hashes.is_empty()));
                assert!(observations.iter().any(|o| !o.inventory_hashes.is_empty()));
            }
            assert_eq!(reopen(&parent, &expected, &artifacts).unwrap(), actual);
            assert!(execute(&parent, &expected, &artifacts, provider.clone()).is_err());
            assert_eq!(provider.observations().unwrap(), observations);
            assert_eq!(reopen(&parent, &expected, &artifacts).unwrap(), actual);
            let saved_observations = read(&output.join("auditor-observations.json")).unwrap();
            let saved_receipt = read(&output.join("auditor-receipt.json")).unwrap();
            for field in [
                "empty",
                "payload",
                "context",
                "ledger",
                "source",
                "inventory",
                "history",
                "parent",
            ] {
                let mut changed = observations.clone();
                if field == "empty" {
                    changed.clear();
                } else {
                    let last = changed.last_mut().unwrap();
                    match field {
                        "payload" => last.payload_hash = "f".repeat(64),
                        "context" => last.context_id = "foreign-context".into(),
                        "ledger" => last.input_ledger_hash = "f".repeat(64),
                        "source" => {
                            last.source_hashes
                                .insert("foreign-source".into(), "f".repeat(64));
                        }
                        "inventory" => {
                            last.inventory_hashes
                                .insert("foreign-inventory".into(), "f".repeat(64));
                        }
                        "history" => last.history_hash = Some("f".repeat(64)),
                        "parent" => {
                            last.parent_result_hashes
                                .insert("foreign-parent".into(), "f".repeat(64));
                        }
                        _ => unreachable!(),
                    }
                }
                std::fs::write(
                    output.join("auditor-observations.json"),
                    wire(&changed).unwrap(),
                )
                .unwrap();
                // Forge the matching receipt too: byte equality alone previously accepted this pair.
                let forged = collect(&parent, &expected, &id, &output).unwrap();
                std::fs::write(
                    output.join("auditor-receipt.json"),
                    analyst::bytes(&forged).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    reopen(&parent, &expected, &artifacts).unwrap_err(),
                    "Auditor observation binding changed.",
                    "{field}"
                );
            }
            std::fs::write(output.join("auditor-observations.json"), saved_observations).unwrap();
            std::fs::write(output.join("auditor-receipt.json"), saved_receipt).unwrap();
            assert_eq!(reopen(&parent, &expected, &artifacts).unwrap(), actual);
            assert_eq!(provider.observations().unwrap(), observations);
            return;
        }
        std::fs::write(output.join("definition.json"), b"{}").unwrap();
        assert!(stage(&parent, &artifacts, &expected, true).is_err());
        assert_eq!(read_task(), first);
        let db = rusqlite::Connection::open(db).unwrap();
        let tasks: i64 = db
            .query_row("SELECT count(*) FROM investigation_tasks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            tasks, 2,
            "the saved parent and exactly one unexecuted child"
        );
    }
}
