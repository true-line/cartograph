//! Separate request7/@10 capture authority; accepts identity data, never a
//! provider implementation, dispatch lease or execution callback.
use super::completion::prepare::{
    self as capture, CaptureFixture, Family, PreparationBinding, PreparedState,
};
use agents::investigation::InvestigationProvider;
use std::path::Path;
#[path = "expanded/analyst.rs"]
mod analyst;
#[path = "expanded/captured_auditor.rs"]
mod captured_auditor;
#[path = "expanded/captured_local.rs"]
pub(super) mod captured_local;
#[path = "expanded/cloud_run.rs"]
mod cloud_run;
#[path = "expanded/estate.rs"]
mod estate;
#[path = "expanded/negative_cases.rs"]
mod negative_cases;
#[path = "expanded/start_timing.rs"]
mod start_timing;
#[path = "expanded/vendure_prepare.rs"]
pub(super) mod vendure_prepare;

fn prepare(
    directory: &Path,
    descriptor: &InvestigationProvider,
    binding: PreparationBinding,
) -> Result<PreparedState, String> {
    capture::prepare_family(directory, descriptor, binding, Family::Expanded)
}
// The caller must supply its independently frozen run definition, not read
// expected authority back from this directory's capture manifest.
fn reopen(directory: &Path, expected: &PreparationBinding) -> Result<PreparedState, String> {
    capture::reopen_family(directory, Family::Expanded, Some(expected))
}

