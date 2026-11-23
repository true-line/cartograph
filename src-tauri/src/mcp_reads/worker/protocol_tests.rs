//! AC-0289: lifecycle and ID ownership survive failed read acquisition.
use super::*;
use mcp_local::work_budget::WorkLimits;

#[test]
fn mcp_worker_protocol_survives_failed_acquisition_and_keeps_one_id_ledger() {
    for mode in [AcquisitionMode::Complete, AcquisitionMode::Scoped] {
        let dir = tempfile::tempdir().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = format!(
            "test.cartograph.{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let app =
            crate::test_app::TestApp::new(tauri::test::mock_builder().build(context).unwrap());
        app.manage(crate::registered_source_tests::app_state(dir.path()));
        // Make only this disposable graph unavailable. Protocol calls must not
        // attempt a graph lock; a subsequent read must report the failure.
        let handle = app.handle().clone();
        assert!(
            std::thread::spawn(move || {
                let state = handle.state::<crate::AppState>();
                let _guard = state.graph.lock().unwrap();
                panic!("synthetic graph failure");
            })
            .join()
            .is_err()
        );
        let mut limits = tests::limits();
        limits.acquisition_mode = mode;
        limits.reserved_bytes = NonZeroUsize::new(required_reservation(limits).unwrap()).unwrap();
        let budget = WorkBudget::new(WorkLimits {
            max_workers: NonZeroUsize::new(1).unwrap(),
            max_reserved_bytes: limits.reserved_bytes,
            max_cumulative_read_bytes: 100_000,
        });
        tauri::async_runtime::block_on(async {
            let mut worker = Worker::start(
                app.handle().clone(),
                "lazy-fixture".into(),
                spec::ExportMode::VerifiedOnly,
                limits,
                budget.clone(),
            )
            .await
            .unwrap();
            let initialize = worker.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap();
            assert!(matches!(initialize, DispatchAction::Respond(v) if v.get("result").is_some()));
            assert_eq!(
                worker
                    .receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                    .await
                    .unwrap(),
                DispatchAction::Ignore
            );
            let list = worker
                .receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
                .await
                .unwrap();
            assert!(
                matches!(list, DispatchAction::Respond(v) if v["result"]["tools"].as_array().is_some_and(|tools| tools.len() == if mode == AcquisitionMode::Complete {6} else {7}))
            );
            let call = |id| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"cartograph_capabilities","arguments":{}}});
            let DispatchAction::Respond(error) = worker.receive(call(3)).await.unwrap() else {
                panic!("bounded error")
            };
            assert_eq!(error["result"]["isError"], true);
            assert!(super::super::dispatch::fits(
                &error,
                limits.dispatch.max_response_bytes.get()
            ));
            assert!(
                matches!(worker.receive(json!({"jsonrpc":"2.0","id":4,"method":"ping"})).await.unwrap(), DispatchAction::Respond(v) if v.get("result").is_some())
            );
            app.state::<crate::AppState>().graph.clear_poison();
            let DispatchAction::Respond(recovered) = worker.receive(call(5)).await.unwrap() else {
                panic!("recovered read")
            };
            assert_ne!(recovered["result"]["isError"], true);
            assert!(recovered["result"]["structuredContent"].is_object());
            // Acquisition never replaces the connection's prior ID ledger.
            assert_eq!(
                worker.receive(call(3)).await.unwrap(),
                DispatchAction::Close
            );
        });
    }
}

// AC-0289: host policy must be valid before lifecycle can advertise its caps.
#[test]
fn scoped_worker_refuses_invalid_graph_limits_before_reserving_or_advertising() {
    let dir = tempfile::tempdir().unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(crate::registered_source_tests::app_state(dir.path()));
    let base = tests::limits();
    let budget = WorkBudget::new(WorkLimits {
        max_workers: NonZeroUsize::new(1).unwrap(),
        max_reserved_bytes: base.reserved_bytes,
        max_cumulative_read_bytes: 100_000,
    });
    let maximum = core_graph::SnapshotReadLimits::investigation(); // the hard ceilings
    for field in 0..4 {
        for above in [false, true] {
            let mut limits = base;
            limits.acquisition_mode = AcquisitionMode::Scoped;
            let (value, max) = match field {
                0 => (&mut limits.graph.max_rows, maximum.max_rows),
                1 => (&mut limits.graph.max_bytes, maximum.max_bytes),
                2 => (&mut limits.graph.max_depth, maximum.max_depth),
                _ => (&mut limits.graph.max_values, maximum.max_values),
            };
            *value = if above { max + 1 } else { 0 };
            let result = tauri::async_runtime::block_on(Worker::start(
                app.handle().clone(),
                "invalid-scope".into(),
                spec::ExportMode::VerifiedOnly,
                limits,
                budget.clone(),
            ));
            assert!(matches!(result, Err(ReadError::InvalidRequest)));
            assert!(budget.is_idle().unwrap());
        }
    }
}

