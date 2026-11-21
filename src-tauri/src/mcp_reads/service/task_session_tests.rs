//! Actual Service/socket session handoff; no provider or owner data is used.
use super::*;
use mcp_local::{
    connection::Connection, handshake::connect_authenticated, record::ConnectionRecord,
};
use serde_json::{Value, json};
use std::{num::NonZeroUsize, time::Duration};
use tauri::Manager;

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
async fn rpc(connection: &mut Connection, id: usize, method: &str, params: Value) -> Value {
    connection
        .write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .await
        .unwrap();
    let reply = connection.read().await.unwrap().unwrap();
    assert_eq!(reply["id"], id);
    reply
}
async fn session(record: &ConnectionRecord) -> Connection {
    let mut connection =
        connect_authenticated(record, nz(65536), nz(65536), Duration::from_secs(5))
            .await
            .unwrap();
    let init = rpc(&mut connection, 1, "initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"service-fixture","version":"1"}})).await;
    assert!(init.get("result").is_some());
    connection
        .write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await
        .unwrap();
    connection
}
async fn idle(service: &Service) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let idle = {
            let state = service.0.lock().unwrap();
            state
                .active
                .as_ref()
                .map(|a| &a.budget)
                .or(state.draining.as_ref())
                .is_none_or(|b| b.is_idle().unwrap())
        };
        if idle {
            return;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn service_task_sessions_follow_host_grant_and_rotation() {
    // AC-0320/0321: the grant comes from host activation, never RPC arguments.
    let stores = tempfile::tempdir().unwrap();
    let endpoint = tempfile::tempdir().unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(crate::registered_source_tests::app_state(stores.path()));
    let state = app.state::<crate::AppState>();
    let client = state.jobs.lock().unwrap().create_mcp_client(nz(1)).unwrap();
    let mut policy = tests::policy();
    policy.worker = crate::mcp_reads::worker::tests::task_limits(&state);
    let transport = super::super::reservation::TransportReservation::for_limits(policy.transport)
        .unwrap()
        .activation_bytes;
    policy.aggregate.max_reserved_bytes = nz(transport
        .checked_add(policy.worker.reserved_bytes.get())
        .unwrap());
    let service = Service::default();
    tauri::async_runtime::block_on(async {
        let mut insufficient = policy;
        insufficient.worker.reserved_bytes = nz(policy.worker.reserved_bytes.get() - 1);
        assert_eq!(
            service
                .enable_selected(
                    app.handle().clone(),
                    endpoint.path(),
                    insufficient,
                    Some((&state.jobs.lock().unwrap(), client.clone(), nz(1))),
                )
                .unwrap_err(),
            ReadError::Limit
        );
        assert_eq!(std::fs::read_dir(endpoint.path()).unwrap().count(), 0);
        assert_eq!(service.owner_status().unwrap()["service"], "disabled");
        let readonly = service
            .enable(app.handle().clone(), endpoint.path(), policy)
            .unwrap();
        let readonly_record = ConnectionRecord::read(&readonly).unwrap();
        let mut read = session(&readonly_record).await;
        let list = rpc(&mut read, 2, "tools/list", json!({})).await;
        assert!(
            list["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| !tool["name"]
                    .as_str()
                    .unwrap()
                    .starts_with("cartograph_task_"))
        );
        let denied = rpc(
            &mut read,
            3,
            "tools/call",
            json!({"name":"cartograph_task_status","arguments":{"task_id":"missing"}}),
        )
        .await;
        assert_eq!(denied["error"]["code"], -32602);
        drop(read);
        service.disable().await.unwrap();
        idle(&service).await;
        let mut insufficient = policy;
        insufficient.worker.reserved_bytes = nz(policy.worker.reserved_bytes.get() - 1);
        assert!(
            service
                .enable_tasks(
                    app.handle().clone(),
                    endpoint.path(),
                    insufficient,
                    client.clone(),
                    nz(1)
                )
                .is_err()
        );
        assert_eq!(service.owner_status().unwrap()["service"], "disabled");
        let unknown = agents::investigation::McpClientId::from_host_bytes([0; 16]);
        assert!(
            service
                .enable_tasks(
                    app.handle().clone(),
                    endpoint.path(),
                    policy,
                    unknown,
                    nz(1)
                )
                .is_err()
        );
        let path = service
            .enable_tasks(
                app.handle().clone(),
                endpoint.path(),
                policy,
                client.clone(),
                nz(1),
            )
            .unwrap();
        let observed = service.owner_status().unwrap();
        assert_eq!(observed["task_dispatch_available"], true);
        assert_eq!(observed["task_grant"]["client_id"], client.as_str());
        assert_eq!(observed["task_grant"]["maximum_identities"], 1);
        let record = ConnectionRecord::read(&path).unwrap();
        // Wrong activation credentials cannot reach the task-aware factory.
        assert!(!readonly.exists());
        let mut unauthenticated = record
            .connect(nz(65536), nz(65536), Duration::from_secs(5))
            .await
            .unwrap();
        unauthenticated
            .write(&readonly_record.authentication_preamble())
            .await
            .unwrap();
        assert!(matches!(unauthenticated.read().await, Ok(None) | Err(_)));
        drop(unauthenticated);
        idle(&service).await;
        let mut connection = session(&record).await;
        let list = rpc(&mut connection, 2, "tools/list", json!({})).await;
        for (index, tool) in crate::investigations::mcp_dispatch::TOOLS
            .into_iter()
            .enumerate()
        {
            assert!(
                list["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["name"] == tool.name())
            );
            let arguments = match tool {
                mcp_wire::tools::TaskTool::Start => json!({"request":{}}),
                mcp_wire::tools::TaskTool::Events => json!({"task_id":"missing","after":0}),
                _ => json!({"task_id":"missing"}),
            };
            let reply = rpc(
                &mut connection,
                index + 3,
                "tools/call",
                json!({"name":tool.name(),"arguments":arguments}),
            )
            .await;
            assert_eq!(reply["result"]["isError"], true);
            assert!(super::super::dispatch::fits(
                &reply,
                policy.worker.dispatch.max_response_bytes.get()
            ));
        }
        drop(connection);
        idle(&service).await;
        let rotated = service
            .rotate(app.handle().clone(), endpoint.path())
            .await
            .unwrap();
        assert!(
            connect_authenticated(&record, nz(65536), nz(65536), Duration::from_secs(5))
                .await
                .is_err()
        );
        let mut connection = session(&ConnectionRecord::read(&rotated).unwrap()).await;
        let list = rpc(&mut connection, 2, "tools/list", json!({})).await;
        assert!(
            list["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| !tool["name"]
                    .as_str()
                    .unwrap()
                    .starts_with("cartograph_task_"))
        );
        let observed = service.owner_status().unwrap();
        assert_eq!(observed["selected_client"], client.as_str());
        assert!(observed["task_grant"].is_null());
        drop(connection);
        service.disable().await.unwrap();
        idle(&service).await;
        assert!(!rotated.exists());
    });
}