fn reopen_control(
    directory: &Path,
    expected: &PreparationBinding,
    fixture: CaptureFixture,
) -> Result<PreparedState, String> {
    capture::reopen_fixture(directory, Family::Expanded, Some(expected), fixture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::*;
    use core_graph::GraphStore;
    use tauri::Manager;

    pub(super) fn binding() -> PreparationBinding {
        PreparationBinding {
            run_id: "expanded-preparation-test".into(),
            protocol_hash: "a".repeat(64),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        }
    }
    // Descriptor claims are frozen input, not verified runtime/model evidence.
    fn descriptor() -> InvestigationProvider {
        InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: "local:unverified-control".into(),
            model: "unverified-control-model".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            protocol_version: None,
            available: true,
            unavailable_reason: None,
        }
    }

    fn seal_manifest(directory: &Path, manifest: &capture::CaptureManifest) {
        let bytes = serde_json::to_vec(manifest).unwrap();
        std::fs::write(
            directory.join("capture-manifest.hash"),
            core_prov::content_hash(&bytes),
        )
        .unwrap();
        std::fs::write(directory.join("capture-manifest.json"), bytes).unwrap();
    }

    // AC-0287 preparation only: production resolvers/start, no invocation capability.
    #[test]
    fn expanded_prepare_freezes_request7_without_execution_and_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let prepared = prepare(directory.path(), &descriptor(), binding()).unwrap();
        for request in [&prepared.analyst_request, &prepared.auditor_request] {
            assert_eq!(request.schema_version, 7);
            assert_eq!(request.limit_profile, "investigation-v2");
            assert_eq!(request.obligations.schema_version, 2);
            assert_eq!(
                request.obligations.contract,
                "investigation-completion-obligations@2"
            );
            assert_eq!(request.obligations.obligations.len(), 2);
            assert!(
                request
                    .normalized_with_policy(&INVESTIGATION_V2_POLICY)
                    .is_ok()
            );
            assert!(request.normalized().is_err());
        }
        assert_eq!(prepared.analyst_request.specialist_id, "domain-analyst@10");
        assert_eq!(
            prepared.auditor_request.specialist_id,
            "evidence-auditor@10"
        );
        assert_eq!(
            prepared.auditor_request.parent_id.as_deref(),
            Some(prepared.analyst_id.as_str())
        );
        let state = prepared.app.state::<crate::AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&prepared.analyst_id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Queued);
        assert_eq!(detail.usage.model_invocations, 0);
        assert_eq!(detail.provider, descriptor());
        assert!(
            jobs.investigation_result(&prepared.analyst_id)
                .unwrap()
                .is_none()
        );
        assert!(
            jobs.existing_investigation_completion(&prepared.auditor_request)
                .unwrap()
                .is_none()
        );
        drop(jobs);
        let db = rusqlite::Connection::open(prepared.app_data.join("state.db")).unwrap();
        let (count, record): (i64, Vec<u8>) = db
            .query_row(
                "SELECT count(*), record FROM investigation_tasks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        for table in ["investigation_invocations", "completion_attempts"] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
        let record: serde_json::Value = serde_json::from_slice(&record).unwrap();
        assert_eq!(record["schema_version"], 3);
        assert!(record["execution"].is_null());
        let manifest = prepared.manifest.clone();
        let request_bytes = serde_json::to_vec(&prepared.auditor_request).unwrap();
        drop(db);
        drop(prepared);
        assert!(capture::reopen_family(directory.path(), Family::V7, None).is_err());
        let reopened = reopen(directory.path(), &binding()).unwrap();
        assert_eq!(reopened.manifest, manifest);
        assert_eq!(
            serde_json::to_vec(&reopened.auditor_request).unwrap(),
            request_bytes
        );
        assert!(prepare(directory.path(), &descriptor(), binding()).is_err());
    }

    // AC-0287: every family component and independently resolved capture remain bound.
    #[test]
    fn expanded_prepare_rejects_mixed_family_and_frozen_artifact_drift() {
        for field in [
            "schema_version",
            "specialist_id",
            "limit_profile",
            "envelope",
            "parent_id",
            "source",
            "graph",
            "run_id",
            "protocol_hash",
            "source_commit",
            "source_tree",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let prepared = prepare(directory.path(), &descriptor(), binding()).unwrap();
            if field == "graph" {
                let state = prepared.app.state::<crate::AppState>();
                let mut graph = state.graph.lock().unwrap();
                let mut node = graph.read_snapshot().unwrap().0.remove(0);
                node.props["drift"] = true.into();
                graph.put_node(&node).unwrap();
            } else if field == "source" {
                std::fs::write(directory.path().join("source-control.ts"), b"replacement").unwrap();
            } else if ["run_id", "protocol_hash", "source_commit", "source_tree"].contains(&field) {
                let mut manifest = prepared.manifest.clone();
                match field {
                    "run_id" => manifest.binding.run_id = "substituted-run".into(),
                    "protocol_hash" => manifest.binding.protocol_hash = "d".repeat(64),
                    "source_commit" => manifest.binding.source_commit = "e".repeat(40),
                    _ => manifest.binding.source_tree = "f".repeat(40),
                }
                seal_manifest(directory.path(), &manifest);
            } else {
                let mut value = serde_json::to_value(&prepared.auditor_request).unwrap();
                match field {
                    "schema_version" => value[field] = 4.into(),
                    "specialist_id" => value[field] = "evidence-auditor@7".into(),
                    "limit_profile" => value[field] = "investigation-v1".into(),
                    "envelope" => value["obligations"]["schema_version"] = 1.into(),
                    _ => value[field] = "foreign-parent".into(),
                }
                let bytes = serde_json::to_vec(&value).unwrap();
                let mut manifest = prepared.manifest.clone();
                manifest.auditor_wire_hash = core_prov::content_hash(&bytes);
                // A mixed tuple cannot have an admitted @10 intent hash. Valid
                // foreign-parent requests are rehashed to reach role binding.
                if field == "parent_id" {
                    let request = StagedObligationRequest::decode_with_policy(
                        std::str::from_utf8(&bytes).unwrap(),
                        &INVESTIGATION_V2_POLICY,
                    )
                    .unwrap();
                    manifest.auditor_request_hash = request
                        .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap();
                }
                std::fs::write(directory.path().join("auditor-request.json"), bytes).unwrap();
                seal_manifest(directory.path(), &manifest);
            }
            drop(prepared);
            let error = reopen(directory.path(), &binding()).err().expect(field);
            if ["run_id", "protocol_hash", "source_commit", "source_tree"].contains(&field) {
                assert_eq!(
                    error, "Frozen preparation differs from the expected binding.",
                    "{field}"
                );
            }
        }
    }
}
