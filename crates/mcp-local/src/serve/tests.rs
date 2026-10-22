//! #718 native lifecycle foundation, not installed application acceptance.
use super::*;
use crate::endpoint::Endpoint;
use serde_json::json;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::net::UnixStream;

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}
fn cap() -> NonZeroUsize {
    NonZeroUsize::new(4096).unwrap()
}
fn pair() -> (Connection, Connection) {
    let (left, right) = UnixStream::pair().unwrap();
    (
        Connection::from_stream(left, cap(), cap(), Duration::from_secs(1)).unwrap(),
        Connection::from_stream(right, cap(), cap(), Duration::from_secs(1)).unwrap(),
    )
}
fn preamble(capability: &Capability) -> Value {
    json!({"protocol":mcp_wire::capability::LOCAL_PROTOCOL,"capability":capability.expose_for_local_transport()})
}
struct Echo {
    calls: Arc<AtomicUsize>,
}
impl Session for Echo {
    fn dispatch(
        &mut self,
        message: Value,
    ) -> impl Future<Output = Result<Reply, ServeError>> + Send {
        self.calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if message.get("error") == Some(&json!(true)) {
                return Err(ServeError::Application);
            }
            Ok(if message.get("ignore") == Some(&json!(true)) {
                Reply::Ignore
            } else if message.get("close") == Some(&json!(true)) {
                Reply::Close
            } else {
                Reply::Respond(message)
            })
        }
    }
}

#[test]
fn private_endpoint_authenticates_before_factory_and_serves_sequentially() {
    run(async {
        let parent = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(parent.path()).unwrap();
        let client = UnixStream::connect(endpoint.socket_path()).await.unwrap();
        let server = endpoint.accept().await.unwrap();
        let mut client =
            Connection::from_stream(client, cap(), cap(), Duration::from_secs(1)).unwrap();
        let server = Connection::from_stream(server, cap(), cap(), Duration::from_secs(1)).unwrap();
        let capability = Capability::generate().unwrap();
        let auth = preamble(&capability);
        let (activation, revoked) = watch::channel(false);
        let constructed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&constructed);
        let calls = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&calls);
        let task = tokio::spawn(async move {
            serve_connection(
                server,
                &capability,
                Duration::from_secs(2),
                revoked,
                move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async move { Ok(Echo { calls: received }) }
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(constructed.load(Ordering::SeqCst), 0);
        client.write(&auth).await.unwrap();
        assert!(mcp_wire::capability::is_acknowledgement(
            &client.read().await.unwrap().unwrap()
        ));
        client.write(&json!({"ignore":true})).await.unwrap();
        client.write(&json!({"id":7,"result":{}})).await.unwrap();
        assert_eq!(
            client.read().await.unwrap(),
            Some(json!({"id":7,"result":{}}))
        );
        assert_eq!(constructed.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        client.shutdown();
        assert_eq!(task.await.unwrap(), Ok(()));
        drop(activation);
    });
}

#[test]
fn unauthenticated_peers_and_initial_revocation_never_invoke_factory() {
    run(async {
        for rejected_before_auth in [false, true] {
            let (server, mut client) = pair();
            let capability = Capability::generate().unwrap();
            let (activation, revoked) = watch::channel(rejected_before_auth);
            let constructed = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&constructed);
            if !rejected_before_auth {
                client
                    .write(&json!({"protocol":"wrong","capability":"wrong"}))
                    .await
                    .unwrap();
            }
            let result = serve_connection(
                server,
                &capability,
                Duration::from_secs(1),
                revoked,
                move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Echo {
                            calls: Arc::new(AtomicUsize::new(0)),
                        })
                    }
                },
            )
            .await;
            assert_eq!(
                result,
                Err(if rejected_before_auth {
                    ServeError::Revoked
                } else {
                    ServeError::Authentication
                })
            );
            assert_eq!(constructed.load(Ordering::SeqCst), 0);
            assert_eq!(client.read().await.unwrap(), None);
            drop(activation);
        }
        let (server, mut client) = pair();
        let capability = Capability::generate().unwrap();
        let (activation, revoked) = watch::channel(false);
        drop(activation);
        assert_eq!(
            serve_connection(
                server,
                &capability,
                Duration::from_secs(1),
                revoked,
                || -> std::future::Ready<Result<Echo, ServeError>> {
                    panic!("closed activation must not create application session");
                }
            )
            .await,
            Err(ServeError::Revoked)
        );
        assert_eq!(client.read().await.unwrap(), None);
    });
}

#[test]
fn revocation_racing_valid_preamble_cannot_acquire_application_session() {
    run(async {
        let (server, mut client) = pair();
        let capability = Capability::generate().unwrap();
        let auth = preamble(&capability);
        let (activation, revoked) = watch::channel(false);
        let constructed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&constructed);
        let task = async move {
            serve_connection(
                server,
                &capability,
                Duration::from_secs(1),
                revoked,
                move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(Echo {
                            calls: Arc::new(AtomicUsize::new(0)),
                        })
                    }
                },
            )
            .await
        };
        let mut task = std::pin::pin!(task);
        // Poll authentication into its pending read, then make both the valid
        // preamble and revocation ready before polling the server again. This
        // tests precedence without relying on executor/socket scheduling.
        std::future::poll_fn(|cx| {
            assert!(task.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        client.write(&auth).await.unwrap();
        activation.send(true).unwrap();
        assert_eq!(task.await, Err(ServeError::Revoked));
        assert_eq!(constructed.load(Ordering::SeqCst), 0);
        // Dropping a socket with unread preamble bytes may reset its peer on
        // Unix. Both EOF and that I/O closure are legitimate; no ACK or other
        // response, deadline, or malformed frame is accepted here.
        assert!(matches!(
            client.read().await,
            Ok(None) | Err(crate::connection::ConnectionError::Io)
        ));
    });
}

