//! One explicitly selected role through the production coordinator and worker.
//! Reviewer/owner gates belong to the outer stage entrypoint, never a provider.
use super::{
    prepare::PreparedState,
    records::{self, CheckStatus, MechanicalCheck, Role, RoleReceipt, RunBinding},
};
use crate::{
    investigations::{runtime::descriptor, worker::run_completion},
    jobs::{
        ClaimMode, JobStore,
        investigations::{InvestigationResponseMeta, PendingInvestigationStep},
    },
};
use agents::investigation::*;
use core_graph::SnapshotReadLimits;
use core_prov::{ConfidenceTier, Tier, content_hash};
use llm::bounded::{
    AuthorizedBoundedCompletion, BoundedCallError, BoundedCompletion, CompletionControl,
    FailureCode, InvocationOutcome, ProviderProfile,
};
use llm::{Embedding, LlmProvider, Locality, ProviderCaps, ProviderError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};
use tauri::Manager;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct InvocationObservation {
    pub payload_hash: String,
    pub input_ledger_hash: String,
    pub context_id: String,
    pub source_hashes: BTreeMap<String, String>,
    pub inventory_hashes: BTreeMap<String, String>,
    pub history_hash: Option<String>,
    pub parent_result_hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseObservation>,
}
/// Untrusted transport outcome, not host admission or semantic acceptance.
/// Old observations omit this field and retain their original wire bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub(in super::super) enum ResponseObservation {
    #[serde(rename = "cartograph-response-observation@1")]
    Returned {
        response_class: ResponseClass,
        response_hash: String,
        reported_output_tokens: Option<u64>,
    },
    #[serde(rename = "cartograph-response-failure@1")]
    Failed {
        code: FailureCode,
        outcome: InvocationOutcome,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in super::super) enum ResponseClass {
    Finish,
    Read,
    Other,
}
impl ResponseObservation {
    fn from_result(result: &Result<BoundedCompletion, BoundedCallError>) -> Self {
        match result {
            Ok(response) => {
                let value = serde_json::from_str::<Value>(&response.text).ok();
                let response_class = match value.as_ref() {
                    Some(v) if v["action"]["type"] == "finish" || v["type"] == "finish" => {
                        ResponseClass::Finish
                    }
                    Some(v)
                        if matches!(
                            v["type"].as_str(),
                            Some(
                                "query_context"
                                    | "read_evidence"
                                    | "read_coverage_inventory"
                                    | "replay_query"
                            )
                        ) =>
                    {
                        ResponseClass::Read
                    }
                    _ => ResponseClass::Other,
                };
                Self::Returned {
                    response_class,
                    response_hash: content_hash(response.text.as_bytes()),
                    reported_output_tokens: response.usage.as_ref().and_then(|u| u.output_tokens),
                }
            }
            Err(error) => Self::Failed {
                code: error.code,
                outcome: error.outcome,
            },
        }
    }
}
#[derive(Debug, Serialize)]
pub(super) struct RoleEvidence {
    pub detail: InvestigationDetail,
    pub result: Option<InvestigationResult>,
    pub ledger: Option<InvestigationInputLedger>,
    pub events: Vec<InvestigationEvent>,
    pub mechanical_checks: Vec<MechanicalCheck>,
    pub receipt: RoleReceipt,
    pub observations: Vec<InvocationObservation>,
    pub representation_reviewable: bool,
}
#[derive(Debug)]
pub(super) struct RoleRunError {
    pub post_dispatch: bool,
    pub reason: String,
    pub observations: Vec<InvocationObservation>,
}
impl From<String> for RoleRunError {
    fn from(reason: String) -> Self {
        Self {
            post_dispatch: false,
            reason,
            observations: vec![],
        }
    }
}
impl From<&str> for RoleRunError {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}
impl std::fmt::Display for RoleRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}
impl std::error::Error for RoleRunError {}
pub(in super::super) struct ObservedProvider {
    inner: Arc<dyn LlmProvider>,
    context_id: String,
    observations: Mutex<Vec<InvocationObservation>>,
    next: bool,
}
impl ObservedProvider {
    pub(in super::super) fn next(inner: Arc<dyn LlmProvider>, context_id: String) -> Self {
        Self {
            inner,
            context_id,
            observations: Mutex::new(vec![]),
            next: true,
        }
    }
    pub(in super::super) fn observations(&self) -> Result<Vec<InvocationObservation>, String> {
        self.observations
            .lock()
            .map(|v| v.clone())
            .map_err(|_| "Invocation observations unavailable.".into())
    }
}
fn observation(
    request: &AuthorizedBoundedCompletion,
    next: bool,
) -> Result<InvocationObservation, String> {
    let payload = request.payload();
    let prompt: Value =
        serde_json::from_str(&payload.prompt).map_err(|_| "Actual input is not JSON.")?;
    if prompt["schema_version"] != if next { 9 } else { 6 } {
        return Err(if next {
            "Actual invocation is not input9."
        } else {
            "Actual invocation is not input6."
        }
        .into());
    }
    let string = |key: &str| {
        prompt[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("Actual input lacks {key}."))
    };
    let mut source_hashes = BTreeMap::new();
    if payload.spans.len()
        > if next {
            INVESTIGATION_V2_POLICY.limits().evidence_items
        } else {
            12
        }
    {
        return Err("Too many actual source spans.".into());
    }
    for span in &payload.spans {
        if source_hashes
            .insert(span.id.clone(), content_hash(span.text.as_bytes()))
            .is_some()
        {
            return Err("Duplicate actual source span.".into());
        }
    }
    let mut inventory_hashes = BTreeMap::new();
    let inventories = prompt["coverage_details"]
        .as_array()
        .ok_or("Actual coverage details missing.")?;
    if inventories.len()
        > if next {
            INVESTIGATION_V2_POLICY.limits().tool_actions as usize
        } else {
            8
        }
    {
        return Err("Too many actual inventories.".into());
    }
    for detail in inventories {
        let identity = detail["inventory_hash"]
            .as_str()
            .ok_or("Inventory identity missing.")?;
        let body = detail["inventory_json"]
            .as_str()
            .ok_or("Actual inventory body missing.")?;
        let hash = content_hash(body.as_bytes());
        if inventory_hashes
            .insert(identity.into(), hash.clone())
            .is_some_and(|prior| prior != hash)
        {
            return Err("Conflicting actual inventory bodies.".into());
        }
    }
    let history = prompt["prior_findings_as_t3_history"]
        .as_array()
        .ok_or("Actual history missing.")?;
    let history_hash = if history.is_empty() {
        None
    } else {
        Some(content_hash(
            &serde_json::to_vec(history).map_err(|error| error.to_string())?,
        ))
    };
    let mut parent_result_hashes = BTreeMap::new();
    for raw in history {
        let entry: Value =
            serde_json::from_str(raw.as_str().ok_or("History is not a string record.")?)
                .map_err(|error| error.to_string())?;
        if !entry["saved_result"].is_null() {
            let result: InvestigationResult = serde_json::from_value(entry["saved_result"].clone())
                .map_err(|error| error.to_string())?;
            if parent_result_hashes
                .insert(
                    result.result_id.clone(),
                    content_hash(&serde_json::to_vec(&result).map_err(|error| error.to_string())?),
                )
                .is_some()
            {
                return Err("Duplicate parent history result.".into());
            }
        }
    }
    Ok(InvocationObservation {
        response: None,
        payload_hash: request.payload_hash().into(),
        input_ledger_hash: string("input_ledger_hash")?,
        context_id: string("context_id")?,
        source_hashes,
        inventory_hashes,
        history_hash,
        parent_result_hashes,
    })
}
impl LlmProvider for ObservedProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn supports_bounded_response_schema(&self) -> bool {
        self.inner.supports_bounded_response_schema()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let fail = || BoundedCallError {
            code: FailureCode::InvalidInput,
            outcome: InvocationOutcome::NotDispatched,
        };
        let observed = observation(request, self.next).map_err(|_| fail())?;
        if observed.context_id != self.context_id {
            return Err(fail());
        }
        let ordinal = {
            let mut observations = self.observations.lock().map_err(|_| fail())?;
            if observations.len()
                >= if self.next {
                    INVESTIGATION_V2_POLICY.limits().model_invocations as usize
                } else {
                    8
                }
            {
                return Err(fail());
            }
            let ordinal = observations.len();
            observations.push(observed);
            ordinal
        };
        // Same authorized request and cancellation/control object, exactly once.
        let result = self.inner.complete_bounded(request, control);
        // Retain metadata before host admission can roll back its transaction.
        // Never dispatch again when observation bookkeeping fails.
        if let Ok(mut observations) = self.observations.lock() {
            observations[ordinal].response = Some(ResponseObservation::from_result(&result));
        }
        result
    }
}
fn check(name: &str, passed: bool) -> MechanicalCheck {
    MechanicalCheck {
        name: name.into(),
        status: if passed {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
    }
}
fn unknown(name: &str) -> MechanicalCheck {
    MechanicalCheck {
        name: name.into(),
        status: CheckStatus::Unknown,
    }
}
pub(in super::super) fn rows<T: DeserializeOwned + Serialize>(
    path: &Path,
    id: &str,
    table: &str,
    max_rows: usize,
    max_body: usize,
) -> Result<Vec<T>, String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())?;
    let (count,bytes):(i64,i64)=connection.query_row(&format!("SELECT count(*),coalesce(sum(length(CAST(body AS BLOB))),0) FROM {table} WHERE task_id=?1"),[id],|row|Ok((row.get(0)?,row.get(1)?))).map_err(|error|error.to_string())?;
    if count < 0 || count as usize > max_rows || !(0..=2 * 1024 * 1024).contains(&bytes) {
        return Err("Durable audit rows exceed their bound.".into());
    }
    let mut statement=connection.prepare(&format!("SELECT CASE WHEN typeof(body)='blob' AND length(body)<=?2 THEN body END,CASE WHEN typeof(body_hash)='text' AND length(body_hash)=64 THEN body_hash END FROM {table} WHERE task_id=?1 ORDER BY rowid")).map_err(|error|error.to_string())?;
    statement
        .query_map(rusqlite::params![id, max_body as i64], |row| {
            Ok((
                row.get::<_, Option<Vec<u8>>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .map(|row| {
            let (Some(body), Some(hash)) = row.map_err(|error| error.to_string())? else {
                return Err("Audit body has invalid type or size.".into());
            };
            if content_hash(&body) != hash {
                return Err("Audit body hash changed.".into());
            }
            let typed: T = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
            if serde_json::to_vec(&typed).map_err(|error| error.to_string())? != body {
                return Err("Audit body is not canonical.".into());
            }
            Ok(typed)
        })
        .collect()
}
pub(in super::super) type SavedState = (
    InvestigationDetail,
    Option<InvestigationResult>,
    Option<InvestigationInputLedger>,
    Vec<InvestigationEvent>,
);
pub(in super::super) fn read_state(jobs: &JobStore, id: &str) -> Result<SavedState, String> {
    let state = jobs
        .investigation_snapshot(id)
        .map_err(|error| error.to_string())?;
    bounded_events(state, 96)
}
pub(in super::super) fn read_expanded_state(
    jobs: &JobStore,
    id: &str,
) -> Result<SavedState, String> {
    let state = jobs
        .investigation_snapshot(id)
        .map_err(|error| error.to_string())?;
    expanded(state)
}
/// `read_expanded_state` plus the refusal diagnostics, from one snapshot.
pub(in super::super) fn read_expanded_receipt_state(
    jobs: &JobStore,
    id: &str,
) -> Result<
    (
        SavedState,
        Vec<agents::investigation::diagnostic_sidecar::Sidecar>,
    ),
    String,
> {
    let (state, diagnostics) = jobs
        .investigation_receipt_snapshot(id)
        .map_err(|error| error.to_string())?;
    Ok((expanded(state)?, diagnostics))
}
fn expanded(state: SavedState) -> Result<SavedState, String> {
    if state.0.limits.profile != INVESTIGATION_V2_POLICY.limits().profile {
        return Err("Expanded receipt requires the registered V2 policy.".into());
    }
    let policy = crate::jobs::investigations::policy_for_detail(&state.0)
        .map_err(|error| error.to_string())?;
    bounded_events(state, policy.journal_events() as usize)
}
fn bounded_events(state: SavedState, maximum: usize) -> Result<SavedState, String> {
    if state.3.len() > maximum {
        return Err("Event journal exceeds its bound.".into());
    }
    Ok(state)
}
fn revalidate(prepared: &PreparedState, request: &StagedObligationRequest) -> Result<(), String> {
    let state = prepared.app.state::<crate::AppState>();
    crate::investigations::completion_choices::validate_request(
        prepared.app.handle(),
        &state,
        request,
    )?;
    for source in request
        .obligations
        .sources()
        .map_err(|error| error.to_string())?
    {
        let (current, _guard) = crate::investigations::inspection::resolve(
            &state,
            &source.fact,
            &source.graph_snapshot_id,
            &source.receipt_id,
            source.receipt_inventory_index as usize,
        )?;
        if current != source {
            return Err("Frozen source selection changed.".into());
        }
    }
    Ok(())
}
/// Runs only the requested stage. This function never starts another role and
/// never treats a teammate message or mechanical result as operator authorization.
pub(super) fn run_role(
    prepared: &PreparedState,
    role: Role,
    provider: Arc<dyn LlmProvider>,
) -> Result<RoleEvidence, RoleRunError> {
    let state = prepared.app.state::<crate::AppState>();
    let request = match role {
        Role::Analyst => &prepared.analyst_request,
        Role::Auditor => &prepared.auditor_request,
    };
    if descriptor(provider.as_ref(), InvestigationProviderMode::Local)
        .map_err(|error| error.to_string())?
        != prepared.manifest.provider
    {
        return Err("Execution provider differs from frozen descriptor.".into());
    }
    revalidate(prepared, request)?;
    let id = match role {
        Role::Analyst => {
            let detail = state
                .jobs
                .lock()
                .map_err(|_| "Job store unavailable.")?
                .investigation(&prepared.analyst_id)
                .map_err(|error| error.to_string())?;
            if detail.summary.status != InvestigationStatus::Queued
                || detail.usage.model_invocations != 0
                || detail.summary.has_result
                || prepared.auditor_id.is_some()
            {
                return Err("Analyst stage was already started.".into());
            }
            prepared.analyst_id.clone()
        }
        Role::Auditor => {
            let mut jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
            if prepared.auditor_id.is_some()
                || jobs
                    .existing_investigation_completion(request)
                    .map_err(|error| error.to_string())?
                    .is_some()
            {
                return Err("Auditor stage was already started.".into());
            }
            let parent = jobs
                .investigation(&prepared.analyst_id)
                .map_err(|error| error.to_string())?;
            if parent.summary.status != InvestigationStatus::Completed
                || parent.summary.completion_outcome
                    != Some(InvestigationCompletionOutcome::Supported)
                || parent.summary.obligation_status != Some(CompletionStatus::Satisfied)
                || jobs
                    .investigation_result(&prepared.analyst_id)
                    .map_err(|error| error.to_string())?
                    .is_none()
            {
                return Err("Analyst parent did not support the requested control.".into());
            }
            let started = jobs
                .start_completion_investigation(request, Some(&prepared.manifest.provider))
                .map_err(|error| error.to_string())?;
            if !started.created {
                return Err("Auditor stage is not a fresh invocation.".into());
            }
            started.detail.summary.investigation_id
        }
    };
    let detail = state
        .jobs
        .lock()
        .map_err(|_| "Job store unavailable.")?
        .investigation(&id)
        .map_err(|error| error.to_string())?;
    let plan = state
        .jobs
        .lock()
        .map_err(|_| "Job store unavailable.")?
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .map_err(|error| error.to_string())?;
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .map_err(|error| error.to_string())?;
    let (_, execution) = state
        .jobs
        .lock()
        .map_err(|_| "Job store unavailable.")?
        .claim_execution(&plan, reservation)
        .map_err(|error| error.to_string())?;
    let live = state
        .investigations
        .insert(&id)
        .map_err(|error| error.to_string())?;
    let observed = Arc::new(ObservedProvider {
        inner: provider,
        context_id: request.expected_context_id.clone(),
        observations: Mutex::new(vec![]),
        next: false,
    });
    run_completion(
        prepared.app.handle().clone(),
        id.clone(),
        request.clone(),
        execution,
        observed.clone(),
        live,
        Instant::now(),
    );
    let observations = match observed.observations.lock() {
        Ok(observations) => observations.clone(),
        Err(poisoned) => {
            return Err(RoleRunError {
                post_dispatch: true,
                reason: "Invocation observations unavailable.".into(),
                observations: poisoned.into_inner().clone(),
            });
        }
    };
    collect_evidence(prepared, role, &id, observations.clone()).map_err(|reason| RoleRunError {
        post_dispatch: true,
        reason,
        observations,
    })
}
fn collect_evidence(
    prepared: &PreparedState,
    role: Role,
    id: &str,
    observations: Vec<InvocationObservation>,
) -> Result<RoleEvidence, String> {
    let state = prepared.app.state::<crate::AppState>();
    let request = match role {
        Role::Analyst => &prepared.analyst_request,
        Role::Auditor => &prepared.auditor_request,
    };
    let (detail, result, ledger, events) = {
        let jobs = state.jobs.lock().map_err(|_| "Job store unavailable.")?;
        read_state(&jobs, id)?
    };
    let fresh =
        JobStore::open(prepared.app_data.join("state.db")).map_err(|error| error.to_string())?;
    let fresh_state = read_state(&fresh, id)?;
    let mut checks = vec![check(
        "fresh_coordinator_reopen_exact",
        fresh_state
            == (
                detail.clone(),
                result.clone(),
                ledger.clone(),
                events.clone(),
            ),
    )];
    let graph = state
        .graph
        .lock()
        .map_err(|_| "Graph store unavailable.")?
        .read_snapshot_bounded(SnapshotReadLimits::default())
        .map_err(|error| error.to_string())?;
    checks.push(check(
        "graph_unchanged",
        content_hash(&serde_json::to_vec(&graph).map_err(|error| error.to_string())?)
            == prepared.graph_before_hash,
    ));
    let database = prepared.app_data.join("state.db");
    let steps = rows::<PendingInvestigationStep>(
        &database,
        id,
        "investigation_invocations",
        8,
        256 * 1024,
    )?;
    let ledgers =
        rows::<InvestigationInputLedger>(&database, id, "investigation_inputs", 16, 256 * 1024)?;
    let archives = rows::<InvestigationCoverageArchive>(
        &database,
        id,
        "investigation_coverage_archives",
        1,
        64 * 1024,
    )?;
    let responses =
        rows::<InvestigationResponseMeta>(&database, id, "investigation_responses", 8, 4096)?;
    checks.push(MechanicalCheck {
        name: "known_response_usage_charged_once".into(),
        status: response_usage_status(&steps, &responses, &detail, &events),
    });
    let sealed = sealed_checks(request, &steps, &ledgers, &observations)?;
    checks.push(check("actual_payloads_match_sealed_disclosures", sealed.0));
    checks.push(check(
        "all_invocations_bind_request4_context",
        sealed.1
            && steps.len() == detail.usage.model_invocations as usize
            && observations.len() == steps.len(),
    ));
    let archive = archives.first();
    checks.push(check(
        "frozen_archive_identity",
        archive.is_some_and(|archive| {
            archive.proof().context_id == prepared.manifest.context_id
                && request
                    .obligations
                    .obligations
                    .iter()
                    .filter_map(|obligation| match obligation {
                        Obligation::Inventory { target, .. } => Some(target),
                        _ => None,
                    })
                    .all(|target| {
                        archive
                            .read(&InvestigationCoverageRead {
                                context_id: target.context_id.clone(),
                                repo: target.repo.clone(),
                                inventory_hash: target.inventory_hash.clone(),
                                max_bytes: 32768,
                            })
                            .is_ok_and(|detail| {
                                content_hash(detail.inventory_json.as_bytes())
                                    == prepared.manifest.inventory_hash
                            })
                    })
        }),
    ));
    let supported =
        detail.summary.completion_outcome == Some(InvestigationCompletionOutcome::Supported);
    let finishing = match result
        .as_ref()
        .and_then(|result| result.invocation_step_id.as_ref())
    {
        Some(step) => steps.iter().find(|saved| &saved.step_id == step),
        None => None,
    };
    let finishing_ledger = finishing.and_then(|step| {
        ledgers.iter().find(|ledger| {
            ledger.fingerprint().ok().as_deref() == Some(step.input_ledger_hash.as_str())
        })
    });
    let exact = match (result.as_ref(), finishing, finishing_ledger) {
        (Some(result), Some(step), Some(ledger)) => {
            finishing_checks(prepared, request, result, step, ledger)
        }
        _ => None,
    };
    checks.push(if supported {
        check(
            "required_original_source_inventory_supplied_and_cited",
            exact == Some(true),
        )
    } else {
        unknown("required_original_source_inventory_supplied_and_cited")
    });
    let weak = result.as_ref().is_some_and(|result| {
        result.findings.iter().all(|finding| {
            finding.tier == Tier::Agentic && finding.confidence_tier == ConfidenceTier::InferredWeak
        })
    });
    checks.push(if result.is_some() {
        check("findings_remain_t3_inferred_weak", weak)
    } else {
        unknown("findings_remain_t3_inferred_weak")
    });
    let parent_match = if role == Role::Auditor {
        let parent = state
            .jobs
            .lock()
            .map_err(|_| "Job store unavailable.")?
            .investigation_result(&prepared.analyst_id)
            .map_err(|error| error.to_string())?
            .ok_or("Analyst result missing.")?;
        let hash = content_hash(&serde_json::to_vec(&parent).map_err(|error| error.to_string())?);
        !observations.is_empty()
            && observations.iter().zip(&steps).all(|(observation, step)| {
                ledgers
                    .iter()
                    .find(|ledger| {
                        ledger.fingerprint().ok().as_deref()
                            == Some(step.input_ledger_hash.as_str())
                    })
                    .is_some_and(|ledger| {
                        observation.history_hash == ledger.supplied_history_hash
                            && observation.parent_result_hashes.get(&parent.result_id)
                                == Some(&hash)
                    })
            })
    } else {
        true
    };
    checks.push(check("auditor_exact_saved_parent_history", parent_match));
    checks.push(check(
        "shared_budgets_preserved",
        detail.limits == InvestigationLimits::default()
            && detail.usage.model_invocations <= detail.limits.model_invocations
            && detail.usage.tool_actions <= detail.limits.tool_actions
            && detail.usage.generated_token_reservations
                <= detail.limits.generated_token_reservations
            && detail.usage.evidence_bytes <= detail.limits.evidence_bytes,
    ));
    let binding = RunBinding {
        run_id: prepared.manifest.binding.run_id.clone(),
        protocol_hash: prepared.manifest.binding.protocol_hash.clone(),
        source_commit: prepared.manifest.binding.source_commit.clone(),
        source_tree: prepared.manifest.binding.source_tree.clone(),
        context_id: request.expected_context_id.clone(),
        request_hash: request.intent_hash().map_err(|error| error.to_string())?,
        obligations_hash: request
            .obligations
            .fingerprint()
            .map_err(|error| error.to_string())?,
    };
    let receipt = records::record_role(
        role,
        binding,
        &detail,
        result.as_ref(),
        &events,
        checks.clone(),
    )
    .map_err(|error| error.to_string())?;
    Ok(RoleEvidence {
        detail,
        result,
        ledger,
        events,
        mechanical_checks: checks,
        receipt,
        observations,
        representation_reviewable: sealed.2 && (!supported || exact == Some(true)),
    })
}
fn sealed_checks(
    request: &StagedObligationRequest,
    steps: &[PendingInvestigationStep],
    ledgers: &[InvestigationInputLedger],
    observations: &[InvocationObservation],
) -> Result<(bool, bool, bool), String> {
    sealed_checks_with_policy(
        request,
        steps,
        ledgers,
        observations,
        &INVESTIGATION_V1_POLICY,
    )
}
pub(in super::super) fn sealed_checks_with_policy(
    request: &StagedObligationRequest,
    steps: &[PendingInvestigationStep],
    ledgers: &[InvestigationInputLedger],
    observations: &[InvocationObservation],
    policy: &InvestigationPolicy,
) -> Result<(bool, bool, bool), String> {
    let contract = InvestigationCompletionContract {
        schema_version: 1,
        request_hash: request
            .intent_hash_with_policy(policy)
            .map_err(|error| error.to_string())?,
        obligations_hash: request
            .obligations
            .fingerprint_with_policy(policy)
            .map_err(|error| error.to_string())?,
        obligations: request.obligations.clone(),
    };
    let contract_hash = contract
        .fingerprint_with_policy(policy)
        .map_err(|error| error.to_string())?;
    let mut matching = !steps.is_empty() && steps.len() == observations.len();
    let mut bound = matching;
    let mut reviewable = true;
    for (step, observed) in steps.iter().zip(observations) {
        let Some(ledger) = ledgers.iter().find(|ledger| {
            ledger.fingerprint_with_policy(policy).ok().as_deref()
                == Some(step.input_ledger_hash.as_str())
        }) else {
            return Err("Invocation ledger missing.".into());
        };
        let Some(disclosure) = step.disclosure.as_ref() else {
            return Err("Invocation disclosure missing.".into());
        };
        // The caller has authenticated the saved request/task binding. Acquisition
        // disclosures additionally require that exact role, not generic shape validation.
        let role: SpecialistId =
            serde_json::from_value(Value::String(request.specialist_id.clone()))
                .map_err(|_| "Invalid saved specialist identity.".to_owned())?;
        disclosure
            .validate_saved_role_shape(ledger, policy, role)
            .map_err(|error| error.to_string())?;
        bound &= disclosure.schema_version == if request.schema_version == 7 { 6 } else { 4 }
            && disclosure.completion_contract_hash.as_ref() == Some(&contract_hash)
            && disclosure.completion_metadata_hash.is_some()
            && observed.context_id == request.expected_context_id
            && ledger
                .coverage
                .as_ref()
                .is_some_and(|coverage| coverage.context_id == request.expected_context_id);
        let mut sources = BTreeMap::new();
        for source in &disclosure.sources {
            if source.representation.is_supplied() {
                let hash = source
                    .disclosed_text_hash
                    .as_ref()
                    .ok_or("Supplied source body hash missing.")?;
                if sources
                    .insert(source.citation_id.clone(), hash.clone())
                    .is_some()
                {
                    return Err("Duplicate sealed source ID.".into());
                }
                reviewable &= !source.redacted
                    && ledger
                        .citations
                        .iter()
                        .find(|citation| citation.citation_id == source.citation_id)
                        .is_some_and(|citation| citation.text_hash.as_ref() == Some(hash));
            }
        }
        let mut inventories = BTreeMap::new();
        for detail in &disclosure.coverage_details {
            let read = ledger
                .coverage_reads
                .get(detail.read_index)
                .ok_or("Sealed inventory read missing.")?;
            if inventories
                .insert(
                    read.request.inventory_hash.clone(),
                    detail.disclosed_body_hash.clone(),
                )
                .is_some_and(|prior| prior != detail.disclosed_body_hash)
            {
                return Err("Conflicting sealed inventory bodies.".into());
            }
            reviewable &=
                !detail.redacted && detail.disclosed_body_hash == read.request.inventory_hash;
        }
        matching &= observed.payload_hash == step.payload_hash
            && observed.payload_hash == disclosure.payload_hash
            && observed.input_ledger_hash == step.input_ledger_hash
            && observed.source_hashes == sources
            && observed.inventory_hashes == inventories
            && observed.history_hash == ledger.supplied_history_hash;
    }
    Ok((matching, bound, reviewable))
}
fn finishing_checks(
    prepared: &PreparedState,
    request: &StagedObligationRequest,
    result: &InvestigationResult,
    step: &PendingInvestigationStep,
    ledger: &InvestigationInputLedger,
) -> Option<bool> {
    let disclosure = step.disclosure.as_ref()?;
    let state = prepared.app.state::<crate::AppState>();
    let mut sources = 0;
    let mut inventories = 0;
    let mut exact = true;
    for obligation in &request.obligations.obligations {
        match obligation {
            Obligation::Source { target, .. } => {
                sources += 1;
                let mut unreadable = false;
                let supplied_and_cited = ledger
                    .citations
                    .iter()
                    .filter(|citation| target.matches(citation))
                    .any(|citation| {
                        let Some(source) = disclosure
                            .sources
                            .iter()
                            .find(|source| source.citation_id == citation.citation_id)
                        else {
                            return false;
                        };
                        if !source.representation.is_supplied()
                            || source.redacted
                            || source.disclosed_text_hash != citation.text_hash
                            || !result
                                .findings
                                .iter()
                                .any(|finding| finding.citation_ids.contains(&citation.citation_id))
                        {
                            return false;
                        }
                        let Ok(retained) = crate::investigations::history::read(
                            &state,
                            &result.investigation_id,
                            ledger,
                            &citation.citation_id,
                        ) else {
                            unreadable = true;
                            return false;
                        };
                        retained.status == InvestigationCitationStatus::Available
                            && retained.text.as_ref().is_some_and(|text| {
                                citation.text_hash.as_deref()
                                    == Some(content_hash(text.as_bytes()).as_str())
                            })
                    });
                if !supplied_and_cited && unreadable {
                    return None;
                }
                exact &= target.captured.file.digest == prepared.manifest.source_hash
                    && supplied_and_cited;
            }
            Obligation::Inventory { target, .. } => {
                inventories += 1;
                exact &= target.inventory_hash == prepared.manifest.inventory_hash
                    && target.context_id == prepared.manifest.context_id
                    && ledger
                        .coverage_reads
                        .iter()
                        .enumerate()
                        .any(|(index, read)| {
                            read.request.context_id == target.context_id
                                && read.request.repo == target.repo
                                && read.request.inventory_hash == target.inventory_hash
                                && disclosure.coverage_details.iter().any(|detail| {
                                    detail.read_index == index
                                        && !detail.redacted
                                        && detail.disclosed_body_hash == target.inventory_hash
                                })
                        });
            }
        }
    }
    Some(exact && sources == 1 && inventories == 1)
}
#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;

fn response_usage_status(
    steps: &[PendingInvestigationStep],
    responses: &[InvestigationResponseMeta],
    detail: &InvestigationDetail,
    events: &[InvestigationEvent],
) -> CheckStatus {
    let usage = &detail.usage;
    let mut step_ids = std::collections::BTreeSet::new();
    if steps.len() != usage.model_invocations as usize
        || steps.iter().any(|step| !step_ids.insert(&step.step_id))
    {
        return CheckStatus::Failed;
    }
    if events
        .iter()
        .filter(|event| event.kind == InvestigationEventKind::ModelStarted)
        .map(|event| event.step_id.as_deref())
        .ne(steps.iter().map(|step| Some(step.step_id.as_str())))
    {
        return CheckStatus::Failed;
    }
    let mut seen = std::collections::BTreeSet::new();
    if responses.iter().any(|response| {
        !seen.insert(&response.step_id)
            || !steps.iter().any(|step| step.step_id == response.step_id)
    }) {
        return CheckStatus::Failed;
    }
    let mut input = None;
    let mut output = None;
    let mut cache_read = None;
    for (ordinal, step) in steps.iter().enumerate() {
        let Some(response) = responses
            .iter()
            .find(|response| response.step_id == step.step_id)
        else {
            if ordinal + 1 != steps.len()
                || usage.reported_input_tokens.is_some()
                || usage.reported_output_tokens.is_some()
                || usage.reported_cache_read_tokens.is_some()
                || !authenticated_unadmitted_tail(step, detail, events)
            {
                return CheckStatus::Failed;
            }
            return CheckStatus::Unknown;
        };
        let add = |previous: Option<u64>, current: Option<u64>| -> Option<Option<u64>> {
            if ordinal == 0 {
                Some(current)
            } else {
                match (previous, current) {
                    (Some(a), Some(b)) => a.checked_add(b).map(Some),
                    _ => Some(None),
                }
            }
        };
        let Some(next) = add(input, response.reported_input_tokens) else {
            return CheckStatus::Failed;
        };
        input = next;
        let Some(next) = add(output, response.reported_output_tokens) else {
            return CheckStatus::Failed;
        };
        output = next;
        let Some(next) = add(cache_read, response.reported_cache_read_tokens) else {
            return CheckStatus::Failed;
        };
        cache_read = next;
    }
    if input == usage.reported_input_tokens
        && output == usage.reported_output_tokens
        && cache_read == usage.reported_cache_read_tokens
    {
        CheckStatus::Passed
    } else {
        CheckStatus::Failed
    }
}

fn authenticated_unadmitted_tail(
    step: &PendingInvestigationStep,
    detail: &InvestigationDetail,
    events: &[InvestigationEvent],
) -> bool {
    let terminal = match detail.summary.status {
        InvestigationStatus::Failed => InvestigationEventKind::Failed,
        InvestigationStatus::Cancelled => InvestigationEventKind::Cancelled,
        InvestigationStatus::OutcomeUnknown => InvestigationEventKind::OutcomeUnknown,
        _ => return false,
    };
    let starts = events
        .iter()
        .filter(|event| {
            event.kind == InvestigationEventKind::ModelStarted
                && event.step_id.as_deref() == Some(&step.step_id)
        })
        .collect::<Vec<_>>();
    !detail.summary.has_result
        && !detail.summary.invocation_pending
        && events
            .iter()
            .all(|event| event.investigation_id == detail.summary.investigation_id)
        && events
            .windows(2)
            .all(|pair| pair[0].sequence.checked_add(1) == Some(pair[1].sequence))
        && starts.len() == 1
        && events.last().is_some_and(|event| {
            event.kind == terminal
                && event.sequence == detail.summary.last_event_sequence
                && event.revision == detail.summary.revision
                && event.sequence > starts[0].sequence
        })
        && !events.iter().any(|event| {
            event.step_id.as_deref() == Some(&step.step_id)
                && event.kind != InvestigationEventKind::ModelStarted
        })
}

#[cfg(test)]
pub(super) fn recording_failure_provider(prepared: &PreparedState) -> Arc<dyn LlmProvider> {
    Arc::new(tests::RecordingFailureProvider {
        inner: crate::investigations::worker::tests::Scripted::new(
            crate::investigations::worker::tests::Mode::CompletionSupported,
        ),
        database: prepared.app_data.join("state.db"),
        id: prepared.analyst_id.clone(),
    })
}
