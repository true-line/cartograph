//! AC-0287: public command orchestration with genuine retained captures and a
//! deterministic in-process client. No network/provider construction occurs.
use super::*;
use llm::bounded::{
    AuthorizedBoundedCompletion, BoundedCallError, BoundedCompletion, CallDirective,
    CompletionControl, ProviderProfile,
};
use llm::{Embedding, LlmProvider, Locality, ProviderCaps, ProviderError};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

struct PublicProvider {
    cloud: bool,
    available: AtomicBool,
    profiles: AtomicUsize,
    calls: AtomicUsize,
}
impl PublicProvider {
    fn new(cloud: bool) -> Self {
        Self {
            cloud,
            available: AtomicBool::new(true),
            profiles: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        }
    }
}
impl LlmProvider for PublicProvider {
    fn id(&self) -> &str {
        "public-entry-scripted"
    }
    fn locality(&self) -> Locality {
        if self.cloud {
            Locality::Cloud
        } else {
            Locality::Local
        }
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("no embeddings in public entry fixture")
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.profiles.fetch_add(1, Ordering::SeqCst);
        if !self.available.load(Ordering::SeqCst) {
            return Err(BoundedCallError::unsupported());
        }
        Ok(ProviderProfile {
            protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.id().into(),
            locality: self.locality(),
            endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
            requested_model: "public-scripted-model".into(),
        })
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let text = match call {
            0 => json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],"max_facts":1,"max_bytes":32768,"cursor":null}}),
            1 => {
                let target = &prompt["completion_contract"]["obligations"]["obligations"][0]["target"];
                json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]})
            },
            2 => {
                let citation = prompt["citations"].as_array().unwrap().iter()
                    .find(|citation| citation["origin"]["kind"] == "captured_primary_source").unwrap();
                json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{
                    "claim_kind":"inferred_interpretation","title":"Bounded source observation",
                    "statement":"The captured callable contains a conditional exit.","citation_ids":[citation["citation_id"]],
                    "limitations":["Runtime inputs remain unknown."]}],"knowledge_completeness":"partial",
                    "limitations":["This result establishes no complete business policy."]}})
            },
            _ => panic!("public entry must not retry or dispatch a fourth call"),
        }.to_string();
        Ok(BoundedCompletion {
            text,
            requested_model: "public-scripted-model".into(),
            response_model: Some("public-scripted-model".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}
fn detail(app: &tauri::AppHandle<tauri::test::MockRuntime>, id: &str) -> InvestigationDetail {
    app.state::<AppState>()
        .jobs
        .lock()
        .unwrap()
        .investigation(id)
        .unwrap()
}
fn wait<T>(mut read: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = read() {
            return value;
        }
        assert!(
            Instant::now() < until,
            "public worker did not reach expected durable state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn finished(app: &tauri::AppHandle<tauri::test::MockRuntime>, id: &str) -> InvestigationDetail {
    wait(|| {
        let value = detail(app, id);
        matches!(
            value.summary.status,
            InvestigationStatus::Completed
                | InvestigationStatus::Cancelled
                | InvestigationStatus::Failed
                | InvestigationStatus::OutcomeUnknown
                | InvestigationStatus::Interrupted
        )
        .then_some(value)
    })
}

#[test]
fn public_completion_start_request7_runs_worker_and_recovers_nonce_without_sources_or_provider() {
    completion_replay(false);
}

// AC-0320/0322: coordinator retry precedes mutable sources/provider and checks the live grant.
#[test]
fn mcp_coordinator_start_recovers_without_sources_or_provider_and_refuses_revoked_grant() {
    completion_replay(true);
}

fn completion_replay(mcp: bool) {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, request, app_data| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        let capacity = std::num::NonZeroUsize::new(1).unwrap();
        let grant = mcp.then(|| {
            let client = state
                .jobs
                .lock()
                .unwrap()
                .create_mcp_client(capacity)
                .unwrap();
            McpTaskActivation::granted(client, capacity)
        });
        let session = grant.as_ref().map(McpTaskActivation::session);
        let start = |request| {
            completion_start::start_access(
                request,
                app.clone(),
                Instant::now(),
                session.as_ref(),
                mcp.then(|| crate::mcp_reads::worker::tests::limits().scope),
            )
        };
        let mut refused_limits = crate::mcp_reads::worker::tests::limits().scope;
        refused_limits.max_source_registry_bytes = std::num::NonZeroUsize::new(1).unwrap();
        if mcp {
            // AC-0327: refuse discovery before durable admission or provider lookup.
            let profiles = provider.profiles.load(Ordering::SeqCst);
            let error = completion_start::start_access(
                request.clone(),
                app.clone(),
                Instant::now(),
                session.as_ref(),
                Some(refused_limits),
            )
            .unwrap_err();
            assert!(
                matches!(error, InvestigationStartError::Rejected { code: "refresh_required", message, .. }
                if message == crate::mcp_reads::ReadError::Limit.to_string())
            );
            assert!(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .existing_investigation_completion_access(&request, session.as_ref())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        }
        let started = start(request.clone()).unwrap();
        let saved = finished(app, &started.investigation_id);
        assert_eq!(
            saved.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            saved.error
        );
        assert_eq!(saved.summary.schema_version, 7);
        let expected_origin = match grant.as_ref() {
            Some(grant) => InvestigationOrigin::Mcp {
                client_id: grant.selected_client().unwrap().unwrap(),
            },
            None => InvestigationOrigin::App,
        };
        assert_eq!(saved.origin, expected_origin);
        assert_eq!(
            saved.summary.completion_outcome,
            Some(InvestigationCompletionOutcome::Supported)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let result = state
            .jobs
            .lock()
            .unwrap()
            .investigation_result(&started.investigation_id)
            .unwrap()
            .unwrap();
        assert_eq!(result.schema_version, 7);
        assert!(
            result
                .disclosure_hash
                .unwrap()
                .starts_with("investigation-disclosure-v6:")
        );
        assert_eq!(saved.usage.evidence_items, 1);
        // Remove real retained bytes and disable the injected client's profile.
        // Deduplication must precede both mutable dependencies.
        rusqlite::Connection::open(app_data.join("retained-source/captures.sqlite"))
            .unwrap()
            .execute("DELETE FROM objects", [])
            .unwrap();
        provider.available.store(false, Ordering::SeqCst);
        let profiles = provider.profiles.load(Ordering::SeqCst);
        // AC-0322: durable replay still wins over later tighter discovery caps.
        let recovered = if mcp {
            completion_start::start_access(
                request.clone(),
                app.clone(),
                Instant::now(),
                session.as_ref(),
                Some(refused_limits),
            )
            .unwrap()
        } else {
            start(request.clone()).unwrap()
        };
        assert_eq!(recovered, saved.summary);
        assert_eq!(detail(app, &started.investigation_id), saved);
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        if let Some(grant) = grant.as_ref() {
            grant.revoke();
            assert!(start(request.clone()).is_err());
            assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
            assert_eq!(detail(app, &started.investigation_id), saved);
        }
        let mut fresh = request;
        fresh.request_nonce = "public-command-new-nonce-unavailable".into();
        assert!(start(fresh).is_err());
    });
}

#[test]
fn public_completion_start_recovers_committed_request6_after_catalog_replacement() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), true, |app, request, app_data| {
        assert_eq!(request.schema_version, 6);
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV9));
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        let saved = state
            .jobs
            .lock()
            .unwrap()
            .existing_investigation_completion(&request)
            .unwrap()
            .unwrap();
        assert_eq!(saved.summary.status, InvestigationStatus::Completed);
        let result = state
            .jobs
            .lock()
            .unwrap()
            .investigation_result(&saved.summary.investigation_id)
            .unwrap()
            .unwrap();
        assert_eq!(result.schema_version, 6);
        rusqlite::Connection::open(app_data.join("retained-source/captures.sqlite"))
            .unwrap()
            .execute("DELETE FROM objects", [])
            .unwrap();
        provider.available.store(false, Ordering::SeqCst);
        let profiles = provider.profiles.load(Ordering::SeqCst);
        let recovered = completion_start::start(request, app.clone(), Instant::now()).unwrap();
        assert_eq!(recovered, saved.summary);
        assert_eq!(detail(app, &saved.summary.investigation_id), saved);
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    });
}

