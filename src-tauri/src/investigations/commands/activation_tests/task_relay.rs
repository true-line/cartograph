//! Opt-in real relay/Unix transport, synthetic provider, disposable stores only.
use super::*;
use crate::mcp_reads::worker::Worker;
use mcp_local::{
    activation::{Activation, ActivationLimits},
    serve::ServeError,
    work_budget::{WorkBudget, WorkLimits},
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
};

pub(super) struct Relay {
    child: Child,
    replies: mpsc::Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
    id: usize,
}
impl Relay {
    pub(super) fn open(record: &std::path::Path) -> Self {
        let binary = std::path::PathBuf::from(
            std::env::var_os("CARTOGRAPH_TEST_MCP_RELAY")
                .expect("explicit source-built relay required"),
        );
        assert!(binary.is_absolute() && binary.is_file());
        let mut child = Command::new(binary)
            .arg("--record")
            .arg(record)
            .args(["--max-frame-bytes", "65536", "--timeout-ms", "10000"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, replies) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let count = input
                    .by_ref()
                    .take(65537)
                    .read_until(b'\n', &mut bytes)
                    .unwrap();
                if count == 0 {
                    break;
                }
                assert!(count <= 65536 && bytes.last() == Some(&b'\n'));
                if sender
                    .send(serde_json::from_slice(&bytes).unwrap())
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut relay = Self {
            child,
            replies,
            reader: Some(reader),
            id: 0,
        };
        let init = relay.rpc("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"task-fixture","version":"1"}}));
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        relay.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        relay
    }
    fn write(&mut self, message: Value) {
        let bytes = serde_json::to_vec(&message).unwrap();
        assert!(bytes.len() < 65536);
        let input = self.child.stdin.as_mut().unwrap();
        input.write_all(&bytes).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
    }
    pub(super) fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        self.write(json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}));
        let reply = self.replies.recv_timeout(Duration::from_secs(15)).unwrap();
        assert_eq!(reply["id"], self.id);
        reply
    }
    pub(super) fn call(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name":name,"arguments":args}))
    }
    pub(super) fn close(mut self) {
        self.child.stdin.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut stderr = Vec::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .take(4097)
            .read_to_end(&mut stderr)
            .unwrap();
        assert!(stderr.is_empty());
        self.reader.take().unwrap().join().unwrap();
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn endpoint(
    endpoint: &std::path::Path,
    app: tauri::AppHandle<tauri::test::MockRuntime>,
    principal: Arc<std::sync::Mutex<agents::investigation::McpTaskSession>>,
    budget: WorkBudget,
    limits: crate::mcp_reads::worker::WorkerLimits,
    worker_exits: Option<mpsc::Sender<tauri::async_runtime::JoinHandle<()>>>,
) -> Activation {
    Activation::start(
        endpoint,
        ActivationLimits {
            max_connections: std::num::NonZeroUsize::new(1).unwrap(),
            read_frame_bytes: limits.max_request_frame_bytes,
            write_frame_bytes: limits.dispatch.max_response_bytes,
            frame_deadline: Duration::from_secs(10),
            session_lifetime: Duration::from_secs(60),
        },
        move || {
            let (app, session, budget, worker_exits) = (
                app.clone(),
                principal.lock().unwrap().clone(),
                budget.clone(),
                worker_exits.clone(),
            );
            async move {
                let mut worker = Worker::start_with_tasks(
                    app,
                    "task-relay".into(),
                    spec::ExportMode::VerifiedOnly,
                    limits,
                    budget,
                    Some(session),
                )
                .await
                .map_err(|_| ServeError::Application)?;
                if let Some(exits) = worker_exits {
                    exits.send(worker.test_exit.take().unwrap()).unwrap();
                }
                Ok(worker)
            }
        },
    )
    .unwrap()
}

#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_task_relay_runs_all_operations_and_reconnects_without_reexecution() {
    exercise_task_relay(None, false, false);
}

#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_task_relay_active_cancellation_preserves_definitive_outcome() {
    exercise_task_relay(
        Some(llm::bounded::InvocationOutcome::NotDispatched),
        false,
        false,
    );
}

#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_task_relay_active_cancellation_preserves_unknown_without_reexecution() {
    exercise_task_relay(Some(llm::bounded::InvocationOutcome::Unknown), false, false);
}

#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_task_relay_consent_reconnect_and_owner_decline_preserve_zero_calls() {
    exercise_task_relay(None, true, false);
}

#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_task_relay_replays_after_host_reconstruction_without_provider() {
    // AC-0322/0324/0329: fresh host/stores, not a native process crash claim.
    exercise_task_relay(None, false, true);
}

