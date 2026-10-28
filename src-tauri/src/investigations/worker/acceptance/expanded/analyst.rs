//! Shared analyst execution and durable receipt helpers. Auditor execution is separate.
use super::{CaptureFixture, PreparationBinding, PreparedState, reopen, reopen_control};
use crate::{
    investigations::{runtime::descriptor, worker::run_completion},
    jobs::{ClaimMode, JobStore, investigations::PendingInvestigationStep},
};
use agents::investigation::*;
use core_prov::content_hash;
use llm::LlmProvider;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::Instant,
};
use tauri::Manager;

const MAX_RECEIPT: usize = 2 * 1024 * 1024;
pub(super) fn bytes(value: &Value) -> Result<Vec<u8>, String> {
    crate::investigations::context::bounded_json(value, MAX_RECEIPT).map_err(|e| e.to_string())
}
pub(super) fn exclusive(path: &Path, body: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(body)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    File::open(path.parent().ok_or("Missing artifact parent.")?)
        .and_then(|parent| parent.sync_all())
        .map_err(|e| e.to_string())
}
// Preserve exact bounded journal bodies, including their independently validated
// historical admission cutoffs. Never reconstruct modes from terminal usage.
fn rows(db: &rusqlite::Connection, id: &str, table: &str) -> Result<Vec<String>, String> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
            [table],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Ok(vec![]);
    }
    let (count, size): (i64, i64) = db
        .query_row(
            &format!("SELECT count(*),coalesce(sum(length(body)),0) FROM {table} WHERE task_id=?1"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if !(0..=112).contains(&count) || !(0..=MAX_RECEIPT as i64).contains(&size) {
        return Err("Journal exceeds receipt bound.".into());
    }
    let mut statement = db
        .prepare(&format!(
            "SELECT body,body_hash FROM {table} WHERE task_id=?1 ORDER BY rowid"
        ))
        .map_err(|e| e.to_string())?;
    statement
        .query_map([id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .map(|row| {
            let (body, hash) = row.map_err(|e| e.to_string())?;
            if content_hash(&body) != hash {
                return Err("Journal hash changed.".into());
            }
            String::from_utf8(body).map_err(|e| e.to_string())
        })
        .collect()
}
pub(super) struct Authority<'a> {
    pub app: &'a tauri::App<tauri::test::MockRuntime>,
    pub app_data: &'a Path,
    pub analyst_id: &'a str,
    pub request: &'a StagedObligationRequest,
    pub binding: &'a PreparationBinding,
    pub manifest_hash: &'a str,
    pub provider: &'a InvestigationProvider,
}
impl<'a> Authority<'a> {
    fn synthetic(prepared: &'a PreparedState) -> Self {
        Self {
            app: &prepared.app,
            app_data: &prepared.app_data,
            analyst_id: &prepared.analyst_id,
            request: &prepared.analyst_request,
            binding: &prepared.manifest.binding,
            manifest_hash: &prepared.manifest_hash,
            provider: &prepared.manifest.provider,
        }
    }
}
fn collect(prepared: &PreparedState) -> Result<Value, String> {
    collect_bound(&Authority::synthetic(prepared))
}
pub(super) fn collect_bound(authority: &Authority<'_>) -> Result<Value, String> {
    let database = authority.app_data.join("state.db");
    let jobs = JobStore::open(&database).map_err(|e| e.to_string())?;
    // This production reader authenticates task3 and historical @10 metadata.
    let (detail, result, ledger, events) =
        super::super::completion::execution::read_expanded_state(&jobs, authority.analyst_id)?;
    if !matches!(
        detail.summary.status,
        InvestigationStatus::Completed
            | InvestigationStatus::Failed
            | InvestigationStatus::Cancelled
            | InvestigationStatus::Interrupted
            | InvestigationStatus::OutcomeUnknown
    ) {
        return Err("Analyst is not terminal.".into());
    }
    let db =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
    let invocations = rows(&db, authority.analyst_id, "investigation_invocations")?;
    let calls = invocations
        .iter()
        .map(|body| {
            serde_json::from_str::<PendingInvestigationStep>(body).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let capacity = rows(
        &db,
        authority.analyst_id,
        "investigation_admission_refusals",
    )?;
    let finish = rows(&db, authority.analyst_id, "staged_completion_refusals")?;
    if calls.len() as u32 != detail.usage.model_invocations {
        return Err("Invocation denominator changed.".into());
    }
    let modes = calls
        .iter()
        .map(|call| {
            call.disclosure
                .as_ref()
                .filter(|d| d.schema_version == 6)
                .and_then(|d| d.admission.as_ref())
                .map(|a| a.mode)
                .ok_or("Invocation lacks saved disclosure6 admission.")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let finish_only_calls = modes
        .iter()
        .filter(|mode| {
            **mode == agents::investigation::staged_refusal::CompletionCallMode::FinishOnly
        })
        .count();
    Ok(
        json!({"schema_version":1,"family":"request7-specialist10-prompt9-disclosure6-result7","role":"analyst",
        "binding":authority.binding,"manifest_hash":authority.manifest_hash,
        "request_hash":authority.request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
        "obligations_hash":authority.request.obligations.fingerprint_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e|e.to_string())?,
        "initial_prompt_measurement_basis":"first_saved_invocation_only_not_pre_dispatch_preparation",
        "initial_prompt_bytes":calls.first().map(|call| call.input_bytes),
        "finish_only_calls":finish_only_calls,"choice_calls":calls.len()-finish_only_calls,
        "capacity_refusal_count":capacity.len(),"finish_refusal_count":finish.len(),
        "detail":detail,"result":result,"ledger":ledger,"events":events,
        "invocations":invocations,"capacity_refusals":capacity,"finish_refusals":finish,
        "tokenizer_context_capacity":"unknown","full_obligation_acceptance":"not_established",
        "auditor":"requires_separate_independent_review_and_execution"}),
    )
}
fn reopen_receipt(directory: &Path, expected: &PreparationBinding) -> Result<Value, String> {
    reopen_control_receipt(directory, expected, CaptureFixture::Baseline)
}
fn reopen_control_receipt(
    directory: &Path,
    expected: &PreparationBinding,
    fixture: CaptureFixture,
) -> Result<Value, String> {
    let prepared = reopen_control(directory, expected, fixture)?;
    let mut saved = Vec::new();
    File::open(directory.join("analyst-receipt.json"))
        .map_err(|e| e.to_string())?
        .take(MAX_RECEIPT as u64 + 1)
        .read_to_end(&mut saved)
        .map_err(|e| e.to_string())?;
    let fresh = collect(&prepared)?;
    if saved != bytes(&fresh)? {
        return Err("Analyst receipt differs from fresh durable authority.".into());
    }
    Ok(fresh)
}
fn run_analyst(
    directory: &Path,
    expected: &PreparationBinding,
    provider: Arc<dyn LlmProvider>,
) -> Result<Value, String> {
    run_control(directory, expected, provider, CaptureFixture::Baseline)
}
fn run_control(
    directory: &Path,
    expected: &PreparationBinding,
    provider: Arc<dyn LlmProvider>,
    fixture: CaptureFixture,
) -> Result<Value, String> {
    let prepared = reopen_control(directory, expected, fixture)?;
    if prepared.auditor_id.is_some() {
        return Err("Unexpected auditor task.".into());
    }
    execute_bound(directory, &Authority::synthetic(&prepared), provider)?;
    reopen_control_receipt(directory, expected, fixture)
}
pub(super) fn execute_bound(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
) -> Result<Value, String> {
    if descriptor(provider.as_ref(), InvestigationProviderMode::Local).map_err(|e| e.to_string())?
        != *authority.provider
    {
        return Err("Provider differs from frozen identity.".into());
    }
    // Durable exclusive marker precedes every dispatch capability. Any later
    // error, unknown acknowledgement or receipt failure keeps it consumed.
    exclusive(
        &directory.join("analyst-attempt.json"),
        &bytes(&json!({"binding":authority.binding,"manifest_hash":authority.manifest_hash}))?,
    )?;
    let state = authority.app.state::<crate::AppState>();
    let mut jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
    let detail = jobs
        .investigation(authority.analyst_id)
        .map_err(|e| e.to_string())?;
    if detail.summary.status != InvestigationStatus::Queued || detail.usage.model_invocations != 0 {
        return Err("Analyst is not fresh.".into());
    }
    let plan = jobs
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .map_err(|e| e.to_string())?;
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .map_err(|e| e.to_string())?;
    let (_, execution) = jobs
        .claim_execution(&plan, reservation)
        .map_err(|e| e.to_string())?;
    drop(jobs);
    let live = state
        .investigations
        .insert(authority.analyst_id)
        .map_err(|e| e.to_string())?;
    run_completion(
        authority.app.handle().clone(),
        authority.analyst_id.to_owned(),
        authority.request.clone(),
        execution,
        provider,
        live,
        Instant::now(),
    );
    let receipt = collect_bound(authority)?;
    exclusive(&directory.join("analyst-receipt.json"), &bytes(&receipt)?)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::super::tests::binding;
    use super::*;
    use crate::investigations::worker::tests::{
        Mode, Scripted, completion_tests::expanded_scripted_provider,
    };

    // AC-0287: this scripted analyst evidence leaves semantic review and #598 open.
    #[test]
    fn expanded_analyst_terminal_receipt_reopens_partial_refusal_and_unknown_without_replay() {
        for scenario in ["partial", "refused", "unknown", "receipt_error"] {
            let directory = tempfile::tempdir().unwrap();
            let provider: Arc<dyn LlmProvider> = match scenario {
                "unknown" => Arc::new(Scripted::new(Mode::Unknown)),
                "refused" => expanded_scripted_provider(false),
                _ => expanded_scripted_provider(true),
            };
            let descriptor =
                descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap();
            let prepared = super::super::prepare(directory.path(), &descriptor, binding()).unwrap();
            drop(prepared);
            if scenario == "receipt_error" {
                std::fs::create_dir(directory.path().join("analyst-receipt.json")).unwrap();
            }
            let receipt = run_analyst(directory.path(), &binding(), provider.clone());
            assert!(directory.path().join("analyst-attempt.json").is_file());
            let prepared = reopen(directory.path(), &binding()).unwrap();
            assert!(prepared.auditor_id.is_none());
            let before = collect(&prepared).unwrap();
            if scenario == "receipt_error" {
                assert!(receipt.is_err());
                assert!(reopen_receipt(directory.path(), &binding()).is_err());
            } else {
                let receipt = receipt.unwrap();
                assert_eq!(receipt, before);
                assert_eq!(
                    reopen_receipt(directory.path(), &binding()).unwrap(),
                    receipt
                );
                assert!(receipt["initial_prompt_bytes"].as_u64().unwrap() > 0);
                assert_eq!(receipt["capacity_refusal_count"], 0);
                match scenario {
                    "partial" => {
                        assert_eq!(receipt["result"]["knowledge_completeness"], "partial");
                        assert_eq!(receipt["result"]["schema_version"], 7);
                    }
                    "refused" => {
                        assert_eq!(receipt["finish_refusal_count"], 2);
                        assert!(receipt["result"].is_null());
                    }
                    _ => assert_eq!(
                        receipt["detail"]["status"],
                        json!(InvestigationStatus::OutcomeUnknown)
                    ),
                }
                std::fs::write(directory.path().join("analyst-receipt.json"), b"{}").unwrap();
                assert!(reopen_receipt(directory.path(), &binding()).is_err());
            }
            assert!(run_analyst(directory.path(), &binding(), provider).is_err());
            assert_eq!(
                collect(&reopen(directory.path(), &binding()).unwrap()).unwrap(),
                before
            );
        }
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::super::{Family, capture};
    use super::*;
    use crate::investigations::worker::tests::completion_tests::expanded_capacity_provider;
    use agents::investigation::staged_refusal::CompletionCallMode;

    // AC-0287: actual worker refusals and saved calls under the unchanged policy.
    #[test]
    fn expanded_analyst_capacity_refusals_reach_genuine_finish_only_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let mut binding = super::super::tests::binding();
        binding.run_id = "expanded-capacity-control".into();
        binding.protocol_hash = "d".repeat(64);
        let provider = expanded_capacity_provider();
        let descriptor = descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap();
        assert!(
            capture::prepare_fixture(
                directory.path(),
                &descriptor,
                binding.clone(),
                Family::V7,
                CaptureFixture::Capacity
            )
            .is_err()
        );
        let prepared = capture::prepare_fixture(
            directory.path(),
            &descriptor,
            binding.clone(),
            Family::Expanded,
            CaptureFixture::Capacity,
        )
        .unwrap();
        let targets = prepared
            .analyst_request
            .obligations
            .sources_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        assert_eq!(targets.len(), 1);
        assert!(targets[0].captured.byte_end - targets[0].captured.byte_start >= 7 * 1024);
        drop(prepared);
        assert!(reopen(directory.path(), &binding).is_err());
        let receipt = run_control(
            directory.path(),
            &binding,
            provider.clone(),
            CaptureFixture::Capacity,
        )
        .unwrap();
        assert_eq!(
            reopen_control_receipt(directory.path(), &binding, CaptureFixture::Capacity).unwrap(),
            receipt
        );
        eprintln!(
            "actual boundary: calls={}, refusals={}, FinishOnly={}, events={}, receipt bytes={}, first refusal={}",
            receipt["detail"]["usage"]["model_invocations"],
            receipt["capacity_refusal_count"],
            receipt["finish_only_calls"],
            receipt["events"].as_array().unwrap().len(),
            bytes(&receipt).unwrap().len(),
            receipt["capacity_refusals"][0]
        );
        let count = receipt["capacity_refusal_count"].as_u64().unwrap();
        assert!(count > 0);
        assert_eq!(receipt["finish_refusal_count"], 0);
        assert_eq!(receipt["finish_only_calls"], 1);
        assert!(receipt["result"].is_null());
        assert_eq!(
            receipt["detail"]["status"],
            json!(InvestigationStatus::Failed)
        );
        assert_eq!(
            receipt["detail"]["error"],
            crate::jobs::investigations::InvestigationStopReason::FinishOnlyToolProposal
                .test_message()
        );
        let usage = &receipt["detail"]["usage"];
        let first_refusal: Value =
            serde_json::from_str(receipt["capacity_refusals"][0].as_str().unwrap()).unwrap();
        let before_refusal = &first_refusal["call"]["admission"]["consumed_budgets"];
        assert_eq!(usage["evidence_items"], 2);
        for debit in [
            "evidence_requests",
            "evidence_items",
            "evidence_bytes",
            "captured_validation_bytes",
            "tool_actions",
        ] {
            assert_eq!(usage[debit], before_refusal[debit], "{debit}");
            for raw in receipt["capacity_refusals"].as_array().unwrap() {
                let entry: Value = serde_json::from_str(raw.as_str().unwrap()).unwrap();
                assert_eq!(
                    entry["call"]["admission"]["consumed_budgets"][debit], before_refusal[debit],
                    "{debit}"
                );
            }
        }
        assert_eq!(receipt["ledger"]["queries"].as_array().unwrap().len(), 1);
        let ledger: InvestigationInputLedger =
            serde_json::from_value(receipt["ledger"].clone()).unwrap();
        assert_eq!(ledger.queries.len(), 1);
        let source_citations = ledger
            .citations
            .iter()
            .filter(|citation| {
                matches!(
                    citation.origin,
                    InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
                )
            })
            .count();
        assert_eq!(source_citations, 2);
        assert_eq!(
            source_citations as u64,
            usage["evidence_items"].as_u64().unwrap()
        );
        let calls = receipt["invocations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|raw| {
                serde_json::from_str::<PendingInvestigationStep>(raw.as_str().unwrap()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls.len() as u64,
            count + usage["evidence_items"].as_u64().unwrap() + 2
        ); // Query, acquired/refused reads, final stop.
        assert_eq!(usage["model_invocations"], calls.len() as u64);
        assert_eq!(
            usage["generated_token_reservations"],
            calls.len() as u64
                * u64::from(
                    INVESTIGATION_V2_POLICY
                        .limits()
                        .generated_tokens_per_invocation
                )
        );
        assert_eq!(calls.len(), 63);
        assert_eq!(usage["model_invocations"], 63);
        for (ordinal, call) in calls.iter().enumerate() {
            assert!(
                call.input_bytes > 0
                    && call.input_bytes
                        <= llm::bounded::CompletionLimitValues::default().input_bytes as usize
            );
            let admission = call
                .disclosure
                .as_ref()
                .unwrap()
                .admission
                .as_ref()
                .unwrap();
            assert_eq!(
                admission.consumed_budgets.model_invocations as usize,
                ordinal
            );
            if ordinal + 1 == calls.len() {
                assert_eq!(admission.consumed_budgets.model_invocations, 62);
            }
            assert_eq!(
                admission.mode,
                if ordinal + 1 == calls.len() {
                    CompletionCallMode::FinishOnly
                } else {
                    CompletionCallMode::Choice
                }
            );
        }
        let events = receipt["events"].as_array().unwrap();
        assert!(
            events.len() > 96 && events.len() <= INVESTIGATION_V2_POLICY.journal_events() as usize
        );
        assert!(bytes(&receipt).unwrap().len() <= MAX_RECEIPT);
        assert!(
            run_control(
                directory.path(),
                &binding,
                provider,
                CaptureFixture::Capacity
            )
            .is_err()
        );
        assert_eq!(
            reopen_control_receipt(directory.path(), &binding, CaptureFixture::Capacity).unwrap(),
            receipt
        );
    }
}
