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
    /// Called only by the authenticated connection factory. Activation policy
    /// and numeric ceilings remain host responsibilities, never client arguments.
    pub(crate) async fn start<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        mode: spec::ExportMode,
        limits: WorkerLimits,
        budget: WorkBudget,
    ) -> Result<Self, ReadError> {
        Self::start_with_tasks(app, session_id, mode, limits, budget, None).await
    }

    /// Internal authenticated factory path. Public activation still calls start
    /// without tasks until the complete detached-execution admission lands.
    pub(crate) async fn start_with_tasks<R: tauri::Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        mode: spec::ExportMode,
        limits: WorkerLimits,
        budget: WorkBudget,
        tasks: Option<agents::investigation::McpTaskSession>,
    ) -> Result<Self, ReadError> {
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
            if tasks
                .as_ref()
                .is_some_and(|session| session.with_grant(|_, _| ()).is_ok())
                && limits.reserved_bytes.get()
                    < task_preparation_minimum(limits).ok_or(ReadError::Limit)?
            {
                return Err(ReadError::Limit);
            }
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
                                        limits.scope,
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
                                        let session = ReadSession::acquire(
                                            &state.graph,
                                            &state.decisions,
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

fn response_cap(
    tool: ReadTool,
    arguments: &serde_json::Map<String, Value>,
    limits: WorkerLimits,
) -> usize {
    let requested = match (limits.acquisition_mode, tool) {
        (AcquisitionMode::Complete, ReadTool::Discover) => arguments
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

/// Enforced preparation floor only. Store replay, later reads and provider
/// phases still need composition before public task activation can be enabled.
#[cfg(unix)]
pub(crate) fn task_preparation_minimum(limits: WorkerLimits) -> Option<usize> {
    let (_, preparation) = crate::investigations::preparation_workspace::workspace_bytes(
        limits.scope,
        agents::investigation::MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
        limits.max_request_frame_bytes.get(),
    )?;
    required_reservation(limits)?
        .checked_add(preparation.checked_mul(crate::jobs::investigations::MAX_LIVE_TASKS)?)
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
    pub(crate) fn task_limits() -> WorkerLimits {
        let limits = limits();
        #[cfg(unix)]
        let limits = {
            let mut limits = limits;
            limits.reserved_bytes = nz(task_preparation_minimum(limits).unwrap());
            limits
        };
        limits
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
        let app = tauri::test::mock_builder().build(context).unwrap();
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
        let app = tauri::test::mock_builder().build(context).unwrap();
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
            assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);
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
                spec::artifact_catalog().len()
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
