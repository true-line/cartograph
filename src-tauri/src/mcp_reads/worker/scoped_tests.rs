// AC-0308 / T-0308: synthetic worker and native socket acceptance.
//! Scoped acquisition through the actual blocking worker and native transport.
use super::tests::limits;
use super::*;
use mcp_local::work_budget::WorkLimits;
fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

#[cfg(unix)]
#[test]
fn scoped_worker_opens_large_graph_while_complete_mode_refuses_without_fallback() {
    use core_graph::GraphPatch;
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "test.cartograph.{}",
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    let state = crate::registered_source_tests::app_state(dir.path());
    let patch = GraphPatch {
        upsert_nodes: (0..60_001)
            .map(|i| core_graph::Node {
                id: format!("n{i:06}"),
                label: "Symbol".into(),
                props: json!({"name":format!("n{i:06}")}),
            })
            .collect(),
        ..GraphPatch::default()
    };
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .apply_patch_if_snapshot_matches(&(vec![], vec![]), &patch)
            .unwrap()
    );
    app.manage(state);
    let budget = |limits: WorkerLimits| {
        WorkBudget::new(WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: limits.reserved_bytes,
            max_cumulative_read_bytes: 100_000,
        })
    };
    tauri::async_runtime::block_on(async {
        let complete = limits();
        let error = Worker::start(
            app.handle().clone(),
            "complete".into(),
            spec::ExportMode::VerifiedOnly,
            complete,
            budget(complete),
        )
        .await;
        assert!(matches!(error, Err(ReadError::Limit)));
        let mut scoped = limits();
        scoped.acquisition_mode = AcquisitionMode::Scoped;
        scoped.reserved_bytes = nz(required_reservation(scoped).unwrap());
        let mut worker = Worker::start(
            app.handle().clone(),
            "scoped".into(),
            spec::ExportMode::VerifiedOnly,
            scoped,
            budget(scoped),
        )
        .await
        .unwrap();
        assert!(matches!(worker.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).await.unwrap(), DispatchAction::Respond(_)));
        assert_eq!(
            worker
                .receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .unwrap(),
            DispatchAction::Ignore
        );
        let DispatchAction::Respond(response) = worker
            .receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap()
        else {
            panic!("expected scoped tool list")
        };
        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "cartograph_read_fact"));
        assert!(
            !tools
                .iter()
                .any(|t| t["name"] == "cartograph_read_artifact")
        );
        let DispatchAction::Respond(capabilities) = worker.receive(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cartograph_capabilities","arguments":{}}})).await.unwrap() else { panic!("expected scoped capabilities") };
        let basis = capabilities["result"]["structuredContent"]["session_basis_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let DispatchAction::Respond(page) = worker.receive(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cartograph_discover","arguments":{"session_id":"scoped","session_basis_id":basis,"request":{"query":{"by":"identifier","id":"n000002"},"labels":[],"max_scan_rows":3,"max_candidates":1,"max_bytes":65536}}}})).await.unwrap() else { panic!("expected scoped discovery") };
        let discovery = &page["result"]["structuredContent"]["discovery"];
        assert_eq!(discovery["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(discovery["complete_system"], false);
        assert_eq!(discovery["scanned_rows"], 3);
        let DispatchAction::Respond(detail) = worker.receive(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cartograph_read_fact","arguments":{"session_id":"scoped","session_basis_id":basis,"fact":{"kind":"node","id":"n000002"},"max_bytes":65536}}})).await.unwrap() else { panic!("expected scoped detail") };
        assert_eq!(
            detail["result"]["structuredContent"]["detail"]["fact"]["properties"]["name"],
            "n000002"
        );
    });
}

#[test]
fn scoped_native_socket_reads_large_graph_and_refuses_stale_continuation() {
    use core_graph::{GraphPatch, GraphStore, Node};
    use mcp_local::{
        connection::Connection,
        serve::{ServeError, serve_connection},
    };
    use mcp_wire::capability::{Capability, LOCAL_PROTOCOL};
    use serde_json::json;
    use std::{sync::Arc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "test.cartograph.{}",
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    let state = crate::registered_source_tests::app_state(dir.path());
    let patch = GraphPatch {
        upsert_nodes: (0..60_001)
            .map(|i| Node {
                id: format!("n{i:06}"),
                label: "Symbol".into(),
                props: json!({"name":format!("n{i:06}")}),
            })
            .collect(),
        ..GraphPatch::default()
    };
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .apply_patch_if_snapshot_matches(&(vec![], vec![]), &patch)
            .unwrap()
    );
    app.manage(state);
    let handle = app.handle().clone();
    let mut policy = limits();
    policy.acquisition_mode = AcquisitionMode::Scoped;
    policy.reserved_bytes = nz(required_reservation(policy).unwrap());
    let budget = WorkBudget::new(WorkLimits {
        max_workers: nz(1),
        max_reserved_bytes: policy.reserved_bytes,
        max_cumulative_read_bytes: 100_000,
    });
    tauri::async_runtime::block_on(async {
        let (server, client) = tokio::net::UnixStream::pair().unwrap();
        let connect = |stream| {
            Connection::from_stream(stream, nz(65536), nz(65536), Duration::from_secs(5)).unwrap()
        };
        let server = connect(server);
        let mut client = connect(client);
        let secret = Arc::new(Capability::generate().unwrap());
        let server_secret = Arc::clone(&secret);
        let (revoke, revoked) = tokio::sync::watch::channel(false);
        let server_budget = budget.clone();
        let task = tauri::async_runtime::spawn(async move {
            serve_connection(
                server,
                &server_secret,
                Duration::from_secs(30),
                revoked,
                || async move {
                    Worker::start(
                        handle,
                        "native-scoped".into(),
                        spec::ExportMode::VerifiedOnly,
                        policy,
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
        client.write(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-scoped","version":"1"}}})).await.unwrap();
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
        client.write(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cartograph_capabilities","arguments":{}}})).await.unwrap();
        let capabilities = client.read().await.unwrap().unwrap();
        let basis = capabilities["result"]["structuredContent"]["session_basis_id"]
            .as_str()
            .unwrap()
            .to_owned();
        client.write(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cartograph_discover","arguments":{"session_id":"native-scoped","session_basis_id":basis,"request":{"query":{"by":"text","text":"n000"},"labels":[],"max_scan_rows":3,"max_candidates":3,"max_bytes":65536}}}})).await.unwrap();
        let response = client.read().await.unwrap().unwrap();
        let page = &response["result"]["structuredContent"]["discovery"];
        assert_eq!(page["candidates"].as_array().unwrap().len(), 3);
        assert_eq!(page["complete_system"], false);
        let cursor = page["next_cursor"].clone();
        assert!(cursor.is_object());
        app.state::<crate::AppState>()
            .graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: "changed".into(),
                label: "Symbol".into(),
                props: json!({}),
            })
            .unwrap();
        client.write(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cartograph_discover","arguments":{"session_id":"native-scoped","session_basis_id":basis,"request":{"query":{"by":"text","text":"n000"},"labels":[],"max_scan_rows":3,"max_candidates":3,"max_bytes":65536,"cursor":cursor}}}})).await.unwrap();
        let stale = client.read().await.unwrap().unwrap();
        assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
        budget.revoke();
        revoke.send(true).unwrap();
        assert_eq!(task.await.unwrap(), Err(ServeError::Revoked));
        assert!(client.read().await.unwrap().is_none());
    });
}
