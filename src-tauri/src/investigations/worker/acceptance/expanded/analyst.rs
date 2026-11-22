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
    super::vendure_prepare::sync_directory(path.parent().ok_or("Missing artifact parent.")?)
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
    pub run_limits: Option<super::vendure_prepare::RunLimits>,
}
impl<'a> Authority<'a> {
    pub(super) fn synthetic(prepared: &'a PreparedState) -> Self {
        Self {
            app: &prepared.app,
            app_data: &prepared.app_data,
            analyst_id: &prepared.analyst_id,
            request: &prepared.analyst_request,
            binding: &prepared.manifest.binding,
            manifest_hash: &prepared.manifest_hash,
            provider: &prepared.manifest.provider,
            run_limits: None,
        }
    }
}
fn collect(prepared: &PreparedState) -> Result<Value, String> {
    collect_bound(&Authority::synthetic(prepared))
}
pub(super) fn collect_bound(authority: &Authority<'_>) -> Result<Value, String> {
    collect_role(authority, "analyst")
}
fn receipt_family(frozen: &str, saved: SpecialistId, role: &str) -> Result<&'static str, String> {
    if frozen == "evidence-auditor@14" || saved == SpecialistId::EvidenceAuditorV14 {
        if frozen != "evidence-auditor@14"
            || saved != SpecialistId::EvidenceAuditorV14
            || role != "auditor"
        {
            return Err("Auditor correction receipt identity mismatch.".into());
        }
        return Ok("request7-specialist14-prompt13-disclosure6-result7");
    }
    if frozen == "evidence-auditor@13" || saved == SpecialistId::EvidenceAuditorV13 {
        if frozen != "evidence-auditor@13"
            || saved != SpecialistId::EvidenceAuditorV13
            || role != "auditor"
        {
            return Err("Auditor correction receipt identity mismatch.".into());
        }
        return Ok("request7-specialist13-prompt12-disclosure6-result7");
    }
    if frozen == "domain-analyst@29" || saved == SpecialistId::DomainAnalystV29 {
        if frozen != "domain-analyst@29"
            || saved != SpecialistId::DomainAnalystV29
            || role != "analyst"
        {
            return Err("Tool-calling receipt identity mismatch.".into());
        }
        return Ok("request7-specialist29-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@28" || saved == SpecialistId::DomainAnalystV28 {
        if frozen != "domain-analyst@28"
            || saved != SpecialistId::DomainAnalystV28
            || role != "analyst"
        {
            return Err("Visible schema receipt identity mismatch.".into());
        }
        return Ok("request7-specialist28-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@27" || saved == SpecialistId::DomainAnalystV27 {
        if frozen != "domain-analyst@27"
            || saved != SpecialistId::DomainAnalystV27
            || role != "analyst"
        {
            return Err("Finding withholding receipt identity mismatch.".into());
        }
        return Ok("request7-specialist27-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@26" || saved == SpecialistId::DomainAnalystV26 {
        if frozen != "domain-analyst@26"
            || saved != SpecialistId::DomainAnalystV26
            || role != "analyst"
        {
            return Err("Finding withholding receipt identity mismatch.".into());
        }
        return Ok("request7-specialist26-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@25" || saved == SpecialistId::DomainAnalystV25 {
        if frozen != "domain-analyst@25"
            || saved != SpecialistId::DomainAnalystV25
            || role != "analyst"
        {
            return Err("Finding withholding receipt identity mismatch.".into());
        }
        return Ok("request7-specialist25-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@24" || saved == SpecialistId::DomainAnalystV24 {
        if frozen != "domain-analyst@24"
            || saved != SpecialistId::DomainAnalystV24
            || role != "analyst"
        {
            return Err("Combined feedback receipt identity mismatch.".into());
        }
        return Ok("request7-specialist24-prompt14-disclosure6-result7");
    }
    if frozen == "domain-analyst@23" || saved == SpecialistId::DomainAnalystV23 {
        if frozen != "domain-analyst@23"
            || saved != SpecialistId::DomainAnalystV23
            || role != "analyst"
        {
            return Err("Claim feedback receipt identity mismatch.".into());
        }
        return Ok("request7-specialist23-prompt13-disclosure6-result7");
    }

    if frozen == "domain-analyst@22" || saved == SpecialistId::DomainAnalystV22 {
        if frozen != "domain-analyst@22"
            || saved != SpecialistId::DomainAnalystV22
            || role != "analyst"
        {
            return Err("Finish feedback receipt identity mismatch.".into());
        }
        return Ok("request7-specialist22-prompt12-disclosure6-result7");
    }
    if frozen == "domain-analyst@21" || saved == SpecialistId::DomainAnalystV21 {
        if frozen != "domain-analyst@21"
            || saved != SpecialistId::DomainAnalystV21
            || role != "analyst"
        {
            return Err("Capacity exclusion receipt identity mismatch.".into());
        }
        return Ok("request7-specialist21-prompt11-disclosure6-result7");
    }
    if frozen == "domain-analyst@20" || saved == SpecialistId::DomainAnalystV20 {
        if frozen != "domain-analyst@20"
            || saved != SpecialistId::DomainAnalystV20
            || role != "analyst"
        {
            return Err("Typed correction receipt identity mismatch.".into());
        }
        return Ok("request7-specialist20-prompt9-disclosure6-result7");
    }
    if frozen == "domain-analyst@19" || saved == SpecialistId::DomainAnalystV19 {
        if frozen != "domain-analyst@19"
            || saved != SpecialistId::DomainAnalystV19
            || role != "analyst"
        {
            return Err("Correction receipt identity mismatch.".into());
        }
        return Ok("request7-specialist19-prompt9-disclosure6-result7");
    }
    if frozen == "domain-analyst@18" || saved == SpecialistId::DomainAnalystV18 {
        if frozen != "domain-analyst@18"
            || saved != SpecialistId::DomainAnalystV18
            || role != "analyst"
        {
            return Err("Acquisition receipt identity mismatch.".into());
        }
        return Ok("request7-specialist18-prompt9-disclosure6-result7");
    }
    // Historical receipts, including auditor11, retain their exact family bytes.
    Ok("request7-specialist10-prompt9-disclosure6-result7")
}

pub(super) fn collect_role(authority: &Authority<'_>, role: &str) -> Result<Value, String> {
    let mut receipt = collect_saved(
        authority.app_data,
        authority.analyst_id,
        authority.request,
        authority.binding,
        authority.manifest_hash,
        role,
    )?;
    bind_run_limits(
        &mut receipt,
        authority.app_data,
        authority.analyst_id,
        authority.run_limits,
    )?;
    Ok(receipt)
}
fn bind_run_limits(
    receipt: &mut Value,
    app_data: &Path,
    analyst_id: &str,
    expected: Option<super::vendure_prepare::RunLimits>,
) -> Result<(), String> {
    if let Some(expected) = expected {
        let jobs = JobStore::open(app_data.join("state.db")).map_err(|e| e.to_string())?;
        if jobs
            .captured_run_limits(analyst_id)
            .map_err(|e| e.to_string())?
            != Some((expected.active_seconds, expected.request_timeout_ms))
        {
            return Err("receipt run limits differ from frozen definition".into());
        }
        receipt["run_limits"] = serde_json::to_value(expected).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn collect_saved(
    app_data: &Path,
    analyst_id: &str,
    request: &StagedObligationRequest,
    binding: &PreparationBinding,
    manifest_hash: &str,
    role: &str,
) -> Result<Value, String> {
    let database = app_data.join("state.db");
    let jobs = JobStore::open(&database).map_err(|e| e.to_string())?;
    // This production reader authenticates the saved task and specialist metadata.
    let (detail, result, ledger, events) =
        super::super::completion::execution::read_expanded_state(&jobs, analyst_id)?;
    let family = receipt_family(&request.specialist_id, detail.summary.specialist_id, role)?;
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
    let invocations = rows(&db, analyst_id, "investigation_invocations")?;
    let calls = invocations
        .iter()
        .map(|body| {
            serde_json::from_str::<PendingInvestigationStep>(body).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let capacity = rows(&db, analyst_id, "investigation_admission_refusals")?;
    let finish = rows(&db, analyst_id, "staged_completion_refusals")?;
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
    let mut receipt = json!({"schema_version":1,"family":family,"role":role,
        "binding":binding,"manifest_hash":manifest_hash,
        "request_hash":request.intent_hash_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e| e.to_string())?,
        "obligations_hash":request.obligations.fingerprint_with_policy(&INVESTIGATION_V2_POLICY).map_err(|e|e.to_string())?,
        "initial_prompt_measurement_basis":"first_saved_invocation_only_not_pre_dispatch_preparation",
        "initial_prompt_bytes":calls.first().map(|call| call.input_bytes),
        "finish_only_calls":finish_only_calls,"choice_calls":calls.len()-finish_only_calls,
        "capacity_refusal_count":capacity.len(),"finish_refusal_count":finish.len(),
        "detail":detail,"result":result,"ledger":ledger,"events":events,
        "invocations":invocations,"capacity_refusals":capacity,"finish_refusals":finish,
        "tokenizer_context_capacity":"unknown","full_obligation_acceptance":"not_established",
        "auditor":"requires_separate_independent_review_and_execution"});
    let diagnostics = jobs
        .investigation_refusal_diagnostics(analyst_id)
        .map_err(|e| e.to_string())?;
    // Historical receipts retain their exact serialized shape and bytes.
    if !diagnostics.is_empty() {
        receipt["finish_diagnostics"] =
            serde_json::to_value(diagnostics).map_err(|e| e.to_string())?;
    }
    Ok(receipt)
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
#[derive(Clone, Copy)]
pub(super) enum BoundaryClock {
    Real,
    FrozenActive,
}

fn run_control(
    directory: &Path,
    expected: &PreparationBinding,
    provider: Arc<dyn LlmProvider>,
    fixture: CaptureFixture,
) -> Result<Value, String> {
    run_control_with_clock(directory, expected, provider, fixture, BoundaryClock::Real)
}
fn run_control_frozen_active(
    directory: &Path,
    expected: &PreparationBinding,
    provider: Arc<dyn LlmProvider>,
    fixture: CaptureFixture,
) -> Result<Value, String> {
    run_control_with_clock(
        directory,
        expected,
        provider,
        fixture,
        BoundaryClock::FrozenActive,
    )
}
fn run_control_with_clock(
    directory: &Path,
    expected: &PreparationBinding,
    provider: Arc<dyn LlmProvider>,
    fixture: CaptureFixture,
    clock: BoundaryClock,
) -> Result<Value, String> {
    let prepared = reopen_control(directory, expected, fixture)?;
    if prepared.auditor_id.is_some() {
        return Err("Unexpected auditor task.".into());
    }
    execute_bound_with_clock(directory, &Authority::synthetic(&prepared), provider, clock)?;
    reopen_control_receipt(directory, expected, fixture)
}
pub(super) fn execute_bound(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
) -> Result<Value, String> {
    execute_bound_with_clock(directory, authority, provider, BoundaryClock::Real)
}
fn execute_bound_with_clock(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
    clock: BoundaryClock,
) -> Result<Value, String> {
    execute_role_with_clock(directory, authority, provider, "analyst", clock)?;
    let receipt = collect_bound(authority)?;
    exclusive(&directory.join("analyst-receipt.json"), &bytes(&receipt)?)?;
    Ok(receipt)
}
pub(super) fn execute_role(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
    role: &str,
) -> Result<(), String> {
    execute_role_with_clock(directory, authority, provider, role, BoundaryClock::Real)
}
fn execute_role_with_clock(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
    role: &str,
    clock: BoundaryClock,
) -> Result<(), String> {
    if descriptor(provider.as_ref(), authority.provider.mode).map_err(|e| e.to_string())?
        != *authority.provider
    {
        return Err("Provider differs from frozen identity.".into());
    }
    // A hosted run needs the owner's exact authorization and its scoped
    // consent before the attempt marker is consumed. Any later error drops the
    // consent, which reverts the tier.
    let hosted = match authority.provider.mode {
        InvestigationProviderMode::Cloud => Some(super::cloud_run::Consent::grant(
            authority.app.handle(),
            authority.provider,
            &super::cloud_run::authorization_from_env(authority.provider)?,
            &directory.join(format!("{role}-consent-log.jsonl")),
        )?),
        InvestigationProviderMode::Local => None,
    };
    execute_role_with_granted_consent(directory, authority, provider, role, clock, hosted)
}

// A synthetic test can grant the same scoped consent with explicit identity and
// cap, without environment mutation or constructing a network provider.
pub(super) fn execute_role_with_granted_consent(
    directory: &Path,
    authority: &Authority<'_>,
    provider: Arc<dyn LlmProvider>,
    role: &str,
    clock: BoundaryClock,
    hosted: Option<super::cloud_run::Consent>,
) -> Result<(), String> {
    if descriptor(provider.as_ref(), authority.provider.mode).map_err(|e| e.to_string())?
        != *authority.provider
        || hosted.is_some() != (authority.provider.mode == InvestigationProviderMode::Cloud)
    {
        return Err("Provider or scoped consent differs from frozen identity.".into());
    }
    // Durable exclusive marker precedes every dispatch capability. Any later
    // error, unknown acknowledgement or receipt failure keeps it consumed.
    exclusive(
        &directory.join(format!("{role}-attempt.json")),
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
    let started = Instant::now();
    let run = move || match clock {
        BoundaryClock::Real => run_completion(
            authority.app.handle().clone(),
            authority.analyst_id.to_owned(),
            authority.request.clone(),
            execution,
            provider,
            live,
            started,
        ),
        BoundaryClock::FrozenActive => {
            crate::investigations::worker::run_completion_with_frozen_active_clock(
                authority.app.handle().clone(),
                authority.analyst_id.to_owned(),
                authority.request.clone(),
                execution,
                provider,
                live,
                started,
            )
        }
    };
    match hosted {
        Some(consent) => consent.approve_while(authority.analyst_id, run),
        None => {
            run();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::binding;
    use super::*;
    use crate::investigations::worker::tests::{
        Mode, Scripted, completion_tests::expanded_scripted_provider,
    };

    // AC-0554: no provider or fabricated durable state is needed for identity controls.
    #[test]
    fn captured_receipt_family_preserves_history_and_binds_acquisition_identity() {
        let legacy = "request7-specialist10-prompt9-disclosure6-result7";
        for version in [10, 11, 12, 13, 14, 15, 17] {
            let id = format!("domain-analyst@{version}");
            assert_eq!(
                receipt_family(&id, serde_json::from_value(json!(id)).unwrap(), "analyst").unwrap(),
                legacy
            );
        }
        assert_eq!(
            receipt_family(
                "evidence-auditor@11",
                SpecialistId::EvidenceAuditorV11,
                "auditor"
            )
            .unwrap(),
            legacy
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@18",
                SpecialistId::DomainAnalystV18,
                "analyst"
            )
            .unwrap(),
            "request7-specialist18-prompt9-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@19",
                SpecialistId::DomainAnalystV19,
                "analyst"
            )
            .unwrap(),
            "request7-specialist19-prompt9-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@20",
                SpecialistId::DomainAnalystV20,
                "analyst"
            )
            .unwrap(),
            "request7-specialist20-prompt9-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@21",
                SpecialistId::DomainAnalystV21,
                "analyst"
            )
            .unwrap(),
            "request7-specialist21-prompt11-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@22",
                SpecialistId::DomainAnalystV22,
                "analyst"
            )
            .unwrap(),
            "request7-specialist22-prompt12-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@24",
                SpecialistId::DomainAnalystV24,
                "analyst"
            )
            .unwrap(),
            "request7-specialist24-prompt14-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@25",
                SpecialistId::DomainAnalystV25,
                "analyst"
            )
            .unwrap(),
            "request7-specialist25-prompt14-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@26",
                SpecialistId::DomainAnalystV26,
                "analyst"
            )
            .unwrap(),
            "request7-specialist26-prompt14-disclosure6-result7"
        );
        // AC-0835 (#1335): @28 has its own receipt family; @27's is unchanged.
        assert_eq!(
            receipt_family(
                "domain-analyst@27",
                SpecialistId::DomainAnalystV27,
                "analyst"
            )
            .unwrap(),
            "request7-specialist27-prompt14-disclosure6-result7"
        );
        assert_eq!(
            receipt_family(
                "domain-analyst@28",
                SpecialistId::DomainAnalystV28,
                "analyst"
            )
            .unwrap(),
            "request7-specialist28-prompt14-disclosure6-result7"
        );
        // AC-1429 (ADR-0085): @29 has its own receipt family.
        assert_eq!(
            receipt_family(
                "domain-analyst@29",
                SpecialistId::DomainAnalystV29,
                "analyst"
            )
            .unwrap(),
            "request7-specialist29-prompt14-disclosure6-result7"
        );
        for (frozen, saved, role) in [
            ("domain-analyst@29", "domain-analyst@28", "analyst"),
            ("domain-analyst@28", "domain-analyst@29", "analyst"),
            ("domain-analyst@29", "domain-analyst@29", "auditor"),
            ("domain-analyst@28", "domain-analyst@27", "analyst"),
            ("domain-analyst@27", "domain-analyst@28", "analyst"),
            ("domain-analyst@28", "domain-analyst@28", "auditor"),
            ("domain-analyst@26", "domain-analyst@25", "analyst"),
            ("domain-analyst@25", "domain-analyst@26", "analyst"),
            ("domain-analyst@26", "domain-analyst@26", "auditor"),
            ("domain-analyst@25", "domain-analyst@24", "analyst"),
            ("domain-analyst@24", "domain-analyst@25", "analyst"),
            ("domain-analyst@25", "domain-analyst@25", "auditor"),
            ("domain-analyst@25", "domain-analyst@17", "analyst"),
            ("domain-analyst@17", "domain-analyst@25", "analyst"),
            ("domain-analyst@25", "evidence-auditor@11", "auditor"),
            ("evidence-auditor@11", "domain-analyst@25", "auditor"),
            ("domain-analyst@24", "domain-analyst@23", "analyst"),
            ("domain-analyst@23", "domain-analyst@24", "analyst"),
            ("domain-analyst@24", "domain-analyst@24", "auditor"),
            ("domain-analyst@24", "domain-analyst@17", "analyst"),
            ("domain-analyst@17", "domain-analyst@24", "analyst"),
            ("domain-analyst@24", "evidence-auditor@11", "auditor"),
            ("evidence-auditor@11", "domain-analyst@24", "auditor"),
            ("domain-analyst@22", "domain-analyst@21", "analyst"),
            ("domain-analyst@21", "domain-analyst@22", "analyst"),
            ("domain-analyst@22", "domain-analyst@22", "auditor"),
            ("domain-analyst@21", "domain-analyst@20", "analyst"),
            ("domain-analyst@20", "domain-analyst@21", "analyst"),
            ("domain-analyst@21", "domain-analyst@21", "auditor"),
            ("domain-analyst@20", "domain-analyst@19", "analyst"),
            ("domain-analyst@19", "domain-analyst@20", "analyst"),
            ("domain-analyst@20", "domain-analyst@20", "auditor"),
            ("domain-analyst@19", "domain-analyst@18", "analyst"),
            ("domain-analyst@18", "domain-analyst@19", "analyst"),
            ("domain-analyst@19", "domain-analyst@19", "auditor"),
            ("domain-analyst@18", "domain-analyst@17", "analyst"),
            ("domain-analyst@17", "domain-analyst@18", "analyst"),
            ("domain-analyst@18", "domain-analyst@18", "auditor"),
            ("domain-analyst@18", "evidence-auditor@11", "auditor"),
        ] {
            assert!(
                receipt_family(frozen, serde_json::from_value(json!(saved)).unwrap(), role)
                    .is_err()
            );
        }
    }

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
            // AC-0287 / #1707: historical roles retain the receipt's old shape,
            // including after refusal and unknown-outcome recovery.
            assert!(before.get("finish_diagnostics").is_none());
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
        let receipt = run_control_frozen_active(
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
            "actual boundary: calls={}, refusals={}, FinishOnly={}, events={}, receipt bytes={}, status={}, error={:?}, usage={}",
            receipt["detail"]["usage"]["model_invocations"],
            receipt["capacity_refusal_count"],
            receipt["finish_only_calls"],
            receipt["events"].as_array().unwrap().len(),
            bytes(&receipt).unwrap().len(),
            receipt["detail"]["status"],
            receipt["detail"]["error"],
            receipt["detail"]["usage"]
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

// AC-0287 / #1707: offline historical compatibility; opens source read-only,
// then runs all production state authentication against a disposable copy.
#[test]
#[ignore = "requires explicit historical development artifact root; no provider construction"]
fn copied_historical_analyst_receipt_reopens_byte_identically() {
    use sha2::{Digest, Sha256};
    let sha = |bytes: &[u8]| {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let root =
        std::path::PathBuf::from(super::super::required("CARTOGRAPH_HISTORICAL_ANALYST_ROOT"));
    let output = root.join("analyst-output");
    let read = |name: &str| {
        super::super::vendure::read_preparation_artifact(&output.join(name), MAX_RECEIPT).unwrap()
    };
    let saved = read("analyst-receipt.json");
    let request_bytes = read("analyst-request.json");
    let request: StagedObligationRequest = serde_json::from_slice(&request_bytes).unwrap();
    let start_bytes = read("start.json");
    assert_eq!(
        sha(&start_bytes),
        super::super::required("CARTOGRAPH_HISTORICAL_START_SHA256")
    );
    let start: Value = serde_json::from_slice(&start_bytes).unwrap();
    assert_eq!(
        sha(&request_bytes),
        start["request_sha256"].as_str().unwrap()
    );
    let definition_bytes = read("definition.json");
    assert_eq!(
        sha(&definition_bytes),
        start["definition_sha256"].as_str().unwrap()
    );
    let definition: Value = serde_json::from_slice(&definition_bytes).unwrap();
    let binding: PreparationBinding =
        serde_json::from_value(definition["binding"].clone()).unwrap();
    let manifest: Value = serde_json::from_slice(&read("captured-analyst.json")).unwrap();
    let manifest_hash = content_hash(&serde_json::to_vec(&manifest).unwrap());
    let source = rusqlite::Connection::open_with_flags(
        root.join("profile/data/state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let copied = tempfile::tempdir().unwrap();
    let mut destination = rusqlite::Connection::open(copied.path().join("state.db")).unwrap();
    assert_eq!(
        rusqlite::backup::Backup::new(&source, &mut destination)
            .unwrap()
            .step(-1)
            .unwrap(),
        rusqlite::backup::StepResult::Done
    );
    drop(source);
    drop(destination);
    let mut fresh = collect_saved(
        copied.path(),
        start["analyst_id"].as_str().unwrap(),
        &request,
        &binding,
        &manifest_hash,
        "analyst",
    )
    .unwrap();
    // Reuse the production receipt projection, including authenticated frozen
    // time overrides. Never copy an expected field out of the old receipt.
    bind_run_limits(
        &mut fresh,
        copied.path(),
        start["analyst_id"].as_str().unwrap(),
        serde_json::from_value(definition.get("run_limits").cloned().unwrap_or(Value::Null))
            .unwrap(),
    )
    .unwrap();
    let captured: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        fresh.get("finish_diagnostics"),
        captured.get("finish_diagnostics"),
        "historical diagnostic presence or content changed"
    );
    assert!(
        saved == bytes(&fresh).unwrap(),
        "historical receipt bytes changed"
    );
}