fn exercise_task_relay(
    cancel_outcome: Option<llm::bounded::InvocationOutcome>,
    decline: bool,
    reconstruct: bool,
) {
    // AC-0321/0323/0324/0325: actual capability handshake and Worker protocol.
    // Fixture factory grants a session; public activation and byte admission stay off.
    let provider = Arc::new(PublicProvider::new(decline));
    *provider.cancel_outcome.lock().unwrap() = cancel_outcome;
    let (resume, paused) = mpsc::channel();
    if !decline {
        *provider.pause.lock().unwrap() = Some(paused);
    }
    let seed = Arc::new(PublicProvider::new(false));
    let run = |app: &tauri::AppHandle<tauri::test::MockRuntime>,
               mut request: agents::investigation::obligations::StagedObligationRequest,
               _: &std::path::Path| {
        let state = app.state::<AppState>();
        if decline {
            let mut settings = state.settings.lock().unwrap();
            settings.set_provider("T3", "cloud").unwrap();
            settings
                .grant_consent("T3", "Synthetic relay fixture; no network.")
                .unwrap();
            request.provider_mode = InvestigationProviderMode::Cloud;
        }
        state
            .investigations
            .set_test_provider(request.provider_mode, provider.clone());
        let one = std::num::NonZeroUsize::new(1).unwrap();
        let two_clients = std::num::NonZeroUsize::new(2).unwrap();
        let owner_task = state
            .jobs
            .lock()
            .unwrap()
            .investigation_history(None, None)
            .unwrap()
            .items[0]
            .investigation_id
            .clone();
        let client = state
            .jobs
            .lock()
            .unwrap()
            .create_mcp_client(two_clients)
            .unwrap();
        let grant = McpTaskActivation::granted(client.clone(), one);
        // Only the host fixture changes the authenticated principal between
        // connections; neither the relay nor an RPC argument supplies it.
        let principal = Arc::new(std::sync::Mutex::new(grant.session()));
        let session = principal.clone();
        let limits = crate::mcp_reads::worker::tests::task_limits(&state);
        let budget = WorkBudget::new(WorkLimits {
            max_workers: if decline { two_clients } else { one },
            max_reserved_bytes: std::num::NonZeroUsize::new(
                limits.reserved_bytes.get() * if decline { 2 } else { 1 },
            )
            .unwrap(),
            max_cumulative_read_bytes: 1_000_000,
        });
        let observed_budget = budget.clone();
        let (worker_exits, worker_exit) = mpsc::channel();
        let endpoint_dir = tempfile::tempdir().unwrap();
        let handle = app.clone();
        tauri::async_runtime::block_on(async {
            let activation = endpoint(
                endpoint_dir.path(),
                handle,
                session,
                budget,
                limits,
                Some(worker_exits),
            );
            let record = activation.record_path().to_owned();
            let mut relay = Relay::open(&record);
            let listed = relay.rpc("tools/list", json!({}));
            for name in [
                "cartograph_task_start",
                "cartograph_task_status",
                "cartograph_task_events",
                "cartograph_task_result",
                "cartograph_task_cancel",
            ] {
                assert!(
                    listed["result"]["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|tool| tool["name"] == name)
                );
            }
            // AC-0327: preserve the task refusal code across the real relay.
            let mut stale = request.clone();
            stale.request_nonce = "relay-stale-context".into();
            stale.expected_context_id = format!("context-v2:{}", "0".repeat(64));
            let refused = relay.call("cartograph_task_start", json!({"request":stale}));
            assert_eq!(refused["result"]["isError"], true);
            assert_eq!(
                refused["result"]["structuredContent"]["error"],
                "refresh_required"
            );
            let start = relay.call("cartograph_task_start", json!({"request":request}));
            let id = start["result"]["structuredContent"]["investigation_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !decline && !provider.entered.load(Ordering::SeqCst) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            if decline {
                wait(|| super::pending(&state, &id).unwrap());
                let owner = detail(app, &id);
                assert_eq!(owner.summary.status, InvestigationStatus::AwaitingConsent);
                let status = relay.call("cartograph_task_status", json!({"task_id":id}));
                assert_eq!(
                    status["result"]["structuredContent"],
                    serde_json::to_value(owner.summary).unwrap()
                );
                assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            }
            let pending = relay.call("cartograph_task_result", json!({"task_id":id}));
            assert_eq!(pending["result"]["isError"], false);
            assert_eq!(
                pending["result"]["structuredContent"],
                json!({"result":null})
            );
            if cancel_outcome.is_some() {
                // AC-0325/0329: cancel through transport while invocation is held.
                let cancelled = relay.call("cartograph_task_cancel", json!({"task_id":id}));
                assert_eq!(cancelled["result"]["isError"], false);
                assert_eq!(
                    cancelled["result"]["structuredContent"]["cancel_requested"],
                    true
                );
                assert!(!observed_budget.is_idle().unwrap());
            }
            relay.close();
            // The connection worker has exited; the held task must retain the permit.
            let exit = worker_exit.recv_timeout(Duration::from_secs(10)).unwrap();
            tokio::time::timeout(Duration::from_secs(10), exit)
                .await
                .unwrap()
                .unwrap();
            assert!(!observed_budget.is_idle().unwrap());
            assert!(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .investigation_result(&id)
                    .unwrap()
                    .is_none()
            );
            if decline {
                // Consent keeps the detached worker's permit; reconnect needs a
                // separately admitted connection in this synthetic budget.
                assert!(!observed_budget.is_idle().unwrap());
                let mut reconnected = Relay::open(&record);
                let replay = reconnected.call("cartograph_task_start", json!({"request":request}));
                assert_eq!(
                    replay["result"]["structuredContent"]["investigation_id"],
                    id
                );
                let consent = super::pending(&state, &id).unwrap().unwrap();
                let before = detail(app, &id);
                for (revision, hash) in [
                    (consent.revision + 1, consent.preview.payload_hash.clone()),
                    (consent.revision, "0".repeat(64)),
                ] {
                    assert!(
                        consent_decision(app, &id, &consent.step_id, revision, &hash, false)
                            .is_err()
                    );
                    assert_eq!(detail(app, &id), before);
                }
                assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
                consent_decision(
                    app,
                    &id,
                    &consent.step_id,
                    consent.revision,
                    &consent.preview.payload_hash,
                    false,
                )
                .unwrap();
                reconnected.close();
                wait(|| observed_budget.is_idle().unwrap().then_some(()));
            } else {
                resume.send(()).unwrap();
            }
            let terminal = finished(app, &id);
            let expected = match cancel_outcome {
                None if decline => InvestigationStatus::Cancelled,
                None => InvestigationStatus::Completed,
                Some(llm::bounded::InvocationOutcome::NotDispatched) => {
                    InvestigationStatus::Cancelled
                }
                Some(llm::bounded::InvocationOutcome::Unknown) => {
                    InvestigationStatus::OutcomeUnknown
                }
                Some(llm::bounded::InvocationOutcome::ResponseReceived) => unreachable!(),
            };
            assert_eq!(terminal.summary.status, expected);
            assert_eq!(
                terminal.summary.cancel_requested,
                cancel_outcome.is_some() || decline
            );
            let mut relay = Relay::open(&record);
            let replay = relay.call("cartograph_task_start", json!({"request":request}));
            assert_eq!(
                replay["result"]["structuredContent"]["investigation_id"],
                id
            );
            let mut changed = request.clone();
            changed.question.push_str(" changed");
            let conflict = relay.call("cartograph_task_start", json!({"request":changed}));
            assert_eq!(conflict["result"]["isError"], true);
            let status = relay.call("cartograph_task_status", json!({"task_id":id}));
            assert_eq!(
                status["result"]["structuredContent"],
                serde_json::to_value(&terminal.summary).unwrap()
            );
            let events = relay.call("cartograph_task_events", json!({"task_id":id,"after":0}));
            assert_eq!(
                events["result"]["structuredContent"],
                serde_json::to_value(
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .investigation_events(&id, 0)
                        .unwrap()
                )
                .unwrap()
            );
            let result = relay.call("cartograph_task_result", json!({"task_id":id}));
            assert_eq!(
                result["result"]["structuredContent"]["result"],
                serde_json::to_value(
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .investigation_result(&id)
                        .unwrap()
                )
                .unwrap()
            );
            if cancel_outcome.is_some() || decline {
                assert!(result["result"]["structuredContent"]["result"].is_null());
            }
            let cancel = relay.call("cartograph_task_cancel", json!({"task_id":id}));
            assert_eq!(
                cancel["result"]["structuredContent"],
                status["result"]["structuredContent"]
            );
            grant.revoke();
            let denied = relay.call("cartograph_task_status", json!({"task_id":id}));
            assert_eq!(denied["result"]["isError"], true);
            relay.close();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !observed_budget.is_idle().unwrap() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            let foreign = state
                .jobs
                .lock()
                .unwrap()
                .create_mcp_client(two_clients)
                .unwrap();
            let foreign_grant = McpTaskActivation::granted(foreign, one);
            *principal.lock().unwrap() = foreign_grant.session();
            let mut foreign_relay = Relay::open(&record);
            for name in [
                "cartograph_task_status",
                "cartograph_task_events",
                "cartograph_task_result",
                "cartograph_task_cancel",
            ] {
                let mut refusal = None;
                for hidden_id in [&id, &owner_task, &"missing-task".to_owned()] {
                    let args = if name == "cartograph_task_events" {
                        json!({"task_id":hidden_id,"after":0})
                    } else {
                        json!({"task_id":hidden_id})
                    };
                    let denied = foreign_relay.call(name, args);
                    assert_eq!(denied["result"]["isError"], true);
                    if let Some(expected) = &refusal {
                        assert_eq!(&denied["result"], expected);
                    }
                    refusal = Some(denied["result"].clone());
                }
            }
            assert_eq!(detail(app, &id), terminal);
            foreign_relay.close();
            activation.disable().await.unwrap();
            assert!(!record.exists());
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                if decline {
                    0
                } else if cancel_outcome.is_some() {
                    1
                } else {
                    3
                }
            );
            (
                request,
                client,
                id,
                status["result"]["structuredContent"].clone(),
                events["result"]["structuredContent"].clone(),
                result["result"]["structuredContent"].clone(),
            )
        })
    };
    let reopen = |(request, client, id, status, events, result): (
        agents::investigation::obligations::StagedObligationRequest,
        agents::investigation::McpClientId,
        String,
        Value,
        Value,
        Value,
    ),
                  app_data: &std::path::Path| {
        if !reconstruct {
            return;
        }
        let fresh = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        fresh.manage(crate::registered_source_tests::app_state(app_data));
        let state = fresh.state::<AppState>();
        assert!(
            state
                .jobs
                .lock()
                .unwrap()
                .mcp_client_exists(&client)
                .unwrap()
        );
        let foreign = state
            .jobs
            .lock()
            .unwrap()
            .mcp_clients(None, std::num::NonZeroUsize::new(2).unwrap())
            .unwrap()
            .into_iter()
            .find(|saved| saved != &client)
            .unwrap();
        // No synthetic provider is installed in the new runtime. Replay must
        // resolve persisted intent before provider or mutable-context lookup.
        let one = std::num::NonZeroUsize::new(1).unwrap();
        let grant = McpTaskActivation::granted(client, one);
        let principal = Arc::new(std::sync::Mutex::new(grant.session()));
        let limits = crate::mcp_reads::worker::tests::task_limits(&state);
        let budget = WorkBudget::new(WorkLimits {
            max_workers: one,
            max_reserved_bytes: limits.reserved_bytes,
            max_cumulative_read_bytes: 1_000_000,
        });
        let directory = tempfile::tempdir().unwrap();
        tauri::async_runtime::block_on(async {
            let disabled = McpTaskActivation::default();
            let readonly = endpoint(
                directory.path(),
                fresh.handle().clone(),
                Arc::new(std::sync::Mutex::new(disabled.session())),
                budget.clone(),
                limits,
                None,
            );
            let mut denied = Relay::open(readonly.record_path());
            assert_eq!(
                denied.call("cartograph_task_status", json!({"task_id":id}))["error"]["code"],
                -32602
            );
            denied.close();
            readonly.disable().await.unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !budget.is_idle().unwrap() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            let activation = endpoint(
                directory.path(),
                fresh.handle().clone(),
                principal,
                budget.clone(),
                limits,
                None,
            );
            let record = activation.record_path().to_owned();
            let mut relay = Relay::open(&record);
            let replay = relay.call("cartograph_task_start", json!({"request":request}));
            assert_eq!(
                replay["result"]["structuredContent"]["investigation_id"],
                id
            );
            for (name, expected) in [
                ("cartograph_task_status", status.clone()),
                ("cartograph_task_events", events),
                ("cartograph_task_result", result),
                ("cartograph_task_cancel", status),
            ] {
                let args = if name == "cartograph_task_events" {
                    json!({"task_id":id,"after":0})
                } else {
                    json!({"task_id":id})
                };
                assert_eq!(
                    relay.call(name, args)["result"]["structuredContent"],
                    expected
                );
            }
            relay.close();
            activation.disable().await.unwrap();
            assert!(!record.exists());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !budget.is_idle().unwrap() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            let foreign_grant = McpTaskActivation::granted(foreign, one);
            let foreign_endpoint = endpoint(
                directory.path(),
                fresh.handle().clone(),
                Arc::new(std::sync::Mutex::new(foreign_grant.session())),
                budget.clone(),
                limits,
                None,
            );
            let foreign_record = foreign_endpoint.record_path().to_owned();
            let mut other = Relay::open(&foreign_record);
            for name in [
                "cartograph_task_status",
                "cartograph_task_events",
                "cartograph_task_result",
                "cartograph_task_cancel",
            ] {
                let args = if name == "cartograph_task_events" {
                    json!({"task_id":id,"after":0})
                } else {
                    json!({"task_id":id})
                };
                assert_eq!(other.call(name, args)["result"]["isError"], true);
            }
            other.close();
            foreign_endpoint.disable().await.unwrap();
            assert!(!foreign_record.exists());
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !budget.is_idle().unwrap() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        });
    };
    worker::with_public_start_fixture_then(seed, false, run, reopen);
}
