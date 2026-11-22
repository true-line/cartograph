//! AC-0570: scripted hosted calls cross the old reservation ceiling without I/O.
use super::*;

struct HostedSource {
    calls: AtomicUsize,
}
impl LlmProvider for HostedSource {
    fn id(&self) -> &str {
        "anthropic:claude-opus-5-5"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        NextProvider::new(false).capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("offline fixture must never embed")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Opus, "unused")
            .unwrap()
            .bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        assert_eq!(
            request.limits().values().max_output_tokens,
            llm::bounded::HOSTED_OUTPUT_TOKENS
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(call < 10, "unexpected extra call");
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let action = if call < 8 {
            // Distinct exact queries build genuine immutable invocation history.
            json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                "max_facts":call+1,"max_bytes":32768,"cursor":null}})
        } else if call == 8 {
            let option = &prompt["compact_context"]["evidence_options"][0];
            let range = option["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|range| range["role"] == "definition_initializer")
                .unwrap();
            json!({"type":"read_evidence","fact":option["fact"],"role":range["role"],"index":range["index"]})
        } else {
            let citation = prompt["citations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            json!({"schema_version":1,"outcome":"supported","action":{
                "type":"finish","findings":[{"claim_kind":"inferred_interpretation",
                "title":"Observed bounded guard","statement":"The acquired callable includes a conditional exit.",
                "citation_ids":[citation["citation_id"]],"limitations":["Runtime inputs remain unknown."]}],
                "knowledge_completeness":"partial","limitations":["No complete business policy was established."]}})
        };
        Ok(BoundedCompletion {
            text: action.to_string(),
            requested_model: "claude-opus-5-5".into(),
            response_model: Some("claude-opus-5-5".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}

/// Run `f` on a scripted Cloud provider, approving every step it asks for.
fn run_with_approvals(f: &NextFixture, provider: Arc<dyn LlmProvider>) {
    // Standing consent is confined to this temporary scripted fixture.
    {
        let state = f.base.base.app.state::<AppState>();
        let mut settings = state.settings.lock().unwrap();
        settings.set_enabled("T3", true).unwrap();
        settings.set_provider("T3", "cloud").unwrap();
        settings
            .grant_consent(
                "T3",
                r#"{"provider":"offline-scripted-opus","network":false}"#,
            )
            .unwrap();
    }
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let app = f.base.base.app.handle().clone();
        let id = f.id.clone();
        let live = f.live.clone();
        let done = &done;
        scope.spawn(move || {
            // Bound a stalled approval step, not all ten calls under concurrent CI load.
            let mut deadline = Instant::now() + Duration::from_secs(45);
            let mut approved = String::new();
            while !done.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    let state = app.state::<AppState>();
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .cancel_next_prompt_investigation(&id)
                        .unwrap();
                    state.investigations.wake(&id);
                    break;
                }
                let preview = live.pending.lock().unwrap().clone();
                if let Some(preview) = preview
                    && preview.step_id != approved
                {
                    let state = app.state::<AppState>();
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .approve_next_prompt_investigation(
                            &id,
                            preview.revision,
                            &preview.step_id,
                            &preview.preview.payload_hash,
                        )
                        .unwrap();
                    approved = preview.step_id;
                    deadline = Instant::now() + Duration::from_secs(45);
                    state.investigations.wake(&id);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        f.run(provider);
        done.store(true, Ordering::SeqCst);
    });
}

#[test]
fn hosted_ninth_source_call_settles_finishes_and_reopens() {
    let local = NextProvider::new(false);
    let base = CompletionFixture::new(&local, SpecialistId::DomainAnalystV9, true, false, false);
    let request = base.request.clone();
    let provider = Arc::new(HostedSource {
        calls: AtomicUsize::new(0),
    });
    let f = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::DomainAnalystV10,
        base,
        request,
    );
    run_with_approvals(&f, provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 10);
    assert_eq!(
        detail.usage.generated_token_reservations,
        10 * llm::bounded::HOSTED_OUTPUT_TOKENS
    );
    assert_eq!(detail.usage.evidence_items, 1);
    assert!(detail.summary.has_result);
    let db = f.base.base.app_data.join("state.db");
    let bytes = |path: &std::path::Path| -> Vec<u8> {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT body FROM investigation_results WHERE task_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = bytes(&db);
    let reopened = crate::jobs::JobStore::open(&db).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap(),
        detail
    );
    assert_eq!(bytes(&db), before);
}

/// A hosted provider whose transport takes the request and then fails.
struct HostedFailure {
    calls: AtomicUsize,
}
impl LlmProvider for HostedFailure {
    fn id(&self) -> &str {
        "anthropic:claude-opus-5-5"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        NextProvider::new(false).capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("offline fixture must never embed")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Opus, "unused")
            .unwrap()
            .bounded_profile()
    }
    fn complete_bounded(
        &self,
        _: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // What the real transport does: report the body it is about to send,
        // then fail after sending it.
        let dispatching = control.dispatching.expect("a cloud call is recorded");
        assert!(dispatching(FAILED_CALL_BYTES));
        Err(BoundedCallError {
            code: llm::bounded::FailureCode::HttpStatus,
            outcome: llm::bounded::InvocationOutcome::ResponseReceived,
        })
    }
}
const FAILED_CALL_BYTES: u64 = 4_321;

