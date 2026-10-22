//! #718 / AC-0289 Unix transport foundation, not server acceptance.
use super::*;
use serde_json::json;
use std::future::Future;

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}
fn cap(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}
fn connection(stream: UnixStream, size: usize, deadline: Duration) -> Connection {
    Connection::from_stream(stream, cap(size), cap(size), deadline).unwrap()
}

#[test]
fn pair_verifies_peer_and_roundtrips() {
    run(async {
        let (left, right) = UnixStream::pair().unwrap();
        assert_eq!(
            left.peer_cred().unwrap().uid(),
            rustix::process::geteuid().as_raw()
        );
        let mut left = connection(left, 128, Duration::from_secs(1));
        let mut right = connection(right, 128, Duration::from_secs(1));
        let message = json!({"text":"é🙂\n"});
        left.write(&message).await.unwrap();
        assert_eq!(right.read().await.unwrap(), Some(message));
        right.write(&json!({"ok":true})).await.unwrap();
        assert_eq!(left.read().await.unwrap(), Some(json!({"ok":true})));
        left.shutdown();
        assert_eq!(right.read().await.unwrap(), None);
        assert_eq!(right.read().await, Err(ConnectionError::Io));
    });
}

#[test]
fn fragmented_utf8_crlf_and_exact_limits() {
    run(async {
        let (left, mut right) = UnixStream::pair().unwrap();
        let input = "\"é🙂\"\r\n";
        let mut left = connection(left, input.len() - 1, Duration::from_secs(1));
        let sender = tokio::spawn(async move {
            for byte in input.as_bytes() {
                right.write_all(&[*byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        assert_eq!(left.read().await.unwrap(), Some(json!("é🙂")));
        sender.await.unwrap();
    });
}

#[test]
fn hostile_frames_fail_closed() {
    run(async {
        for (input, expected) in [
            (b"{}\r\n".as_slice(), mcp_wire::FrameError::Limit),
            (b"xxxxxx", mcp_wire::FrameError::Limit),
            (b"{\n", mcp_wire::FrameError::InvalidJson),
            (b"\xff\n", mcp_wire::FrameError::InvalidJson),
            (b"{}", mcp_wire::FrameError::Unterminated),
        ] {
            let (left, mut right) = UnixStream::pair().unwrap();
            let mut left = connection(left, 2, Duration::from_secs(1));
            right.write_all(input).await.unwrap();
            drop(right);
            assert_eq!(left.read().await, Err(ConnectionError::Frame(expected)));
            assert_eq!(left.read().await, Err(ConnectionError::Io));
        }
    });
}

#[test]
fn idle_and_slowloris_use_total_frame_deadline() {
    run(async {
        let (left, _right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 64, Duration::from_millis(20));
        assert_eq!(left.read().await, Err(ConnectionError::Deadline));
        let (left, mut right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 64, Duration::from_millis(40));
        let sender = tokio::spawn(async move {
            loop {
                if right.write_all(b" ").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        assert_eq!(left.read().await, Err(ConnectionError::Deadline));
        sender.await.unwrap();
    });
}

#[test]
fn oversize_serialization_emits_no_payload_and_drops_stream() {
    run(async {
        let (left, mut right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 2, Duration::from_secs(1));
        assert_eq!(
            left.write(&json!("\0")).await,
            Err(ConnectionError::Frame(mcp_wire::FrameError::Limit))
        );
        let mut bytes = [0; 8];
        use tokio::io::AsyncReadExt;
        assert_eq!(right.read(&mut bytes).await.unwrap(), 0);
        assert_eq!(left.write(&json!({})).await, Err(ConnectionError::Io));
    });
}

#[test]
fn disconnected_peer_and_invalid_deadline() {
    run(async {
        let (left, right) = UnixStream::pair().unwrap();
        assert!(matches!(
            Connection::from_stream(left, cap(2), cap(2), Duration::ZERO),
            Err(ConnectionError::InvalidDeadline)
        ));
        drop(right);
        let (left, right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 2, Duration::from_secs(1));
        drop(right);
        assert_eq!(left.write(&json!({})).await, Err(ConnectionError::Io));
    });
}

#[test]
fn client_connect_verifies_real_listener_and_missing_app_fails() {
    run(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let mut client = Connection::connect(&path, cap(2), cap(2), Duration::from_secs(1))
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut server = connection(stream, 2, Duration::from_secs(1));
        client.write(&json!({})).await.unwrap();
        assert_eq!(server.read().await.unwrap(), Some(json!({})));
        assert!(matches!(
            Connection::connect(
                &directory.path().join("missing"),
                cap(2),
                cap(2),
                Duration::from_secs(1)
            )
            .await,
            Err(ConnectionError::Io)
        ));
    });
}

#[test]
fn blocked_write_and_cancelled_read_close_connection() {
    run(async {
        let (left, _right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 8 * 1024 * 1024, Duration::from_millis(20));
        assert_eq!(
            left.write(&json!("x".repeat(4 * 1024 * 1024))).await,
            Err(ConnectionError::Deadline)
        );
        assert_eq!(left.read().await, Err(ConnectionError::Io));
        let (left, _right) = UnixStream::pair().unwrap();
        let mut left = connection(left, 64, Duration::from_secs(10));
        assert!(
            timeout(Duration::from_millis(10), left.read())
                .await
                .is_err()
        );
        assert_eq!(left.read().await, Err(ConnectionError::Io));
    });
}
