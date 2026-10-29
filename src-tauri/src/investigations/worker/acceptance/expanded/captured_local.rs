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
// Same explicit loopback constructor as the existing manual local acceptance.
// Construction performs no model call; the frozen descriptor remains authority.
fn configured_provider(
    url: &str,
    model: &str,
    expected: &InvestigationProvider,
) -> Result<std::sync::Arc<dyn llm::LlmProvider>, String> {
    let provider = std::sync::Arc::new(
        llm::OllamaProvider::new(url, model, std::time::Duration::from_secs(180))
            .map_err(|error| error.to_string())?,
    );
    if crate::investigations::runtime::descriptor(
        provider.as_ref(),
        InvestigationProviderMode::Local,
    )
    .map_err(|error| error.to_string())?
        != *expected
    {
        return Err("Explicit local configuration differs from frozen identity.".into());
    }
    Ok(provider)
}
// AC-0429: pure constructor/identity checks; no server, model call or attempt.
#[test]
fn captured_local_explicit_configuration_matches_only_frozen_descriptor() {
    let expected = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "ollama:explicit-control-model".into(),
        model: "explicit-control-model".into(),
        endpoint: "http://127.0.0.1:1/api/chat".into(),
        deployment: None,
        protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
        available: true,
        unavailable_reason: None,
    };
    let provider =
        configured_provider("http://127.0.0.1:1/", "explicit-control-model", &expected).unwrap();
    assert_eq!(
        crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Local
        )
        .unwrap(),
        expected
    );
    assert!(configured_provider("http://127.0.0.1:1/", "different-model", &expected).is_err());
    assert!(
        configured_provider("http://127.0.0.1:2/", "explicit-control-model", &expected).is_err()
    );
    assert!(
        configured_provider(
            "http://example.invalid/",
            "explicit-control-model",
            &expected
        )
        .is_err()
    );
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
    let provider = if execute {
        Some(
            configured_provider(
                &super::super::required("CARTOGRAPH_ACCEPTANCE_URL"),
                &super::super::required("CARTOGRAPH_ACCEPTANCE_MODEL"),
                &expected.provider,
            )
            .unwrap(),
        )
    } else {
        None
    };
    let output = path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT");
    vendure_prepare::reopen_bound(
        [
            &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
            &definition,
            &output,
        ],
        &expected,
        vendure::InputPolicy::FrozenVendure,
        |app, request, id, manifest, data| {
            if let Some(provider) = provider {
                app.state::<crate::AppState>()
                    .investigations
                    .set_test_provider(InvestigationProviderMode::Local, provider);
            }
            use_task(
                (app, data),
                request,
                id,
                manifest,
                &output,
                &expected,
                execute,
            )
        },
    )
    .unwrap();
}

// External orchestration must freshly authenticate review before preparation.
// Local attestation bytes bind content, never reviewer identity.
#[test]
#[ignore = "manual captured auditor; requires independent definition, independent frozen reviewed-parent authority; stages only"]
fn vendure_captured_local_auditor_prepare() {
    use super::captured_auditor::{self, Artifacts};
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let parent_definition_path = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let parent_definition: Definition = decode_record(
        std::str::from_utf8(
            &vendure::read_preparation_artifact(&parent_definition_path, 128 * 1024).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let definition_path = path("CARTOGRAPH_VENDURE_AUDITOR_DEFINITION");
    let expected: captured_auditor::Definition = decode_record(
        std::str::from_utf8(
            &vendure::read_preparation_artifact(&definition_path, 128 * 1024).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let review =
        vendure::read_preparation_artifact(&path("CARTOGRAPH_VENDURE_AUDITOR_REVIEW"), 128 * 1024)
            .unwrap();
    let parent_output = path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT");
    let output = path("CARTOGRAPH_VENDURE_AUDITOR_OUTPUT");
    vendure_prepare::reopen_bound(
        [
            &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
            &parent_definition_path,
            &parent_output,
        ],
        &parent_definition,
        vendure::InputPolicy::FrozenVendure,
        |app, request, id, manifest, data| {
            let hash =
                core_prov::content_hash(&serde_json::to_vec(manifest).map_err(|e| e.to_string())?);
            let parent = analyst::Authority {
                app,
                app_data: data,
                analyst_id: id,
                request,
                binding: &parent_definition.binding,
                manifest_hash: &hash,
                provider: &parent_definition.provider,
            };
            let artifacts = Artifacts {
                parent_output: &parent_output,
                output: &output,
                definition_path: &definition_path,
                review: &review,
            };
            captured_auditor::stage(&parent, &artifacts, &expected, true)
                .map(|id| serde_json::json!({"auditor_id":id,"provider_calls":0}))
        },
    )
    .unwrap();
}