#[test]
fn public_completion_consent_requires_exact_preview_for_approval_and_decline() {
    for approve in [true, false] {
        let seed = Arc::new(PublicProvider::new(false));
        worker::with_public_start_fixture(seed, false, |app, mut request, _| {
            let provider = Arc::new(PublicProvider::new(true));
            let state = app.state::<AppState>();
            {
                let mut settings = state.settings.lock().unwrap();
                settings.set_provider("T3", "cloud").unwrap();
                settings
                    .grant_consent(
                        "T3",
                        "Scripted public command fixture; no network transport.",
                    )
                    .unwrap();
            }
            state
                .investigations
                .set_test_provider(InvestigationProviderMode::Cloud, provider.clone());
            request.provider_mode = InvestigationProviderMode::Cloud;
            let started = completion_start::start(request, app.clone(), Instant::now()).unwrap();
            let first = wait(|| pending(&state, &started.investigation_id).unwrap());
            let saved = detail(app, &started.investigation_id);
            assert_eq!(saved.summary.status, InvestigationStatus::AwaitingConsent);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            assert!(
                consent_decision(
                    app,
                    &started.investigation_id,
                    &first.step_id,
                    first.revision + 1,
                    &first.preview.payload_hash,
                    approve
                )
                .is_err()
            );
            assert!(
                consent_decision(
                    app,
                    &started.investigation_id,
                    &first.step_id,
                    first.revision,
                    &"0".repeat(64),
                    approve
                )
                .is_err()
            );
            assert_eq!(detail(app, &started.investigation_id), saved);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            consent_decision(
                app,
                &started.investigation_id,
                &first.step_id,
                first.revision,
                &first.preview.payload_hash,
                approve,
            )
            .unwrap();
            assert!(
                consent_decision(
                    app,
                    &started.investigation_id,
                    &first.step_id,
                    first.revision,
                    &first.preview.payload_hash,
                    approve
                )
                .is_err()
            );
            if approve {
                // Every later real invocation needs its own exact command approval.
                for _ in 0..2 {
                    let next = wait(|| pending(&state, &started.investigation_id).unwrap());
                    assert_ne!(next.step_id, first.step_id);
                    consent_decision(
                        app,
                        &started.investigation_id,
                        &next.step_id,
                        next.revision,
                        &next.preview.payload_hash,
                        true,
                    )
                    .unwrap();
                }
            }
            let final_detail = finished(app, &started.investigation_id);
            assert_eq!(
                final_detail.summary.status,
                if approve {
                    InvestigationStatus::Completed
                } else {
                    InvestigationStatus::Cancelled
                },
                "{:?}",
                final_detail.error
            );
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                if approve { 3 } else { 0 }
            );
            assert_eq!(
                final_detail.usage.model_invocations,
                if approve { 3 } else { 0 }
            );
            assert_eq!(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .investigation_result(&started.investigation_id)
                    .unwrap()
                    .is_some(),
                approve
            );
        });
    }
}