// AC-0580 / #1767: a cloud request that was sent and then failed is in the
// egress record.
#[test]
fn hosted_call_that_fails_after_dispatch_is_in_the_egress_record() {
    let local = NextProvider::new(false);
    let base = CompletionFixture::new(&local, SpecialistId::DomainAnalystV9, true, false, false);
    let request = base.request.clone();
    let provider = Arc::new(HostedFailure {
        calls: AtomicUsize::new(0),
    });
    let f = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::DomainAnalystV10,
        base,
        request,
    );
    let state = f.base.base.app.state::<AppState>();
    let sent = || {
        let settings = state.settings.lock().unwrap();
        settings.egress_summary().unwrap().bytes_sent
    };
    assert_eq!(sent(), 0);
    run_with_approvals(&f, provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_ne!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(sent(), FAILED_CALL_BYTES);
}

// Manual, provider-free compatibility probe against isolated database copies.
#[test]
#[ignore = "requires CARTOGRAPH_HISTORY_COPY; never point at an owner database"]
fn historical_source_limits_database_reopens() {
    let path = std::path::PathBuf::from(std::env::var("CARTOGRAPH_HISTORY_COPY").unwrap());
    assert!(
        path.starts_with("/tmp/cartograph-codex/1724-history-copies")
            || path.starts_with("/private/tmp/cartograph-codex/1724-history-copies")
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let ids = conn
        .prepare("SELECT id FROM investigation_tasks ORDER BY ordinal")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(conn);
    assert!(!ids.is_empty());
    let store = crate::jobs::JobStore::open(&path).unwrap();
    for id in ids {
        let detail = store.investigation(&id).unwrap();
        assert!(matches!(
            detail.summary.status,
            InvestigationStatus::Completed
                | InvestigationStatus::Failed
                | InvestigationStatus::Cancelled
                | InvestigationStatus::OutcomeUnknown
        ));
        let _ = store.investigation_result(&id).unwrap();
    }
}

/// A cloud provider that queries once, reads the source, then follows
/// `script` for every later call. It records whether any call after the read
/// carried source spans or named the withheld read.
struct RefusedSource {
    calls: AtomicUsize,
    spans_after_read: AtomicUsize,
    withheld_seen: AtomicUsize,
    script: fn(usize, &Value) -> Value,
    max_calls: usize,
}
impl RefusedSource {
    fn new(script: fn(usize, &Value) -> Value, max_calls: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            spans_after_read: AtomicUsize::new(0),
            withheld_seen: AtomicUsize::new(0),
            script,
            max_calls,
        })
    }
}
fn insufficient(_: usize, _: &Value) -> Value {
    json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
        "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
        "limitations":["The acquired source was withheld from this provider."]}})
}
impl LlmProvider for RefusedSource {
    fn id(&self) -> &str {
        "anthropic:claude-opus-5-5"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        NextProvider::new(false).capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("offline fixture must never embed")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Opus, "unused")
            .unwrap()
            .bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        _: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(call < self.max_calls, "unexpected extra call");
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        if call >= 2 {
            self.spans_after_read
                .fetch_add(request.payload().spans.len(), Ordering::SeqCst);
            if prompt["withheld_sources"][0]["outcome"] == "firewall_refused" {
                self.withheld_seen.fetch_add(1, Ordering::SeqCst);
            }
        }
        let action = match call {
            0 => json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                "max_facts":1,"max_bytes":32768,"cursor":null}}),
            1 => {
                let option = &prompt["compact_context"]["evidence_options"][0];
                let range = option["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|range| range["role"] == "definition_initializer")
                    .unwrap();
                json!({"type":"read_evidence","fact":option["fact"],"role":range["role"],"index":range["index"]})
            }
            _ => (self.script)(call, &prompt),
        };
        Ok(BoundedCompletion {
            text: action.to_string(),
            requested_model: "claude-opus-5-5".into(),
            response_model: Some("claude-opus-5-5".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}

// AC-1414 / #679: on a cloud run, a read whose path the firewall denies is
// acquired, settled with its refusal in one transaction, and withheld: no
// later call carries its span, the next prompt names it `firewall_refused`,
// and the store keeps the acquisition, the refusal and its event.
#[test]
fn hosted_refused_read_is_withheld_and_recorded() {
    super::super::TEST_READ_DENY_RULES.with(|rules| *rules.borrow_mut() = Some(vec!["*".into()]));
    let local = NextProvider::new(false);
    let base = CompletionFixture::new(&local, SpecialistId::DomainAnalystV9, true, false, false);
    let request = base.request.clone();
    let provider = RefusedSource::new(insufficient, 3);
    let f = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::DomainAnalystV10,
        base,
        request,
    );
    run_with_approvals(&f, provider.clone());
    super::super::TEST_READ_DENY_RULES.with(|rules| *rules.borrow_mut() = None);
    let state = f.base.base.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        3,
        "{:?}",
        detail.error
    );
    assert_eq!(provider.spans_after_read.load(Ordering::SeqCst), 0);
    assert_eq!(provider.withheld_seen.load(Ordering::SeqCst), 1);
    assert_eq!(detail.usage.evidence_items, 1);
    let events = state
        .jobs
        .lock()
        .unwrap()
        .investigation_events(&f.id, 0)
        .unwrap();
    assert!(
        events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ReadFirewallRefused)
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap(),
        detail
    );
    // The refusal is an authentic own-role attempt, so an insufficient-evidence
    // finish with a limitation completes.
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
}

