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
            let deadline = Instant::now() + Duration::from_secs(45);
            let mut approved = String::new();
            while !done.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    let state = app.state::<AppState>();
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .cancel_investigation(&id)
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
                    state.investigations.wake(&id);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        f.run(provider.clone());
        done.store(true, Ordering::SeqCst);
    });
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
