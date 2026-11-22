//! The actual bounded specialist loop. Only the owned worker keeps source and
//! provider payloads; every admitted operation crosses the durable coordinator.

use super::{
    AcquisitionSink, HostError,
    context::{AcquisitionRequest, FrozenContext, bounded_json},
    runtime::{LiveTask, RunClock, descriptor},
};
use crate::jobs::investigations::{
    HostLimitKind, InvestigationResponseMeta, InvestigationStopReason, InvestigationStoreError,
    InvestigationTransition, PendingInvestigationStep, PreparationPhase,
};
use crate::{AppState, job_execution::JobExecution, jobs::ExecutionCheck};
use agents::investigation::*;
use core_prov::content_hash;
use llm::bounded::{
    BoundedCallError, BoundedCompletion, CallDirective, CompletionControl, CompletionLimits,
    PreparedBoundedCompletion,
};
use llm::{ConsentGrant, EgressFirewall, LlmProvider, Locality};
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;
use tauri::Manager;

#[path = "worker/next_metadata.rs"]
mod next_metadata;
#[path = "worker/next_prompt.rs"]
mod next_prompt;
#[path = "worker/next_source.rs"]
mod next_source;
#[path = "worker/semantic_audit.rs"]
mod semantic_audit;

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
    fn operational_class(&self) -> Option<agents::investigation::InvestigationOperationalClass> {
        use agents::investigation::InvestigationOperationalClass as Class;
        Some(match self {
            Self::Host(HostError::Operational) => Class::HostOperational,
            Self::Store(InvestigationStoreError::Invalid) => Class::StoreInvalid,
            Self::Store(InvestigationStoreError::Missing) => Class::StoreMissing,
            Self::Store(InvestigationStoreError::Conflict) => Class::StoreConflict,
            Self::Store(InvestigationStoreError::Stale) => Class::StoreStale,
            Self::Store(InvestigationStoreError::Unavailable) => Class::StoreUnavailable,
            Self::Store(InvestigationStoreError::Ownership) => Class::StoreOwnership,
            Self::Store(InvestigationStoreError::Storage) => Class::StoreStorage,
            _ => return None,
        })
    }
    fn operational_at(self, phase: agents::investigation::InvestigationFailurePhase) -> Self {
        match self.operational_class() {
            Some(error_class) => {
                Self::Stop(InvestigationStopReason::OperationalAt { phase, error_class })
            }
            None => self,
        }
    }

    // Host InvalidInput and limit failures get a fixed phase; the first phase wins.
    fn preparation_at(self, phase: PreparationPhase) -> Self {
        use agents::investigation::InvestigationFailurePhase as Phase;
        let diagnostic_phase = match phase {
            PreparationPhase::Continuation => Phase::Continuation,
            PreparationPhase::Finish => Phase::Finish,
            PreparationPhase::Metadata => Phase::Metadata,
            PreparationPhase::Query => Phase::Query,
            PreparationPhase::Inventory => Phase::Inventory,
            PreparationPhase::Candidate => Phase::Candidate,
            PreparationPhase::Certification => Phase::Certification,
            PreparationPhase::Seal => Phase::Seal,
            PreparationPhase::Publication => Phase::Publication,
            PreparationPhase::Source => Phase::Source,
            PreparationPhase::SourcePrepare => Phase::SourcePrepare,
            PreparationPhase::SourceCertification => Phase::SourceCertification,
            PreparationPhase::SourceReservation => Phase::SourceReservation,
        };
        match self.operational_at(diagnostic_phase) {
            Self::Host(HostError::InvalidInput) => {
                Self::Stop(InvestigationStopReason::PreparationAt(phase))
            }
            Self::Host(HostError::LimitExceeded) => {
                Self::Stop(InvestigationStopReason::HostLimitAt {
                    phase,
                    limit: HostLimitKind::Unspecified,
                })
            }
            other => other,
        }
    }
    // Catch residual next-family preparation failures outside annotated calls.
    fn preparation_after_call(self, specialist: SpecialistId, invocations: u32) -> Self {
        if specialist.uses_next_prompt_admission() && invocations > 0 {
            self.preparation_at(PreparationPhase::Continuation)
        } else {
            self
        }
    }
    fn reason(&self) -> Option<InvestigationStopReason> {
        use InvestigationStopReason as Stop;
        Some(match self {
            Self::AlreadyTerminal => return None,
            Self::Stop(reason) => *reason,
            Self::Host(HostError::InputChanged) => Stop::InputChanged,
            Self::Host(HostError::SourceUnavailable) => Stop::SourceUnavailable,
            Self::Host(HostError::LimitExceeded) => Stop::HostLimit,
            Self::Host(HostError::StoreCapacity)
            | Self::Store(InvestigationStoreError::Capacity) => Stop::StoreCapacity,
            Self::Store(InvestigationStoreError::Cancelled) => Stop::Cancelled,
            Self::Host(HostError::InvalidInput) => Stop::PreparationFailed,
            Self::Host(HostError::Operational) | Self::Store(_) => Stop::OperationalAt {
                phase: agents::investigation::InvestigationFailurePhase::Unspecified,
                error_class: self.operational_class().expect("operational class matched"),
            },
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
        RunClock::new(started),
        None,
        false,
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
    run_completion_with_clock(
        app,
        id,
        request,
        execution,
        provider,
        live,
        RunClock::new(started),
        task_limits,
        false,
        None,
    );
}

#[cfg(test)]
pub(super) fn run_completion_with_frozen_active_clock<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: obligations::StagedObligationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
) {
    run_completion_with_clock(
        app,
        id,
        request,
        execution,
        provider,
        live,
        RunClock::frozen_active(started),
        None,
        false,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_completion_with_clock<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: obligations::StagedObligationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    clock: RunClock,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
    dispatch_audit: bool,
    auditor: Option<ConfiguredAuditor>,
) {
    let projected = (|| {
        let state = app.state::<AppState>();
        let detail = read_detail(&state, &id, &execution)?;
        let policy = crate::jobs::investigations::policy_for_detail(&detail)?;
        Ok::<_, Failure>(FrozenContext::completion_acquisition_request_with_policy(
            &request, &policy,
        )?)
    })();
    // A drafting run stages its draft once the run has ended (SPEC-15 slice
    // 6). The run's own result and history are already settled by then.
    let drafting = (request.specialist_id == agents::section_run::SECTION_WRITER_ROLE)
        .then(|| (app.clone(), id.clone()));
    match projected {
        Ok(projected) => {
            run_inner(
                app,
                id,
                Ok(projected),
                Some(request),
                execution,
                provider,
                live,
                clock,
                task_limits,
                dispatch_audit,
                auditor,
            );
            if let Some((app, id)) = drafting {
                super::section_drafts::stage_after_run(&app, &id);
            }
        }
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
    mut clock: RunClock,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
    dispatch_audit: bool,
    auditor: Option<ConfiguredAuditor>,
) {
    let state = app.state::<AppState>();
    let _cleanup = Cleanup {
        state: &state,
        id: &id,
        live: &live,
    };
    let mut rejected = None;
    let outcome = (|| {
        let request = request?;
        let (detail, request_timeout_ms) = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_owned_runtime(&execution, &id)?;
        clock.apply_saved_limits(&detail.limits);
        let mut worker = Worker {
            pending_patch: None,
            last_completion: None,
            app: &app,
            state: &state,
            id: &id,
            execution: &execution,
            live: &live,
            detail,
            clock: &mut clock,
            task_limits,
            request_timeout_ms,
            auditor: auditor.as_ref(),
            dispatch_audit, // public production entry points always pass false
            #[cfg(test)]
            audit_probe: None,
        };
        // ADR-0085: a tool-calling version gets its own runtime for this run;
        // the runtime process ends when the run's provider is dropped.
        let run = super::tool_runtime::provider_for_run(
            request.specialist_id,
            &id,
            &provider,
            super::tool_runtime::RuntimeCommand::from_env().as_ref(),
        )
        .map_err(|_| HostError::InvalidInput)?;
        let result = worker
            .execute_inner(&request, completion_request.as_ref(), run.provider.as_ref())
            .map_err(|failure| {
                failure.preparation_after_call(
                    request.specialist_id,
                    worker.detail.usage.model_invocations,
                )
            });
        rejected = worker.last_completion.take();
        // AC-1423: name the runtime the run executed on. Attribution only:
        // a failed write never changes the run's outcome.
        if let Some(runtime) = run.runtime()
            && let Ok(mut jobs) = state.jobs.lock()
        {
            let _ = jobs.record_investigation_runtime(&id, &runtime);
        }
        result
    })();
    if let Err(failure) = outcome
        && let Some(reason) = failure.reason()
    {
        // No source/provider retry. If even terminal publication fails, the
        // durable pending phase remains available to exact-owner recovery.
        stop_keeping(
            &app,
            &state,
            &id,
            &execution,
            reason,
            clock.active_milliseconds(),
            rejected.as_ref(),
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

// Test-only checkpoint after genuine finish admission; no replacement DTOs or
// database rewrites. Production dispatch never installs this callback.
#[cfg(test)]
type AuditProbe<'a> = dyn Fn(
        &JobExecution,
        &InvestigationDetail,
        &InvestigationInput,
        &PreparedBoundedCompletion,
    ) -> Result<bool, Failure>
    + 'a;

pub(super) struct ConfiguredAuditor {
    pub provider: Arc<dyn LlmProvider>,
    pub reported_model_digest: Option<String>,
    pub analyst_reported_model_digest: Option<String>,
}

struct Worker<'a, R: tauri::Runtime> {
    app: &'a tauri::AppHandle<R>,
    state: &'a AppState,
    id: &'a str,
    execution: &'a JobExecution,
    live: &'a LiveTask,
    detail: InvestigationDetail,
    request_timeout_ms: u64,
    dispatch_audit: bool,
    auditor: Option<&'a ConfiguredAuditor>,
    #[cfg(test)]
    audit_probe: Option<&'a AuditProbe<'a>>,
    clock: &'a mut RunClock,
    task_limits: Option<super::mcp_dispatch::TaskReadLimits>,
    pending_patch: Option<agents::investigation::finish_patch::VolatileFinish>,
    /// The latest completed response, kept if it ends the task (AC-1403).
    last_completion: Option<crate::jobs::investigations::RejectedCompletion>,
}

impl<R: tauri::Runtime> Worker<'_, R> {
    /// Keep the canonical action bytes with the held response, so a later
    /// host failure records exactly what `action_hash` covers (AC-1405).
    fn hold_action(&mut self, canonical: Vec<u8>) -> String {
        let hash = content_hash(&canonical);
        if let Some(held) = self.last_completion.as_mut() {
            held.action = Some(canonical);
        }
        hash
    }

    fn completion_limits(
        &self,
        policy: &InvestigationPolicy,
        input: &InvestigationInput,
    ) -> Result<CompletionLimits, Failure> {
        let mut values = super::provider_workspace::completion_limit_values(policy)?;
        values.request_timeout_ms = self.request_timeout_ms;
        values.max_output_tokens = input.invocation_output_tokens(policy)?;
        CompletionLimits::for_investigation(values).map_err(provider_failure)
    }

    fn execute_inner(
        &mut self,
        request: &AcquisitionRequest,
        completion_request: Option<&obligations::StagedObligationRequest>,
        provider: &dyn LlmProvider,
    ) -> Result<(), Failure> {
        let effective_policy = crate::jobs::investigations::policy_for_detail(&self.detail)?;
        let policy = &effective_policy;
        if request.policy.limits() != policy.limits()
            || request.specialist_id != self.detail.summary.specialist_id
        {
            return Err(Failure::Store(InvestigationStoreError::Invalid));
        }
        self.check_new_work()?;
        let history = self.parent_history(request)?;
        let mut _inspection_guards = Vec::new();
        for target in request.inspection_requirements.as_deref().unwrap_or(&[]) {
            let (actual, guard) = super::inspection::resolve_with_limits(
                self.state,
                &target.fact,
                &target.graph_snapshot_id,
                &target.receipt_id,
                target.receipt_inventory_index as usize,
                super::context::task_graph_limits(self.task_limits.is_some()),
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

        let enable_audit = self.dispatch_audit;
        #[cfg(test)]
        let enable_audit = enable_audit || self.audit_probe.is_some();
        if enable_audit {
            self.detail = self
                .state
                .jobs
                .lock()
                .map_err(|_| HostError::Operational)?
                .enable_next_prompt_semantic_audit(self.execution, self.detail.summary.revision)?;
        }

        let mut source_replay_correction_for: Option<String> = None;
        loop {
            self.check_new_work()?;
            if self.detail.usage.model_invocations >= self.detail.limits.model_invocations {
                return Err(Failure::Stop(
                    if request.specialist_id.uses_query_budget_refusals() {
                        InvestigationStopReason::BudgetCap(
                            agents::investigation::QueryBudgetTermination::ModelInvocations,
                        )
                    } else {
                        InvestigationStopReason::LimitExceeded
                    },
                ));
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
                if snapshot.ledger() != &context.ledger
                    || snapshot.patch() != self.pending_patch.as_ref().map(|held| held.basis())
                {
                    self.pending_patch = None;
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
            let mut input = if let Some(snapshot) = &next_snapshot {
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
                snapshot.attach_acquisition(
                    InvestigationInput::prepare_v9_withholding_with_policy(
                        &snapshot.request().question,
                        self.detail.summary.specialist_id,
                        context.ledger.clone(),
                        &query_pages,
                        &context.supplied_evidence(),
                        &history,
                        &pre_call_usage,
                        snapshot.basis(),
                        snapshot.request(),
                        snapshot.metadata(),
                        &inventory_details,
                        snapshot.preparation_mode()?,
                        snapshot.capacity_refusal_count(),
                        snapshot.capacity_feedback(),
                        policy,
                        &context.withheld,
                    )?,
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
            if let Some(refusal) = completion_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.query_refusal())
            {
                input = input
                    .with_query_refusal_feedback(refusal, request.specialist_id, policy)
                    .map_err(|error| {
                        if error == InvestigationError::LimitExceeded {
                            Failure::Stop(InvestigationStopReason::HostLimit)
                        } else {
                            Failure::from(error)
                        }
                    })?;
            }
            if source_replay_correction_for.is_some() {
                input = input.with_source_replay_feedback()?;
            }
            // Input owns its projection; saved replay rows are no longer needed
            // during disclosure, consent or provider execution.
            drop(next_snapshot);
            let step_id = format!("step-{}", self.detail.usage.model_invocations + 1);
            let action = input.action(format!("{}:{step_id}", self.id))?;
            let input_bytes = bounded_json(&action.payload, 128 * 1024)?.len();
            let limits = self.completion_limits(policy, &input)?;
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
            #[cfg(test)]
            self.clock
                .invocation_committed(self.detail.usage.model_invocations);
            let deadline = self.clock.deadline().map_err(|_| {
                Failure::Stop(InvestigationStopReason::BoundedProvider(BoundedCallError {
                    code: llm::bounded::FailureCode::DeadlineExceeded,
                    outcome: llm::bounded::InvocationOutcome::NotDispatched,
                }))
            })?;
            let directive = || execution_directive(self.state, self.execution, self.id);
            // #1767: the egress is recorded when the request is handed to the
            // transport, with the transport's exact body count, so a call
            // that then fails or times out is counted like one that returns.
            let cloud = provider.locality() == Locality::Cloud;
            let provider_label = format!(
                "{}:{}",
                self.detail.provider.provider_id, self.detail.provider.model
            );
            let settings = &self.state.settings;
            let record_egress = |bytes: u64| {
                settings.lock().is_ok_and(|mut settings| {
                    settings.record_egress(&provider_label, bytes).is_ok()
                })
            };
            let recorded = std::cell::Cell::new(None);
            let dispatching = |bytes: u64| {
                let written = record_egress(bytes);
                recorded.set(Some(written));
                written
            };
            let completion = provider.complete_bounded(
                &authorized,
                &CompletionControl {
                    deadline,
                    check: &directive,
                    dispatching: cloud.then_some(&dispatching),
                },
            );
            if recorded.get() == Some(false) {
                // Nothing was sent: the record could not be written.
                return Err(Failure::Host(HostError::Operational).operational_at(
                    agents::investigation::InvestigationFailurePhase::EgressRecording,
                ));
            }
            let completion = completion.map_err(provider_failure)?;
            if cloud && recorded.get().is_none() && !record_egress(completion.request_bytes) {
                // A provider with its own transport did not use the dispatch
                // hook and reports its request size only on a completion. It
                // is recorded before the response is validated, so a response
                // the host then rejects is still counted. Every provider this
                // app constructs sends through the bounded transport and is
                // recorded before dispatch above.
                return Err(Failure::Host(HostError::Operational).operational_at(
                    agents::investigation::InvestigationFailurePhase::EgressRecording,
                ));
            }
            // Complete response bytes are still untrusted. A custom bounded
            // provider does not bypass output or immutable model identity checks.
            validate_completion(&completion, &prepared)?;
            self.last_completion = Some(crate::jobs::investigations::RejectedCompletion {
                step_id: step_id.clone(),
                text: completion.text.clone(),
                action: None,
            });
            input
                .validate_response_metadata(completion.response_model.as_deref())
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponseMetadata))?;
            if let Some(mode) = admission_mode
                && self
                    .admit_next_finish_or_action_refusal(
                        completion_request.ok_or(HostError::InvalidInput)?,
                        &input,
                        &prepared,
                        &completion,
                        mode,
                        &step_id,
                    )
                    .map_err(|error| error.preparation_at(PreparationPhase::Finish))?
            {
                if terminal(self.detail.summary.status) {
                    return Ok(());
                }
                #[cfg(test)]
                if let Some(probe) = self.audit_probe
                    && probe(self.execution, &self.detail, &input, &prepared)?
                {
                    return Ok(());
                }
                if self.dispatch_audit
                    && self.dispatch_semantic_audit(provider, &input, &prepared, &context)?
                {
                    return Ok(());
                }
                continue;
            }
            if admission_mode.is_some() {
                if self
                    .admit_next_metadata_response(
                        completion_request.ok_or(HostError::InvalidInput)?,
                        &input,
                        &prepared,
                        &completion,
                        &step_id,
                        provider,
                        &mut context,
                        &history,
                    )
                    .map_err(|error| error.preparation_at(PreparationPhase::Metadata))?
                {
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
                )
                .map_err(|error| error.preparation_at(PreparationPhase::Source))?;
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
                    if request.specialist_id.uses_cited_narrative() {
                        CompletionInvestigationAction::decode_message(
                            &completion.text,
                            &proof.context_id,
                        )
                    } else {
                        CompletionInvestigationAction::decode(&completion.text, &proof.context_id)
                    }
                }
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let value: serde_json::Value = serde_json::from_str(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = self.hold_action(bounded_json(&value, 32 * 1024)?);
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
                    // Not decoded for any offered specialist until the host
                    // executes artifact reads (ADR-0089 step 2).
                    CompletionInvestigationAction::Read(
                        QualifiedInvestigationAction::ReadArtifact(_),
                    ) => return Err(Failure::Stop(InvestigationStopReason::InvalidResponse)),
                }
            } else if let Some(proof) = &context.ledger.coverage {
                let qualified =
                    QualifiedInvestigationAction::decode(&completion.text, &proof.context_id)
                        .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                // Hash the entire admitted qualified envelope, including its
                // context identity, rather than its legacy projection.
                let value: serde_json::Value = serde_json::from_str(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = self.hold_action(bounded_json(&value, 32 * 1024)?);
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
                    QualifiedInvestigationAction::ReadArtifact(_) => {
                        return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
                    }
                }
            } else {
                let action = InvestigationAction::decode(&completion.text)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                let hash = self.hold_action(bounded_json(&action, 32 * 1024)?);
                (Some(action), None, hash)
            };
            if source_replay_correction_for.is_some()
                && !matches!(action.as_ref(), Some(InvestigationAction::Finish { .. }))
            {
                return Err(Failure::Stop(
                    InvestigationStopReason::FinishOnlyToolProposal,
                ));
            }
            let response = InvestigationResponseMeta {
                finish_feedback_witness: None,
                finish_telemetry: None,
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
                reported_cache_read_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.cache_read_input_tokens),
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
                let result = match input.admit_result_for_step(
                    self.id,
                    &step_id,
                    disclosure.as_ref(),
                    action,
                    completion.response_model,
                    timestamp,
                ) {
                    Ok(result) => result,
                    Err(InvestigationError::SourceReplay)
                        if completion_request.is_none()
                            && source_replay_correction_for.is_none() =>
                    {
                        self.admit_source_replay_refusal(&response)?;
                        source_replay_correction_for = Some(step_id);
                        if terminal(self.detail.summary.status) {
                            return Ok(());
                        }
                        continue;
                    }
                    Err(error) => {
                        return Err(Failure::Stop(InvestigationStopReason::InvalidFinish(error)));
                    }
                };
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
                    match context.prepare_query_with_diagnostic(state, query.clone()) {
                        Ok(prepared) => context.publish_context_read(prepared, &mut sink)?,
                        Err(super::context::ContextReadPreparationError::QueryRejected(reason))
                            if completion_request.is_some() =>
                        {
                            use agents::investigation::query_refusal::QueryRefusalRecord;
                            let mut record = QueryRefusalRecord {
                                budget: None,
                                schema_version: 1,
                                record_id: String::new(),
                                task_id: sink.worker.id.into(),
                                step_id: step_id.clone(),
                                action_hash: String::new(),
                                input_ledger_hash: context
                                    .ledger
                                    .fingerprint_with_policy(policy)?,
                                input_revision: context.ledger.revision,
                                context_id: context
                                    .ledger
                                    .coverage
                                    .as_ref()
                                    .map(|proof| proof.context_id.clone()),
                                query,
                                reason,
                            };
                            record.action_hash = record.query_action_hash()?;
                            record.record_id = record.content_id()?;
                            sink.worker
                                .transition(InvestigationTransition::QueryRefused {
                                    record: Box::new(record),
                                })?;
                        }
                        Err(error) => return Err(error.into_host_error().into()),
                    }
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
                        // Classify the exact ordinary-read membership refusal before
                        // acquisition; completion keeps its separate refusal contract.
                        if !context.ledger.selected_facts.iter().any(|s| s.fact == fact) {
                            return Err(Failure::Stop(InvestigationStopReason::UnadmittedRead));
                        }
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
        self.clock.deadline().map_err(|error| {
            if self
                .detail
                .summary
                .specialist_id
                .uses_query_budget_refusals()
                && self.clock.active_milliseconds()
                    >= self.detail.limits.active_seconds.saturating_mul(1000)
            {
                Failure::Stop(InvestigationStopReason::BudgetCap(
                    agents::investigation::QueryBudgetTermination::ActiveTime,
                ))
            } else {
                Failure::from(error)
            }
        })?;
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
                .investigation_next_parent_history_with_policy(
                    parent_id,
                    conversation,
                    &request.policy,
                )?
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
        let remaining = self.clock.consent_wait_remaining();
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

    fn admit_source_replay_refusal(
        &mut self,
        response: &InvestigationResponseMeta,
    ) -> Result<(), Failure> {
        for _ in 0..3 {
            let updated = {
                let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let current = jobs.investigation(self.id)?;
                jobs.advance_investigation(
                    self.execution,
                    current.summary.revision,
                    InvestigationTransition::SourceReplayRefused {
                        step_id: response.step_id.clone(),
                        reported_input_tokens: response.reported_input_tokens,
                        reported_output_tokens: response.reported_output_tokens,
                        reported_cache_read_tokens: response.reported_cache_read_tokens,
                        active_milliseconds: response.active_milliseconds,
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
        | Failure::Stop(InvestigationStopReason::StoreCapacity) => HostError::StoreCapacity,
        Failure::Stop(reason)
            if reason.terminal_class() == InvestigationStopReason::LimitExceeded =>
        {
            HostError::LimitExceeded
        }
        _ => HostError::Operational,
    }
}
/// The investigation firewall: the saved egress policy and the project's
/// saved read-deny rules on top of the defaults (ADR-0083, #679).
fn firewall(state: &AppState) -> Result<EgressFirewall, HostError> {
    let settings = state.settings.lock().map_err(|_| HostError::Operational)?;
    Ok(EgressFirewall::new(
        settings
            .egress_policy()
            .map_err(|_| HostError::Operational)?,
    )
    .with_read_deny_rules(
        settings
            .read_deny_rules()
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
    Failure::Stop(InvestigationStopReason::BoundedProvider(error))
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
        return Err(Failure::Stop(
            InvestigationStopReason::InvalidCompletionEnvelope,
        ));
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
    stop_keeping(app, state, id, execution, reason, active_milliseconds, None);
}
/// Stop the task; a rejected response that caused the stop is kept with it.
fn stop_keeping<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    id: &str,
    execution: &JobExecution,
    reason: InvestigationStopReason,
    active_milliseconds: u64,
    rejected: Option<&crate::jobs::investigations::RejectedCompletion>,
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
                jobs.stop_next_prompt_investigation_keeping(
                    execution,
                    current.summary.revision,
                    reason,
                    elapsed,
                    rejected,
                )
            } else {
                jobs.advance_investigation(
                    execution,
                    current.summary.revision,
                    match rejected {
                        Some(rejected) => InvestigationTransition::StopRejected {
                            reason,
                            active_milliseconds: elapsed,
                            rejected: rejected.clone(),
                        },
                        None => InvestigationTransition::StopWithElapsed {
                            reason,
                            active_milliseconds: elapsed,
                        },
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
pub(super) use tests::{
    with_public_start_fixture, with_public_start_fixture_then, with_response_app,
};

// Test only: exercise the actual response coordinator on a genuine saved call
// without opening the public/run/resume gates or dispatching any provider.
#[cfg(test)]
pub(super) fn handle_gated_response(
    app: &tauri::AppHandle<tauri::test::MockRuntime>,
    saved: &crate::jobs::investigations::CapturedSourceInvocationFixture,
    completion: &BoundedCompletion,
    before_handle: impl FnOnce(),
) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(saved.execution(), saved.id())
        .unwrap();
    let live = state.investigations.insert(saved.id()).unwrap();
    let mut clock = RunClock::new(Instant::now() - Duration::from_secs(1));
    let mode = saved
        .current()
        .seal_disclosure_with_policy(
            saved.prepared(),
            &crate::jobs::investigations::policy_for_detail(&detail).unwrap(),
        )
        .unwrap()
        .unwrap()
        .admission
        .unwrap()
        .mode;
    let mut worker = Worker {
        pending_patch: None,
        last_completion: None,
        app,
        state: &state,
        id: saved.id(),
        execution: saved.execution(),
        live: &live,
        detail,
        clock: &mut clock,
        task_limits: None,
        request_timeout_ms: saved.prepared().limits().values().request_timeout_ms,
        dispatch_audit: false,
        auditor: None,
        audit_probe: None,
    };
    before_handle();
    let result = validate_completion(completion, saved.prepared())
        .and_then(|()| {
            saved
                .current()
                .validate_response_metadata(completion.response_model.as_deref())?;
            worker.admit_next_finish_or_action_refusal(
                saved.request(),
                saved.current(),
                saved.prepared(),
                completion,
                mode,
                &saved.admission().response.step_id,
            )
        })
        .map_err(|error| format!("{error:?}"));
    state.investigations.remove(saved.id());
    result
}
