//! One admitted blocking worker owns the app borrows and frozen dispatcher.
//! Dropping the async client closes its queue; running synchronous work retains
//! its permit until it exits. No store lock or receipt read runs on the UI thread.
use super::{
    ReadError, ReadSession,
    dispatch::{DispatchAction, DispatchLimits, Dispatcher, EvidenceWorkLimits},
};
use crate::investigations::mcp_dispatch::{self, TaskDispatcher};
use mcp_local::work_budget::WorkBudget;
use mcp_wire::admission::{Action, ProtocolAdmission, ReadTool};
use mcp_wire::tools::Tool;
use serde_json::{Value, json};
use std::num::NonZeroUsize;
use tauri::Manager;
use tokio::sync::{mpsc, oneshot};

pub(crate) const WORKER_MODEL: &str = "mcp-worker-payload-v1";
pub(crate) const SCOPED_WORKER_MODEL: &str = "mcp-scoped-worker-payload-v1";

/// Human-selected acquisition strategy; failures never switch strategies.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcquisitionMode {
    #[default]
    Complete,
    Scoped,
}

enum ActiveDispatcher<'a> {
    Complete(Box<Dispatcher<'a>>),
    Scoped(Box<super::scoped_dispatch::ScopedDispatcher<'a>>),
}

#[derive(Clone, Copy)]
pub(crate) struct WorkerLimits {
    pub acquisition_mode: AcquisitionMode,
    pub max_request_frame_bytes: NonZeroUsize,
    pub graph: core_graph::SnapshotReadLimits,
    pub coverage: core_graph::coverage::CoverageReadLimits,
    pub scope: super::scope::ScopeLimits,
    pub dispatch: DispatchLimits,
    pub evidence: EvidenceWorkLimits,
    /// Host-supplied logical worker reservation, checked against the derived
    /// phase minimum before worker admission. Transport is reserved separately;
    /// neither ledger is an RSS guarantee.
    pub reserved_bytes: NonZeroUsize,
}

#[derive(Clone, Copy)]
pub(crate) struct WorkerStartup {
    limits: WorkerLimits,
    task_phase: TaskPhaseAdmission,
}

#[derive(Clone, Copy)]
enum TaskPhaseAdmission {
    NoTaskGrant,
    TaskGrant {
        app_path_bytes: usize,
        minimum: usize,
    },
}

impl WorkerStartup {
    #[cfg(test)]
    async fn prepare_on_blocking_pool(
        compute: impl FnOnce() -> Result<Self, ReadError> + Send + 'static,
    ) -> Result<Self, ReadError> {
        tokio::task::spawn_blocking(compute)
            .await
            .map_err(|_| ReadError::Storage)?
    }

    #[cfg(unix)]
    pub(crate) fn prepare(
        limits: WorkerLimits,
        task_granted: bool,
        app_path_bytes: usize,
    ) -> Result<Self, ReadError> {
        Self::prepare_with(limits, task_granted, app_path_bytes, task_phase_minimum)
    }

    #[cfg(not(unix))]
    pub(crate) fn prepare(
        limits: WorkerLimits,
        _task_granted: bool,
        _app_path_bytes: usize,
    ) -> Result<Self, ReadError> {
        Ok(Self {
            limits,
            task_phase: TaskPhaseAdmission::NoTaskGrant,
        })
    }

    #[cfg(unix)]
    fn prepare_with(
        limits: WorkerLimits,
        task_granted: bool,
        app_path_bytes: usize,
        compute: impl FnOnce(WorkerLimits, usize) -> Option<usize>,
    ) -> Result<Self, ReadError> {
        let task_phase = if task_granted {
            let minimum = compute(limits, app_path_bytes).ok_or(ReadError::Limit)?;
            if limits.reserved_bytes.get() < minimum {
                return Err(ReadError::Limit);
            }
            TaskPhaseAdmission::TaskGrant {
                app_path_bytes,
                minimum,
            }
        } else {
            TaskPhaseAdmission::NoTaskGrant
        };
        Ok(Self { limits, task_phase })
    }

    #[cfg(unix)]
    fn validate_task_phase(
        &self,
        task_granted: bool,
        app_path_bytes: usize,
    ) -> Result<(), ReadError> {
        let minimum = match self.task_phase {
            TaskPhaseAdmission::NoTaskGrant if !task_granted => return Ok(()),
            TaskPhaseAdmission::TaskGrant { minimum, .. } if !task_granted => minimum,
            TaskPhaseAdmission::TaskGrant {
                app_path_bytes: expected,
                minimum,
            } if task_granted && expected == app_path_bytes => minimum,
            _ => return Err(ReadError::InvalidRequest),
        };
        if self.limits.reserved_bytes.get() < minimum {
            return Err(ReadError::Limit);
        }
        Ok(())
    }
}

struct Request {
    value: serde_json::Value,
    reply: oneshot::Sender<Result<DispatchAction, ReadError>>,
}

pub(crate) struct Worker {
    #[cfg(test)]
    pub(crate) test_exit: Option<tauri::async_runtime::JoinHandle<()>>,
    requests: mpsc::Sender<Request>,
    max_request_frame_bytes: NonZeroUsize,
}
impl Worker {
    /// Read-only fixture shorthand; production uses the authenticated factory
    /// with an optional activation-owned task session.
    #[cfg(test)]
    pub(crate) async fn start<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        mode: spec::ExportMode,
        limits: WorkerLimits,
        budget: WorkBudget,
    ) -> Result<Self, ReadError> {
        Self::start_with_tasks(app, session_id, mode, limits, budget, None).await
    }

    /// Test convenience; production must pass the validated preflight value.
    /// Preparation runs on Tokio's blocking pool so this legacy test path does
    /// not monopolize an async worker.
    #[cfg(test)]
    pub(crate) async fn start_with_tasks<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        mode: spec::ExportMode,
        limits: WorkerLimits,
        budget: WorkBudget,
        tasks: Option<agents::investigation::McpTaskSession>,
    ) -> Result<Self, ReadError> {
        let task_granted = tasks
            .as_ref()
            .is_some_and(|session| session.with_grant(|_, _| ()).is_ok());
        #[cfg(unix)]
        let app_path_bytes = if task_granted {
            app.state::<crate::AppState>()
                .primary_sources
                .retained_app_path_bytes()
        } else {
            0
        };
        #[cfg(not(unix))]
        let app_path_bytes = 0;
        let startup = WorkerStartup::prepare_on_blocking_pool(move || {
            WorkerStartup::prepare(limits, task_granted, app_path_bytes)
        })
        .await?;
        Self::start_with_startup(app, session_id, mode, startup, budget, tasks).await
    }

    pub(crate) async fn start_with_startup<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        mode: spec::ExportMode,
        startup: WorkerStartup,
        budget: WorkBudget,
        tasks: Option<agents::investigation::McpTaskSession>,
    ) -> Result<Self, ReadError> {
        let limits = startup.limits;
        if session_id.is_empty()
            || (limits.acquisition_mode == AcquisitionMode::Scoped
                && (session_id.len() > 64 || session_id.chars().any(char::is_control)))
        {
            return Err(ReadError::InvalidRequest);
        }
        if limits.acquisition_mode == AcquisitionMode::Scoped {
            super::scoped_session::ScopedReadSession::validate_graph_limits(limits.graph)?;
        }
        #[cfg(unix)]
        {
            validate_reservation(limits)?;
            let task_granted = tasks
                .as_ref()
                .is_some_and(|session| session.with_grant(|_, _| ()).is_ok());
            let path_bytes = if task_granted {
                app.state::<crate::AppState>()
                    .primary_sources
                    .retained_app_path_bytes()
            } else {
                0
            };
            startup.validate_task_phase(task_granted, path_bytes)?;
        }
        let permit = budget
            .try_acquire(limits.reserved_bytes, 0)
            .map_err(|_| ReadError::Limit)?;
        let (requests, mut incoming) = mpsc::channel::<Request>(1);
        let (ready, initialized) = oneshot::channel::<Result<(), ReadError>>();
        let worker_exit = tauri::async_runtime::spawn_blocking(move || {
            // Own the permit in the actual worker, not in the async waiter.
            let _permit = std::sync::Arc::new(permit);
            let tasks = tasks.map(|session| {
                TaskDispatcher::new(
                    session,
                    super::task_reservation::TaskReservations::from(_permit.clone()),
                )
            });
            let state = app.state::<crate::AppState>();
            let mut session_id = Some(session_id);
            let mut scope = || super::scope::coverage_scope(&app, &state, limits.scope);
            let mut context = super::scoped_session::ScopedReadContext {
                graph: &state.graph,
                decisions: &state.decisions,
                scope: &mut scope,
            };
            let tools = match limits.acquisition_mode {
                AcquisitionMode::Complete => super::dispatch::IMPLEMENTED
                    .into_iter()
                    .chain([ReadTool::Evidence])
                    .collect::<Vec<_>>(),
                AcquisitionMode::Scoped => super::scoped_dispatch::TOOLS.to_vec(),
            };
            let mut tools = tools.into_iter().map(Tool::Read).collect::<Vec<_>>();
            if tasks.as_ref().is_some_and(TaskDispatcher::available) {
                tools.extend(mcp_dispatch::TOOLS.into_iter().map(Tool::Task));
            }
            let mut admission = ProtocolAdmission::new(tools, limits.dispatch.max_request_ids);
            let mut dispatcher = None;
            if ready.send(Ok(())).is_err() {
                return;
            }
            while let Some(request) = incoming.blocking_recv() {
                if request.reply.is_closed() {
                    break;
                }
                if _permit
                    .charge_read(limits.evidence.max_file_validation_bytes.get())
                    .is_err()
                {
                    let _ = request.reply.send(Err(ReadError::Limit));
                    break;
                }
                let action = match admission.receive(request.value) {
                    Action::Ignore => DispatchAction::Ignore,
                    Action::Close(_) => DispatchAction::Close,
                    Action::Respond(value) => {
                        bounded(value, limits.dispatch.max_response_bytes.get())
                    }
                    Action::ListTools { id, tools } => {
                        let tools = tools
                            .into_iter()
                            .filter(|tool| {
                                !matches!(tool, Tool::Task(_))
                                    || tasks.as_ref().is_some_and(TaskDispatcher::available)
                            })
                            .map(|tool| match tool {
                                Tool::Task(tool) => mcp_dispatch::descriptor(tool),
                                Tool::Read(tool) => match limits.acquisition_mode {
                                    AcquisitionMode::Complete => {
                                        super::dispatch::tool_descriptor(tool)
                                    }
                                    AcquisitionMode::Scoped => {
                                        super::scoped_dispatch::schemas::descriptor(
                                            tool,
                                            limits.graph,
                                        )
                                    }
                                },
                            })
                            .collect::<Vec<_>>();
                        bounded(
                            json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools}}),
                            limits.dispatch.max_response_bytes.get(),
                        )
                    }
                    Action::Call {
                        id,
                        tool: Tool::Task(tool),
                        arguments,
                    } => {
                        let result =
                            tasks
                                .as_ref()
                                .ok_or(ReadError::InvalidRequest)
                                .and_then(|tasks| {
                                    tasks.call(
                                        &app,
                                        &id,
                                        tool,
                                        arguments,
                                        mcp_dispatch::TaskReadLimits {
                                            scope: limits.scope,
                                            evidence: limits.evidence,
                                        },
                                        limits.dispatch.max_response_bytes.get(),
                                    )
                                });
                        bounded(
                            result.unwrap_or_else(|error| super::dispatch::tool_error(&id, error)),
                            limits.dispatch.max_response_bytes.get(),
                        )
                    }
                    Action::Call {
                        id,
                        tool: Tool::Read(tool),
                        arguments,
                    } => {
                        let result = (|| {
                            if dispatcher.is_none() {
                                dispatcher = Some(match limits.acquisition_mode {
                                    AcquisitionMode::Complete => {
                                        let session = ReadSession::acquire_with_overlay(
                                            &state.graph,
                                            state.knowledge_overlay.as_deref(),
                                            &state.decisions,
                                            // The reviews the export gathers.
                                            &*state,
                                            || {
                                                super::scope::coverage_scope(
                                                    &app,
                                                    &state,
                                                    limits.scope,
                                                )
                                            },
                                            limits.graph,
                                            limits.coverage,
                                            mode,
                                        )?;
                                        ActiveDispatcher::Complete(Box::new(
                                            Dispatcher::with_evidence(
                                                session,
                                                session_id
                                                    .as_ref()
                                                    .expect("unacquired session ID")
                                                    .clone(),
                                                limits.dispatch,
                                                &state,
                                                limits.evidence,
                                            )?,
                                        ))
                                    }
                                    AcquisitionMode::Scoped => {
                                        let session =
                                            super::scoped_session::ScopedReadSession::acquire(
                                                &mut context,
                                                limits.graph,
                                                limits.coverage,
                                                mode,
                                            )?;
                                        ActiveDispatcher::Scoped(Box::new(
                                            super::scoped_dispatch::ScopedDispatcher::new(
                                                session,
                                                session_id
                                                    .as_ref()
                                                    .expect("unacquired session ID")
                                                    .clone(),
                                                limits.dispatch,
                                                &state,
                                                limits.evidence,
                                            )?,
                                        ))
                                    }
                                });
                            }
                            session_id = None; // Dispatcher now owns the session metadata.
                            Ok::<_, ReadError>(())
                        })();
                        match result {
                            Err(error) => bounded(
                                super::dispatch::tool_error(&id, error),
                                response_cap(tool, &arguments, limits),
                            ),
                            Ok(()) => {
                                match dispatcher.as_mut().expect("successful read acquisition") {
                                    ActiveDispatcher::Complete(dispatcher) => {
                                        dispatcher.call_admitted(&id, tool, arguments)
                                    }
                                    ActiveDispatcher::Scoped(dispatcher) => {
                                        dispatcher.call_admitted(&id, tool, arguments, &mut context)
                                    }
                                }
                            }
                        }
                    }
                };
                let close = matches!(action, DispatchAction::Close);
                if request.reply.send(Ok(action)).is_err() || close {
                    break;
                }
            }
        });
        #[cfg(not(test))]
        drop(worker_exit);
        initialized.await.map_err(|_| ReadError::Storage)??;
        Ok(Self {
            #[cfg(test)]
            test_exit: Some(worker_exit),
            requests,
            max_request_frame_bytes: limits.max_request_frame_bytes,
        })
    }

    pub(crate) async fn receive(
        &mut self,
        value: serde_json::Value,
    ) -> Result<DispatchAction, ReadError> {
        let mut counter = RequestCounter {
            size: 0,
            limit: self.max_request_frame_bytes.get(),
        };
        serde_json::to_writer(&mut counter, &value).map_err(|_| ReadError::Limit)?;
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Request { value, reply })
            .await
            .map_err(|_| ReadError::Storage)?;
        response.await.map_err(|_| ReadError::Storage)?
    }
}