#[test]
fn authenticated_task_tools_share_lifecycle_and_revocation_precedes_store_access() {
    // AC-0320/0321/0325: internal factory only; public task activation stays off.
    use agents::investigation::{McpClientId, McpTaskActivation};
    let dir = tempfile::tempdir().unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(crate::registered_source_tests::app_state(dir.path()));
    let grant = McpTaskActivation::granted(
        McpClientId::from_host_bytes([9; 16]),
        NonZeroUsize::new(4).unwrap(),
    );
    let limits = tests::task_limits(&app.state::<crate::AppState>());
    let budget = WorkBudget::new(WorkLimits {
        max_workers: NonZeroUsize::new(1).unwrap(),
        max_reserved_bytes: limits.reserved_bytes,
        max_cumulative_read_bytes: 100_000,
    });
    tauri::async_runtime::block_on(async {
        // AC-0321/0327: unavailable grants retain ordinary read-only admission.
        let revoked = McpTaskActivation::granted(
            McpClientId::from_host_bytes([8; 16]),
            NonZeroUsize::new(4).unwrap(),
        );
        revoked.revoke();
        for unavailable in [McpTaskActivation::default(), revoked] {
            let read_limits = tests::limits();
            let read_budget = WorkBudget::new(WorkLimits {
                max_workers: NonZeroUsize::new(1).unwrap(),
                max_reserved_bytes: read_limits.reserved_bytes,
                max_cumulative_read_bytes: 100_000,
            });
            let mut reader = Worker::start_with_tasks(
                app.handle().clone(),
                "read-only-grant".into(),
                spec::ExportMode::VerifiedOnly,
                read_limits,
                read_budget,
                Some(unavailable.session()),
            )
            .await
            .unwrap();
            reader.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap();
            reader
                .receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .unwrap();
            let DispatchAction::Respond(list) = reader
                .receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
                .await
                .unwrap()
            else {
                panic!("read-only tools")
            };
            let tools = list["result"]["tools"].as_array().unwrap();
            assert!(!tools.is_empty());
            assert!(tools.iter().all(|tool| {
                !mcp_dispatch::TOOLS
                    .iter()
                    .any(|task| tool["name"] == task.name())
            }));
        }
        #[cfg(unix)]
        {
            let mut insufficient = limits;
            insufficient.reserved_bytes =
                NonZeroUsize::new(limits.reserved_bytes.get() - 1).unwrap();
            let refused = Worker::start_with_tasks(
                app.handle().clone(),
                "too-small".into(),
                spec::ExportMode::VerifiedOnly,
                insufficient,
                budget.clone(),
                Some(grant.session()),
            )
            .await;
            assert!(matches!(refused, Err(ReadError::Limit)));
            assert!(budget.is_idle().unwrap());
        }
        let mut worker = Worker::start_with_tasks(
            app.handle().clone(),
            "tasks".into(),
            spec::ExportMode::VerifiedOnly,
            limits,
            budget,
            Some(grant.session()),
        )
        .await
        .unwrap();
        worker.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap();
        worker
            .receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await
            .unwrap();
        let DispatchAction::Respond(list) = worker
            .receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap()
        else {
            panic!("list")
        };
        for tool in mcp_dispatch::TOOLS {
            assert!(
                list["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["name"] == tool.name())
            );
        }
        grant.revoke();
        // A revoked request must not try the store even when it is unavailable.
        let state = app.state::<crate::AppState>();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = state.jobs.lock().unwrap();
            panic!("poison fixture store");
        }));
        for (index, tool) in mcp_dispatch::TOOLS.into_iter().enumerate() {
            let DispatchAction::Respond(error) = worker.receive(json!({"jsonrpc":"2.0","id":index+3,"method":"tools/call","params":{"name":tool.name(),"arguments":{}}})).await.unwrap() else { panic!("error") };
            assert_eq!(error["result"]["isError"], true);
            assert!(error.to_string().contains("invalid_request"));
            assert!(!error.to_string().contains("storage_unavailable"));
        }
        state.jobs.clear_poison();
        let DispatchAction::Respond(list) = worker
            .receive(
                json!({"jsonrpc":"2.0","id":3 + mcp_dispatch::TOOLS.len(),"method":"tools/list"}),
            )
            .await
            .unwrap()
        else {
            panic!("list")
        };
        assert!(
            !list["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"].as_str().unwrap().starts_with("cartograph_task_"))
        );
        assert_eq!(
            worker
                .receive(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
                .await
                .unwrap(),
            DispatchAction::Close
        );
    });
}
