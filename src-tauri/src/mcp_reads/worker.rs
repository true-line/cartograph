//! One admitted blocking worker owns the app borrows and frozen dispatcher.
//! Dropping the async client closes its queue; running synchronous work retains
//! its permit until it exits. No store lock or receipt read runs on the UI thread.
use super::{
    ReadError, ReadSession,
    dispatch::{DispatchAction, DispatchLimits, Dispatcher, EvidenceWorkLimits},
};
use mcp_local::work_budget::WorkBudget;
use std::num::NonZeroUsize;
use tauri::Manager;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Copy)]
pub(crate) struct WorkerLimits {
    pub graph: core_graph::SnapshotReadLimits,
    pub coverage: core_graph::coverage::CoverageReadLimits,
    pub dispatch: DispatchLimits,
    pub evidence: EvidenceWorkLimits,
    /// Host's conservative total reservation, including raw/projected/index and
    /// compilation copies. This value is not a measured RSS guarantee.
    pub reserved_bytes: NonZeroUsize,
}

struct Request {
    value: serde_json::Value,
    reply: oneshot::Sender<Result<DispatchAction, ReadError>>,
}

pub(crate) struct Worker {
    requests: mpsc::Sender<Request>,
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
        let permit = budget
            .try_acquire(limits.reserved_bytes, 0)
            .map_err(|_| ReadError::Limit)?;
        let (requests, mut incoming) = mpsc::channel::<Request>(1);
        let (ready, initialized) = oneshot::channel();
        tauri::async_runtime::spawn_blocking(move || {
            // Own the permit in the actual worker, not in the async waiter.
            let _permit = permit;
            let state = app.state::<crate::AppState>();
            let result = (|| {
                _permit.check_active().map_err(|_| ReadError::Limit)?;
                if ready.is_closed() {
                    return Err(ReadError::Storage);
                }
                let session = ReadSession::acquire(
                    &state.graph,
                    &state.decisions,
                    || crate::context::coverage_scope(&app, &state).map_err(|_| ReadError::Storage),
                    limits.graph,
                    limits.coverage,
                    mode,
                )?;
                Dispatcher::with_evidence(
                    session,
                    session_id,
                    limits.dispatch,
                    &state,
                    limits.evidence,
                )
            })();
            let mut dispatcher = match result {
                Ok(dispatcher) => dispatcher,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
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
                let action = dispatcher.receive(request.value);
                let close = matches!(action, DispatchAction::Close);
                if request.reply.send(Ok(action)).is_err() || close {
                    break;
                }
            }
        });
        initialized.await.map_err(|_| ReadError::Storage)??;
        Ok(Self { requests })
    }

    pub(crate) async fn receive(
        &mut self,
        value: serde_json::Value,
    ) -> Result<DispatchAction, ReadError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Request { value, reply })
            .await
            .map_err(|_| ReadError::Storage)?;
        response.await.map_err(|_| ReadError::Storage)?
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
        WorkerLimits {
            graph: core_graph::SnapshotReadLimits::default(),
            coverage: core_graph::coverage::CoverageReadLimits::default(),
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
        }
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
            max_reserved_bytes: nz(1_000_000),
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
            max_reserved_bytes: nz(1_000_000),
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
