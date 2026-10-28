//! Explicit manual local analyst execution. Independent review, auditor and
//! native lifecycle acceptance remain separate; terminal reopen never dispatches.
use super::super::vendure;
use super::{
    analyst,
    vendure_prepare::{self, Definition},
};
use agents::investigation::*;
use serde_json::Value;
use std::path::Path;
use tauri::Manager;

pub(in super::super) fn use_task(
    profile: (&tauri::App<tauri::test::MockRuntime>, &Path),
    request: &StagedObligationRequest,
    id: &str,
    manifest: &Value,
    output: &Path,
    expected: &Definition,
    execute: bool,
) -> Result<Value, String> {
    let (app, app_data) = profile;
    let hash = core_prov::content_hash(&serde_json::to_vec(manifest).map_err(|e| e.to_string())?);
    let authority = analyst::Authority {
        app,
        app_data,
        analyst_id: id,
        request,
        binding: &expected.binding,
        manifest_hash: &hash,
        provider: &expected.provider,
    };
    if execute {
        let provider = app
            .state::<crate::AppState>()
            .investigations
            .provider(InvestigationProviderMode::Local)
            .map_err(|e| e.to_string())?;
        analyst::execute_bound(output, &authority, provider)?;
    }
    let saved =
        vendure::read_preparation_artifact(&output.join("analyst-receipt.json"), 2 * 1024 * 1024)?;
    let fresh = analyst::collect_bound(&authority)?;
    if saved != analyst::bytes(&fresh)? {
        return Err("Analyst receipt differs from fresh durable authority.".into());
    }
    Ok(fresh)
}
pub(in super::super) fn run(
    paths: [&Path; 4],
    expected: &Definition,
    policy: vendure::InputPolicy,
    execute: bool,
) -> Result<Value, String> {
    vendure_prepare::reopen_bound(
        paths,
        expected,
        policy,
        |app, request, id, manifest, data| {
            use_task(
                (app, data),
                request,
                id,
                manifest,
                paths[3],
                expected,
                execute,
            )
        },
    )
}
// AC-0429: explicit manual adapter; ordinary tests do not execute a real provider.
#[test]
#[ignore = "manual captured-profile local provider; requires independent frozen definition and explicit execute/reopen action"]
fn vendure_captured_local_analyst() {
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let definition = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let expected: Definition = agents::investigation::decode_record(
        std::str::from_utf8(&vendure::read_preparation_artifact(&definition, 128 * 1024).unwrap())
            .unwrap(),
    )
    .unwrap();
    let execute = match super::super::required("CARTOGRAPH_VENDURE_ANALYST_ACTION").as_str() {
        "execute" => true,
        "reopen" => false,
        _ => panic!("explicit execute or reopen required"),
    };
    run(
        [
            &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
            &definition,
            &path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT"),
        ],
        &expected,
        vendure::InputPolicy::FrozenVendure,
        execute,
    )
    .unwrap();
}
