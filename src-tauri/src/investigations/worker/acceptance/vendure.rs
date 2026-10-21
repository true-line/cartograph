//! Preparation only: real ingest of frozen source, with no provider or task.
//! This does not execute MT-H4-02's provider, review or native lifecycle stages.

use crate::*;
use serde::Serialize;
use std::path::Path;
use tauri::Manager;

#[path = "vendure/input.rs"]
mod input;
#[path = "vendure/profile.rs"]
mod profile;
#[path = "vendure/report.rs"]
mod report;
#[cfg(test)]
#[path = "vendure/tests.rs"]
mod tests;

const MANIFEST_SHA: &str = "96d2595907c4b1ea05991c7f0f2460df2f7b75267c486d5ee3e96c3d6ae49b83";
const PROOF_SHA: &str = "22d1cb69a593d6cf87abf1207cac9a50e82e6df61181874f9ab2fbd79e30a763";
const COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";

#[derive(Serialize)]
struct Preparation {
    schema_version: u32,
    status: &'static str,
    code_revision: String,
    source_root: std::path::PathBuf,
    native_config_sha256: String,
    input: input::VerifiedInput,
    profile: profile::NativeProfile,
    graph: report::GraphPreparation,
    ingest: IngestSummary,
    provider_calls: u32,
    remaining_stages: [&'static str; 5],
}

fn prepare(
    source: &Path,
    manifest: &Path,
    proof: &Path,
    config: &Path,
    revision: String,
    frozen_vendure: bool,
) -> Result<Preparation, String> {
    let input = input::verify(source, manifest, proof)?;
    if frozen_vendure
        && (input.repo != "vendurehq/vendure"
            || input.commit_sha != COMMIT
            || input.manifest_sha256 != MANIFEST_SHA
            || input.proof_sha256 != PROOF_SHA
            || input.files.len() != 17
            || input.files.values().map(|file| file.byte_len).sum::<u64>() != 270_721)
    {
        return Err("frozen Vendure input identity mismatch".into());
    }
    let (profile, native_config_sha256) = profile::NativeProfile::read(config)?;
    profile.create_fresh(source)?;
    let app = profile.app()?;
    let state = app.state::<AppState>();
    let registered = register_local_source(&state, source)?;
    let operation = source_operation(&state, vec![(registered.clone(), false)])?;
    let (_, execution) = start_job(&state, &registered.ingest_job_kind())?;
    let ingest = run_ingest(&registered, &operation, &execution, app.handle(), &state)
        .map_err(|_| "production ingest failed; inspect retained job events".to_string())?;
    if input::verify(source, manifest, proof)? != input {
        return Err("source or input proof changed during production ingest".into());
    }
    let graph = report::collect(&state, &registered, &input, profile.data())?;
    if input::verify(source, manifest, proof)? != input {
        return Err("source or input proof changed during preparation".into());
    }
    Ok(Preparation {
        schema_version: 1,
        status: "captured_preparation_only",
        code_revision: revision,
        source_root: crate::paths::canonicalize(source).map_err(|_| "source root unavailable")?,
        native_config_sha256,
        input,
        profile,
        graph,
        ingest,
        provider_calls: 0,
        remaining_stages: [
            "discovery_contract_and_frozen_dispatch_review",
            "actual_provider_analyst_and_independent_auditor",
            "withheld_oracle_semantic_review_and_negative_cases",
            "native_process_restart_and_retained_source_reopen",
            "disposable_capture_forgetting",
        ],
    })
}

/// Manual, non-provider preparation of the original 17-file source closure.
/// Output directory is exclusive; retained databases remain in the explicit
/// native profile, ready for a later reviewed handoff without database copying.
#[test]
#[ignore = "manual frozen Vendure capture preparation; no provider calls"]
fn vendure_capture_preparation() {
    let required = |name| std::path::PathBuf::from(super::required(name));
    let source = required("CARTOGRAPH_VENDURE_SOURCE");
    let manifest = required("CARTOGRAPH_VENDURE_MANIFEST");
    let proof = required("CARTOGRAPH_VENDURE_PROOF");
    let config = required("CARTOGRAPH_VENDURE_NATIVE_CONFIG");
    let output = required("CARTOGRAPH_VENDURE_PREPARATION_OUTPUT");
    assert!(output.is_absolute());
    let source_root = crate::paths::canonicalize(&source).unwrap();
    let parent = crate::paths::canonicalize(output.parent().unwrap()).unwrap();
    assert_eq!(output.parent().unwrap(), parent);
    assert!(!output.starts_with(&source_root));
    std::fs::create_dir(&output).expect("Preparation output must be new");
    let revision = super::source_revision();
    match prepare(&source, &manifest, &proof, &config, revision, true) {
        Ok(preparation) => {
            report::write_new(&output.join("preparation.json"), &preparation).unwrap()
        }
        Err(error) => {
            report::write_new(
                &output.join("preparation-failed.json"),
                &serde_json::json!({"schema_version":1,"status":"preparation_failed",
                    "provider_calls":0,"acceptance_passed":false,"reason":error}),
            )
            .unwrap();
            panic!("Vendure preparation failed; no acceptance or provider run occurred");
        }
    }
}