#[test]
fn whole_session_expiry_covers_unauthenticated_and_pending_factory_work() {
    run(async {
        for authenticated in [false, true] {
            let (server, mut client) = pair();
            let capability = Capability::generate().unwrap();
            let auth = preamble(&capability);
            let (activation, revoked) = watch::channel(false);
            let constructed = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&constructed);
            let task = tokio::spawn(async move {
                serve_connection(
                    server,
                    &capability,
                    Duration::from_millis(20),
                    revoked,
                    move || {
                        counter.fetch_add(1, Ordering::SeqCst);
                        std::future::pending::<Result<Echo, ServeError>>()
                    },
                )
                .await
            });
            if authenticated {
                client.write(&auth).await.unwrap();
                assert!(mcp_wire::capability::is_acknowledgement(
                    &client.read().await.unwrap().unwrap()
                ));
            }
            assert_eq!(task.await.unwrap(), Err(ServeError::Expired));
            assert_eq!(
                constructed.load(Ordering::SeqCst),
                usize::from(authenticated)
            );
            assert_eq!(client.read().await.unwrap(), None);
            drop(activation);
        }
    });
}

#[test]
fn external_cancellation_and_sender_drop_close_pending_factory_streams() {
    run(async {
        for abort in [false, true] {
            let (server, mut client) = pair();
            let capability = Capability::generate().unwrap();
            let auth = preamble(&capability);
            let (activation, revoked) = watch::channel(false);
            let task = tokio::spawn(async move {
                serve_connection(server, &capability, Duration::from_secs(1), revoked, || {
                    std::future::pending::<Result<Echo, ServeError>>()
                })
                .await
            });
            client.write(&auth).await.unwrap();
            assert!(mcp_wire::capability::is_acknowledgement(
                &client.read().await.unwrap().unwrap()
            ));
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                drop(activation);
            } else {
                drop(activation);
                assert_eq!(task.await.unwrap(), Err(ServeError::Revoked));
            }
            assert_eq!(client.read().await.unwrap(), None);
        }
    });
}

#[test]
fn zero_lifetime_closes_before_authentication_or_factory() {
    run(async {
        let (server, mut client) = pair();
        let capability = Capability::generate().unwrap();
        let (_activation, revoked) = watch::channel(false);
        assert_eq!(
            serve_connection(server, &capability, Duration::ZERO, revoked, || async {
                Ok(Echo {
                    calls: Arc::new(AtomicUsize::new(0)),
                })
            })
            .await,
            Err(ServeError::InvalidLifetime)
        );
        assert_eq!(client.read().await.unwrap(), None);
    });
}

struct Stalled {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Session for Stalled {
    fn dispatch(&mut self, _: Value) -> impl Future<Output = Result<Reply, ServeError>> + Send {
        let _ = self.entered.take().unwrap().send(());
        std::future::pending()
    }
}

#[test]
fn revocation_and_expiry_drop_stream_during_application_dispatch() {
    run(async {
        for revoke in [true, false] {
            let (server, mut client) = pair();
            let capability = Capability::generate().unwrap();
            let auth = preamble(&capability);
            let (activation, revoked) = watch::channel(false);
            let (entered, started) = tokio::sync::oneshot::channel();
            let lifetime = if revoke {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(50)
            };
            let task = tokio::spawn(async move {
                serve_connection(server, &capability, lifetime, revoked, || async {
                    Ok(Stalled {
                        entered: Some(entered),
                    })
                })
                .await
            });
            client.write(&auth).await.unwrap();
            assert!(mcp_wire::capability::is_acknowledgement(
                &client.read().await.unwrap().unwrap()
            ));
            client.write(&json!({"id":1})).await.unwrap();
            started.await.unwrap();
            if revoke {
                activation.send(true).unwrap();
            }
            assert_eq!(
                task.await.unwrap(),
                Err(if revoke {
                    ServeError::Revoked
                } else {
                    ServeError::Expired
                })
            );
            assert_eq!(client.read().await.unwrap(), None);
        }
    });
}

#[test]
fn application_close_and_errors_drop_the_native_stream() {
    run(async {
        for error in [false, true] {
            let (server, mut client) = pair();
            let capability = Capability::generate().unwrap();
            let auth = preamble(&capability);
            let (_activation, revoked) = watch::channel(false);
            let task = tokio::spawn(async move {
                serve_connection(
                    server,
                    &capability,
                    Duration::from_secs(1),
                    revoked,
                    || async {
                        Ok(Echo {
                            calls: Arc::new(AtomicUsize::new(0)),
                        })
                    },
                )
                .await
            });
            client.write(&auth).await.unwrap();
            assert!(mcp_wire::capability::is_acknowledgement(
                &client.read().await.unwrap().unwrap()
            ));
            client
                .write(&if error {
                    json!({"error":true})
                } else {
                    json!({"close":true})
                })
                .await
                .unwrap();
            assert_eq!(
                task.await.unwrap(),
                if error {
                    Err(ServeError::Application)
                } else {
                    Ok(())
                }
            );
            assert_eq!(client.read().await.unwrap(), None);
        }
    });
}
