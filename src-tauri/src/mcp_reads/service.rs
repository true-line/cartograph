//! Disabled-by-default application ownership. No settings persistence or startup
//! activation: a human-facing adapter must supply reviewed policy explicitly.
use super::{
    ReadError,
    worker::{Worker, WorkerLimits, WorkerStartup},
};
use mcp_local::{
    activation::{Activation, ActivationLimits, ActivationStatus},
    serve::ServeError,
    work_budget::{WorkBudget, WorkLimits},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
pub(crate) struct Policy {
    pub transport: ActivationLimits,
    pub worker: WorkerLimits,
    pub aggregate: WorkLimits,
    pub export_mode: spec::ExportMode,
}
impl Policy {
    /// Hold transport payload capacity for the full activation independently of
    /// workers: responses and unauthenticated peers can outlive worker permits.
    fn worker_budget(&self) -> Result<WorkLimits, ReadError> {
        let transport = super::reservation::TransportReservation::for_limits(self.transport)
            .ok_or(ReadError::InvalidRequest)?;
        let remaining = self
            .aggregate
            .max_reserved_bytes
            .get()
            .checked_sub(transport.activation_bytes)
            .and_then(std::num::NonZeroUsize::new)
            .ok_or(ReadError::InvalidRequest)?;
        Ok(WorkLimits {
            max_reserved_bytes: remaining,
            ..self.aggregate
        })
    }

    /// Validate every activation entry point before state or endpoint mutation.
    /// Raw numeric fields retain the positive bounds enforced by IPC decoding.
    pub(crate) fn validate(&self) -> Result<(), ReadError> {
        use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
        use std::time::Instant;
        let t = self.transport;
        let w = self.worker;
        let g = w.graph;
        let c = w.coverage;
        let d = w.dispatch;
        let e = w.evidence;
        let a = self.aggregate;
        let worker_budget = self.worker_budget()?;
        let required = super::worker::required_reservation(w).ok_or(ReadError::InvalidRequest)?;
        if w.max_request_frame_bytes != t.read_frame_bytes
            || w.reserved_bytes.get() < required
            || w.scope.max_sources.get() > context_hub::coverage::MAX_COVERAGE_REPOS
            || g.max_bytes == 0
            || g.max_bytes > SnapshotReadLimits::MAX_BYTES
            || g.max_rows == 0
            || g.max_rows > SnapshotReadLimits::MAX_ROWS
            || g.max_depth == 0
            || g.max_depth > SnapshotReadLimits::MAX_DEPTH
            || g.max_values == 0
            || g.max_values > SnapshotReadLimits::MAX_VALUES
            || c.max_records == 0
            || c.max_records > CoverageReadLimits::MAX_RECORDS
            || c.max_bytes == 0
            || c.max_bytes > CoverageReadLimits::MAX_BYTES
            || t.frame_deadline.is_zero()
            || t.session_lifetime.is_zero()
            || t.frame_deadline > t.session_lifetime
            || Instant::now().checked_add(t.frame_deadline).is_none()
            || Instant::now().checked_add(t.session_lifetime).is_none()
            || t.frame_deadline.as_nanos().div_ceil(1_000_000) > u128::from(u64::MAX)
            || t.session_lifetime.as_nanos().div_ceil(1_000_000) > u128::from(u64::MAX)
            || d.max_response_bytes > t.write_frame_bytes
            || d.trace.max_flows == 0
            || d.trace.max_hops == 0
            || d.trace.max_output_bytes == 0
            || d.trace.max_output_bytes > d.max_response_bytes.get()
            || d.compile.max_bytes == 0
            || d.compile.max_bytes > d.max_response_bytes.get()
            || d.compile.adr.max_rows == 0
            || d.compile.adr.max_output_bytes == 0
            || d.compile.adr.max_output_bytes > d.compile.max_bytes
            || e.max_span_bytes > e.max_file_validation_bytes
            || e.max_file_validation_bytes.get() > a.max_cumulative_read_bytes
            || w.reserved_bytes > worker_budget.max_reserved_bytes
            || a.max_workers > t.max_connections
        {
            return Err(ReadError::InvalidRequest);
        }
        Ok(())
    }
}
mod lifecycle;

struct Active {
    tasks: agents::investigation::McpTaskActivation,
    policy: Policy,
    activation: Activation,
    budget: WorkBudget,
    transport: ActivationLimits,
}
type CleanupCompletion = tokio::sync::watch::Receiver<Option<Result<(), ReadError>>>;
#[derive(Default)]
struct State {
    active: Option<Active>,
    draining: Option<WorkBudget>,
    stopping: bool,
    shutdown: bool,
    cleanup_failed: bool,
    cleanup: Option<CleanupCompletion>,
    rotation: Option<Arc<()>>,
}
#[derive(Default)]
pub(crate) struct Service(Arc<Mutex<State>>);
impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            if let Some(active) = state.active.take() {
                active.tasks.revoke();
                active.budget.revoke();
            }
            if let Some(budget) = &state.draining {
                budget.revoke();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Disabled,
    Running,
    Stopped,
    Draining,
    CleanupFailed,
}

impl State {
    fn status(&self) -> Result<Status, ReadError> {
        let state = self;
        if state.cleanup_failed {
            return Ok(Status::CleanupFailed);
        }
        if let Some(active) = &state.active {
            return Ok(match active.activation.status() {
                ActivationStatus::Running => Status::Running,
                ActivationStatus::Stopped | ActivationStatus::Failed(_) => Status::Stopped,
            });
        }
        if state.stopping
            || state
                .draining
                .as_ref()
                .map(|budget| budget.is_idle())
                .transpose()
                .map_err(|_| ReadError::Storage)?
                == Some(false)
        {
            Ok(Status::Draining)
        } else {
            Ok(Status::Disabled)
        }
    }
}

impl Service {
    pub(crate) fn status(&self) -> Result<Status, ReadError> {
        self.0.lock().map_err(|_| ReadError::Storage)?.status()
    }

    /// Generate a JSON client entry for an existing host-selected executable.
    /// No shell quoting, credentials, launch, config-file writes or installation.
    /// The endpoint is activation-specific: disable/restart requires fresh config.
    pub(crate) fn client_configuration(
        &self,
        relay: &Path,
    ) -> Result<serde_json::Value, ReadError> {
        use std::os::unix::fs::PermissionsExt;
        if !relay.is_absolute() {
            return Err(ReadError::InvalidRequest);
        }
        let metadata = std::fs::symlink_metadata(relay).map_err(|_| ReadError::Storage)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(ReadError::InvalidRequest);
        }
        let command = relay.to_str().ok_or(ReadError::InvalidRequest)?;
        let state = self.0.lock().map_err(|_| ReadError::Storage)?;
        let active = state.active.as_ref().ok_or(ReadError::StaleBasis)?;
        if active.activation.status() != ActivationStatus::Running {
            return Err(ReadError::StaleBasis);
        }
        let record = active
            .activation
            .record_path()
            .to_str()
            .ok_or(ReadError::Storage)?;
        let cap = active
            .transport
            .read_frame_bytes
            .max(active.transport.write_frame_bytes);
        let millis = active
            .transport
            .frame_deadline
            .as_nanos()
            .div_ceil(1_000_000);
        let millis = u64::try_from(millis).map_err(|_| ReadError::InvalidRequest)?;
        Ok(serde_json::json!({"mcpServers":{"cartograph":{
            "command":command,
            "args":["--record",record,"--max-frame-bytes",cap.to_string(),"--timeout-ms",millis.to_string()]
        }}}))
    }

    /// Requires a runtime and a trusted runtime parent. The record is returned
    /// for explicit relay configuration; no provider or external client starts.
    pub(crate) fn enable<R: tauri::Runtime>(
        &self,
        app: tauri::AppHandle<R>,
        parent: &Path,
        policy: Policy,
    ) -> Result<PathBuf, ReadError> {
        self.enable_selected(app, parent, policy, None)
    }

    /// Owner IPC entry: admission is checked before publishing an endpoint.
    pub(crate) fn enable_tasks<R: tauri::Runtime>(
        &self,
        app: tauri::AppHandle<R>,
        parent: &Path,
        policy: Policy,
        client: agents::investigation::McpClientId,
        maximum_identities: std::num::NonZeroUsize,
    ) -> Result<PathBuf, ReadError> {
        use tauri::Manager;
        policy.validate()?;
        let state = app.state::<crate::AppState>();
        let registered = state
            .jobs
            .lock()
            .map_err(|_| ReadError::Storage)?
            .mcp_client_exists(&client)
            .map_err(|_| ReadError::Storage)?;
        if !registered {
            return Err(ReadError::InvalidRequest);
        }
        // Principals are durable; do not hold the store mutex during endpoint
        // startup or while acquiring Service/grant lifecycle locks.
        let tasks = agents::investigation::McpTaskActivation::granted(client, maximum_identities);
        Self::enable_transition(&self.0, app.clone(), parent, policy, tasks, None)
    }

    // Host-only boundary. No transport argument selects a principal or grant.
    fn enable_selected<R: tauri::Runtime>(
        &self,
        app: tauri::AppHandle<R>,
        parent: &Path,
        policy: Policy,
        selection: Option<(
            &crate::jobs::JobStore,
            agents::investigation::McpClientId,
            std::num::NonZeroUsize,
        )>,
    ) -> Result<PathBuf, ReadError> {
        let tasks = match selection {
            None => Default::default(),
            Some((store, client, capacity)) => {
                if !store
                    .mcp_client_exists(&client)
                    .map_err(|_| ReadError::Storage)?
                {
                    return Err(ReadError::InvalidRequest);
                }
                agents::investigation::McpTaskActivation::granted(client, capacity)
            }
        };
        Self::enable_transition(&self.0, app, parent, policy, tasks, None)
    }

    fn enable_transition<R: tauri::Runtime>(
        shared: &Mutex<State>,
        app: tauri::AppHandle<R>,
        parent: &Path,
        policy: Policy,
        tasks: agents::investigation::McpTaskActivation,
        rotation: Option<&Arc<()>>,
    ) -> Result<PathBuf, ReadError> {
        policy.validate()?;
        // Validate and retain the exact task-worker floor before publishing an
        // endpoint. The accepted-session factory reuses this prepared config.
        let task_session = tasks
            .owner_snapshot()
            .map_err(|_| ReadError::Storage)?
            .map(|_| tasks.session());
        let task_startup = if task_session.is_some() {
            use tauri::Manager;
            let app_state = app
                .try_state::<crate::AppState>()
                .ok_or(ReadError::Storage)?;
            WorkerStartup::prepare(
                policy.worker,
                true,
                app_state.primary_sources.retained_app_path_bytes(),
            )?
        } else {
            WorkerStartup::prepare(policy.worker, false, 0)?
        };
        let mut state = shared.lock().map_err(|_| ReadError::Storage)?;
        match (&state.rotation, rotation) {
            (None, None) => {}
            (Some(current), Some(expected)) if Arc::ptr_eq(current, expected) => {}
            _ => return Err(ReadError::InvalidRequest),
        }
        if state.active.is_some() || state.stopping || state.shutdown || state.cleanup_failed {
            return Err(ReadError::InvalidRequest);
        }
        if state
            .draining
            .as_ref()
            .map(|budget| budget.is_idle())
            .transpose()
            .map_err(|_| ReadError::Storage)?
            == Some(false)
        {
            return Err(ReadError::Limit);
        }
        // Never replace the ledger of an old activation until its actual
        // synchronous workers have all exited, even if their sockets are gone.
        state.draining = None;
        state.cleanup = None;
        let budget = WorkBudget::new(policy.worker_budget()?);
        let factory_budget = budget.clone();
        let activation = Activation::start(parent, policy.transport, move || {
            let app = app.clone();
            let budget = factory_budget.clone();
            let tasks = task_session.clone();
            let startup = task_startup;
            async move {
                let id = mcp_wire::capability::Capability::generate()
                    .map_err(|_| ServeError::Application)?;
                Worker::start_with_startup(
                    app,
                    id.expose_for_local_transport(),
                    policy.export_mode,
                    startup,
                    budget,
                    tasks,
                )
                .await
                .map_err(|_| ServeError::Application)
            }
        })
        .map_err(|_| ReadError::Storage)?;
        let record = activation.record_path().to_path_buf();
        state.active = Some(Active {
            tasks,
            policy,
            activation,
            budget,
            transport: policy.transport,
        });
        state.rotation = None;
        Ok(record)
    }

    /// Revoke admission before awaiting transport cleanup. A concurrent enable
    /// cannot bypass this transition or any still-running synchronous worker.
    pub(crate) async fn disable(&self) -> Result<(), ReadError> {
        self.stop(false).await
    }

    /// Permanently reject queued enable work before joining transport cleanup.
    /// App exit joins any cleanup already started by a human disable request.
    pub(crate) async fn shutdown(&self) -> Result<(), ReadError> {
        self.stop(true).await
    }

    async fn stop(&self, shutdown: bool) -> Result<(), ReadError> {
        Self::await_cleanup(self.begin_stop(shutdown)?).await
    }

    async fn await_cleanup(completion: Option<CleanupCompletion>) -> Result<(), ReadError> {
        let Some(mut completion) = completion else {
            return Ok(());
        };
        loop {
            if let Some(result) = *completion.borrow_and_update() {
                return result;
            }
            completion.changed().await.map_err(|_| ReadError::Storage)?;
        }
    }

    fn begin_stop(&self, shutdown: bool) -> Result<Option<CleanupCompletion>, ReadError> {
        let mut state = self.0.lock().map_err(|_| ReadError::Storage)?;
        state.shutdown |= shutdown;
        state.rotation = None;
        self.begin_stop_locked(&mut state)
    }

    fn begin_stop_locked(&self, state: &mut State) -> Result<Option<CleanupCompletion>, ReadError> {
        if let Some(completion) = &state.cleanup {
            return Ok(Some(completion.clone()));
        }
        let Some(active) = state.active.take() else {
            return Ok(None);
        };
        active.tasks.revoke();
        active.budget.revoke();
        state.draining = Some(active.budget.clone());
        state.stopping = true;
        let (completed, completion) = tokio::sync::watch::channel(None);
        state.cleanup = Some(completion.clone());
        let shared = Arc::clone(&self.0);
        // Ownership and the completion channel are established synchronously,
        // before this caller can be cancelled at its first await. The inner
        // join also turns unexpected cleanup-task failure into persistent state.
        tauri::async_runtime::spawn(async move {
            let result = tauri::async_runtime::spawn(async move {
                active
                    .activation
                    .disable()
                    .await
                    .map_err(|_| ReadError::Storage)
            })
            .await
            .unwrap_or(Err(ReadError::Storage));
            let result = match shared.lock() {
                Ok(mut state) => {
                    state.stopping = false;
                    state.cleanup_failed = result.is_err();
                    result
                }
                Err(_) => Err(ReadError::Storage),
            };
            completed.send_replace(Some(result));
        });
        Ok(Some(completion))
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests {
    use super::*;
    use std::{num::NonZeroUsize, time::Duration};
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }
    pub(super) fn policy() -> Policy {
        let mut policy = Policy {
            transport: ActivationLimits {
                max_connections: nz(2),
                read_frame_bytes: nz(65536),
                write_frame_bytes: nz(65536),
                frame_deadline: Duration::from_secs(2),
                session_lifetime: Duration::from_secs(10),
            },
            worker: crate::mcp_reads::worker::tests::limits(),
            aggregate: WorkLimits {
                max_workers: nz(1),
                max_reserved_bytes: nz(1_000_000),
                max_cumulative_read_bytes: 100_000,
            },
            export_mode: spec::ExportMode::VerifiedOnly,
        };
        let baseline =
            super::super::reservation::TransportReservation::for_limits(policy.transport)
                .unwrap()
                .activation_bytes;
        policy.aggregate.max_reserved_bytes = nz(baseline
            .checked_add(policy.worker.reserved_bytes.get())
            .unwrap());
        policy
    }
    #[test]
    fn direct_policy_validation_rejects_every_ingress_bound_before_endpoint_mutation() {
        use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
        use std::num::NonZeroU64;
        let invalid: &[fn(&mut Policy)] = &[
            |p| p.worker.graph.max_bytes = 0,
            |p| p.worker.graph.max_rows = 0,
            |p| p.worker.graph.max_depth = 0,
            |p| p.worker.graph.max_values = 0,
            |p| p.worker.graph.max_bytes = SnapshotReadLimits::MAX_BYTES + 1,
            |p| p.worker.graph.max_rows = SnapshotReadLimits::MAX_ROWS + 1,
            |p| p.worker.graph.max_depth = SnapshotReadLimits::MAX_DEPTH + 1,
            |p| p.worker.graph.max_values = SnapshotReadLimits::MAX_VALUES + 1,
            |p| p.worker.coverage.max_records = 0,
            |p| p.worker.coverage.max_bytes = 0,
            |p| p.worker.coverage.max_records = CoverageReadLimits::MAX_RECORDS + 1,
            |p| p.worker.coverage.max_bytes = CoverageReadLimits::MAX_BYTES + 1,
            |p| p.worker.scope.max_sources = nz(context_hub::coverage::MAX_COVERAGE_REPOS + 1),
            |p| p.transport.frame_deadline = Duration::ZERO,
            |p| p.transport.session_lifetime = Duration::ZERO,
            |p| p.transport.frame_deadline = p.transport.session_lifetime + Duration::from_nanos(1),
            |p| p.transport.session_lifetime = Duration::MAX,
            |p| {
                p.transport.frame_deadline = Duration::MAX;
                p.transport.session_lifetime = Duration::MAX;
            },
            |p| p.worker.dispatch.max_response_bytes = nz(p.transport.write_frame_bytes.get() + 1),
            |p| p.worker.dispatch.trace.max_flows = 0,
            |p| p.worker.dispatch.trace.max_hops = 0,
            |p| p.worker.dispatch.trace.max_output_bytes = 0,
            |p| {
                p.worker.dispatch.trace.max_output_bytes =
                    p.worker.dispatch.max_response_bytes.get() + 1
            },
            |p| p.worker.dispatch.compile.max_bytes = 0,
            |p| {
                p.worker.dispatch.compile.max_bytes = p.worker.dispatch.max_response_bytes.get() + 1
            },
            |p| p.worker.dispatch.compile.adr.max_rows = 0,
            |p| p.worker.dispatch.compile.adr.max_output_bytes = 0,
            |p| {
                p.worker.dispatch.compile.adr.max_output_bytes =
                    p.worker.dispatch.compile.max_bytes + 1
            },
            |p| {
                p.worker.evidence.max_span_bytes =
                    NonZeroU64::new(p.worker.evidence.max_file_validation_bytes.get() + 1).unwrap()
            },
            |p| {
                p.worker.evidence.max_file_validation_bytes =
                    NonZeroU64::new(p.aggregate.max_cumulative_read_bytes + 1).unwrap()
            },
            |p| p.aggregate.max_cumulative_read_bytes = 0,
            |p| p.transport.read_frame_bytes = nz(usize::MAX),
            |p| p.transport.write_frame_bytes = nz(usize::MAX),
            |p| p.transport.max_connections = nz(usize::MAX),
            |p| {
                p.aggregate.max_reserved_bytes = nz(
                    super::super::reservation::TransportReservation::for_limits(p.transport)
                        .unwrap()
                        .activation_bytes,
                )
            },
            |p| {
                p.worker.reserved_bytes =
                    nz(p.worker_budget().unwrap().max_reserved_bytes.get() + 1)
            },
            |p| p.worker.reserved_bytes = nz(p.aggregate.max_reserved_bytes.get() + 1),
            |p| p.aggregate.max_workers = nz(p.transport.max_connections.get() + 1),
        ];
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        assert!(policy().validate().is_ok());
        for (index, mutate) in invalid.iter().enumerate() {
            let mut candidate = policy();
            mutate(&mut candidate);
            assert_eq!(
                candidate.validate(),
                Err(ReadError::InvalidRequest),
                "case {index}"
            );
            // No async runtime is entered: failure must precede transport startup.
            assert_eq!(
                service
                    .enable(app.handle().clone(), parent.path(), candidate)
                    .unwrap_err(),
                ReadError::InvalidRequest,
                "case {index}"
            );
            assert_eq!(service.status().unwrap(), Status::Disabled);
            assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn policy_enforces_exact_worker_floor_and_transport_frame_binding() {
        let mut candidate = policy();
        assert_eq!(candidate.validate(), Ok(()));
        candidate.worker.reserved_bytes = nz(candidate.worker.reserved_bytes.get() - 1);
        assert_eq!(candidate.validate(), Err(ReadError::InvalidRequest));
        candidate = policy();
        candidate.worker.max_request_frame_bytes =
            nz(candidate.transport.read_frame_bytes.get() - 1);
        assert_eq!(candidate.validate(), Err(ReadError::InvalidRequest));
    }
    #[test]
    fn shared_policy_accepts_exact_existing_numeric_boundaries() {
        use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
        let mut candidate = policy();
        candidate.worker.graph = SnapshotReadLimits {
            max_bytes: SnapshotReadLimits::MAX_BYTES,
            max_rows: SnapshotReadLimits::MAX_ROWS,
            max_depth: SnapshotReadLimits::MAX_DEPTH,
            max_values: SnapshotReadLimits::MAX_VALUES,
        };
        candidate.worker.coverage = CoverageReadLimits {
            max_records: CoverageReadLimits::MAX_RECORDS,
            max_bytes: CoverageReadLimits::MAX_BYTES,
        };
        candidate.worker.scope.max_sources = nz(context_hub::coverage::MAX_COVERAGE_REPOS);
        candidate.transport.frame_deadline = candidate.transport.session_lifetime;
        candidate.aggregate.max_workers = candidate.transport.max_connections;
        candidate.worker.evidence.max_span_bytes =
            candidate.worker.evidence.max_file_validation_bytes;
        candidate.aggregate.max_cumulative_read_bytes =
            candidate.worker.evidence.max_file_validation_bytes.get();
        candidate.worker.reserved_bytes = nz(
            super::super::reservation::worker_bound::WorkerReservation::for_limits(
                candidate.worker,
            )
            .unwrap()
            .required_bytes,
        );
        let transport =
            super::super::reservation::TransportReservation::for_limits(candidate.transport)
                .unwrap()
                .activation_bytes;
        candidate.aggregate.max_reserved_bytes = nz(transport
            .checked_add(candidate.worker.reserved_bytes.get())
            .unwrap());
        assert_eq!(candidate.validate(), Ok(()));
    }

    #[test]
    fn mcp_service_starts_disabled_and_cleans_record_before_reenable() {
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        assert_eq!(service.status().unwrap(), Status::Disabled);
        tauri::async_runtime::block_on(async {
            let first = service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            assert!(first.is_file());
            assert_eq!(service.status().unwrap(), Status::Running);
            // The live worker ledger excludes the transport reservation even
            // when no authenticated worker or response is currently present.
            let budget = service
                .0
                .lock()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .budget
                .clone();
            let worker_bytes = policy().worker.reserved_bytes.get();
            assert!(budget.try_acquire(nz(worker_bytes + 1), 0).is_err());
            let permit = budget.try_acquire(nz(worker_bytes), 0).unwrap();
            drop(permit);
            assert!(
                service
                    .enable(app.handle().clone(), parent.path(), policy())
                    .is_err()
            );
            service.disable().await.unwrap();
            assert!(!first.exists());
            assert_eq!(service.status().unwrap(), Status::Disabled);
            let second = service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            assert_ne!(first, second);
            service.disable().await.unwrap();
            assert!(!second.exists());
        });
    }
    #[test]
    fn mcp_service_refuses_new_activation_until_old_worker_releases_budget() {
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        let budget = WorkBudget::new(policy().aggregate);
        let still_running = budget.try_acquire(nz(100), 0).unwrap();
        budget.revoke();
        service.0.lock().unwrap().draining = Some(budget);
        assert_eq!(service.status().unwrap(), Status::Draining);
        tauri::async_runtime::block_on(async {
            assert_eq!(
                service
                    .enable(app.handle().clone(), parent.path(), policy())
                    .unwrap_err(),
                ReadError::Limit
            );
            drop(still_running);
            assert_eq!(service.status().unwrap(), Status::Disabled);
            service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            service.disable().await.unwrap();
        });
    }

    #[test]
    fn mcp_shutdown_latches_before_queued_enable_and_joins_existing_cleanup() {
        // AC-0288: exit cannot be bypassed by activation queued off the UI thread.
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        let (completed, completion) = tokio::sync::watch::channel(None);
        {
            let mut state = service.0.lock().unwrap();
            state.stopping = true;
            state.cleanup = Some(completion);
        }
        let mut shutdown = std::pin::pin!(service.shutdown());
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(shutdown.as_mut().poll(&mut context), Poll::Pending);
        assert!(service.0.lock().unwrap().shutdown);
        assert_eq!(
            service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap_err(),
            ReadError::InvalidRequest
        );
        {
            let mut state = service.0.lock().unwrap();
            state.stopping = false;
        }
        completed.send_replace(Some(Ok(())));
        assert_eq!(shutdown.as_mut().poll(&mut context), Poll::Ready(Ok(())));
        assert_eq!(
            service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap_err(),
            ReadError::InvalidRequest
        );
    }

    #[test]
    fn mcp_shutdown_joins_cleanup_after_disable_waiter_is_cancelled() {
        // AC-0288: cancellation drops the waiter, never the cleanup owner.
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        tauri::async_runtime::block_on(async {
            let record = service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            {
                let mut disable = std::pin::pin!(service.disable());
                let mut context = Context::from_waker(Waker::noop());
                // Either result is allowed: completion may win on another worker.
                assert!(matches!(
                    disable.as_mut().poll(&mut context),
                    Poll::Pending | Poll::Ready(Ok(()))
                ));
            }
            service.shutdown().await.unwrap();
            assert!(!record.exists());
            assert_eq!(service.status().unwrap(), Status::Disabled);
            assert!(service.0.lock().unwrap().shutdown);
        });
    }

    #[test]
    fn mcp_cleanup_failure_persists_and_denies_reactivation() {
        // AC-0288: a record replacement never becomes verified disabled state.
        let parent = tempfile::tempdir().unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        tauri::async_runtime::block_on(async {
            let record = service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            std::fs::rename(&record, record.with_file_name("original-record")).unwrap();
            std::fs::write(&record, b"replacement").unwrap();
            assert_eq!(service.disable().await.unwrap_err(), ReadError::Storage);
            assert_eq!(service.status().unwrap(), Status::CleanupFailed);
            assert!(record.exists());
            assert_eq!(
                service
                    .enable(app.handle().clone(), parent.path(), policy())
                    .unwrap_err(),
                ReadError::InvalidRequest
            );
            assert_eq!(service.disable().await.unwrap_err(), ReadError::Storage);
            assert_eq!(service.shutdown().await.unwrap_err(), ReadError::Storage);
            assert_eq!(service.status().unwrap(), Status::CleanupFailed);
        });
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod configuration_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn mcp_configuration_preserves_paths_without_exporting_capabilities_or_stale_records() {
        let parent = tempfile::tempdir().unwrap();
        let relay = parent.path().join("relay with spaces 🧭");
        std::fs::write(&relay, b"fixture only, never executed").unwrap();
        std::fs::set_permissions(&relay, std::fs::Permissions::from_mode(0o700)).unwrap();
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        let service = Service::default();
        assert!(service.client_configuration(&relay).is_err());
        tauri::async_runtime::block_on(async {
            let record = service
                .enable(app.handle().clone(), parent.path(), super::tests::policy())
                .unwrap();
            let private: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
            let config = service.client_configuration(&relay).unwrap();
            let entry = &config["mcpServers"]["cartograph"];
            assert_eq!(entry["command"], relay.to_str().unwrap());
            assert_eq!(entry["args"][1], record.to_str().unwrap());
            assert_eq!(entry["args"].as_array().unwrap().len(), 6);
            assert!(
                !serde_json::to_string(&config)
                    .unwrap()
                    .contains(private["capability"].as_str().unwrap())
            );
            service.disable().await.unwrap();
            assert_eq!(
                service.client_configuration(&relay).unwrap_err(),
                ReadError::StaleBasis
            );
        });
        std::fs::set_permissions(&relay, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            service.client_configuration(&relay).unwrap_err(),
            ReadError::InvalidRequest
        );
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "service/task_session_tests.rs"]
mod task_session_tests;
