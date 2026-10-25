//! The actual bounded specialist loop. Only the owned worker keeps source and
//! provider payloads; every admitted operation crosses the durable coordinator.

use super::{
    AcquisitionSink, HostError,
    context::{AcquisitionRequest, FrozenContext, bounded_json},
    runtime::{LiveTask, RunClock, descriptor},
};
use crate::jobs::investigations::{
    InvestigationResponseMeta, InvestigationStopReason, InvestigationStoreError,
    InvestigationTransition, PendingInvestigationStep,
};
use crate::{AppState, job_execution::JobExecution, jobs::ExecutionCheck};
use agents::investigation::*;
use core_prov::content_hash;
use llm::bounded::{
    BoundedCallError, BoundedCompletion, CallDirective, CompletionControl, CompletionLimits,
    FailureCode, InvocationOutcome, PreparedBoundedCompletion,
};
use llm::{ConsentGrant, EgressFirewall, LlmProvider, Locality};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;

#[path = "worker/next_metadata.rs"]
mod next_metadata;
#[path = "worker/next_prompt.rs"]
mod next_prompt;
#[path = "worker/next_source.rs"]
mod next_source;

#[derive(Debug)]
enum Failure {
    AlreadyTerminal,
    Host(HostError),
    Store(InvestigationStoreError),
    Stop(InvestigationStopReason),
}

impl From<HostError> for Failure {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}
impl From<InvestigationStoreError> for Failure {
    fn from(error: InvestigationStoreError) -> Self {
        Self::Store(error)
    }
}
impl From<InvestigationError> for Failure {
    fn from(error: InvestigationError) -> Self {
        Self::Host(error.into())
    }
}

impl Failure {
    fn reason(&self) -> Option<InvestigationStopReason> {
        use InvestigationStopReason as Stop;
        Some(match self {
            Self::AlreadyTerminal => return None,
            Self::Stop(reason) => *reason,
            Self::Host(HostError::InputChanged) => Stop::InputChanged,
            Self::Host(HostError::SourceUnavailable) => Stop::SourceUnavailable,
            Self::Host(HostError::LimitExceeded)
            | Self::Store(InvestigationStoreError::Capacity) => Stop::LimitExceeded,
            Self::Store(InvestigationStoreError::Cancelled) => Stop::Cancelled,
            Self::Host(HostError::InvalidInput) => Stop::PreparationFailed,
            Self::Host(HostError::Operational) | Self::Store(_) => Stop::Operational,
        })
    }
}

/// The legacy entry keeps its original request/start contract.
#[allow(clippy::too_many_arguments)]
pub(super) fn run<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: StartInvestigationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
) {
    run_inner(
        app,
        id,
        AcquisitionRequest::try_from(&request),
        None,
        execution,
        provider,
        live,
        started,
        None,
    )
}

/// V7 executes only from real immutable request4 authority.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_completion<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: obligations::StagedObligationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
) {
    run_completion_with_limits(app, id, request, execution, provider, live, started, None);
}

/// Retain the caller's admitted discovery and evidence limits after detaching execution.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_completion_with_limits<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: obligations::StagedObligationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
) {
    match FrozenContext::completion_acquisition_request(&request) {
        Ok(projected) => run_inner(
            app,
            id,
            Ok(projected),
            Some(request),
            execution,
            provider,
            live,
            started,
            task_limits,
        ),
        Err(_) => {
            let state = app.state::<AppState>();
            let _cleanup = Cleanup {
                state: &state,
                id: &id,
                live: &live,
            };
            stop(
                &app,
                &state,
                &id,
                &execution,
                InvestigationStopReason::PreparationFailed,
                0,
            );
        }
    }
}

/// Called inside a blocking host worker. The execution's final Arc holder cannot
/// leave while a provider call is outstanding, even if its async waiter is lost.
#[allow(clippy::too_many_arguments)]
fn run_inner<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: Result<AcquisitionRequest, HostError>,
    completion_request: Option<obligations::StagedObligationRequest>,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
) {
    let state = app.state::<AppState>();
    let _cleanup = Cleanup {
        state: &state,
        id: &id,
        live: &live,
    };
    let mut clock = RunClock::new(started);
    let outcome = (|| {
        let request = request?;
        let detail = read_detail(&state, &id, &execution)?;
        let mut worker = Worker {
            app: &app,
            state: &state,
            id: &id,
            execution: &execution,
            live: &live,
            detail,
            clock: &mut clock,
            task_limits,
        };
        worker.execute_inner(&request, completion_request.as_ref(), provider.as_ref())
    })();
    if let Err(failure) = outcome
        && let Some(reason) = failure.reason()
    {
        // No source/provider retry. If even terminal publication fails, the
        // durable pending phase remains available to exact-owner recovery.
        stop(
            &app,
            &state,
            &id,
            &execution,
            reason,
            clock.active_milliseconds(),
        );
    }
}

