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
        let app = tauri::test::mock_builder().build(context).unwrap();
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
                matches!(list, DispatchAction::Respond(v) if v["result"]["tools"].as_array().is_some_and(|tools| tools.len() == if mode == AcquisitionMode::Complete {5} else {6}))
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