/// A test worker's thread holds an app handle. Dropping the worker closes its
/// request channel and waits for that thread to leave, so the handle is gone
/// before the test's app is freed (#1621).
#[cfg(test)]
impl Drop for Worker {
    fn drop(&mut self) {
        let Some(exit) = self.test_exit.take() else {
            return;
        };
        let (closed, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.requests, closed));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !exit.inner().is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

fn response_cap(
    tool: ReadTool,
    arguments: &serde_json::Map<String, Value>,
    limits: WorkerLimits,
) -> usize {
    let requested = match (limits.acquisition_mode, tool) {
        (AcquisitionMode::Complete, ReadTool::Discover | ReadTool::Query) => arguments
            .get("request")
            .and_then(|v| v.get("request"))
            .and_then(|v| v.get("max_bytes")),
        (AcquisitionMode::Scoped, ReadTool::Discover) => {
            arguments.get("request").and_then(|v| v.get("max_bytes"))
        }
        _ => arguments.get("max_bytes"),
    };
    requested
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(limits.dispatch.max_response_bytes.get())
        .min(limits.dispatch.max_response_bytes.get())
}

fn bounded(value: Value, cap: usize) -> DispatchAction {
    if super::dispatch::fits(&value, cap) {
        return DispatchAction::Respond(value);
    }
    let error = super::dispatch::error_response(value.get("id"), ReadError::Limit);
    if super::dispatch::fits(&error, cap) {
        DispatchAction::Respond(error)
    } else {
        DispatchAction::Close
    }
}

#[cfg(unix)]
fn validate_reservation(limits: WorkerLimits) -> Result<(), ReadError> {
    let required = required_reservation(limits).ok_or(ReadError::Limit)?;
    if limits.reserved_bytes.get() < required {
        return Err(ReadError::Limit);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn required_reservation(limits: WorkerLimits) -> Option<usize> {
    match limits.acquisition_mode {
        AcquisitionMode::Complete => Some(
            super::reservation::worker_bound::WorkerReservation::for_limits(limits)?.required_bytes,
        ),
        AcquisitionMode::Scoped => super::reservation::scoped_bound::required_bytes(limits)?
            .checked_add(super::scoped_dispatch::logical_protocol_workspace_bytes(
                limits,
            )?),
    }
}

/// Preparation, retained reads and modeled replay/provider component floors.
/// Later source acquisition still needs composition before dispatch enablement.
#[cfg(unix)]
pub(crate) fn task_phase_minimum(limits: WorkerLimits, app_path_bytes: usize) -> Option<usize> {
    let (preparation, context, execution) = task_phase_workspaces(limits, app_path_bytes)?;
    let per_task = task_execution_peak(preparation, context, execution)?;
    required_reservation(limits)?
        .checked_add(per_task.checked_mul(crate::jobs::investigations::MAX_LIVE_TASKS)?)
}

// Keep the actual retained term observable before phase maxima can mask it.
#[cfg(unix)]
fn task_phase_workspaces(
    limits: WorkerLimits,
    app_path_bytes: usize,
) -> Option<(usize, usize, usize)> {
    let (context, preparation) = crate::investigations::preparation_workspace::workspace_bytes(
        limits.scope,
        agents::investigation::MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
        limits.max_request_frame_bytes.get(),
    )?;
    let provider = crate::investigations::provider_workspace::payload_peak(
        &agents::investigation::INVESTIGATION_V2_POLICY,
    )?;
    let (_, decode) = crate::jobs::investigations::next_prompt_record_workspace(
        agents::investigation::MAX_RECORD_BYTES,
    )?;
    // Keep the row/decode owners through pure validation conservatively; do
    // not rely on later row-lifetime optimizations in the parent branch.
    let validation =
        decode.checked_add(crate::jobs::investigations::next_prompt_contract_workspace()?)?;
    let construction = agents::investigation::InvestigationInput::construction_workspace_bytes(
        &agents::investigation::INVESTIGATION_V2_POLICY,
    )?;
    // Initial @18 preparation retains the saved normalized contract through
    // input construction, then derives choices before dropping that contract.
    // Its two hashes and fixed keys add less than 256 bytes to the envelope.
    // Charge construction scratch alongside candidate derivation conservatively;
    // completed input owners are retained in context below, not hidden slack.
    let initial_choices = context_hub::logical::json_bytes(
        agents::investigation::INVESTIGATION_V2_POLICY
            .envelope_bytes()
            .checked_add(256)?,
    )?
    .checked_add(construction)?
    .checked_add(
        agents::investigation::acquisition_choices::AcquisitionChoices::candidate_workspace_bytes(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )?,
    )?;
    // The writer holds its projection through retained-source validation and
    // replay. Candidate preparation survives commit; original invocation,
    // current/candidate inputs and source guards stay in context below.
    let settlement = crate::jobs::investigations::source_settlement_workspace(
        provider,
        crate::investigations::provider_workspace::source_settlement_retained(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )?,
        crate::investigations::source_preparation_workspace::peak_bytes(
            limits.evidence,
            app_path_bytes,
        )?,
    )?
    .1;
    // Finish publication retains its authenticated response/core while the
    // selected completion basis and result/attempt validation are acquired.
    // Current Inputs and the actual provider preparation/response stay in the
    // context term below, outside this phase maximum.
    let finish = crate::jobs::investigations::next_prompt_result_workspace()?
        .1
        .max(
            crate::jobs::investigations::next_prompt_staged_workspace()?
                .1
                .checked_add(crate::jobs::investigations::completion_basis_workspace()?.0)?,
        ) // current writer basis survives historical refusal replay
        .checked_add(crate::jobs::investigations::next_prompt_saved_call_workspace()?.0)?
        .checked_add(context_hub::logical::json_bytes(
            agents::investigation::MAX_ACTION_BYTES.checked_mul(2)?,
        )?)? // saved call DTO plus separately encoded original action
        .checked_add(
            agents::investigation::InvestigationInput::combined_correction_workspace_bytes(
                &agents::investigation::INVESTIGATION_V2_POLICY,
            )?,
        )?;
    let audit_limits = llm::bounded::CompletionLimits::new(
        crate::investigations::provider_workspace::completion_limit_values(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )
        .ok()?,
    )
    .ok()?;
    let audit = crate::investigations::provider_workspace::audit_retained(
        &agents::investigation::INVESTIGATION_V2_POLICY,
    )?
    .checked_add(
        crate::jobs::investigations::next_prompt_audit_transaction_workspace(
            crate::investigations::source_preparation_workspace::peak_bytes(
                limits.evidence,
                app_path_bytes,
            )?,
            &audit_limits,
        )?,
    )?;
    let execution = provider
        .max(audit)
        .max(
            crate::investigations::provider_workspace::audit_payload_peak(
                &agents::investigation::INVESTIGATION_V2_POLICY,
            )?,
        )
        .max(settlement)
        .max(initial_choices)
        .max(
            crate::investigations::source_candidate_workspace::workspace(
                agents::investigation::MAX_MANIFEST_BYTES,
                agents::investigation::MAX_INSPECTION_TARGET_BYTES,
            )?
            .peak()?,
        )
        .max(
            crate::investigations::source_preparation_workspace::peak_bytes(
                limits.evidence,
                app_path_bytes,
            )?,
        )
        // Source receipts/guards/outcomes are retained in context below. This
        // adds only the nested manifest and bounded object/span acquisition.
        .max(
            super::reservation::capture::workspace(
                limits.evidence,
                agents::investigation::MAX_INSPECTION_TARGET_BYTES,
            )?
            .peak(),
        )
        .max(validation)
        .max(crate::jobs::investigations::archive_acquisition_workspace()?)
        .max(crate::jobs::investigations::next_prompt_ledger_workspace()?)
        .max(crate::jobs::investigations::next_prompt_metadata_workspace()?.2)
        .max(crate::jobs::investigations::next_prompt_saved_call_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_timeline_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_capacity_replay_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_saved_validation_workspace()?.0)
        .max(crate::jobs::investigations::next_prompt_saved_validation_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_pending_source_workspace()?)
        .max(crate::jobs::investigations::next_prompt_source_capacity_workspace()?.1)
        .max(finish)
        .max(crate::jobs::investigations::next_prompt_staged_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_correction_workspace()?)
        .max(crate::jobs::investigations::next_prompt_task_load_workspace()?.1)
        .max(crate::jobs::investigations::next_prompt_task_write_workspace()?)
        .max(
            crate::jobs::investigations::next_prompt_attempt_replay_workspace()?.checked_add(
                agents::investigation::disclosure_validation_workspace(
                    &agents::investigation::INVESTIGATION_V2_POLICY,
                    agents::investigation::MAX_RECORD_BYTES,
                )?,
            )?,
        );
    // FrozenContext survives the whole execution loop, including saved-task
    // reads and validation. Preparation already includes its retained context.
    // Prepared inventory remains owned during durable publication/replay.
    // The returned source snapshot survives input/firewall construction until
    // prepared_source_publication drops it before the final revalidation.
    let (snapshot, snapshot_peak) = crate::jobs::investigations::source_snapshot_workspace()?;
    let (metadata_snapshot, metadata_peak) =
        crate::jobs::investigations::next_prompt_metadata_continuation_workspace()?;
    let execution = snapshot_peak
        .max(metadata_peak)
        .max(snapshot.max(metadata_snapshot).checked_add(execution)?);
    let execution = crate::investigations::inventory_admission_workspace(execution)?;
    let (query_retained, query_peak) =
        crate::investigations::query_workspace::admitted_workspace(limits.scope, app_path_bytes)?;
    // Each next_metadata transition prepares one query OR inventory candidate;
    // publish_admitted_metadata consumes it before another read. Legacy query /
    // read_inventory likewise consume their candidate in publish_context_read.
    // Prior published pages/ledger are in retained_read_bytes below, not a second
    // prepared candidate. This nesting conservatively overlaps wrapper owners;
    // batching unpublished reads would require symmetric peak composition.
    let execution = query_peak.max(query_retained.checked_add(execution)?);
    let context = context.checked_add(
        crate::investigations::preparation_workspace::retained_read_bytes(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )?,
    )?;
    // A metadata continuation retains the current input while constructing a
    // candidate. Charge both completed inputs throughout execution; rejected
    // construction, certificates and sealing peaks remain separate work.
    let context = context.checked_add(
        agents::investigation::InvestigationInput::retained_workspace_bytes(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )?
        .checked_mul(2)?,
    )?;
    // The original invocation's preparation, authorization and response survive
    // into next_metadata. They overlap every candidate acquisition/firewall
    // phase, rather than disappearing when execution selects its largest peak.
    let context = context.checked_add(
        crate::investigations::provider_workspace::continuation_retained(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )?,
    )?;
    // @27's one-turn first finish overlaps correction preparation, patch
    // decoding and merged admission. Keep its existing-cap-derived ownership
    // outside phase maxima; this adds no new numeric ceiling or durable text.
    let context = context.checked_add(agents::investigation::finish_patch::workspace_bytes()?)?;
    // Source leases, acknowledged receipts and the actual read outcome survive
    // nested replay and publication. Their ownership is outside the phase max;
    // source acquisition/publication scratch still needs separate composition.
    let context = context.checked_add(
        crate::investigations::source_retained_workspace::retained_bytes(
            limits.evidence,
            app_path_bytes,
        )?,
    )?;
    let (history, history_projection) =
        crate::jobs::investigations::parent_history_projection_workspace()?;
    // Parent history is acquired first, then survives preparation and every
    // later execution phase. Acquisition includes legacy family reloads.
    let history_acquisition = crate::jobs::investigations::parent_history_acquisition_workspace()?;
    Some((
        history_acquisition
            .max(history_projection)
            .max(preparation.checked_add(history)?),
        context.checked_add(history)?,
        execution,
    ))
}

#[cfg(unix)]
fn task_execution_peak(preparation: usize, context: usize, execution: usize) -> Option<usize> {
    Some(preparation.max(context.checked_add(execution)?))
}

struct RequestCounter {
    size: usize,
    limit: usize,
}
impl std::io::Write for RequestCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .size
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| std::io::Error::other("request limit"))?;
        self.size = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
impl mcp_local::serve::Session for Worker {
    async fn dispatch(
        &mut self,
        message: serde_json::Value,
    ) -> Result<mcp_local::serve::Reply, mcp_local::serve::ServeError> {
        use mcp_local::serve::{Reply, ServeError};
        match self.receive(message).await {
            Ok(DispatchAction::Respond(value)) => Ok(Reply::Respond(value)),
            Ok(DispatchAction::Ignore) => Ok(Reply::Ignore),
            Ok(DispatchAction::Close) => Ok(Reply::Close),
            Err(_) => Err(ServeError::Application),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mcp_local::work_budget::WorkLimits;
    use std::num::NonZeroU64;
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }
    pub(crate) fn task_read_limits() -> crate::investigations::mcp_dispatch::TaskReadLimits {
        let limits = limits();
        crate::investigations::mcp_dispatch::TaskReadLimits {
            scope: limits.scope,
            evidence: limits.evidence,
        }
    }

    #[cfg(unix)]
    #[test]
    fn complete_revision_reservation_accepts_exact_floor_and_refuses_one_under() {
        // AC-0510: complete revision admission accepts the exact derived floor and refuses one-under.
        let mut policy = limits();
        let required = super::required_reservation(policy).unwrap();
        policy.reserved_bytes = NonZeroUsize::new(required).unwrap();
        assert_eq!(super::validate_reservation(policy), Ok(()));
        policy.reserved_bytes = NonZeroUsize::new(required - 1).unwrap();
        assert_eq!(super::validate_reservation(policy), Err(ReadError::Limit));
    }

    pub(crate) fn limits() -> WorkerLimits {
        let limits = WorkerLimits {
            acquisition_mode: AcquisitionMode::Complete,
            max_request_frame_bytes: nz(65536),
            graph: core_graph::SnapshotReadLimits::default(),
            coverage: core_graph::coverage::CoverageReadLimits::default(),
            scope: super::super::scope::tests::limits(),
            dispatch: DispatchLimits {
                max_response_bytes: nz(65536),
                max_request_ids: nz(16),
                trace: flowtracer::TraceLimits {
                    max_flows: 16,
                    max_hops: 64,
                    max_output_bytes: 65536,
                },
                compile: spec::SelectedCompileLimits {
                    max_bytes: 65536,
                    adr: spec::AdrLimits {
                        max_rows: 128,
                        max_output_bytes: 65536,
                    },
                },
            },
            evidence: EvidenceWorkLimits {
                max_file_validation_bytes: NonZeroU64::new(4096).unwrap(),
                max_span_bytes: NonZeroU64::new(1024).unwrap(),
            },
            reserved_bytes: nz(1_000_000),
        };
        #[cfg(unix)]
        let limits = {
            let mut limits = limits;
            limits.reserved_bytes = nz(
                super::super::reservation::worker_bound::WorkerReservation::for_limits(limits)
                    .unwrap()
                    .required_bytes,
            );
            limits
        };
        limits
    }
    pub(crate) fn task_limits(_state: &crate::AppState) -> WorkerLimits {
        let limits = limits();
        #[cfg(unix)]
        let limits = {
            let mut limits = limits;
            limits.reserved_bytes = nz(task_phase_minimum(
                limits,
                _state.primary_sources.retained_app_path_bytes(),
            )
            .unwrap());
            limits
        };
        limits
    }
    // AC-0320: accepted-session startup reuses the prepublication task floor,
    // while preserving its grant, path, and reservation checks.
    #[cfg(unix)]
    #[test]
    fn prepared_task_startup_computes_once_and_rejects_mismatches() {
        use std::cell::Cell;
        let limits = limits();
        let path_bytes = 321;
        let computations = Cell::new(0);
        let startup = WorkerStartup::prepare_with(limits, true, path_bytes, |_, _| {
            computations.set(computations.get() + 1);
            Some(1)
        })
        .unwrap();
        assert_eq!(computations.get(), 1);
        startup.validate_task_phase(true, path_bytes).unwrap();
        assert_eq!(computations.get(), 1);
        // A revoked grant keeps the safe, already-reserved read-only startup.
        startup.validate_task_phase(false, 0).unwrap();
        assert_eq!(computations.get(), 1);

        let no_task = WorkerStartup::prepare_with(limits, false, 0, |_, _| {
            panic!("read-only startup must not compute the task floor")
        })
        .unwrap();
        assert!(matches!(
            no_task.validate_task_phase(true, path_bytes),
            Err(ReadError::InvalidRequest)
        ));
        assert!(matches!(
            startup.validate_task_phase(true, path_bytes + 1),
            Err(ReadError::InvalidRequest)
        ));
        let too_small = WorkerLimits {
            reserved_bytes: nz(1),
            ..limits
        };
        assert!(matches!(
            WorkerStartup::prepare_with(too_small, true, path_bytes, |_, _| Some(2)),
            Err(ReadError::Limit)
        ));
        assert_eq!(startup.limits.reserved_bytes, limits.reserved_bytes);
    }

    // AC-0320: the test-only compatibility path keeps expensive preparation
    // off the async executor, matching the production prepublication path.
    #[test]
    fn test_startup_preparation_runs_off_async_executor() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            use std::{sync::mpsc, time::Duration};
            let limits = limits();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
            let preparing = tokio::spawn(WorkerStartup::prepare_on_blocking_pool(move || {
                let _ = entered_tx.send(());
                if release_rx.recv_timeout(Duration::from_secs(2)).is_err() {
                    return Err(ReadError::Storage);
                }
                WorkerStartup::prepare(limits, false, 0)
            }));
            entered_rx.await.unwrap();
            let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = heartbeat_tx.send(());
            });
            heartbeat_rx.await.unwrap();
            release_tx.send(()).unwrap();
            preparing.await.unwrap().unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn task_floor_retains_context_during_saved_task_replay() {
        // AC-0327: replay dominates in this witness; preparation cannot mask
        // a missing retained-context owner as it can at today's default caps.
        assert_eq!(task_execution_peak(30, 11, 50), Some(61));
        // Preparation already includes its context and is not charged twice.
        assert_eq!(task_execution_peak(100, 11, 50), Some(100));
        assert_eq!(task_execution_peak(1, usize::MAX, 1), None);
    }
    #[cfg(unix)]
    #[test]
    fn task_floor_includes_retained_read_growth() {
        // AC-0327: inspect the production composition before preparation's
        // larger peak can mask a missing retained owner in the final maximum.
        let limits = limits();
        let (initial, _) = crate::investigations::preparation_workspace::workspace_bytes(
            limits.scope,
            agents::investigation::MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            limits.max_request_frame_bytes.get(),
        )
        .unwrap();
        let growth = crate::investigations::preparation_workspace::retained_read_bytes(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        let (_, retained, _) = task_phase_workspaces(limits, 0).unwrap();
        assert!(retained >= initial + growth);
    }
    #[cfg(unix)]
    #[test]
    fn task_continuation_keeps_original_provider_owners_outside_phase_maximum() {
        // AC-0327: observe the actual retained term before a larger preparation
        // or query peak can hide an omitted original invocation owner.
        let limits = limits();
        let policy = &agents::investigation::INVESTIGATION_V2_POLICY;
        let (frozen, _) = crate::investigations::preparation_workspace::workspace_bytes(
            limits.scope,
            agents::investigation::MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            limits.max_request_frame_bytes.get(),
        )
        .unwrap();
        let reads =
            crate::investigations::preparation_workspace::retained_read_bytes(policy).unwrap();
        let inputs = agents::investigation::InvestigationInput::retained_workspace_bytes(policy)
            .unwrap()
            * 2;
        let (history, _) =
            crate::jobs::investigations::parent_history_projection_workspace().unwrap();
        let caps = llm::bounded::CompletionLimits::new(
            crate::investigations::provider_workspace::completion_limit_values(policy).unwrap(),
        )
        .unwrap();
        let firewall = caps.firewall_workspace_bytes().unwrap();
        let completion = caps.transport_workspace_bytes().unwrap().completion_bytes;
        let (_, retained, execution) = task_phase_workspaces(limits, 0).unwrap();
        assert!(
            retained
                >= frozen
                    + reads
                    + inputs
                    + history
                    + firewall.prepared_bytes
                    + firewall.authorized_bytes
                    + completion
        );
        assert!(execution >= firewall.prepare_peak_bytes);
        assert!(
            execution
                >= agents::investigation::InvestigationInput::construction_workspace_bytes(policy,)
                    .unwrap()
        );
        assert!(
            execution
                >= agents::investigation::InvestigationInput::source_preview_workspace_bytes(
                    policy, &caps,
                )
                .unwrap()
        );
    }
    #[cfg(unix)]
    #[test]
    fn task_source_retained_span_is_outside_execution_phase_maximum() {
        // AC-0327: a larger unrelated phase must not hide retained source text.
        let mut limits = limits();
        limits.evidence.max_span_bytes = std::num::NonZeroU64::new(1).unwrap();
        let small = task_phase_workspaces(limits, 16).unwrap();
        limits.evidence.max_span_bytes = std::num::NonZeroU64::new(128).unwrap();
        let large = task_phase_workspaces(limits, 16).unwrap();
        assert_eq!(large.1 - small.1, 127);
        assert_eq!(small.0, large.0);
        assert_eq!(small.2, large.2);
    }
    #[cfg(unix)]
    #[test]
    fn task_phases_retain_parent_history_before_and_after_preparation() {
        // AC-0327: inspect each actual phase before a larger maximum masks
        // omission of the history that survives the entire worker loop.
        let limits = limits();
        let (initial, preparation) = crate::investigations::preparation_workspace::workspace_bytes(
            limits.scope,
            agents::investigation::MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            limits.max_request_frame_bytes.get(),
        )
        .unwrap();
        let growth = crate::investigations::preparation_workspace::retained_read_bytes(
            &agents::investigation::INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        let (history, projection) =
            crate::jobs::investigations::parent_history_projection_workspace().unwrap();
        let (admitted_preparation, retained, _) = task_phase_workspaces(limits, 0).unwrap();
        assert!(admitted_preparation >= preparation + history);
        assert!(admitted_preparation >= projection);
        assert!(
            admitted_preparation
                >= crate::jobs::investigations::parent_history_acquisition_workspace().unwrap()
        );
        assert!(retained >= initial + growth + history);
    }
    #[cfg(unix)]
    #[test]
    fn worker_floor_accepts_exact_and_refuses_one_below() {
        let mut limits = limits();
        assert_eq!(validate_reservation(limits), Ok(()));
        let required =
            super::super::reservation::worker_bound::WorkerReservation::for_limits(limits)
                .unwrap()
                .required_bytes;
        assert_eq!(limits.reserved_bytes.get(), required);
        limits.reserved_bytes = nz(required - 1);
        assert_eq!(validate_reservation(limits), Err(ReadError::Limit));
    }
    #[test]
    fn direct_worker_request_cap_rejects_before_queueing_and_accepts_exact() {
        tauri::async_runtime::block_on(async {
            let value = serde_json::json!({"text":"\u{0001}é"});
            let bytes = serde_json::to_vec(&value).unwrap().len();
            let (requests, mut incoming) = mpsc::channel::<Request>(1);
            let mut worker = Worker {
                test_exit: None,
                requests,
                max_request_frame_bytes: nz(bytes - 1),
            };
            assert_eq!(
                worker.receive(value.clone()).await.unwrap_err(),
                ReadError::Limit
            );
            assert!(matches!(
                incoming.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            worker.max_request_frame_bytes = nz(bytes);
            let expected = value.clone();
            let responder = tauri::async_runtime::spawn(async move {
                let request = incoming.recv().await.unwrap();
                assert_eq!(request.value, expected);
                request.reply.send(Ok(DispatchAction::Ignore)).unwrap();
            });
            let response = worker.receive(value).await;
            responder.await.unwrap();
            assert_eq!(response.unwrap(), DispatchAction::Ignore);
        });
    }
    #[test]
    fn mcp_worker_uses_real_app_stores_and_stops_after_activation_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        // Avoid reading the installed application's optional registry directory.
        context.config_mut().identifier = format!(
            "test.cartograph.{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let app =
            crate::test_app::TestApp::new(tauri::test::mock_builder().build(context).unwrap());
        app.manage(crate::registered_source_tests::app_state(dir.path()));
        let budget = WorkBudget::new(WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: super::tests::limits().reserved_bytes,
            max_cumulative_read_bytes: 10_000,
        });
        tauri::async_runtime::block_on(async {
            let mut worker = Worker::start(
                app.handle().clone(),
                "fixture-worker".into(),
                spec::ExportMode::VerifiedOnly,
                limits(),
                budget.clone(),
            )
            .await
            .unwrap();
            assert!(budget.try_acquire(nz(1), 0).is_err());
            let response = worker.receive(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap();
            assert!(
                matches!(response, DispatchAction::Respond(value) if value.get("result").is_some())
            );
            budget.revoke();
            assert_eq!(
                worker
                    .receive(serde_json::json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
                    .await
                    .unwrap_err(),
                ReadError::Limit
            );
        });
    }
}

#[cfg(all(test, unix))]
mod native_tests {
    use super::*;
    use mcp_local::{
        connection::Connection,
        serve::{ServeError, serve_connection},
        work_budget::WorkLimits,
    };
    use mcp_wire::capability::{Capability, LOCAL_PROTOCOL};
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn mcp_native_socket_serves_real_app_catalog_and_rejects_mutation() {
        // AC-0288/0289 native fixture, not independent installed-client acceptance.
        let dir = tempfile::tempdir().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = format!(
            "test.cartograph.{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let app =
            crate::test_app::TestApp::new(tauri::test::mock_builder().build(context).unwrap());
        app.manage(crate::registered_source_tests::app_state(dir.path()));
        let handle = app.handle().clone();
        let budget = WorkBudget::new(WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: super::tests::limits().reserved_bytes,
            max_cumulative_read_bytes: 100_000,
        });
        tauri::async_runtime::block_on(async move {
            let (server, client) = tokio::net::UnixStream::pair().unwrap();
            let connection = |stream| {
                Connection::from_stream(stream, nz(65536), nz(65536), Duration::from_secs(2))
                    .unwrap()
            };
            let server = connection(server);
            let mut client = connection(client);
            let secret = Arc::new(Capability::generate().unwrap());
            let server_secret = Arc::clone(&secret);
            let (revoke, revoked) = tokio::sync::watch::channel(false);
            let server_budget = budget.clone();
            let task = tauri::async_runtime::spawn(async move {
                serve_connection(
                    server,
                    &server_secret,
                    Duration::from_secs(10),
                    revoked,
                    || async move {
                        Worker::start(
                            handle,
                            "native-fixture".into(),
                            spec::ExportMode::VerifiedOnly,
                            super::tests::limits(),
                            server_budget,
                        )
                        .await
                        .map_err(|_| ServeError::Application)
                    },
                )
                .await
            });
            client.write(&json!({"protocol":LOCAL_PROTOCOL,"capability":secret.expose_for_local_transport()})).await.unwrap();
            assert!(mcp_wire::capability::is_acknowledgement(
                &client.read().await.unwrap().unwrap()
            ));
            client.write(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-fixture","version":"1"}}})).await.unwrap();
            assert!(
                client
                    .read()
                    .await
                    .unwrap()
                    .unwrap()
                    .get("result")
                    .is_some()
            );
            client
                .write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .unwrap();
            client
                .write(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
                .await
                .unwrap();
            let tools = client.read().await.unwrap().unwrap();
            // AC-1809: the four complete reads, cartograph_query and evidence.
            assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
            client.write(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cartograph_capabilities","arguments":{}}})).await.unwrap();
            let capabilities = client.read().await.unwrap().unwrap();
            let basis = capabilities["result"]["structuredContent"]["basis_id"]
                .as_str()
                .unwrap();
            client.write(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cartograph_artifacts","arguments":{"session_id":"native-fixture","basis_id":basis}}})).await.unwrap();
            let catalog = client.read().await.unwrap().unwrap();
            assert_eq!(
                catalog["result"]["structuredContent"]["artifacts"]
                    .as_array()
                    .unwrap()
                    .len(),
                crate::mcp_reads::artifacts::readable_catalog().len()
            );
            client.write(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ingest","arguments":{}}})).await.unwrap();
            assert!(client.read().await.unwrap().unwrap().get("error").is_some());
            budget.revoke();
            revoke.send(true).unwrap();
            assert_eq!(task.await.unwrap(), Err(ServeError::Revoked));
            assert!(client.read().await.unwrap().is_none());
        });
        assert!(
            app.state::<crate::AppState>()
                .graph
                .lock()
                .unwrap()
                .read_snapshot()
                .unwrap()
                .0
                .is_empty()
        );
    }
}

#[cfg(all(test, unix))]
mod scoped_tests;

#[cfg(all(test, unix))]
mod scoped_measure_tests;

#[cfg(all(test, unix))]
mod protocol_tests;
