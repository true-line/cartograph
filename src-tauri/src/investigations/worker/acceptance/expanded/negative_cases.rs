//! Manual closed-fixture route; all automatic tests are non-evidence witnesses.
use super::super::vendure::{
    self,
    negative_cases::{MANIFEST_SHA, NegativeCase598, sha},
};
use super::{
    analyst, cloud_run,
    vendure_prepare::{self, Definition},
};
use agents::investigation::*;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

pub(super) fn validate(case: NegativeCase598, definition: &Definition) -> Result<(), String> {
    let spec = case.spec()?;
    if definition.question != spec.question
        || sha(definition.question.as_bytes()) != case.binding()?["question_sha256"]
        || definition.specialist_id.as_deref() != Some("domain-analyst@27")
        || definition.scope != InvestigationScope::All
        || definition.run_limits.is_some()
        || definition.generation_policy.is_some()
        || definition.provider.provider_id != "openai:gpt-6.1-sol"
        || definition.provider.model != "gpt-6.1-sol"
        || definition.provider.deployment.is_some()
        || definition.provider.unavailable_reason.is_some()
        || definition.provider.protocol_version.as_deref() != Some("bounded-json-completion@2")
    {
        return Err("negative case question, role, scope, provider or limits mismatch".into());
    }
    cloud_run::pinned(&definition.provider)
}
/// Called inside the authenticated capture reopen, before inventory/admission.
/// Every case file must have exactly one selected original range covering its
/// entire evidence. No evaluator bytes or hand-chosen offsets enter the request.
pub(super) fn validate_graph(
    case: NegativeCase598,
    definition: &Definition,
    graph: &vendure::report::GraphPreparation,
) -> Result<(), String> {
    validate(case, definition)?;
    let spec = case.spec()?;
    if definition.selections.len() != spec.inputs.len() {
        return Err("negative source selection count".into());
    }
    for file in spec.inputs {
        let mut found = 0;
        for selection in &definition.selections {
            let candidate = graph.candidates.iter().find(|c| {
                serde_json::to_value(&c.binding.fact).ok()
                    == serde_json::to_value(&selection.fact).ok()
                    && c.binding.receipt_id == selection.receipt_id
                    && selection.graph_snapshot_id == graph.graph_snapshot_id
            });
            let Some(range) = candidate.and_then(|c| c.receipt.ranges().get(selection.ordinal))
            else {
                return Err("negative source selection unavailable".into());
            };
            if range.captured.file.path == file.path
                && range.captured.byte_start <= file.evidence_byte_range[0]
                && range.captured.byte_end >= file.evidence_byte_range[1]
            {
                found += 1;
            }
        }
        if found != 1 {
            return Err("negative source evidence not fully covered".into());
        }
    }
    Ok(())
}
fn prepare(
    case: NegativeCase598,
    paths: [&Path; 4],
    expected: &Definition,
) -> Result<Value, String> {
    validate(case, expected)?;
    vendure_prepare::prepare_bound(
        paths[0],
        paths[1],
        paths[2],
        paths[3],
        expected,
        vendure::InputPolicy::NegativeCases598(case),
    )
}

