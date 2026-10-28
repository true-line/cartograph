//! Existing captured-profile preparation only. No provider implementation or dispatch.
use super::super::vendure;
use super::*;
use agents::{TaskFactKey, investigation::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct Selection {
    pub fact: TaskFactKey,
    pub graph_snapshot_id: String,
    pub receipt_id: String,
    pub ordinal: usize,
}
/// Supplied independently by the caller, never recovered as expected authority
/// from the adjacent frozen output. The preparation SHA256 binds the complete
/// input/profile/config/source/capture/graph/coverage receipt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct Definition {
    pub binding: PreparationBinding,
    pub preparation_sha256: String,
    pub provider: InvestigationProvider,
    pub selections: Vec<Selection>,
    pub question: String,
    pub request_nonce: String,
    pub scope: InvestigationScope,
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
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
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
    capture::validate_binding(&expected.binding)?;
    let definition = json(expected)?;
    if read(definition_path)? != definition
        || expected.selections.is_empty()
        || expected.selections.len() > 64
        || expected.provider.mode != InvestigationProviderMode::Local
        || !expected.provider.available
    {
        return Err("independent capture definition required".into());
    }
    let result = vendure::reopen_preparation(
        config,
        preparation,
        definition_path,
        output,
        policy,
        |app, graph, saved, receipt| {
            if sha(receipt) != expected.preparation_sha256 {
                return Err("independent preparation digest mismatch".into());
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
                specialist_id: "domain-analyst@10".into(),
                request_nonce: expected.request_nonce.clone(),
                question: expected.question.clone(),
                scope: expected.scope.clone(),
                obligations: ObligationEnvelope {
                    schema_version: 2,
                    contract: "investigation-completion-obligations@2".into(),
                    obligations,
                },
                inspection_requirements: None,
                provider_mode: InvestigationProviderMode::Local,
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
            let fresh = match std::fs::create_dir(output) {
                Ok(()) => {
                    sync_directory(output.parent().ok_or("missing capture output parent")?)?;
                    true
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(_) => return Err("capture output unavailable".into()),
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
            let task = if fresh {
                let started = jobs
                    .start_next_prompt_investigation(&request, Some(&expected.provider))
                    .map_err(|e| e.to_string())?;
                if !started.created {
                    return Err("fresh capture nonce already consumed".into());
                }
                started.detail
            } else {
                jobs.existing_investigation_completion(&request)
                    .map_err(|e| e.to_string())?
                    .ok_or("frozen start absent; explicit recovery required")?
            };
            let start = serde_json::json!({"definition_sha256":sha(&definition), "request_sha256":sha(&wire),
                "intent_hash":request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
                "analyst_id":task.summary.investigation_id});
            if fresh {
                write(output, "start.json", &start)?;
            } else if read(&output.join("start.json"))? != json(&start)? {
                return Err("sealed start absent or changed; explicit recovery required".into());
            }
            if task.provider != expected.provider
                || task.summary.status != InvestigationStatus::Queued
                || task.usage.model_invocations != 0
            {
                return Err("stored capture task differs or was executed".into());
            }
            let manifest = serde_json::json!({"schema_version":1,"status":"captured_analyst_preparation_only",
            "definition_sha256":sha(&definition),"preparation_sha256":expected.preparation_sha256,
            "selection_sha256":sha(&json(&expected.selections)?),"question_sha256":sha(expected.question.as_bytes()),
            "source_id":graph.source_id,"capture_id":graph.capture_id,"graph_snapshot_id":graph.graph_snapshot_id,
            "profile":saved["profile"],"native_config_sha256":saved["native_config_sha256"],
            "request_sha256":sha(&wire),"intent_hash":request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
            "analyst_id":task.summary.investigation_id,"conversation_id":task.summary.conversation_id,
            "provider_calls":0,"acceptance_passed":false});
            let path = output.join("captured-analyst.json");
            if path.exists() && read(&path)? != json(&manifest)? {
                return Err("capture manifest differs".into());
            }
            Ok(manifest)
        },
    )?;
    if read(definition_path)? != definition {
        return Err("caller definition changed".into());
    }
    let manifest = output.join("captured-analyst.json");
    if !manifest.exists() {
        write(output, "captured-analyst.json", &result)?;
    }
    Ok(result)
}
#[test]
#[ignore = "manual existing Vendure profile preparation only; no provider"]
fn vendure_prepare_captured_analyst() {
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let definition_path = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let expected: Definition = agents::investigation::decode_record(
        std::str::from_utf8(&read(&definition_path).unwrap()).unwrap(),
    )
    .unwrap();
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
