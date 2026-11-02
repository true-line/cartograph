//! Explicit controlled evidence only; never actual-model acceptance.
use super::*;
use std::{io::Read, net::TcpListener, sync::mpsc, thread, time::Duration};

struct WithheldResponse {
    endpoint: String,
    release: mpsc::Sender<()>,
    server: Option<thread::JoinHandle<Vec<String>>>,
}
impl WithheldResponse {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (release, stopped) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut streams = Vec::new();
            let mut requests = Vec::new();
            loop {
                if !matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    return requests;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let limit =
                            llm::bounded::CompletionLimitValues::default().request_bytes as usize;
                        let header_end = loop {
                            let mut byte = [0];
                            stream.read_exact(&mut byte).unwrap();
                            bytes.push(byte[0]);
                            assert!(bytes.len() <= limit);
                            if bytes.ends_with(b"\r\n\r\n") {
                                break bytes.len();
                            }
                        };
                        let headers = std::str::from_utf8(&bytes).unwrap();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .expect("bounded provider request has content length");
                        assert!(length <= limit);
                        bytes.resize(header_end + length, 0);
                        stream.read_exact(&mut bytes[header_end..]).unwrap();
                        requests.push(core_prov::content_hash(&bytes));
                        // Keep the connection open without sending headers or a body.
                        streams.push(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("loopback accept failed: {error}"),
                }
            }
        });
        Self {
            endpoint,
            release,
            server: Some(server),
        }
    }
    fn finish(mut self) -> Vec<String> {
        let _ = self.release.send(());
        self.server.take().unwrap().join().unwrap()
    }
}
impl Drop for WithheldResponse {
    fn drop(&mut self) {
        let _ = self.release.send(());
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

// AC-0185, AC-0287: admitted worker timeout, terminal durable evidence and no retry.
// Deliberately uses unchanged production timeouts; explicit execution authority required.
#[test]
#[ignore = "controlled #598 witness: requires separately authorized production-duration timeout"]
fn admitted_analyst_elapsed_timeout_reopens_without_next_call() {
    let server = WithheldResponse::new();
    // Use the same policy-to-provider derivation as the worker preparation path.
    let limits = crate::investigations::provider_workspace::completion_limit_values(
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let timeout = Duration::from_millis(limits.request_timeout_ms);
    let provider = Arc::new(
        llm::OllamaProvider::new(&server.endpoint, "controlled-timeout-no-model", timeout).unwrap(),
    );
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18);
    let admitted_limits = {
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        jobs.investigation(&f.id).unwrap().limits
    };
    let before = Instant::now();
    f.run(provider);
    let elapsed = before.elapsed();
    // A server panic fails this join; it cannot produce a passing timeout receipt.
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(elapsed >= timeout);
    // Witness watchdog only; does not change any production or provider limit.
    assert!(elapsed < timeout + Duration::from_secs(60));
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    let reason = InvestigationStopReason::BoundedProvider(llm::bounded::BoundedCallError {
        code: llm::bounded::FailureCode::DeadlineExceeded,
        outcome: llm::bounded::InvocationOutcome::Unknown,
    });
    assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
    assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
    assert_eq!(detail.limits, admitted_limits);
    assert_eq!(detail.limits, INVESTIGATION_V2_POLICY.limits());
    assert_eq!(detail.usage.model_invocations, 1);
    assert_eq!(
        detail.usage.generated_token_reservations,
        detail.limits.generated_tokens_per_invocation
    );
    assert!(detail.usage.active_milliseconds >= limits.request_timeout_ms);
    assert_eq!(detail.usage.reported_input_tokens, None);
    assert_eq!(detail.usage.reported_output_tokens, None);
    assert_eq!(detail.usage.tool_actions, 0);
    assert_eq!(detail.usage.evidence_items, 0);
    assert!(jobs.investigation_result(&f.id).unwrap().is_none());
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert!(!events.has_more);
    assert!(
        !events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::QueryRefused)
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
    assert!(reopened.investigation_result(&f.id).unwrap().is_none());
    println!(
        "{}",
        json!({
            "schema":"cartograph-controlled-timeout-witness@1",
            "origin":"local withholding HTTP stub; no actual model",
            "specialist":"domain-analyst@18", "task_id":f.id,
            "elapsed_ms":elapsed.as_millis(), "configured_request_timeout_ms":limits.request_timeout_ms,
            "request_hashes":requests, "detail":detail, "events":events,
            "durable_reopen_equal":true
        })
    );
}
