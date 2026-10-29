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
    let fresh = analyst::collect_bound(parent)?;
    let receipt = read(&parent_output.join("analyst-receipt.json"))?;
    if receipt != analyst::bytes(&fresh)?
        || sha(&receipt) != expected.parent_receipt_sha256
        || fresh["detail"]["revision"] != expected.parent_revision
        || fresh["detail"]["status"] != "completed"
        || fresh["detail"]["completion_outcome"] != "supported"
        || fresh["detail"]["obligation_status"] != "satisfied"
        || fresh["result"]["result_id"] != expected.parent_result_id
    {
        return Err("Reviewed saved parent differs or is not supported.".into());
    }
    let result: InvestigationResult =
        serde_json::from_value(fresh["result"].clone()).map_err(|e| e.to_string())?;
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
        || request.specialist_id != "evidence-auditor@11"
        || !request
            .obligations
            .obligations
            .iter()
            .any(|o| matches!(o, obligations::Obligation::Inventory { .. }))
        || sources.is_empty()
        || expected.specialist
            != SpecialistId::EvidenceAuditorV11
                .definition()
                .map_err(|e| e.to_string())?
        || request.parent_id.as_deref() != Some(parent.analyst_id)
        || Some(conversation) != fresh["detail"]["conversation_id"].as_str()
        || request.scope != parent.request.scope
        || request.expected_graph_revision != parent.request.expected_graph_revision
        || request.expected_context_id != parent.request.expected_context_id
        || expected.provider.mode != InvestigationProviderMode::Local
        || !expected.provider.available
    {
        return Err("Auditor parent, scope, definition or provider differs.".into());
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
    // Production reader preserves exact saved status/uncertainty and refuses whole history >16KiB.
    jobs.investigation_next_parent_history(parent.analyst_id, conversation)
        .map_err(|e| e.to_string())?;
    let fresh_output = preparing && !output.exists();
    if fresh_output {
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
    let detail = if fresh_output {
        let started = jobs
            .start_next_prompt_investigation(request, Some(&expected.provider))
            .map_err(|e| e.to_string())?;
        if !started.created {
            return Err("Auditor nonce consumed.".into());
        }
        started.detail
    } else {
        jobs.existing_investigation_completion(request)
            .map_err(|e| e.to_string())?
            .ok_or("Auditor start absent; explicit recovery required")?
    };
    let id = detail.summary.investigation_id;
    let start = wire(&json!({"definition_sha256":sha(&definition),"auditor_id":id}))?;
    if fresh_output {
        analyst::exclusive(&output.join("start.json"), &start)?;
    } else if read(&output.join("start.json"))? != start {
        return Err("Auditor start changed.".into());
    }
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
    let id = stage(parent, artifacts, expected, false)?;
    let output = artifacts.output;
    let observed = Arc::new(execution::ObservedProvider::next(
        provider,
        expected.request.expected_context_id.clone(),
    ));
    let hash = content_hash(&wire(expected)?);
    analyst::execute_role(
        output,
        &authority(parent, expected, &id, &hash),
        observed.clone(),
        "auditor",
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
    let (matching, bound, reviewable) = execution::sealed_checks_with_policy(
        &expected.request,
        &steps,
        &ledgers,
        &observations,
        &INVESTIGATION_V2_POLICY,
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
    fn exercise_auditor(scenario: Scenario) {
        let directory = tempfile::tempdir().unwrap();
        let provider = expanded_scripted_provider(true);
        let descriptor = crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Local,
        )
        .unwrap();
        let prepared = super::super::prepare(
            directory.path(),
            &descriptor,
            super::super::tests::binding(),
        )
        .unwrap();
        let parent = analyst::Authority::synthetic(&prepared);
        let receipt = analyst::execute_bound(directory.path(), &parent, provider).unwrap();
        let result: InvestigationResult =
            serde_json::from_value(receipt["result"].clone()).unwrap();
        let mut request = prepared.auditor_request.clone();
        request.specialist_id = "evidence-auditor@11".into();
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
        let expected = Definition {
            binding: parent.binding.clone(),
            parent_revision: receipt["detail"]["revision"].as_u64().unwrap(),
            parent_result_id: result.result_id.clone(),
            parent_result_hash: content_hash(&wire(&result).unwrap()),
            parent_receipt_sha256: review_binding["receipt_sha256"].as_str().unwrap().into(),
            review_issue: 598,
            rubric_sha256: "d".repeat(64),
            review_sha256: sha(&review),
            request,
            specialist: SpecialistId::EvidenceAuditorV11.definition().unwrap(),
            provider: descriptor.clone(),
        };
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
        std::fs::write(&definition_path, wire(&expected).unwrap()).unwrap();
        let id = stage(&parent, &artifacts, &expected, true).unwrap();
        assert_eq!(stage(&parent, &artifacts, &expected, true).unwrap(), id);
        let db = prepared.app_data.join("state.db");
        let read_task = || {
            crate::jobs::JobStore::open(&db)
                .unwrap()
                .investigation(&id)
                .unwrap()
        };
        let first = read_task();
        assert_eq!(first.summary.status, InvestigationStatus::Queued);
        assert_eq!(
            first.summary.specialist_id,
            SpecialistId::EvidenceAuditorV11
        );
        assert_eq!(
            first.summary.parent_id.as_deref(),
            Some(prepared.analyst_id.as_str())
        );
        assert_eq!(first.usage, InvestigationUsage::default());
        assert_eq!(read_task(), first);
        if !matches!(scenario, Scenario::Stage) {
            let unknown = matches!(scenario, Scenario::Unknown);
            let refused = matches!(scenario, Scenario::Refused);
            let raw: Arc<dyn LlmProvider> = if unknown {
                Arc::new(Scripted::new(Mode::Unknown))
            } else if refused {
                expanded_scripted_provider(false)
            } else {
                expanded_auditor_scripted_provider()
            };
            let provider = Arc::new(execution::ObservedProvider::next(
                raw,
                expected.request.expected_context_id.clone(),
            ));
            let actual = execute(&parent, &expected, &artifacts, provider.clone()).unwrap();
            assert_eq!(actual["role"], "auditor");
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
            assert_eq!(actual["detail"]["specialist_id"], "evidence-auditor@11");
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