fn run(
    case: NegativeCase598,
    paths: [&Path; 4],
    expected: &Definition,
    execute: bool,
    env: &dyn Fn(&str) -> Option<String>,
    provider: impl FnOnce() -> Result<Arc<dyn llm::LlmProvider>, String>,
) -> Result<Value, String> {
    validate(case, expected)?;
    // No credential capability is called before the exact owner gate. Reopen
    // never reads authorization, keys, or constructs a provider.
    let authorization = if execute {
        Some(cloud_run::authorization(
            &expected.provider,
            env(cloud_run::AUTHORIZE).as_deref(),
            env(cloud_run::STEP_CAP).as_deref(),
        )?)
    } else {
        None
    };
    vendure_prepare::reopen_bound(
        paths,
        expected,
        vendure::InputPolicy::NegativeCases598(case),
        |app, request, id, manifest, data| {
            if request.parent_id.is_some() || request.conversation_id.is_some() {
                return Err("negative cases cannot have a parent".into());
            }
            let hash = core_prov::content_hash(
                &serde_json::to_vec(manifest).map_err(|_| "manifest encoding")?,
            );
            let authority = analyst::Authority {
                app,
                app_data: data,
                analyst_id: id,
                request,
                binding: &expected.binding,
                manifest_hash: &hash,
                provider: &expected.provider,
                run_limits: None,
            };
            let receipt_path = paths[3].join("negative-case-receipt.json");
            if let Some(authorization) = authorization {
                // Profile-scoped case marker prevents another output/nonce from
                // dispatching the same case again. Never removed on any failure.
                let marker = data.join(format!("negative-{}-attempt.json", case.id()));
                if marker.exists() {
                    return Err("negative case attempt already consumed".into());
                }
                let provider = provider()?; // production uses env-only constructor
                let consent = cloud_run::Consent::grant(
                    app.handle(),
                    &expected.provider,
                    &authorization,
                    &paths[3].join("analyst-consent-log.jsonl"),
                )?;
                analyst::exclusive(
                    &marker,
                    &analyst::bytes(&json!({"fixture":case.binding()?,
                    "task_id":id,"manifest_hash":hash}))?,
                )?;
                analyst::execute_role_with_granted_consent(
                    paths[3],
                    &authority,
                    provider,
                    "analyst",
                    analyst::BoundaryClock::Real,
                    Some(consent),
                )?;
                let receipt = analyst::collect_bound(&authority)?;
                analyst::exclusive(
                    &paths[3].join("analyst-receipt.json"),
                    &analyst::bytes(&receipt)?,
                )?;
                analyst::exclusive(
                    &receipt_path,
                    &analyst::bytes(&json!({
                    "schema":"cartograph-negative-case-receipt@1","fixture":case.binding()?,
                    "analyst":receipt}))?,
                )?;
            }
            let fresh = json!({"schema":"cartograph-negative-case-receipt@1",
                "fixture":case.binding()?,"analyst":analyst::collect_bound(&authority)?});
            if vendure::read_preparation_artifact(&receipt_path, 2 * 1024 * 1024)?
                != analyst::bytes(&fresh)?
            {
                return Err("negative case receipt differs from durable authority".into());
            }
            Ok(fresh)
        },
    )
}

#[test]
#[ignore = "manual owner-authorized negative case; preparation/reopen never dispatch"]
fn captured_negative_case() {
    let env = |name: &str| std::env::var(name).ok();
    let var = |name: &str| env(name).expect("required negative route variable");
    let case = NegativeCase598::parse(&var("CARTOGRAPH_NEGATIVE_CASE")).unwrap();
    assert_eq!(var("CARTOGRAPH_NEGATIVE_MANIFEST_SHA256"), MANIFEST_SHA);
    assert_eq!(
        var("CARTOGRAPH_NEGATIVE_QUESTION_SHA256"),
        case.binding().unwrap()["question_sha256"]
    );
    let paths = [
        var("CARTOGRAPH_ACCEPTANCE_CONFIG"),
        var("CARTOGRAPH_ACCEPTANCE_PREPARATION"),
        var("CARTOGRAPH_ACCEPTANCE_DEFINITION"),
        var("CARTOGRAPH_ACCEPTANCE_OUTPUT"),
    ];
    let paths = paths.each_ref().map(Path::new);
    let raw = vendure::read_preparation_artifact(paths[2], 128 * 1024).unwrap();
    let mut definition: Definition =
        agents::investigation::decode_record(std::str::from_utf8(&raw).unwrap()).unwrap();
    definition.start_sha256 = env("CARTOGRAPH_ACCEPTANCE_START_SHA256");
    let action = var("CARTOGRAPH_ACCEPTANCE_ACTION");
    let result = match action.as_str() {
        "prepare" => prepare(case, paths, &definition),
        "execute" => run(case, paths, &definition, true, &env, || {
            cloud_run::provider(&definition.provider, env)
        }),
        "reopen" => run(case, paths, &definition, false, &env, || {
            panic!("reopen cannot construct a provider")
        }),
        _ => Err("unknown negative route action".into()),
    };
    let receipt = result.expect("negative route refused; do not retry/reset");
    if action != "prepare" {
        println!(
            "Negative case status: {} invocations: {}",
            receipt["analyst"]["detail"]["status"],
            receipt["analyst"]["detail"]["usage"]["model_invocations"]
        );
        if action == "execute" {
            assert_eq!(
                receipt["analyst"]["detail"]["status"], "completed",
                "negative case did not complete; retain consumed attempt and receipts"
            );
        }
    }
}
#[cfg(test)]
#[path = "negative_cases_tests.rs"]
mod tests;
