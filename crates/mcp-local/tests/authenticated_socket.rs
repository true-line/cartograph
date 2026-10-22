//! #718 / AC-0289 native fixture integration; not packaged-client acceptance.
#![cfg(unix)]
use mcp_local::{connection::Connection, endpoint::Endpoint};
use mcp_wire::{
    admission::{Action, Admission},
    capability::{BridgeAction, BridgeAdmission, Capability, LOCAL_PROTOCOL},
};
use serde_json::json;
use std::{num::NonZeroUsize, time::Duration};

#[test]
fn real_socket_authentication_ping_and_revocation() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let parent = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(parent.path()).unwrap();
        let limit = NonZeroUsize::new(4096).unwrap();
        let deadline = Duration::from_secs(2);
        let mut client = Connection::connect(&endpoint.socket_path(), limit, limit, deadline).await.unwrap();
        let mut server = Connection::from_stream(endpoint.accept().await.unwrap(), limit, limit, deadline).unwrap();
        let capability = Capability::generate().unwrap();
        let mut gate = BridgeAdmission::new(Admission::new([], NonZeroUsize::new(8).unwrap()));
        client.write(&json!({"protocol":LOCAL_PROTOCOL,"capability":capability.expose_for_local_transport()})).await.unwrap();
        assert_eq!(gate.receive(server.read().await.unwrap().unwrap(), &capability), BridgeAction::Authenticated);
        client.write(&json!({"jsonrpc":"2.0","id":7,"method":"ping"})).await.unwrap();
        let BridgeAction::Protocol(Action::Respond(response)) = gate.receive(server.read().await.unwrap().unwrap(), &capability) else { panic!() };
        server.write(&response).await.unwrap();
        assert_eq!(client.read().await.unwrap(), Some(json!({"jsonrpc":"2.0","id":7,"result":{}})));
        gate.revoke();
        client.write(&json!({"jsonrpc":"2.0","id":8,"method":"ping"})).await.unwrap();
        assert_eq!(gate.receive(server.read().await.unwrap().unwrap(), &capability), BridgeAction::Reject);
        server.shutdown();
        assert!(client.read().await.unwrap().is_none());
    });
}

#[test]
fn private_record_client_requires_exact_ack_before_mcp() {
    use mcp_local::{handshake::connect_authenticated, record::ConnectionRecord};
    use mcp_wire::capability::acknowledgement;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for valid_ack in [true, false] {
                let parent = tempfile::tempdir().unwrap();
                let mut endpoint = Endpoint::bind(parent.path()).unwrap();
                let secret = Capability::generate().unwrap();
                let record_path = endpoint.publish(&secret).unwrap();
                let record = ConnectionRecord::read(&record_path).unwrap();
                let limit = NonZeroUsize::new(4096).unwrap();
                let deadline = Duration::from_secs(2);
                let server = tokio::spawn(async move {
                    let mut connection = Connection::from_stream(
                        endpoint.accept().await.unwrap(),
                        limit,
                        limit,
                        deadline,
                    )
                    .unwrap();
                    let mut gate =
                        BridgeAdmission::new(Admission::new([], NonZeroUsize::new(8).unwrap()));
                    assert_eq!(
                        gate.receive(connection.read().await.unwrap().unwrap(), &secret),
                        BridgeAction::Authenticated
                    );
                    let ack = if valid_ack {
                        acknowledgement()
                    } else {
                        json!({"protocol":LOCAL_PROTOCOL,"authenticated":false})
                    };
                    connection.write(&ack).await.unwrap();
                    if valid_ack {
                        let BridgeAction::Protocol(Action::Respond(response)) =
                            gate.receive(connection.read().await.unwrap().unwrap(), &secret)
                        else {
                            panic!()
                        };
                        connection.write(&response).await.unwrap();
                    } else {
                        // A failed ACK must cause EOF, never forwarding an MCP request.
                        assert!(connection.read().await.unwrap().is_none());
                    }
                });
                let connected = connect_authenticated(&record, limit, limit, deadline).await;
                if valid_ack {
                    let mut client = connected.unwrap();
                    client
                        .write(&json!({"jsonrpc":"2.0","id":9,"method":"ping"}))
                        .await
                        .unwrap();
                    assert_eq!(
                        client.read().await.unwrap(),
                        Some(json!({"jsonrpc":"2.0","id":9,"result":{}}))
                    );
                } else {
                    assert!(connected.is_err());
                }
                server.await.unwrap();
            }
        });
}

#[test]
fn accepted_handshake_never_dispatches_before_authentication_and_closes_failures() {
    // AC-0289: actual accepted transport, with no context construction on failure.
    use mcp_local::handshake::authenticate_accepted;
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        for valid in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let endpoint = Endpoint::bind(parent.path()).unwrap();
            let secret = Capability::generate().unwrap();
            let preamble = if valid {
                json!({"protocol":LOCAL_PROTOCOL,"capability":secret.expose_for_local_transport()})
            } else { json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}) };
            let cap = NonZeroUsize::new(4096).unwrap();
            let deadline = Duration::from_secs(2);
            let mut client = Connection::connect(&endpoint.socket_path(), cap, cap, deadline).await.unwrap();
            let server = Connection::from_stream(endpoint.accept().await.unwrap(),cap,cap,deadline).unwrap();
            client.write(&preamble).await.unwrap();
            let result = authenticate_accepted(server, &secret).await;
            if valid {
                let mut server = result.unwrap();
                assert!(mcp_wire::capability::is_acknowledgement(&client.read().await.unwrap().unwrap()));
                client.write(&json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await.unwrap();
                assert_eq!(server.read().await.unwrap().unwrap()["id"], 2);
            } else {
                assert!(result.is_err());
                assert!(client.read().await.unwrap().is_none());
            }
        }
    });
}