/// Run `provider` on a cloud task whose every source read the firewall
/// refuses, with the source (and optionally the inventory) obligation.
fn run_refusing(provider: &Arc<RefusedSource>, inventory: bool) -> NextFixture {
    super::super::TEST_READ_DENY_RULES.with(|rules| *rules.borrow_mut() = Some(vec!["*".into()]));
    let local = NextProvider::new(false);
    let base = CompletionFixture::new(
        &local,
        SpecialistId::DomainAnalystV9,
        true,
        inventory,
        false,
    );
    let request = base.request.clone();
    let f = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::DomainAnalystV10,
        base,
        request,
    );
    run_with_approvals(&f, provider.clone());
    super::super::TEST_READ_DENY_RULES.with(|rules| *rules.borrow_mut() = None);
    f
}

fn refusal_events(f: &NextFixture) -> usize {
    let state = f.base.base.app.state::<AppState>();
    let events = state
        .jobs
        .lock()
        .unwrap()
        .investigation_events(&f.id, 0)
        .unwrap();
    events
        .items
        .iter()
        .filter(|event| event.kind == InvestigationEventKind::ReadFirewallRefused)
        .count()
}

/// A supported finish that cites only a graph fact, never the source.
fn supported_without_source(_: usize, prompt: &Value) -> Value {
    let citation = prompt["citations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["origin"]["kind"] != "captured_primary_source")
        .unwrap_or_else(|| panic!("no graph citation in {prompt}"));
    json!({"schema_version":1,"outcome":"supported","action":{
        "type":"finish","findings":[{"claim_kind":"inferred_interpretation",
        "title":"Graph-only claim","statement":"The graph records a business rule.",
        "citation_ids":[citation["citation_id"]],"limitations":["The source was withheld."]}],
        "knowledge_completeness":"partial","limitations":["The source was withheld."]}})
}

// AC-1415 / #679: a refused read is acquired but never exactly supplied, so
// a supported finish cannot meet its obligation. The first unmet finish is
// refused with feedback, the refused read does not reset the counter, and
// the second unmet finish fails atomically with no result; reopen matches.
#[test]
fn hosted_refused_read_cannot_support_and_second_unmet_finish_fails() {
    let provider = RefusedSource::new(supported_without_source, 4);
    let f = run_refusing(&provider, false);
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        4,
        "{:?}",
        detail.error
    );
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::ObligationsUnmet)
    );
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 2);
    assert!(jobs.investigation_result(&f.id).unwrap().is_none());
    assert_eq!(provider.spans_after_read.load(Ordering::SeqCst), 0);
    assert_eq!(provider.withheld_seen.load(Ordering::SeqCst), 2);
    assert_eq!(detail.usage.evidence_items, 1);
    drop(jobs);
    assert_eq!(refusal_events(&f), 1);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap(),
        detail
    );
    assert!(reopened.investigation_result(&f.id).unwrap().is_none());
}

/// After the refused source read: an insufficient finish before the
/// inventory is attempted, then the allowed inventory read, then the same
/// insufficient finish.
fn insufficient_then_inventory(call: usize, prompt: &Value) -> Value {
    if call != 3 {
        return insufficient(call, prompt);
    }
    let target = prompt["completion_contract"]["obligations"]["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|obligation| &obligation["target"])
        .find(|target| target.get("inventory_hash").is_some())
        .unwrap();
    json!({"type":"read_coverage_inventory","context_id":target["context_id"],
        "repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768})
}

// AC-1415 / #679: with a refused source read and an allowed inventory read,
// an insufficient finish needs a completed own-role attempt for every
// obligation. Before the inventory is attempted it is refused; after it,
// the same finish completes, with one refusal and the refusal still recorded.
#[test]
fn hosted_mixed_refusal_needs_every_own_role_attempt() {
    let provider = RefusedSource::new(insufficient_then_inventory, 5);
    let f = run_refusing(&provider, true);
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        5,
        "{:?}",
        detail.error
    );
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(provider.spans_after_read.load(Ordering::SeqCst), 0);
    assert_eq!(detail.usage.evidence_items, 1);
    drop(jobs);
    assert_eq!(refusal_events(&f), 1);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap(),
        detail
    );
}
