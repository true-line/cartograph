//! #718 relay foundation: isolated fake app, never production-client acceptance.
#![cfg(unix)]
use mcp_local::{connection::Connection, endpoint::Endpoint};
use mcp_wire::{
    admission::{Action, Admission},
    capability::{BridgeAction, BridgeAdmission, Capability, acknowledgement},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(record: &Path, cap: usize) -> Process {
    Process(
        Command::new(env!("CARGO_BIN_EXE_cartograph-mcp"))
            .args(["--record"])
            .arg(record)
            .args([
                "--max-frame-bytes",
                &cap.to_string(),
                "--timeout-ms",
                "2000",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
fn finish(child: &mut Process) -> (bool, Vec<u8>, String) {
    let end = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < end, "child did not exit");
        thread::sleep(Duration::from_millis(5));
    };
    let mut output = Vec::new();
    child
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let mut error = String::new();
    child
        .0
        .stderr
        .as_mut()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    (status.success(), output, error)
}
fn fixture<F, Fut>(handler: F) -> (PathBuf, thread::JoinHandle<()>)
where
    F: FnOnce(Connection, Capability) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let mut endpoint = Endpoint::bind(directory.path()).unwrap();
                let capability = Capability::generate().unwrap();
                sender.send(endpoint.publish(&capability).unwrap()).unwrap();
                let stream = endpoint.accept().await.unwrap();
                let cap = NonZeroUsize::new(8192).unwrap();
                handler(
                    Connection::from_stream(stream, cap, cap, Duration::from_secs(2)).unwrap(),
                    capability,
                )
                .await;
            });
    });
    (receiver.recv().unwrap(), server)
}
#[test]
fn actual_child_bridges_lifecycle_ping_and_ignored_notifications() {
    let (path, server) = fixture(|mut connection, capability| async move {
        let mut gate = BridgeAdmission::new(Admission::new([], NonZeroUsize::new(16).unwrap()));
        assert_eq!(
            gate.receive(connection.read().await.unwrap().unwrap(), &capability),
            BridgeAction::Authenticated
        );
        connection.write(&acknowledgement()).await.unwrap();
        loop {
            let Some(message) = connection.read().await.unwrap() else {
                break;
            };
            let is_ping = message.get("method") == Some(&json!("ping"));
            match gate.receive(message, &capability) {
                BridgeAction::Protocol(Action::Respond(response)) => {
                    connection.write(&response).await.unwrap()
                }
                BridgeAction::Protocol(Action::Ignore) => {}
                _ => panic!("unexpected dispatch"),
            }
            if is_ping {
                break;
            }
        }
    });
    let mut child = spawn(&path, 4096);
    let mut stdin = child.0.stdin.take().unwrap();
    for value in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"clear_graph"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
    ] {
        writeln!(stdin, "{value}").unwrap();
    }
    let (success, output, error) = finish(&mut child);
    assert!(success, "{error}");
    assert!(error.is_empty());
    let values: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], 1);
    assert_eq!(values[1], json!({"jsonrpc":"2.0","id":2,"result":{}}));
    drop(stdin);
    server.join().unwrap();
}

#[test]
fn bad_ack_and_missing_app_produce_no_stdout() {
    let (path, server) = fixture(|mut connection, _| async move {
        connection.read().await.unwrap().unwrap();
        connection.write(&json!({"not":"ack"})).await.unwrap();
    });
    let mut child = spawn(&path, 4096);
    let (success, output, error) = finish(&mut child);
    assert!(!success);
    assert!(output.is_empty());
    assert_eq!(error, "local authentication failed or app unavailable\n");
    server.join().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut child = spawn(&directory.path().join("connection.json"), 4096);
    let (success, output, error) = finish(&mut child);
    assert!(!success);
    assert!(output.is_empty());
    assert_eq!(error, "local app unavailable or record invalid\n");
}

#[test]
fn oversized_client_frame_fails_without_forwarding_or_hanging() {
    let (path, server) = fixture(|mut connection, _| async move {
        connection.read().await.unwrap().unwrap();
        connection.write(&acknowledgement()).await.unwrap();
        assert_eq!(connection.read().await.unwrap(), None);
    });
    let mut child = spawn(&path, 256);
    let mut stdin = child.0.stdin.take().unwrap();
    writeln!(stdin, "{}", "x".repeat(257)).unwrap();
    let (success, output, error) = finish(&mut child);
    assert!(!success);
    assert!(output.is_empty());
    assert_eq!(error, "local relay frame failed\n");
    drop(stdin);
    server.join().unwrap();
}

#[test]
fn buffered_frame_after_ack_is_preserved_and_app_exit_stops_open_stdin() {
    let notification = json!({"jsonrpc":"2.0","method":"notifications/test","params":{}});
    let expected = notification.clone();
    let (path, server) = fixture(move |mut connection, _| async move {
        connection.read().await.unwrap().unwrap();
        let mut bytes = Vec::new();
        let cap = NonZeroUsize::new(4096).unwrap();
        mcp_wire::write_frame(&mut bytes, &acknowledgement(), cap).unwrap();
        mcp_wire::write_frame(&mut bytes, &notification, cap).unwrap();
        let mut stream = connection.into_stream().unwrap();
        use tokio::io::AsyncWriteExt;
        stream.get_mut().write_all(&bytes).await.unwrap();
    });
    let mut child = spawn(&path, 4096);
    let stdin = child.0.stdin.take().unwrap();
    let (success, output, error) = finish(&mut child);
    assert!(success, "{error}");
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), expected);
    drop(stdin);
    server.join().unwrap();
}

#[test]
fn explicit_arguments_are_required_and_invalid_values_are_not_echoed() {
    for args in [
        vec![],
        vec![
            "--record",
            "relative",
            "--max-frame-bytes",
            "1",
            "--timeout-ms",
            "1",
        ],
        vec![
            "--record",
            "/private/secret/connection.json",
            "--max-frame-bytes",
            "0",
            "--timeout-ms",
            "1",
        ],
        vec![
            "--record",
            "/private/secret/connection.json",
            "--max-frame-bytes",
            "1",
            "--timeout-ms",
            "0",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cartograph-mcp"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"invalid relay arguments\n");
    }
}

#[test]
fn stdin_eof_half_closes_socket_and_drains_final_response() {
    let (path, server) = fixture(|mut connection, _| async move {
        connection.read().await.unwrap().unwrap();
        connection.write(&acknowledgement()).await.unwrap();
        let mut stream = connection.into_stream().unwrap();
        let cap = NonZeroUsize::new(4096).unwrap();
        let deadline = Duration::from_secs(2);
        let message = mcp_local::connection::read_frame(&mut stream, cap, deadline)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message["method"], "ping");
        // Wait for relay write-half shutdown before producing the final result.
        assert_eq!(
            mcp_local::connection::read_frame(&mut stream, cap, deadline)
                .await
                .unwrap(),
            None
        );
        mcp_local::connection::write_frame(
            stream.get_mut(),
            &json!({"jsonrpc":"2.0","id":1,"result":{}}),
            cap,
            deadline,
        )
        .await
        .unwrap();
    });
    let mut child = spawn(&path, 4096);
    writeln!(
        child.0.stdin.as_mut().unwrap(),
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"ping"})
    )
    .unwrap();
    drop(child.0.stdin.take());
    let (success, output, error) = finish(&mut child);
    assert!(success, "{error}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output).unwrap(),
        json!({"jsonrpc":"2.0","id":1,"result":{}})
    );
    server.join().unwrap();
}