// AC-0322: legacy requests without inspection targets still recover before provider lookup.
#[test]
fn mcp_coordinator_legacy_retry_precedes_provider_and_checks_grant() {
    let directory = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(directory.path());
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(state);
    let state = app.state::<AppState>();
    let capacity = std::num::NonZeroUsize::new(1).unwrap();
    let client = state
        .jobs
        .lock()
        .unwrap()
        .create_mcp_client(capacity)
        .unwrap();
    let grant = McpTaskActivation::granted(client, capacity);
    let session = grant.session();
    let request = StartInvestigationRequest {
        schema_version: 1,
        request_nonce: "coordinator-legacy-retry".into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "What evidence supports the design?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        inspection_requirements: None,
        expected_graph_revision: None,
        expected_context_id: None,
        conversation_id: None,
        parent_id: None,
    };
    let provider = Arc::new(PublicProvider::new(false));
    let descriptor = runtime::descriptor(provider.as_ref(), request.provider_mode).unwrap();
    let saved = state
        .jobs
        .lock()
        .unwrap()
        .start_investigation_access(&request, Some(&descriptor), Some(&session))
        .unwrap()
        .detail;
    provider.available.store(false, Ordering::SeqCst);
    state
        .investigations
        .set_test_provider(request.provider_mode, provider.clone());
    let profiles = provider.profiles.load(Ordering::SeqCst);
    let recovered = start_access(
        request.clone(),
        app.handle().clone(),
        Instant::now(),
        Some(&session),
    )
    .unwrap();
    assert_eq!(recovered, saved.summary);
    grant.revoke();
    assert!(
        start_access(
            request,
            app.handle().clone(),
            Instant::now(),
            Some(&session)
        )
        .is_err()
    );
    assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(detail(app.handle(), &saved.summary.investigation_id), saved);
}
