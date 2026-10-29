//! Captured child staging only; local review bytes bind content, never authenticate a reviewer.
use super::super::completion::control;
use super::{PreparationBinding, analyst};
use agents::investigation::*;
use core_prov::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigations::worker::tests::completion_tests::expanded_scripted_provider;

    // AC-0466: synthetic authentication-shaped bytes prove content gates only.
    #[test]
    fn captured_auditor_preparation_binds_saved_parent_and_stages_without_dispatch() {
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