struct Cleanup<'a> {
    state: &'a AppState,
    id: &'a str,
    live: &'a LiveTask,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.live.pending.lock() {
            *pending = None;
        }
        self.state.investigations.remove(self.id);
    }
}

struct Worker<'a, R: tauri::Runtime> {
    app: &'a tauri::AppHandle<R>,
    state: &'a AppState,
    id: &'a str,
    execution: &'a JobExecution,
    live: &'a LiveTask,
    detail: InvestigationDetail,
    clock: &'a mut RunClock,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
}

impl<R: tauri::Runtime> Worker<'_, R> {
    fn execute_inner(
        &mut self,
        request: &AcquisitionRequest,
        completion_request: Option<&obligations::StagedObligationRequest>,
        provider: &dyn LlmProvider,
    ) -> Result<(), Failure> {
        let policy = InvestigationPolicy::for_limits(&self.detail.limits)
            .ok_or(InvestigationStoreError::Invalid)?;
        if request.policy.profile() != policy.profile()
            || request.specialist_id != self.detail.summary.specialist_id
        {
            return Err(Failure::Store(InvestigationStoreError::Invalid));
        }
        self.check_new_work()?;
        let history = self.parent_history(request)?;
        let mut _inspection_guards = Vec::new();
        for target in request.inspection_requirements.as_deref().unwrap_or(&[]) {
            let (actual, guard) = super::inspection::resolve(
                self.state,
                &target.fact,
                &target.graph_snapshot_id,
                &target.receipt_id,
                target.receipt_inventory_index as usize,
            )
            .map_err(|_| Failure::Stop(InvestigationStopReason::InputChanged))?;
            if actual != *target {
                return Err(Failure::Stop(InvestigationStopReason::InputChanged));
            }
            _inspection_guards.push(guard);
        }
        let archive_capacity = if request.specialist_id.uses_coverage_qualification() {
            Some(
                self.state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .investigation_coverage_archive_capacity(self.id)?,
            )
        } else {
            None
        };
        let mut context = match self.task_limits {
            Some(limits) => FrozenContext::prepare_with_app_scope(
                self.app,
                self.state,
                request,
                &history,
                archive_capacity,
                Some(limits.scope),
            ),
            None => FrozenContext::prepare_with_app(
                self.app,
                self.state,
                request,
                &history,
                archive_capacity,
            ),
        }?;
        if let Some(request) = completion_request {
            request.validate_ledger_with_policy(&context.ledger, policy)?;
        }
        self.check_new_work()?;
        let mut usage = self.detail.usage.clone();
        usage.active_milliseconds = self.clock.active_milliseconds();
        if request.specialist_id.uses_next_prompt_admission() {
            self.publish_initial_next_prompt(
                completion_request.ok_or(HostError::InvalidInput)?,
                provider,
                &context,
                &history,
            )?;
        } else {
            self.transition(InvestigationTransition::InputPrepared {
                coverage_archive: context.coverage_archive.clone().map(Box::new),
                ledger: Box::new(context.ledger.clone()),
                usage,
            })?;
        }

        loop {
            self.check_new_work()?;
            if self.detail.usage.model_invocations >= self.detail.limits.model_invocations {
                return Err(Failure::Stop(InvestigationStopReason::LimitExceeded));
            }
            if descriptor(provider, request.provider_mode)? != self.detail.provider {
                return Err(Failure::Stop(InvestigationStopReason::ProviderFailure));
            }
            let next_snapshot = if request.specialist_id.uses_next_prompt_admission() {
                let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let snapshot = jobs.investigation_next_prompt_snapshot(
                    self.id,
                    completion_request.ok_or(HostError::InvalidInput)?,
                )?;
                if snapshot.ledger() != &context.ledger {
                    return Err(HostError::InputChanged.into());
                }
                self.detail = snapshot.detail().clone();
                Some(snapshot)
            } else {
                None
            };
            let completion_snapshot = if completion_request.is_some() && next_snapshot.is_none() {
                let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let snapshot = jobs.investigation_completion_prompt_snapshot(self.id)?;
                if snapshot.ledger() != &context.ledger {
                    return Err(HostError::InputChanged.into());
                }
                // Usage, attempt witnesses, metadata and turn ordering now share
                // one saved read snapshot. Mutation still rechecks ownership and
                // cancellation through the existing transition boundary.
                self.detail = snapshot.detail().clone();
                Some(snapshot)
            } else {
                None
            };
            // Preserve the exact coherent PRE-call mode, including historical
            // consent usage. Charging a call must never change its saved mode.
            let admission_mode = next_snapshot.as_ref().map(|snapshot| snapshot.mode());
            let input = if let Some(snapshot) = &next_snapshot {
                let mut pre_call_usage = snapshot.detail().usage.clone();
                if snapshot.pending_step().is_none() {
                    pre_call_usage.active_milliseconds = self.clock.active_milliseconds();
                }
                let query_pages = context
                    .query_pages
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let inventory_details = context
                    .inventory_details
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                InvestigationInput::prepare_v9_with_policy(
                    &snapshot.request().question,
                    self.detail.summary.specialist_id,
                    context.ledger.clone(),
                    &query_pages,
                    &context.evidence,
                    &history,
                    &pre_call_usage,
                    snapshot.basis(),
                    snapshot.request(),
                    snapshot.metadata(),
                    &inventory_details,
                    snapshot.mode(),
                    snapshot.capacity_refusal_count(),
                    snapshot.capacity_feedback(),
                    policy,
                )?
            } else if let Some(completion_request) = completion_request {
                let snapshot = completion_snapshot
                    .as_ref()
                    .ok_or(HostError::InvalidInput)?;
                let prepare = if self
                    .detail
                    .summary
                    .specialist_id
                    .uses_query_history_disclosure()
                {
                    InvestigationInput::prepare_v8_with_policy
                } else {
                    InvestigationInput::prepare_v7_with_policy
                };
                prepare(
                    &completion_request.question,
                    self.detail.summary.specialist_id,
                    context.ledger.clone(),
                    &context.query_pages,
                    &context.evidence,
                    &history,
                    &self.detail.usage,
                    snapshot.basis(),
                    completion_request,
                    snapshot.metadata(),
                    &context.inventory_details,
                    policy,
                )?
            } else if request.specialist_id.uses_turn_progress() {
                let basis = self
                    .state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .investigation_turn_basis(self.id)?;
                if request.specialist_id.uses_coverage_qualification() {
                    InvestigationInput::prepare_v6(
                        &request.question,
                        request.specialist_id,
                        context.ledger.clone(),
                        &context.query_pages,
                        &context.evidence,
                        &history,
                        &self.detail.usage,
                        &basis,
                        request.inspection_requirements.as_deref(),
                        &context.inventory_details,
                    )?
                } else if let Some(targets) = &request.inspection_requirements {
                    InvestigationInput::prepare_v5(
                        &request.question,
                        request.specialist_id,
                        context.ledger.clone(),
                        &context.query_pages,
                        &context.evidence,
                        &history,
                        &self.detail.usage,
                        &basis,
                        targets,
                    )?
                } else {
                    InvestigationInput::prepare_v4(
                        &request.question,
                        request.specialist_id,
                        context.ledger.clone(),
                        &context.query_pages,
                        &context.evidence,
                        &history,
                        &self.detail.usage,
                        &basis,
                    )?
                }
            } else {
                InvestigationInput::prepare(
                    &request.question,
                    request.specialist_id,
                    context.ledger.clone(),
                    &context.query_pages,
                    &context.evidence,
                    &history,
                    &self.detail.usage,
                )?
            };
            // Input owns its projection; saved replay rows are no longer needed
            // during disclosure, consent or provider execution.
            drop(next_snapshot);
            let step_id = format!("step-{}", self.detail.usage.model_invocations + 1);
            let action = input.action(format!("{}:{step_id}", self.id))?;
            let input_bytes = bounded_json(&action.payload, 128 * 1024)?.len();
            let limits =
                CompletionLimits::new(super::provider_workspace::completion_limit_values(policy)?)
                    .map_err(provider_failure)?;
            let prepared = firewall(self.state)?
                .prepare_bounded(provider, &action, &limits)
                .map_err(provider_failure)?;
            // The firewall owns its prepared payload; release the original
            // copy before disclosure sealing, consent and provider execution.
            drop(action);
            let disclosure = input.seal_disclosure_with_policy(&prepared, policy)?;
            let step = PendingInvestigationStep {
                step_id: step_id.clone(),
                payload_hash: prepared.preview().egress.payload_hash.clone(),
                input_ledger_hash: context.ledger.fingerprint_with_policy(policy)?,
                input_bytes,
                generated_tokens: limits.values().max_output_tokens,
                disclosure: disclosure.clone(),
            };
            self.check_new_work()?;
            let consent = if provider.locality() == Locality::Cloud {
                Some(self.await_consent(&step, &prepared, &input, &context)?)
            } else {
                None
            };
            self.check_new_work()?;
            // Policy is read again after human waiting. The in-memory grant is
            // constructed only after the durable exact-step approval is observed.
            let authorized = firewall(self.state)?
                .authorize_bounded(provider, &prepared, consent.as_ref())
                .map_err(provider_failure)?;
            self.check_new_work()?;
            if request.specialist_id.uses_next_prompt_admission() {
                self.publish_next_invocation(&input, &prepared, &context, Some(&authorized))?;
                super::emit_changed(self.app, &self.detail);
            } else {
                self.transition(InvestigationTransition::BeginInvocation { step })?;
            }
            self.clear_pending()?;
            let deadline = self.clock.deadline()?;
            let directive = || execution_directive(self.state, self.execution, self.id);
            let completion = provider
                .complete_bounded(
                    &authorized,
                    &CompletionControl {
                        deadline,
                        check: &directive,
                    },
                )
                .map_err(provider_failure)?;
            // Complete response bytes are still untrusted. A custom bounded
            // provider does not bypass output or immutable model identity checks.
            validate_completion(&completion, &prepared)?;
            if provider.locality() == Locality::Cloud {
                // This is the bounded provider's observed request-body count,
                // not an invented byte estimate for failed or unknown calls.
                let provider_label = format!(
                    "{}:{}",
                    self.detail.provider.provider_id, self.detail.provider.model
                );
                self.state
                    .settings
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .record_egress(&provider_label, completion.request_bytes)
                    .map_err(|_| HostError::Operational)?;
            }
            input
                .validate_response_metadata(completion.response_model.as_deref())
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
            if let Some(mode) = admission_mode
                && self.admit_next_finish_response(
                    completion_request.ok_or(HostError::InvalidInput)?,
                    &input,
                    &prepared,
                    &completion,
                    mode,
                    &step_id,
                )?
            {
                if terminal(self.detail.summary.status) {
                    return Ok(());
                }
                continue;
            }
            if admission_mode.is_some() {
                if self.admit_next_metadata_response(
                    completion_request.ok_or(HostError::InvalidInput)?,
                    &input,
                    &prepared,
                    &completion,
                    &step_id,
                    provider,
                    &mut context,
                    &history,
                )? {
                    continue;
                }
                self.admit_next_source_response(
                    completion_request.ok_or(HostError::InvalidInput)?,
                    &input,
                    &prepared,
                    &completion,
                    &step_id,
                    provider,
                    &mut context,
                    &history,
                )?;
                if terminal(self.detail.summary.status) {
                    return Ok(());
                }
                continue;
            }
            let mut completion_finish = None;
            let (action, inventory_read, action_hash) = if completion_request.is_some() {
                let proof = context
                    .ledger
                    .coverage
                    .as_ref()
                    .ok_or(HostError::InvalidInput)?;
                // Replay must resolve against this exact request and saved
                // ledger before ActionAdmitted/BeginTool consume any budget.
                let action = if self
                    .detail
                    .summary
                    .specialist_id
                    .uses_query_history_disclosure()
                {
                    let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                    let saved_ledger = jobs
                        .investigation_ledger(self.id)?
                        .ok_or(HostError::InvalidInput)?;
                    let saved_request_hash = jobs
                        .investigation_completion_metadata(self.id)?
                        .request_hash;
                    if saved_ledger != context.ledger
                        || saved_request_hash
                            != completion_request
                                .ok_or(HostError::InvalidInput)?
                                .intent_hash_with_policy(policy)?
                    {
                        return Err(Failure::Stop(InvestigationStopReason::InputChanged));
                    }
                    ProgressiveInvestigationAction::decode_resolved(
                        &completion.text,
                        &proof.context_id,
                        &saved_request_hash,
                        &saved_ledger,
                        policy,
                    )
                } else {
                    CompletionInvestigationAction::decode(&completion.text, &proof.context_id)
                }
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let value: serde_json::Value = serde_json::from_str(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = content_hash(&bounded_json(&value, 32 * 1024)?);
                // A finish-only tool proposal terminates before ActionAdmitted,
                // BeginTool, query replay publication or source access. It is not
                // a capacity refusal and cannot buy another model iteration.
                if admission_mode == Some(staged_refusal::CompletionCallMode::FinishOnly)
                    && matches!(&action, CompletionInvestigationAction::Read(_))
                {
                    return Err(Failure::Stop(
                        InvestigationStopReason::FinishOnlyToolProposal,
                    ));
                }
                match action {
                    CompletionInvestigationAction::Finish(finish) => {
                        completion_finish = Some(finish);
                        (None, None, hash)
                    }
                    CompletionInvestigationAction::Read(
                        QualifiedInvestigationAction::QueryContext { query, .. },
                    ) => (
                        Some(InvestigationAction::QueryContext { query }),
                        None,
                        hash,
                    ),
                    CompletionInvestigationAction::Read(
                        QualifiedInvestigationAction::ReadCoverageInventory(read),
                    ) => (None, Some(read), hash),
                    CompletionInvestigationAction::Read(
                        QualifiedInvestigationAction::SourceOrFinish(action),
                    ) => (Some(action), None, hash),
                }
            } else if let Some(proof) = &context.ledger.coverage {
                let qualified =
                    QualifiedInvestigationAction::decode(&completion.text, &proof.context_id)
                        .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                // Hash the entire admitted qualified envelope, including its
                // context identity, rather than its legacy projection.
                let value: serde_json::Value = serde_json::from_str(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = content_hash(&bounded_json(&value, 32 * 1024)?);
                match qualified {
                    QualifiedInvestigationAction::QueryContext { query, .. } => (
                        Some(InvestigationAction::QueryContext { query }),
                        None,
                        hash,
                    ),
                    QualifiedInvestigationAction::ReadCoverageInventory(read) => {
                        (None, Some(read), hash)
                    }
                    QualifiedInvestigationAction::SourceOrFinish(action) => {
                        (Some(action), None, hash)
                    }
                }
            } else {
                let action = InvestigationAction::decode(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = content_hash(&bounded_json(&action, 32 * 1024)?);
                (Some(action), None, hash)
            };
            let response = InvestigationResponseMeta {
                step_id: step_id.clone(),
                response_hash: content_hash(completion.text.as_bytes()),
                action_hash,
                observed_model: completion.response_model.clone(),
                reported_input_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.input_tokens),
                reported_output_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.output_tokens),
                active_milliseconds: self.clock.active_milliseconds(),
            };
            if let Some(finish) = completion_finish {
                let request = completion_request.ok_or(HostError::InvalidInput)?;
                let timestamp = self
                    .state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .investigation_timestamp(0)?;
                let snapshot = completion_snapshot
                    .as_ref()
                    .ok_or(HostError::InvalidInput)?;
                let evidence = snapshot
                    .attempts()
                    .iter()
                    .zip(snapshot.attempt_ledgers())
                    .map(|(record, ledger)| staged_attempt::AttemptEvidence { record, ledger })
                    .collect::<Vec<_>>();
                match input
                    .admit_completion_finish_with_policy(
                        request,
                        self.id,
                        &step_id,
                        disclosure.as_ref().ok_or(HostError::InvalidInput)?,
                        &finish,
                        &evidence,
                        completion.response_model.clone(),
                        timestamp,
                        policy,
                    )
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?
                {
                    Ok(result) => {
                        self.admit_finish(response, result)?;
                        return Ok(());
                    }
                    Err(staged_finish::FinishDecision::Refused { .. }) => {
                        // Store rederives inside its transaction and applies the
                        // cancellation/second-refusal terminal precedence atomically.
                        self.admit_refusal(request, response, finish, input)?;
                        if terminal(self.detail.summary.status) {
                            return Ok(());
                        }
                        continue;
                    }
                    Err(staged_finish::FinishDecision::Accepted(_)) => {
                        return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
                    }
                }
            }
            if let Some(action @ InvestigationAction::Finish { .. }) = &action {
                let timestamp = self
                    .state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .investigation_timestamp(0)?;
                let result = input
                    .admit_result_for_step(
                        self.id,
                        &step_id,
                        disclosure.as_ref(),
                        action,
                        completion.response_model,
                        timestamp,
                    )
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                // A valid finish belongs to the last supplied ledger, even if
                // cancellation/Jobs cleanup won while the response was in flight.
                self.admit_finish(response, result)?;
                return Ok(());
            }
            // Late tools do not run or acquire new source after cancellation.
            self.check_new_work()?;
            let tool = if inventory_read.is_some() {
                InvestigationTool::ReadCoverageInventory
            } else {
                action
                    .as_ref()
                    .and_then(InvestigationAction::tool)
                    .ok_or(Failure::Stop(InvestigationStopReason::InvalidResponse))?
            };
            self.transition(InvestigationTransition::ActionAdmitted {
                response,
                tool: Some(tool),
                result: None,
            })?;
            self.check_new_work()?;
            self.transition(InvestigationTransition::BeginTool { tool })?;
            let state = self.state;
            let mut sink = WorkerSink { worker: self };
            let unavailable = match (action, inventory_read) {
                (Some(InvestigationAction::QueryContext { query }), None) => {
                    context.query(state, query, &mut sink)?;
                    None
                }
                (Some(InvestigationAction::ReadEvidence { fact, role, index }), None) => {
                    if completion_request.is_some() {
                        let read = staged_attempt::ReadIntent::Source {
                            fact: fact.clone(),
                            role,
                            index,
                        };
                        context
                            .read_evidence_completion(state, fact, role, index, &mut sink)?
                            .map(|reason| (read, reason))
                    } else {
                        context.read_evidence(state, fact, role, index, &mut sink)?;
                        None
                    }
                }
                (None, Some(read)) => {
                    if completion_request.is_some() {
                        let intent = staged_attempt::ReadIntent::Inventory {
                            context_id: read.context_id.clone(),
                            repo: read.repo.clone(),
                            inventory_hash: read.inventory_hash.clone(),
                            max_bytes: read.max_bytes,
                        };
                        context
                            .read_inventory_completion(read, &mut sink)?
                            .map(|reason| (intent, reason))
                    } else {
                        context.read_inventory(read, &mut sink)?;
                        None
                    }
                }
                _ => return Err(Failure::Stop(InvestigationStopReason::InvalidResponse)),
            };
            if let Some((read, reason)) = unavailable {
                self.transition(InvestigationTransition::ReadUnavailable { read, reason })?;
            }
            self.check_new_work()?;
        }
    }

    fn check_new_work(&mut self) -> Result<(), Failure> {
        self.clock.deadline()?;
        let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
        let detail = jobs.investigation_owned_detail(self.execution, self.id)?;
        if detail.summary.job_id != self.execution.id() {
            return Err(Failure::Stop(InvestigationStopReason::Operational));
        }
        if terminal(detail.summary.status) {
            return Err(Failure::AlreadyTerminal);
        }
        let checked = jobs.check_execution(self.execution);
        if detail.summary.cancel_requested {
            self.detail = detail;
            return match checked {
                Ok(ExecutionCheck::Cancelled | ExecutionCheck::Running)
                | Err(crate::jobs::JobTransitionError::Missing) => {
                    Err(Failure::Stop(InvestigationStopReason::Cancelled))
                }
                _ => Err(Failure::Stop(InvestigationStopReason::Operational)),
            };
        }
        if !matches!(checked, Ok(ExecutionCheck::Running)) {
            return Err(Failure::Stop(InvestigationStopReason::Operational));
        }
        self.detail = detail;
        Ok(())
    }

    fn apply(&mut self, transition: InvestigationTransition) -> Result<(), Failure> {
        self.detail = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .advance_investigation(self.execution, self.detail.summary.revision, transition)?;
        Ok(())
    }

    fn transition(&mut self, transition: InvestigationTransition) -> Result<(), Failure> {
        self.apply(transition)?;
        super::emit_changed(self.app, &self.detail);
        Ok(())
    }

    fn clear_pending(&self) -> Result<(), Failure> {
        *self
            .live
            .pending
            .lock()
            .map_err(|_| HostError::Operational)? = None;
        Ok(())
    }

    fn parent_history(&self, request: &AcquisitionRequest) -> Result<Vec<String>, Failure> {
        let Some(parent_id) = &request.parent_id else {
            return Ok(Vec::new());
        };
        let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
        if request.specialist_id.uses_next_prompt_admission() {
            let conversation = request
                .conversation_id
                .as_deref()
                .ok_or(HostError::InvalidInput)?;
            return Ok(jobs
                .investigation_next_parent_history(parent_id, conversation)?
                .into_history());
        }
        let parent = jobs.investigation(parent_id)?;
        if request.conversation_id.as_ref() != Some(&parent.summary.conversation_id) {
            return Err(Failure::Stop(InvestigationStopReason::PreparationFailed));
        }
        let result = jobs.investigation_result(parent_id)?;
        // No recursive conversation replay or current-source reconstruction.
        // The selected parent's complete saved representation must fit as a
        // whole; excess history fails explicitly rather than silently truncating.
        let raw = bounded_json(
            &serde_json::json!({
                "parent_investigation_id": parent_id,
                "original_graph_snapshot_id": parent.summary.graph_snapshot_id,
                "original_scope": parent.summary.scope,
                "saved_execution_status": parent.summary.status,
                "cancellation_requested": parent.summary.cancel_requested,
                "invocation_outcome_pending": parent.summary.invocation_pending,
                "authority": "Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge.",
                "saved_result": result,
            }),
            16 * 1024,
        )?;
        let history = vec![String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?];
        bounded_json(&history, 16 * 1024)?;
        Ok(history)
    }

    fn await_consent(
        &mut self,
        step: &PendingInvestigationStep,
        prepared: &PreparedBoundedCompletion,
        input: &InvestigationInput,
        context: &FrozenContext,
    ) -> Result<ConsentGrant, Failure> {
        let remaining = self.clock.remaining_wall().min(Duration::from_secs(900));
        if remaining.is_zero() {
            return Err(Failure::Stop(InvestigationStopReason::ConsentExpired));
        }
        let expires_at = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_timestamp(
                remaining
                    .as_secs()
                    .try_into()
                    .map_err(|_| HostError::InvalidInput)?,
            )?;
        if self
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
        {
            self.publish_next_invocation(input, prepared, context, None)?;
        } else {
            self.apply(InvestigationTransition::AwaitConsent { step: step.clone() })?;
        }
        *self
            .live
            .pending
            .lock()
            .map_err(|_| HostError::Operational)? = Some(InvestigationConsent {
            investigation_id: self.id.into(),
            step_id: step.step_id.clone(),
            revision: self.detail.summary.revision,
            preview: prepared.preview().egress.clone(),
            provider_profile: prepared.profile().clone(),
            completion_limits: *prepared.limits(),
            provider: self.detail.provider.clone(),
            expires_at,
        });
        // Publish the invalidation only after the exact in-memory preview exists.
        super::emit_changed(self.app, &self.detail);
        let waiting = Instant::now();
        let outcome = (|| loop {
            if waiting.elapsed() >= remaining || self.clock.remaining_wall().is_zero() {
                return Err(Failure::Stop(InvestigationStopReason::ConsentExpired));
            }
            let observed = {
                let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let detail = jobs.investigation_owned_detail(self.execution, self.id)?;
                if terminal(detail.summary.status) {
                    return Err(Failure::AlreadyTerminal);
                }
                if detail.summary.cancel_requested {
                    return Err(Failure::Stop(InvestigationStopReason::Cancelled));
                }
                if !matches!(
                    jobs.check_execution(self.execution),
                    Ok(ExecutionCheck::Running)
                ) {
                    return Err(Failure::Stop(InvestigationStopReason::Operational));
                }
                jobs.investigation_owned_pending_step(self.execution, self.id)?
            };
            let Some((revision, observed_step, approved)) = observed else {
                return Err(Failure::Stop(InvestigationStopReason::Operational));
            };
            if observed_step != *step {
                return Err(Failure::Stop(InvestigationStopReason::Operational));
            }
            if approved {
                self.detail.summary.revision = revision;
                return Ok(crate::egress::consent_grant_for_approved_step(
                    &prepared.preview().egress,
                ));
            }
            self.live
                .wait(remaining.saturating_sub(waiting.elapsed()))?;
        })();
        self.clock.record_wait(waiting.elapsed());
        if outcome.is_err() {
            self.clear_pending()?;
        }
        outcome
    }

    fn admit_refusal(
        &mut self,
        request: &obligations::StagedObligationRequest,
        response: InvestigationResponseMeta,
        finish: staged_finish::StagedFinish,
        input: InvestigationInput,
    ) -> Result<(), Failure> {
        for _ in 0..3 {
            let updated = {
                let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let current = jobs.investigation(self.id)?;
                jobs.advance_investigation(
                    self.execution,
                    current.summary.revision,
                    InvestigationTransition::FinishRefused {
                        request: Box::new(request.clone()),
                        response: response.clone(),
                        finish: Box::new(finish.clone()),
                        input: Box::new(input.clone()),
                    },
                )
            };
            match updated {
                Ok(detail) => {
                    self.detail = detail;
                    super::emit_changed(self.app, &self.detail);
                    return Ok(());
                }
                Err(InvestigationStoreError::Stale) => continue,
                Err(_) => return Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown)),
            }
        }
        Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown))
    }

    fn admit_finish(
        &mut self,
        response: InvestigationResponseMeta,
        result: InvestigationResult,
    ) -> Result<(), Failure> {
        for _ in 0..3 {
            let updated = {
                let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let current = jobs.investigation(self.id)?;
                jobs.advance_investigation(
                    self.execution,
                    current.summary.revision,
                    InvestigationTransition::ActionAdmitted {
                        response: response.clone(),
                        tool: None,
                        result: Some(Box::new(result.clone())),
                    },
                )
            };
            match updated {
                Ok(detail) => {
                    self.detail = detail;
                    super::emit_changed(self.app, &self.detail);
                    return Ok(());
                }
                Err(InvestigationStoreError::Stale) => continue,
                // A validated response that could not be durably admitted must
                // not be acknowledged; preserve the uncertainty of its old phase.
                Err(_) => return Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown)),
            }
        }
        Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown))
    }
}

