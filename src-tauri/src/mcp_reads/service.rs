//! Disabled-by-default application ownership. No settings persistence or startup
//! activation: a human-facing adapter must supply reviewed policy explicitly.
use super::{
    ReadError,
    worker::{Worker, WorkerLimits},
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
struct Active {
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
}
#[derive(Default)]
pub(crate) struct Service(Arc<Mutex<State>>);
impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            if let Some(active) = state.active.take() {
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

impl Service {
    pub(crate) fn status(&self) -> Result<Status, ReadError> {
        let state = self.0.lock().map_err(|_| ReadError::Storage)?;
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
        if policy.worker.dispatch.max_response_bytes > policy.transport.write_frame_bytes
            || policy.worker.reserved_bytes > policy.aggregate.max_reserved_bytes
            || policy.aggregate.max_workers > policy.transport.max_connections
        {
            return Err(ReadError::InvalidRequest);
        }
        let mut state = self.0.lock().map_err(|_| ReadError::Storage)?;
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
        let budget = WorkBudget::new(policy.aggregate);
        let factory_budget = budget.clone();
        let activation = Activation::start(parent, policy.transport, move || {
            let app = app.clone();
            let budget = factory_budget.clone();
            async move {
                let id = mcp_wire::capability::Capability::generate()
                    .map_err(|_| ServeError::Application)?;
                Worker::start(
                    app,
                    id.expose_for_local_transport(),
                    policy.export_mode,
                    policy.worker,
                    budget,
                )
                .await
                .map_err(|_| ServeError::Application)
            }
        })
        .map_err(|_| ReadError::Storage)?;
        let record = activation.record_path().to_path_buf();
        state.active = Some(Active {
            activation,
            budget,
            transport: policy.transport,
        });
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
        let Some(mut completion) = self.begin_stop(shutdown)? else {
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
        if let Some(completion) = &state.cleanup {
            return Ok(Some(completion.clone()));
        }
        let Some(active) = state.active.take() else {
            return Ok(None);
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{num::NonZeroUsize, time::Duration};
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }
    pub(super) fn policy() -> Policy {
        Policy {
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
        }
    }
    #[test]
    fn mcp_service_starts_disabled_and_cleans_record_before_reenable() {
        let parent = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let service = Service::default();
        assert_eq!(service.status().unwrap(), Status::Disabled);
        tauri::async_runtime::block_on(async {
            let first = service
                .enable(app.handle().clone(), parent.path(), policy())
                .unwrap();
            assert!(first.is_file());
            assert_eq!(service.status().unwrap(), Status::Running);
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
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
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
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
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
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
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
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
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

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn mcp_configuration_preserves_paths_without_exporting_capabilities_or_stale_records() {
        let parent = tempfile::tempdir().unwrap();
        let relay = parent.path().join("relay with spaces 🧭");
        std::fs::write(&relay, b"fixture only, never executed").unwrap();
        std::fs::set_permissions(&relay, std::fs::Permissions::from_mode(0o700)).unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
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
