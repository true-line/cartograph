use super::*;
use crate::{handshake::connect_authenticated, record::ConnectionRecord, serve::Reply};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Echo;
impl Session for Echo {
    async fn dispatch(&mut self, value: Value) -> Result<Reply, ServeError> {
        Ok(Reply::Respond(value))
    }
}
fn limits() -> ActivationLimits {
    ActivationLimits {
        max_connections: NonZeroUsize::new(1).unwrap(),
        read_frame_bytes: NonZeroUsize::new(4096).unwrap(),
        write_frame_bytes: NonZeroUsize::new(4096).unwrap(),
        frame_deadline: Duration::from_secs(2),
        session_lifetime: Duration::from_secs(5),
    }
}
fn run(f: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}
#[test]
fn cap_revocation_cleanup_and_fresh_capability() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let activation = Activation::start(dir.path(), limits(), move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok(Echo) }
        })
        .unwrap();
        let path = activation.record_path().to_owned();
        let old = ConnectionRecord::read(&path).unwrap();
        let old_secret = old.authentication_preamble();
        // An unauthenticated connection occupies the same bounded task slot.
        let mut unauth = old
            .connect(
                limits().read_frame_bytes,
                limits().write_frame_bytes,
                limits().frame_deadline,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut excess = old
            .connect(
                limits().read_frame_bytes,
                limits().write_frame_bytes,
                limits().frame_deadline,
            )
            .await
            .unwrap();
        assert!(matches!(excess.read().await, Ok(None) | Err(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        activation.disable().await.unwrap();
        assert!(!path.exists());
        assert!(matches!(unauth.read().await, Ok(None) | Err(_)));
        let activation = Activation::start(dir.path(), limits(), || async { Ok(Echo) }).unwrap();
        assert_ne!(activation.record_path(), path);
        let fresh = ConnectionRecord::read(activation.record_path()).unwrap();
        let mut stale = fresh
            .connect(
                limits().read_frame_bytes,
                limits().write_frame_bytes,
                limits().frame_deadline,
            )
            .await
            .unwrap();
        stale.write(&old_secret).await.unwrap();
        assert!(matches!(stale.read().await, Ok(None) | Err(_)));
        let mut client = connect_authenticated(
            &fresh,
            limits().read_frame_bytes,
            limits().write_frame_bytes,
            limits().frame_deadline,
        )
        .await
        .unwrap();
        client.write(&json!({"ping":1})).await.unwrap();
        assert_eq!(client.read().await.unwrap(), Some(json!({"ping":1})));
        activation.disable().await.unwrap();
        assert!(matches!(client.read().await, Ok(None) | Err(_)));
    });
}
#[test]
fn drop_requests_cleanup_and_failure_status_stops() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let activation = Activation::start(dir.path(), limits(), || async { Ok(Echo) }).unwrap();
        let record = activation.record_path().to_owned();
        drop(activation);
        tokio::time::timeout(Duration::from_secs(2), async {
            while record.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let activation = Activation::start(dir.path(), limits(), || async { Ok(Echo) }).unwrap();
        let record = ConnectionRecord::read(activation.record_path()).unwrap();
        let socket = activation.record_path().parent().unwrap().join("socket");
        std::fs::remove_file(&socket).unwrap();
        // Wake a pending accept on its still-bound listener is impossible after unlink;
        // the next accept must validate identity before waiting. A completed session
        // wakes the accept loop, which then observes the missing endpoint.
        drop(record);
        // Initial owner scheduling observes the removed endpoint before acceptance.
        tokio::time::timeout(Duration::from_secs(2), async {
            while activation.status() == ActivationStatus::Running {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            activation.status(),
            ActivationStatus::Failed(ActivationError::Endpoint)
        );
        assert_eq!(activation.disable().await, Err(ActivationError::Endpoint));
    });
}
#[test]
fn invalid_duration_does_not_publish() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let mut config = limits();
        config.frame_deadline = Duration::ZERO;
        assert!(matches!(
            Activation::start(dir.path(), config, || async { Ok(Echo) }),
            Err(ActivationError::InvalidLimits)
        ));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    });
}