struct WorkerSink<'a, 'worker, R: tauri::Runtime> {
    worker: &'a mut Worker<'worker, R>,
}
impl<R: tauri::Runtime> AcquisitionSink for WorkerSink<'_, '_, R> {
    fn usage(&self) -> &InvestigationUsage {
        &self.worker.detail.usage
    }
    fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError> {
        self.worker.check_new_work().map_err(acquisition_error)?;
        self.worker
            .transition(InvestigationTransition::ChargeEvidence {
                validation_bytes: bytes,
            })
            .map_err(acquisition_error)
    }
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        mut usage: InvestigationUsage,
    ) -> Result<(), HostError> {
        self.worker.check_new_work().map_err(acquisition_error)?;
        usage.active_milliseconds = self.worker.clock.active_milliseconds();
        self.worker
            .transition(InvestigationTransition::InputAdmitted {
                ledger: Box::new(ledger.clone()),
                usage,
            })
            .map_err(acquisition_error)
    }
}

fn acquisition_error(error: Failure) -> HostError {
    match error {
        Failure::Host(error) => error,
        Failure::Store(InvestigationStoreError::Capacity)
        | Failure::Stop(InvestigationStopReason::LimitExceeded) => HostError::LimitExceeded,
        _ => HostError::Operational,
    }
}
fn firewall(state: &AppState) -> Result<EgressFirewall, HostError> {
    Ok(EgressFirewall::new(
        state
            .settings
            .lock()
            .map_err(|_| HostError::Operational)?
            .egress_policy()
            .map_err(|_| HostError::Operational)?,
    ))
}
fn read_detail(
    state: &AppState,
    id: &str,
    execution: &JobExecution,
) -> Result<InvestigationDetail, Failure> {
    Ok(state
        .jobs
        .lock()
        .map_err(|_| HostError::Operational)?
        .investigation_owned_detail(execution, id)?)
}
fn terminal(status: InvestigationStatus) -> bool {
    matches!(
        status,
        InvestigationStatus::Completed
            | InvestigationStatus::Failed
            | InvestigationStatus::Cancelled
            | InvestigationStatus::Interrupted
            | InvestigationStatus::OutcomeUnknown
    )
}
fn execution_directive(state: &AppState, execution: &JobExecution, id: &str) -> CallDirective {
    let Ok(jobs) = state.jobs.lock() else {
        return CallDirective::OwnershipLost;
    };
    let Ok(detail) = jobs.investigation_owned_detail(execution, id) else {
        return CallDirective::OwnershipLost;
    };
    if detail.summary.job_id != execution.id() {
        return CallDirective::OwnershipLost;
    }
    match jobs.check_execution(execution) {
        Ok(ExecutionCheck::Running)
            if !detail.summary.cancel_requested && !terminal(detail.summary.status) =>
        {
            CallDirective::Continue
        }
        Ok(ExecutionCheck::Cancelled) => CallDirective::Cancelled,
        _ => CallDirective::OwnershipLost,
    }
}
fn provider_failure(error: BoundedCallError) -> Failure {
    Failure::Stop(if error.outcome == InvocationOutcome::Unknown {
        InvestigationStopReason::OutcomeUnknown
    } else {
        match error.code {
            FailureCode::Cancelled => InvestigationStopReason::Cancelled,
            FailureCode::OwnershipLost => InvestigationStopReason::Operational,
            FailureCode::DeadlineExceeded
            | FailureCode::InputTooLarge
            | FailureCode::RequestTooLarge
            | FailureCode::ResponseTooLarge
            | FailureCode::OutputTooLarge => InvestigationStopReason::LimitExceeded,
            _ => InvestigationStopReason::ProviderFailure,
        }
    })
}
fn validate_completion(
    completion: &BoundedCompletion,
    prepared: &PreparedBoundedCompletion,
) -> Result<(), Failure> {
    let limits = prepared.limits().values();
    if completion.text.is_empty()
        || completion.text.len() as u64 > limits.output_text_bytes
        || completion.requested_model != prepared.profile().requested_model
        || completion.request_bytes > limits.request_bytes
        || completion.response_bytes > limits.response_bytes
    {
        return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
    }
    Ok(())
}
fn stop<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    id: &str,
    execution: &JobExecution,
    reason: InvestigationStopReason,
    active_milliseconds: u64,
) {
    for _ in 0..3 {
        let mut next_family = false;
        let attempt = (|| {
            let mut jobs = state
                .jobs
                .lock()
                .map_err(|_| InvestigationStoreError::Storage)?;
            let current = jobs.investigation_owned_detail(execution, id)?;
            if terminal(current.summary.status) {
                return Ok(None);
            }
            next_family = current.summary.specialist_id.uses_next_prompt_admission();
            let elapsed = active_milliseconds.max(current.usage.active_milliseconds);
            if next_family {
                jobs.stop_next_prompt_investigation(
                    execution,
                    current.summary.revision,
                    reason,
                    elapsed,
                )
            } else {
                jobs.advance_investigation(
                    execution,
                    current.summary.revision,
                    InvestigationTransition::StopWithElapsed {
                        reason,
                        active_milliseconds: elapsed,
                    },
                )
            }
            .map(Some)
        })();
        match attempt {
            Ok(Some(detail)) => {
                super::emit_changed(app, &detail);
                return;
            }
            // Preserve legacy cleanup semantics; the new family never refreshes
            // a lost write basis, even for terminal bookkeeping.
            Err(InvestigationStoreError::Stale) if !next_family => continue,
            _ => return,
        }
    }
}

#[cfg(test)]
#[path = "worker/tests.rs"]
mod tests;

#[cfg(test)]
pub(super) use tests::{with_public_start_fixture, with_public_start_fixture_then};
