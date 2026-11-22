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
    let env = |name: &str| std::env::var(name).ok();
    use_task_with_env(
        profile, request, id, manifest, output, expected, execute, &env,
    )
}
#[allow(clippy::too_many_arguments)]
fn use_task_with_env(
    profile: (&tauri::App<tauri::test::MockRuntime>, &Path),
    request: &StagedObligationRequest,
    id: &str,
    manifest: &Value,
    output: &Path,
    expected: &Definition,
    execute: bool,
    env: &dyn Fn(&str) -> Option<String>,
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
        run_limits: expected.run_limits,
    };
    if execute {
        let provider = match expected.provider.mode {
            // Keys come only from the owner-run process environment.
            InvestigationProviderMode::Cloud => {
                super::cloud_run::provider(&expected.provider, env)?
            }
            InvestigationProviderMode::Local => app
                .state::<crate::AppState>()
                .investigations
                .provider(InvestigationProviderMode::Local)
                .map_err(|e| e.to_string())?,
        };
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
// Reuse the authenticated preparation roots before any auditor output/task writes.
// `definition` is the bounded proposal artifact in reopen_basis; stage validates its contents.
pub(in super::super) fn with_auditor_output<T>(
    [config, preparation, definition]: [&Path; 3],
    output: &Path,
    policy: vendure::InputPolicy,
    use_output: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _basis = vendure::reopen_basis(config, preparation, definition, output, policy)?;
    use_output()
}
/// The local override applies only to a Local definition. A Cloud definition is
/// executed by `use_task` through the consented cloud harness, so it must not
/// require (or read) the local-only protocol, thinking, URL and model variables.
fn local_execute_provider(
    execute: bool,
    expected: &InvestigationProvider,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<std::sync::Arc<dyn llm::LlmProvider>>, String> {
    if !execute || expected.mode != InvestigationProviderMode::Local {
        return Ok(None);
    }
    let var = |name: &str| {
        env(name)
            .filter(|value| !value.trim().is_empty() && value.len() <= 4096)
            .ok_or_else(|| format!("Set {name}; see MT-H4-02"))
    };
    configured_provider(
        &var("CARTOGRAPH_ACCEPTANCE_PROTOCOL")?,
        &var("CARTOGRAPH_ACCEPTANCE_THINKING")?,
        &var("CARTOGRAPH_ACCEPTANCE_URL")?,
        &var("CARTOGRAPH_ACCEPTANCE_MODEL")?,
        expected,
    )
    .map(Some)
}
// Same explicit loopback constructor as the existing manual local acceptance.
// Construction performs no model call; the frozen descriptor remains authority.
fn configured_provider(
    protocol: &str,
    thinking: &str,
    url: &str,
    model: &str,
    expected: &InvestigationProvider,
) -> Result<std::sync::Arc<dyn llm::LlmProvider>, String> {
    let enabled = match thinking {
        "enabled" => true,
        "disabled" => false,
        _ => return Err("Explicit local thinking must be enabled or disabled.".into()),
    };
    let timeout = std::time::Duration::from_secs(180);
    let provider: std::sync::Arc<dyn llm::LlmProvider> = match protocol {
        "ollama" => {
            let provider =
                llm::OllamaProvider::new(url, model, timeout).map_err(|error| error.to_string())?;
            // Ollama's constructor explicitly disables thinking; only enabled has a setter.
            let provider = if enabled {
                provider.with_thinking_enabled()
            } else {
                provider
            };
            // #1653: the frozen identity names its context window. Without one,
            // Ollama's server default applies and can silently cut a long
            // investigation prompt short, so a definition that needs it says so.
            std::sync::Arc::new(match frozen_context_window(&expected.provider_id)? {
                Some(tokens) => provider
                    .with_context_window(tokens)
                    .map_err(|error| error.to_string())?,
                None => provider,
            })
        }
        "openai" => {
            let provider =
                llm::openai_compatible::OpenAiCompatibleProvider::new(url, model, timeout)
                    .map_err(|error| error.to_string())?;
            std::sync::Arc::new(if enabled {
                provider.with_thinking_enabled()
            } else {
                provider.with_thinking_disabled()
            })
        }
        _ => return Err("Explicit local protocol must be ollama or openai.".into()),
    };
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
/// The `num-ctx-N` segment of a frozen Ollama identity, if it has one. The
/// window is then bound twice: by this constructor and by the descriptor
/// equality check that follows it.
fn frozen_context_window(provider_id: &str) -> Result<Option<u32>, String> {
    let mut windows = provider_id
        .split(':')
        .filter_map(|segment| segment.strip_prefix("num-ctx-"));
    match (windows.next(), windows.next()) {
        (None, _) => Ok(None),
        (Some(tokens), None) => tokens
            .parse()
            .map(Some)
            .map_err(|_| "Frozen context window is not a whole number.".into()),
        (Some(_), Some(_)) => Err("Frozen identity names two context windows.".into()),
    }
}
// AC-1411 / #1653: a frozen identity with a context window gets exactly that
// `num_ctx`; one without keeps the historical identity; any other window, a
// malformed one or a missing one is refused by the descriptor check.
#[test]
fn captured_local_ollama_binds_the_frozen_context_window() {
    let expected = |provider_id: &str| InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: provider_id.into(),
        model: "explicit-control-model".into(),
        endpoint: "http://127.0.0.1:1/api/chat".into(),
        deployment: None,
        protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
        available: true,
        unavailable_reason: None,
    };
    let build = |provider_id: &str| {
        configured_provider(
            "ollama",
            "disabled",
            "http://127.0.0.1:1/",
            "explicit-control-model",
            &expected(provider_id),
        )
    };
    let provider = build("ollama:explicit-control-model:num-ctx-32768").unwrap();
    assert_eq!(
        crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Local
        )
        .unwrap()
        .provider_id,
        "ollama:explicit-control-model:num-ctx-32768"
    );
    assert!(build("ollama:explicit-control-model").is_ok());
    for refused in [
        "ollama:explicit-control-model:num-ctx-12345",
        "ollama:explicit-control-model:num-ctx-big",
        "ollama:explicit-control-model:num-ctx-8192:num-ctx-8192",
    ] {
        assert!(build(refused).is_err(), "{refused}");
    }
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
    let provider = configured_provider(
        "ollama",
        "disabled",
        "http://127.0.0.1:1/",
        "explicit-control-model",
        &expected,
    )
    .unwrap();
    assert_eq!(
        crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Local
        )
        .unwrap(),
        expected
    );
    assert!(
        configured_provider(
            "ollama",
            "disabled",
            "http://127.0.0.1:1/",
            "different-model",
            &expected
        )
        .is_err()
    );
    assert!(
        configured_provider(
            "ollama",
            "disabled",
            "http://127.0.0.1:2/",
            "explicit-control-model",
            &expected
        )
        .is_err()
    );
    assert!(
        configured_provider(
            "ollama",
            "disabled",
            "http://example.invalid/",
            "explicit-control-model",
            &expected
        )
        .is_err()
    );
}
// Constructor-only checks: no server or inference; protocol cannot override identity.
#[test]
fn captured_local_openai_configuration_binds_protocol_model_and_endpoint() {
    let expected = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "openai-compatible:explicit-control-model:thinking-disabled".into(),
        model: "explicit-control-model".into(),
        endpoint: "http://127.0.0.1:1/v1/chat/completions".into(),
        deployment: None,
        protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
        available: true,
        unavailable_reason: None,
    };
    assert!(
        configured_provider(
            "openai",
            "disabled",
            "http://127.0.0.1:1/",
            "explicit-control-model",
            &expected
        )
        .is_ok()
    );
    for (protocol, url, model) in [
        ("ollama", "http://127.0.0.1:1/", "explicit-control-model"),
        ("", "http://127.0.0.1:1/", "explicit-control-model"),
        ("unknown", "http://127.0.0.1:1/", "explicit-control-model"),
        ("openai", "http://127.0.0.1:2/", "explicit-control-model"),
        ("openai", "http://127.0.0.1:1/", "different-model"),
        (
            "openai",
            "http://example.invalid/",
            "explicit-control-model",
        ),
    ] {
        assert!(configured_provider(protocol, "disabled", url, model, &expected).is_err());
    }
    let ollama = InvestigationProvider {
        provider_id: "ollama:explicit-control-model".into(),
        endpoint: "http://127.0.0.1:1/api/chat".into(),
        ..expected
    };
    assert!(
        configured_provider(
            "openai",
            "disabled",
            "http://127.0.0.1:1/",
            "explicit-control-model",
            &ollama
        )
        .is_err()
    );
}
// Neither a missing/unknown mode nor the opposite mode can match frozen identity.
#[test]
fn captured_local_thinking_is_explicit_and_bound() {
    for (protocol, thinking, opposite, provider_id, endpoint) in [
        (
            "openai",
            "disabled",
            "enabled",
            "openai-compatible:explicit-control-model:thinking-disabled",
            "http://127.0.0.1:1/v1/chat/completions",
        ),
        (
            "openai",
            "enabled",
            "disabled",
            "openai-compatible:explicit-control-model:thinking-enabled",
            "http://127.0.0.1:1/v1/chat/completions",
        ),
        (
            "ollama",
            "enabled",
            "disabled",
            "ollama:explicit-control-model:thinking-enabled",
            "http://127.0.0.1:1/api/chat",
        ),
    ] {
        let expected = InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: provider_id.into(),
            model: "explicit-control-model".into(),
            endpoint: endpoint.into(),
            deployment: None,
            protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
            available: true,
            unavailable_reason: None,
        };
        for mode in [thinking, opposite, "", "unknown"] {
            let result = configured_provider(
                protocol,
                mode,
                "http://127.0.0.1:1/",
                "explicit-control-model",
                &expected,
            );
            assert_eq!(result.is_ok(), mode == thinking);
        }
    }
}
// AC-0569 / #1709: a Cloud definition executes through the consented cloud harness;
// the manual entry must not demand local-only variables before reaching it.
#[test]
fn captured_analyst_entry_reads_local_variables_only_for_local_definitions() {
    let local = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "openai-compatible:explicit-control-model:thinking-disabled".into(),
        model: "explicit-control-model".into(),
        endpoint: "http://127.0.0.1:1/v1/chat/completions".into(),
        deployment: None,
        protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
        available: true,
        unavailable_reason: None,
    };
    let cloud = InvestigationProvider {
        mode: InvestigationProviderMode::Cloud,
        provider_id: "anthropic:claude-opus-5-5".into(),
        model: "claude-opus-5-5".into(),
        endpoint: "https://api.anthropic.com/v1/messages".into(),
        ..local.clone()
    };
    let read = std::cell::RefCell::new(Vec::new());
    let empty = |name: &str| {
        read.borrow_mut().push(name.to_owned());
        None
    };
    assert!(
        local_execute_provider(true, &cloud, &empty)
            .unwrap()
            .is_none()
    );
    assert!(
        local_execute_provider(false, &cloud, &empty)
            .unwrap()
            .is_none()
    );
    assert!(
        local_execute_provider(false, &local, &empty)
            .unwrap()
            .is_none()
    );
    assert!(
        read.borrow().is_empty(),
        "no local variable is read off the Local execute path"
    );
    let error = local_execute_provider(true, &local, &empty).err().unwrap();
    assert!(error.contains("CARTOGRAPH_ACCEPTANCE_PROTOCOL"));
    let configured = |name: &str| {
        Some(
            match name {
                "CARTOGRAPH_ACCEPTANCE_PROTOCOL" => "openai",
                "CARTOGRAPH_ACCEPTANCE_THINKING" => "disabled",
                "CARTOGRAPH_ACCEPTANCE_URL" => "http://127.0.0.1:1/",
                "CARTOGRAPH_ACCEPTANCE_MODEL" => "explicit-control-model",
                _ => return None,
            }
            .to_owned(),
        )
    };
    assert!(
        local_execute_provider(true, &local, &configured)
            .unwrap()
            .is_some()
    );
    assert!(
        local_execute_provider(true, &cloud, &configured)
            .unwrap()
            .is_none()
    );
}
// AC-0429: explicit manual adapter; ordinary tests do not execute a real provider.
#[test]
#[ignore = "manual captured-profile local provider; requires independent frozen definition and explicit execute/reopen action"]
fn vendure_captured_local_analyst() {
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let definition = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let mut expected: Definition = agents::investigation::decode_record(
        std::str::from_utf8(&vendure::read_preparation_artifact(&definition, 128 * 1024).unwrap())
            .unwrap(),
    )
    .unwrap();
    expected.start_sha256 = Some(super::super::required(
        "CARTOGRAPH_VENDURE_ANALYST_START_SHA256",
    ));
    let execute = match super::super::required("CARTOGRAPH_VENDURE_ANALYST_ACTION").as_str() {
        "execute" => true,
        "reopen" => false,
        _ => panic!("explicit execute or reopen required"),
    };
    let output = path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT");
    let env = |name: &str| std::env::var(name).ok();
    analyst_entry(
        [
            &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
            &definition,
            &output,
        ],
        &expected,
        vendure::InputPolicy::FrozenVendure,
        execute,
        &env,
    )
    .unwrap();
}
/// The manual analyst entry after its action is chosen. A Local definition
/// needs the explicit local override; a Cloud definition goes straight to the
/// consented hosted harness in `use_task_with_env` and never reads local
/// variables.
pub(in super::super) fn analyst_entry(
    paths: [&Path; 4],
    expected: &Definition,
    policy: vendure::InputPolicy,
    execute: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Value, String> {
    let provider = local_execute_provider(execute, &expected.provider, env)?;
    let output = paths[3];
    vendure_prepare::reopen_bound(
        paths,
        expected,
        policy,
        |app, request, id, manifest, data| {
            if let Some(provider) = provider {
                app.state::<crate::AppState>()
                    .investigations
                    .set_test_provider(InvestigationProviderMode::Local, provider);
            }
            use_task_with_env(
                (app, data),
                request,
                id,
                manifest,
                output,
                expected,
                execute,
                env,
            )
        },
    )
}

// AC-0463: the closed action is selected before files or provider construction.
fn auditor_action(value: &str) -> Result<&str, String> {
    match value {
        "prepare" | "execute" | "reopen" => Ok(value),
        _ => Err("Explicit auditor prepare, execute or reopen required.".into()),
    }
}
#[test]
fn captured_auditor_requires_explicit_single_action() {
    for value in ["", "run", "execute,reopen", "Execute"] {
        assert!(auditor_action(value).is_err());
    }
    for value in ["prepare", "execute", "reopen"] {
        assert_eq!(auditor_action(value).unwrap(), value);
    }
}
// External orchestration must freshly authenticate review; local bytes bind content only.
#[test]
#[ignore = "manual captured auditor preparation; no provider construction or dispatch"]
fn vendure_captured_local_auditor_prepare() {
    run_captured_auditor("prepare");
}
#[test]
#[ignore = "manual captured auditor; requires independent frozen authority and fresh authenticated review before execute"]
fn vendure_captured_local_auditor_action() {
    run_captured_auditor(&super::super::required("CARTOGRAPH_VENDURE_AUDITOR_ACTION"));
}
fn run_captured_auditor(action: &str) {
    let action = auditor_action(action).unwrap();
    use super::captured_auditor::{self, Artifacts};
    let path = |name| std::path::PathBuf::from(super::super::required(name));
    let parent_definition_path = path("CARTOGRAPH_VENDURE_ANALYST_DEFINITION");
    let mut parent_definition: Definition = decode_record(
        std::str::from_utf8(
            &vendure::read_preparation_artifact(&parent_definition_path, 128 * 1024).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    parent_definition.start_sha256 = Some(super::super::required(
        "CARTOGRAPH_VENDURE_ANALYST_START_SHA256",
    ));
    let definition_path = path("CARTOGRAPH_VENDURE_AUDITOR_DEFINITION");
    let mut expected: captured_auditor::Definition = decode_record(
        std::str::from_utf8(
            &vendure::read_preparation_artifact(&definition_path, 128 * 1024).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    expected.start_sha256 = std::env::var("CARTOGRAPH_VENDURE_AUDITOR_START_SHA256").ok();
    let review =
        vendure::read_preparation_artifact(&path("CARTOGRAPH_VENDURE_AUDITOR_REVIEW"), 128 * 1024)
            .unwrap();
    let parent_output = path("CARTOGRAPH_VENDURE_ANALYST_PREPARATION_OUTPUT");
    let output = path("CARTOGRAPH_VENDURE_AUDITOR_OUTPUT");
    with_auditor_output(
        [
            &path("CARTOGRAPH_VENDURE_NATIVE_CONFIG"),
            &path("CARTOGRAPH_VENDURE_PREPARATION_RECEIPT"),
            &definition_path,
        ],
        &output,
        vendure::InputPolicy::FrozenVendure,
        || {
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
                    let hash = core_prov::content_hash(
                        &serde_json::to_vec(manifest).map_err(|e| e.to_string())?,
                    );
                    let parent = analyst::Authority {
                        app,
                        app_data: data,
                        analyst_id: id,
                        request,
                        binding: &parent_definition.binding,
                        manifest_hash: &hash,
                        provider: &parent_definition.provider,
                        run_limits: parent_definition.run_limits,
                    };
                    let artifacts = Artifacts {
                        parent_output: &parent_output,
                        output: &output,
                        definition_path: &definition_path,
                        review: &review,
                    };
                    match action {
                        "prepare" => captured_auditor::stage(&parent, &artifacts, &expected, true)
                            .map(|id| serde_json::json!({"auditor_id":id,"provider_calls":0})),
                        "reopen" => captured_auditor::reopen(&parent, &expected, &artifacts),
                        "execute" => {
                            captured_auditor::stage(&parent, &artifacts, &expected, false)?;
                            let provider = auditor_execute_provider(&expected.provider, &|key| {
                                std::env::var(key).ok()
                            })?;
                            captured_auditor::execute(&parent, &expected, &artifacts, provider)
                        }
                        _ => unreachable!("validated before opening profile"),
                    }
                },
            )
        },
    )
    .unwrap();
}

// Construct only on execute; the shared execute_role gate owns consent and the attempt.
fn auditor_execute_provider(
    expected: &InvestigationProvider,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<std::sync::Arc<dyn llm::LlmProvider>, String> {
    match expected.mode {
        InvestigationProviderMode::Cloud => {
            super::cloud_run::authorization(
                expected,
                env(super::cloud_run::AUTHORIZE).as_deref(),
                env(super::cloud_run::STEP_CAP).as_deref(),
            )?;
            super::cloud_run::provider(expected, env)
        }
        InvestigationProviderMode::Local => local_execute_provider(true, expected, env)?
            .ok_or_else(|| "Local auditor provider missing.".into()),
    }
}

// AC-0577: constructor-only, no network and no real key; fail before key access without go.
#[test]
fn captured_auditor_cloud_entry_requires_go_and_never_reads_local_override() {
    let expected = InvestigationProvider {
        mode: InvestigationProviderMode::Cloud,
        provider_id: "openai:gpt-6.1-sol".into(),
        model: "gpt-6.1-sol".into(),
        endpoint: llm::openai::API_URL.into(),
        deployment: None,
        protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
        available: true,
        unavailable_reason: None,
    };
    for cap in [None, Some("0"), Some("65"), Some("not-a-number")] {
        assert!(
            auditor_execute_provider(&expected, &|key| match key {
                super::cloud_run::AUTHORIZE => Some(expected.provider_id.clone()),
                super::cloud_run::STEP_CAP => cap.map(str::to_owned),
                _ => panic!("key/local override read before authorization"),
            })
            .is_err()
        );
    }
    assert!(
        auditor_execute_provider(&expected, &|key| match key {
            super::cloud_run::AUTHORIZE => Some("openai:other".into()),
            super::cloud_run::STEP_CAP => Some("32".into()),
            _ => panic!("key/local override read before authorization"),
        })
        .is_err()
    );
    let provider = auditor_execute_provider(&expected, &|key| match key {
        super::cloud_run::AUTHORIZE => Some(expected.provider_id.clone()),
        super::cloud_run::STEP_CAP => Some("32".into()),
        "OPENAI_API_KEY" => Some("synthetic-constructor-only".into()),
        _ => panic!("Cloud entry read a local override"),
    })
    .unwrap();
    assert_eq!(
        crate::investigations::runtime::descriptor(
            provider.as_ref(),
            InvestigationProviderMode::Cloud
        )
        .unwrap(),
        expected
    );
}
