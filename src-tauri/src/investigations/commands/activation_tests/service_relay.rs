//! Actual Service owner activation with a source-built relay and synthetic provider.
use super::{task_relay::Relay, *};
use crate::mcp_reads::{
    service::{Policy, Service, Status},
    worker::tests::task_limits,
};
use mcp_local::{activation::ActivationLimits, work_budget::WorkLimits};
use std::num::NonZeroUsize;
fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
fn policy(state: &AppState) -> Policy {
    let worker = task_limits(state);
    let transport = ActivationLimits {
        max_connections: nz(2),
        read_frame_bytes: worker.max_request_frame_bytes,
        write_frame_bytes: worker.dispatch.max_response_bytes,
        frame_deadline: Duration::from_secs(10),
        session_lifetime: Duration::from_secs(120),
    };
    let transport_bytes =
        crate::mcp_reads::reservation::TransportReservation::for_limits(transport)
            .unwrap()
            .activation_bytes;
    Policy {
        transport,
        worker,
        aggregate: WorkLimits {
            max_workers: nz(2),
            max_reserved_bytes: nz(transport_bytes
                .checked_add(worker.reserved_bytes.get().checked_mul(2).unwrap())
                .unwrap()),
            max_cumulative_read_bytes: 1_000_000,
        },
        export_mode: spec::ExportMode::VerifiedOnly,
    }
}
async fn stop(service: &Service) {
    service.disable().await.unwrap();
    wait(|| (service.status().unwrap() == Status::Disabled).then_some(()));
}
#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_service_relay_completion_and_reconstruction() {
    exercise(None, false);
}
#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_service_relay_definitive_cancellation() {
    exercise(Some(llm::bounded::InvocationOutcome::NotDispatched), false);
}
#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_service_relay_unknown_cancellation() {
    exercise(Some(llm::bounded::InvocationOutcome::Unknown), false);
}
#[test]
#[ignore = "requires explicit source-built CARTOGRAPH_TEST_MCP_RELAY"]
fn mcp_service_relay_consent_decline() {
    exercise(None, true);
}

fn exercise(cancel: Option<llm::bounded::InvocationOutcome>, decline: bool) {
    // AC-0320..0325/0329: authority comes from Service, never a mutable fixture session.
    let provider = Arc::new(PublicProvider::new(decline));
    *provider.cancel_outcome.lock().unwrap() = cancel;
    let (resume, paused) = std::sync::mpsc::channel();
    if !decline {
        *provider.pause.lock().unwrap() = Some(paused);
    }
    worker::with_public_start_fixture_then(
        Arc::new(PublicProvider::new(false)),
        false,
        |app, mut request, _| {
            let state = app.state::<AppState>();
            if decline {
                let mut settings = state.settings.lock().unwrap();
                settings.set_provider("T3", "cloud").unwrap();
                settings
                    .grant_consent("T3", "Synthetic Service relay fixture; no network.")
                    .unwrap();
                request.provider_mode = InvestigationProviderMode::Cloud;
            }
            state
                .investigations
                .set_test_provider(request.provider_mode, provider.clone());
            let client = state.jobs.lock().unwrap().create_mcp_client(nz(2)).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let service = Service::default();
            tauri::async_runtime::block_on(async {
                let mut record = service
                    .enable_tasks(
                        app.clone(),
                        dir.path(),
                        policy(&state),
                        client.clone(),
                        nz(1),
                    )
                    .unwrap();
                let mut relay = Relay::open(&record);
                let start = relay.call("cartograph_task_start", json!({"request": request}));
                let id = start["result"]["structuredContent"]["investigation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                if decline {
                    wait(|| super::pending(&state, &id).unwrap());
                } else {
                    wait(|| provider.entered.load(Ordering::SeqCst).then_some(()));
                }
                let pending = relay.call("cartograph_task_result", json!({"task_id":id}));
                assert_eq!(
                    pending["result"]["structuredContent"],
                    json!({"result":null})
                );
                relay.close();
                // Reconnect while the provider/consent is held: same durable task, no new invocation.
                let mut relay = Relay::open(&record);
                let replay = relay.call("cartograph_task_start", json!({"request":request}));
                assert_eq!(
                    replay["result"]["structuredContent"]["investigation_id"],
                    id
                );
                // The synthetic counter advances only after the held invocation resumes.
                assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
                if cancel.is_some() {
                    let reply = relay.call("cartograph_task_cancel", json!({"task_id":id}));
                    assert_eq!(
                        reply["result"]["structuredContent"]["cancel_requested"],
                        true
                    );
                }
                if decline {
                    let consent = super::pending(&state, &id).unwrap().unwrap();
                    assert!(
                        consent_decision(
                            app,
                            &id,
                            &consent.step_id,
                            consent.revision + 1,
                            &consent.preview.payload_hash,
                            false
                        )
                        .is_err()
                    );
                    consent_decision(
                        app,
                        &id,
                        &consent.step_id,
                        consent.revision,
                        &consent.preview.payload_hash,
                        false,
                    )
                    .unwrap();
                } else if cancel.is_none() {
                    // Actual Service revocation must retain admitted work and its reservation.
                    relay.close();
                    service.disable().await.unwrap();
                    assert!(!record.exists());
                    assert_eq!(service.status().unwrap(), Status::Draining);
                    assert!(!detail(app, &id).summary.cancel_requested);
                    assert!(
                        state
                            .jobs
                            .lock()
                            .unwrap()
                            .investigation_result(&id)
                            .unwrap()
                            .is_none()
                    );
                    resume.send(()).unwrap();
                    assert_eq!(
                        finished(app, &id).summary.status,
                        InvestigationStatus::Completed
                    );
                    wait(|| (service.status().unwrap() == Status::Disabled).then_some(()));
                    record = service
                        .enable_tasks(
                            app.clone(),
                            dir.path(),
                            policy(&state),
                            client.clone(),
                            nz(1),
                        )
                        .unwrap();
                    relay = Relay::open(&record);
                    let replay = relay.call("cartograph_task_start", json!({"request":request}));
                    assert_eq!(
                        replay["result"]["structuredContent"]["investigation_id"],
                        id
                    );
                } else {
                    resume.send(()).unwrap();
                }
                let terminal = finished(app, &id);
                let expected = match cancel {
                    Some(llm::bounded::InvocationOutcome::Unknown) => {
                        InvestigationStatus::OutcomeUnknown
                    }
                    Some(_) => InvestigationStatus::Cancelled,
                    None if decline => InvestigationStatus::Cancelled,
                    None => InvestigationStatus::Completed,
                };
                assert_eq!(terminal.summary.status, expected);
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
                let cancelled = relay.call("cartograph_task_cancel", json!({"task_id":id}));
                assert_eq!(
                    cancelled["result"]["structuredContent"],
                    status["result"]["structuredContent"]
                );
                let mut changed = request.clone();
                changed.question.push_str(" changed");
                assert_eq!(
                    relay.call("cartograph_task_start", json!({"request":changed}))["result"]["isError"],
                    true
                );
                relay.close();
                let rotated = service.rotate(app.clone(), dir.path()).await.unwrap();
                assert!(!record.exists());
                assert!(service.owner_status().unwrap()["task_grant"].is_null());
                let mut readonly = Relay::open(&rotated);
                let listed = readonly.rpc("tools/list", json!({}));
                assert!(
                    listed["result"]["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|tool| !tool["name"]
                            .as_str()
                            .unwrap()
                            .starts_with("cartograph_task_"))
                );
                for name in [
                    "cartograph_task_start",
                    "cartograph_task_status",
                    "cartograph_task_events",
                    "cartograph_task_result",
                    "cartograph_task_cancel",
                ] {
                    let args = if name == "cartograph_task_start" {
                        json!({"request":request})
                    } else if name == "cartograph_task_events" {
                        json!({"task_id":id,"after":0})
                    } else {
                        json!({"task_id":id})
                    };
                    assert_eq!(readonly.call(name, args)["error"]["code"], -32602);
                }
                readonly.close();
                stop(&service).await;
                let foreign = state.jobs.lock().unwrap().create_mcp_client(nz(2)).unwrap();
                let other_record = service
                    .enable_tasks(app.clone(), dir.path(), policy(&state), foreign, nz(1))
                    .unwrap();
                let mut other = Relay::open(&other_record);
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
                stop(&service).await;
                assert!(!other_record.exists());
                assert_eq!(
                    provider.calls.load(Ordering::SeqCst),
                    if decline {
                        0
                    } else if cancel.is_some() {
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
                )
            })
        },
        |(request, client, id, expected), root| {
            let fresh = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            fresh.manage(crate::registered_source_tests::app_state(root));
            let state = fresh.state::<AppState>();
            let dir = tempfile::tempdir().unwrap();
            let service = Service::default();
            // No provider is installed in this fresh host; replay must use retained intent.
            tauri::async_runtime::block_on(async {
                let record = service
                    .enable_tasks(
                        fresh.handle().clone(),
                        dir.path(),
                        policy(&state),
                        client,
                        nz(1),
                    )
                    .unwrap();
                let mut relay = Relay::open(&record);
                let replay = relay.call("cartograph_task_start", json!({"request":request}));
                assert_eq!(
                    replay["result"]["structuredContent"]["investigation_id"],
                    id
                );
                assert_eq!(
                    relay.call("cartograph_task_status", json!({"task_id":id}))["result"]["structuredContent"],
                    expected
                );
                relay.close();
                stop(&service).await;
                assert!(!record.exists());
            });
        },
    );
}
