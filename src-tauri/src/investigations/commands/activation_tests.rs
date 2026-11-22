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
    pause: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    entered: AtomicBool,
    cancel_outcome: std::sync::Mutex<Option<llm::bounded::InvocationOutcome>>,
}
impl PublicProvider {
    fn new(cloud: bool) -> Self {
        Self {
            cloud,
            available: AtomicBool::new(true),
            profiles: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            pause: std::sync::Mutex::new(None),
            entered: AtomicBool::new(false),
            cancel_outcome: std::sync::Mutex::new(None),
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
    fn supports_bounded_response_schema(&self) -> bool {
        // This in-process provider consumes and checks the finite response schema.
        true
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
        if let Some(wait) = self.pause.lock().unwrap().take() {
            self.entered.store(true, Ordering::SeqCst);
            wait.recv_timeout(Duration::from_secs(30)).unwrap();
        }
        if let Some(outcome) = *self.cancel_outcome.lock().unwrap() {
            assert_eq!((control.check)(), CallDirective::Cancelled);
            self.calls.fetch_add(1, Ordering::SeqCst);
            return Err(BoundedCallError {
                code: llm::bounded::FailureCode::Cancelled,
                outcome,
            });
        }
        assert_eq!((control.check)(), CallDirective::Continue);
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        // AC-0552: @18 uses the exact offered finite object, not the historical
        // broad query script. The normal host coordinator still admits it.
        let offered = prompt["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .and_then(|entries| entries.first());
        let text = if let Some(entry) = offered {
            let action = entry["action"].clone();
            let schema: Value = serde_json::from_str(request.payload().response_schema.as_deref().unwrap()).unwrap();
            assert!(schema["anyOf"].as_array().unwrap().iter().filter_map(|branch| branch.get("enum")).any(|finite| finite.as_array().unwrap().contains(&action)));
            action
        } else { match call {
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
        }}.to_string();
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
                mcp.then(crate::mcp_reads::worker::tests::task_read_limits),
                None,
            )
        };
        let mut refused_limits = crate::mcp_reads::worker::tests::task_read_limits();
        refused_limits.scope.max_source_registry_bytes = std::num::NonZeroUsize::new(1).unwrap();
        if mcp {
            // AC-0327: refuse discovery before durable admission or provider lookup.
            let profiles = provider.profiles.load(Ordering::SeqCst);
            let error = completion_start::start_access(
                request.clone(),
                app.clone(),
                Instant::now(),
                session.as_ref(),
                Some(refused_limits),
                None,
            )
            .unwrap_err();
            assert!(
                matches!(error, InvestigationStartError::Rejected { code: "limit_exceeded", message, definitive: true }
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
                None,
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
    // AC-0552: the same exact consent boundary applies to explicitly selected18.
    for role in ["domain-analyst@10", "domain-analyst@18"] {
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
                request.specialist_id = role.into();
                request.provider_mode = InvestigationProviderMode::Cloud;
                let started =
                    completion_start::start(request, app.clone(), Instant::now()).unwrap();
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
}

// AC-0322: legacy requests without inspection targets still recover before provider lookup.
#[test]
fn mcp_coordinator_legacy_retry_precedes_provider_and_checks_grant() {
    let directory = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(directory.path());
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
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

// AC-0325/0327: connection revocation cannot refund a still-running provider call.
#[test]
fn mcp_completion_retains_reservation_until_detached_worker_exits() {
    use crate::mcp_reads::task_reservation::TaskReservations;
    use mcp_local::work_budget::{WorkBudget, WorkLimits};
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, request, _| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(request.provider_mode, provider.clone());
        let one = std::num::NonZeroUsize::new(1).unwrap();
        let client = state.jobs.lock().unwrap().create_mcp_client(one).unwrap();
        let grant = McpTaskActivation::granted(client, one);
        let session = grant.session();
        // Unit reservation proves lifetime only, not the final byte composition.
        let new_budget = || {
            WorkBudget::new(WorkLimits {
                max_workers: one,
                max_reserved_bytes: one,
                max_cumulative_read_bytes: 0,
            })
        };
        let rejected_budget = new_budget();
        let rejected_permit =
            TaskReservations::from(Arc::new(rejected_budget.try_acquire(one, 0).unwrap()));
        rejected_budget.revoke();
        let profiles = provider.profiles.load(Ordering::SeqCst);
        let limits = crate::mcp_reads::worker::tests::task_read_limits();
        let denied = completion_start::start_access(
            request.clone(),
            app.clone(),
            Instant::now(),
            Some(&session),
            Some(limits),
            Some(&rejected_permit),
        )
        .unwrap_err();
        assert!(matches!(
            denied,
            InvestigationStartError::Rejected {
                code: "limit_exceeded",
                ..
            }
        ));
        assert!(
            state
                .jobs
                .lock()
                .unwrap()
                .existing_investigation_completion_access(&request, Some(&session))
                .unwrap()
                .is_none()
        );
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let budget = new_budget();
        let permit = TaskReservations::from(Arc::new(budget.try_acquire(one, 0).unwrap()));
        // Full detached slots refuse before durable task or provider admission.
        let first = permit.try_acquire().unwrap();
        let second = permit.try_acquire().unwrap();
        let error = completion_start::start_access(
            request.clone(),
            app.clone(),
            Instant::now(),
            Some(&session),
            Some(limits),
            Some(&permit),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            InvestigationStartError::Rejected {
                code: "limit_exceeded",
                ..
            }
        ));
        assert!(
            state
                .jobs
                .lock()
                .unwrap()
                .existing_investigation_completion_access(&request, Some(&session))
                .unwrap()
                .is_none()
        );
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        drop((first, second));
        let (release, gate) = std::sync::mpsc::channel();
        *provider.pause.lock().unwrap() = Some(gate);
        // AC-0322: three matching starts share one remaining detached slot.
        // Keep the other slot occupied and the actual worker blocked throughout
        // admission so duplicate attempts cannot rely on an execution finishing.
        let occupied = permit.try_acquire().unwrap();
        let barrier = std::sync::Barrier::new(3);
        let starts = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..3)
                .map(|_| {
                    let barrier = &barrier;
                    let request = &request;
                    let session = &session;
                    let permit = &permit;
                    threads.spawn(move || {
                        barrier.wait();
                        // A new thread: carry the fixture's retired-start scope (AC-1062).
                        super::with_retired_starts(|| {
                            completion_start::start_access(
                                request.clone(),
                                app.clone(),
                                Instant::now(),
                                Some(session),
                                Some(limits),
                                Some(permit),
                            )
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut starts = starts.into_iter().map(Result::unwrap);
        let started = starts.next().unwrap();
        assert!(starts.all(|start| start.investigation_id == started.investigation_id));
        drop(occupied);
        wait(|| provider.entered.load(Ordering::SeqCst).then_some(()));
        budget.revoke();
        drop(permit); // connection no longer owns the reservation
        assert!(!budget.is_idle().unwrap());
        assert!(budget.try_acquire(one, 0).is_err());
        // A committed retry requires no new work, even with a revoked permit.
        let replay = completion_start::start_access(
            request,
            app.clone(),
            Instant::now(),
            Some(&session),
            Some(limits),
            Some(&rejected_permit),
        )
        .unwrap();
        assert_eq!(replay.investigation_id, started.investigation_id);
        grant.revoke(); // no implicit cancellation of admitted durable work
        release.send(()).unwrap();
        let saved = finished(app, &started.investigation_id);
        assert_eq!(
            saved.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            saved.error
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        wait(|| budget.is_idle().unwrap().then_some(()));
    });
}

#[test]
fn mcp_adapter_runs_all_task_operations_and_preserves_principal_replay() {
    // AC-0321/0323/0324/0325: real coordinator + in-process synthetic provider.
    // Direct adapter evidence; this does not qualify the socket/relay or bytes.
    use crate::investigations::mcp_dispatch::TaskDispatcher;
    use crate::mcp_reads::{ReadError, task_reservation::TaskReservations};
    use mcp_local::work_budget::{WorkBudget, WorkLimits};
    use mcp_wire::tools::TaskTool;
    // AC-0552: supported completion families share the same granted adapter.
    for role in ["domain-analyst@10", "domain-analyst@18"] {
        let provider = Arc::new(PublicProvider::new(false));
        worker::with_public_start_fixture(provider.clone(), false, |app, mut request, _| {
            request.specialist_id = role.into();
            request = request
                .normalized_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap();
            let state = app.state::<AppState>();
            state
                .investigations
                .set_test_provider(request.provider_mode, provider.clone());
            let one = std::num::NonZeroUsize::new(1).unwrap();
            let mut jobs = state.jobs.lock().unwrap();
            let owner_task = jobs.investigation_history(None, None).unwrap().items[0]
                .investigation_id
                .clone();
            let client_capacity = std::num::NonZeroUsize::new(2).unwrap();
            let client = jobs.create_mcp_client(client_capacity).unwrap();
            let foreign = jobs.create_mcp_client(client_capacity).unwrap();
            drop(jobs);
            let grant = McpTaskActivation::granted(client, one);
            let foreign_grant = McpTaskActivation::granted(foreign, one);
            let make_dispatch = |session| {
                // Unit permit tests lifetime plumbing, not aggregate admission.
                let budget = WorkBudget::new(WorkLimits {
                    max_workers: one,
                    max_reserved_bytes: one,
                    max_cumulative_read_bytes: 0,
                });
                TaskDispatcher::new(
                    session,
                    TaskReservations::from(Arc::new(budget.try_acquire(one, 0).unwrap())),
                )
            };
            let dispatcher = make_dispatch(grant.session());
            let other = make_dispatch(foreign_grant.session());
            let call = |dispatcher: &TaskDispatcher, tool, args: Value| {
                dispatcher.call(
                    app,
                    &json!("request"),
                    tool,
                    args.as_object().unwrap().clone(),
                    crate::mcp_reads::worker::tests::task_read_limits(),
                    256 * 1024,
                )
            };
            // AC-0327: source admission must retain its task-specific refusal code.
            let mut bounded_scope = crate::mcp_reads::worker::tests::task_read_limits();
            bounded_scope.scope.max_source_registry_bytes = one;
            let refused = dispatcher
                .call(
                    app,
                    &json!("bounded-source"),
                    TaskTool::Start,
                    json!({"request":request}).as_object().unwrap().clone(),
                    bounded_scope,
                    256 * 1024,
                )
                .unwrap_err();
            assert_eq!(refused, ReadError::AdmissionLimit);
            assert_eq!(refused.to_string(), "limit_exceeded");
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            let started = call(&dispatcher, TaskTool::Start, json!({"request":request})).unwrap();
            let id = started["result"]["structuredContent"]["investigation_id"]
                .as_str()
                .unwrap();
            let terminal = finished(app, id);
            assert_eq!(terminal.summary.status, InvestigationStatus::Completed);
            assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
            let status = call(&dispatcher, TaskTool::Status, json!({"task_id":id})).unwrap();
            assert_eq!(
                status["result"]["structuredContent"],
                serde_json::to_value(&terminal.summary).unwrap()
            );
            let events = call(
                &dispatcher,
                TaskTool::Events,
                json!({"task_id":id,"after":0}),
            )
            .unwrap();
            let owner_events = state
                .jobs
                .lock()
                .unwrap()
                .investigation_events(id, 0)
                .unwrap();
            assert_eq!(
                events["result"]["structuredContent"],
                serde_json::to_value(&owner_events).unwrap()
            );
            assert!(!owner_events.items.is_empty());
            assert!(
                owner_events
                    .items
                    .windows(2)
                    .all(|pair| pair[1].sequence == pair[0].sequence + 1)
            );
            let result = call(&dispatcher, TaskTool::Result, json!({"task_id":id})).unwrap();
            let saved = state.jobs.lock().unwrap().investigation_result(id).unwrap();
            assert!(saved.is_some());
            // Only a drafting run's reply carries a drafting outcome.
            assert!(
                result["result"]["structuredContent"]
                    .get("drafting")
                    .is_none()
            );
            assert_eq!(
                result["result"]["structuredContent"]["result"],
                serde_json::to_value(&saved).unwrap()
            );
            let cancelled = call(&dispatcher, TaskTool::Cancel, json!({"task_id":id})).unwrap();
            assert_eq!(
                cancelled["result"]["structuredContent"],
                status["result"]["structuredContent"]
            );
            // A saved nonce survives a now-unavailable provider without another call.
            provider.available.store(false, Ordering::SeqCst);
            let profiles = provider.profiles.load(Ordering::SeqCst);
            let replay = call(&dispatcher, TaskTool::Start, json!({"request":request})).unwrap();
            assert_eq!(
                replay["result"]["structuredContent"]["investigation_id"],
                id
            );
            assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
            let mut changed = request.clone();
            changed.question.push_str(" changed");
            assert!(call(&dispatcher, TaskTool::Start, json!({"request":changed})).is_err());
            for tool in [
                TaskTool::Status,
                TaskTool::Events,
                TaskTool::Result,
                TaskTool::Cancel,
            ] {
                let args = |id| {
                    if tool == TaskTool::Events {
                        json!({"task_id":id,"after":0})
                    } else {
                        json!({"task_id":id})
                    }
                };
                assert_eq!(call(&other, tool, args(id)), Err(ReadError::InvalidRequest));
                assert_eq!(
                    call(&dispatcher, tool, args(&owner_task)),
                    Err(ReadError::InvalidRequest)
                );
            }
            grant.revoke();
            assert_eq!(
                call(&dispatcher, TaskTool::Start, json!({"request":request})),
                Err(ReadError::InvalidRequest)
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        });
    }
}

#[cfg(unix)]
#[path = "activation_tests/task_relay.rs"]
mod task_relay;

#[cfg(unix)]
#[path = "activation_tests/service_relay.rs"]
mod service_relay;

// AC-0552: exact version opt-in uses ordinary start/worker/store and reopen.
#[test]
fn explicit_acquisition_start_reopens_and_recovers_nonce() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, mut request, app_data| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        request.specialist_id = "domain-analyst@18".into();
        request = request
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let conn = rusqlite::Connection::open_with_flags(
            app_data.join("state.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let version = || {
            conn.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
                .unwrap()
        };
        let before = version();
        let profiles = provider.profiles.load(Ordering::SeqCst);
        let mut wrong = request.clone();
        wrong.schema_version = 6;
        assert!(completion_start::start(wrong, app.clone(), Instant::now()).is_err());
        assert_eq!(version(), before);
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let started =
            completion_start::start(request.clone(), app.clone(), Instant::now()).unwrap();
        let terminal = finished(app, &started.investigation_id);
        assert_eq!(
            terminal.summary.specialist_id,
            SpecialistId::DomainAnalystV18
        );
        assert_eq!(
            terminal.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            terminal.error
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let reopened = crate::jobs::JobStore::open(app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation(&started.investigation_id).unwrap(),
            terminal
        );
        assert!(
            reopened
                .investigation_result(&started.investigation_id)
                .unwrap()
                .is_some()
        );
        let mut jobs = state.jobs.lock().unwrap();
        let prior = jobs
            .start_next_prompt_investigation(&request, None)
            .unwrap();
        assert!(!prior.created);
        assert_eq!(prior.detail, terminal);
        assert!(jobs.start_completion_investigation(&request, None).is_err()); // no generic-family bypass
        drop(jobs);
        provider.available.store(false, Ordering::SeqCst);
        let profiles = provider.profiles.load(Ordering::SeqCst);
        let replay = completion_start::start(request.clone(), app.clone(), Instant::now()).unwrap();
        assert_eq!(replay, terminal.summary);
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let mut changed = request;
        changed.question.push('?');
        assert!(completion_start::start(changed, app.clone(), Instant::now()).is_err());
    });
}

// AC-0574: exercise the actual public coordinator, not just JobStore admission.
#[test]
fn desktop_analyst27_command_selects_finish_policy() {
    desktop_analyst_command_selects_finish_policy(SpecialistId::DomainAnalystV27);
}

// AC-0834 (#1335): the public start path opens @28 on @27's saved local finish
// policy, cancels before a model call and replays the saved nonce unchanged.
#[test]
fn desktop_analyst28_command_selects_finish_policy() {
    desktop_analyst_command_selects_finish_policy(SpecialistId::DomainAnalystV28);
}

fn desktop_analyst_command_selects_finish_policy(role: SpecialistId) {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, mut request, _| {
        request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        let (release, gate) = std::sync::mpsc::channel();
        *provider.pause.lock().unwrap() = Some(gate);
        *provider.cancel_outcome.lock().unwrap() =
            Some(llm::bounded::InvocationOutcome::NotDispatched);
        let started = completion_start::start_access(
            request.clone(),
            app.clone(),
            Instant::now(),
            None,
            None,
            None,
        )
        .unwrap();
        wait(|| provider.entered.load(Ordering::SeqCst).then_some(()));
        let saved = detail(app, &started.investigation_id);
        assert_eq!(saved.summary.specialist_id, role);
        assert_eq!(
            saved.limits.generation_policy,
            Some(agents::investigation::GenerationPolicy::LocalFinish4096V1)
        );
        assert_eq!(saved.limits.generated_token_reservations, 131072);
        state
            .jobs
            .lock()
            .unwrap()
            .cancel_next_prompt_investigation(&started.investigation_id)
            .unwrap();
        state.investigations.wake(&started.investigation_id);
        release.send(()).unwrap();
        let terminal = finished(app, &started.investigation_id);
        assert_eq!(terminal.summary.status, InvestigationStatus::Cancelled);
        provider.available.store(false, Ordering::SeqCst);
        let profiles = provider.profiles.load(Ordering::SeqCst);
        assert_eq!(
            completion_start::start_access(request, app.clone(), Instant::now(), None, None, None,)
                .unwrap(),
            terminal.summary
        );
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn desktop_analyst26_command_rejects_creation_but_recovers_saved_nonce() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, mut request, _| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        let profiles = provider.profiles.load(Ordering::SeqCst);
        for role in [
            "domain-analyst@24",
            "domain-analyst@25",
            "domain-analyst@26",
        ] {
            request.specialist_id = role.into();
            // Outside the fixture's retired-start scope: the real gate.
            let error = super::without_retired_starts(|| {
                completion_start::start_access(
                    request.clone(),
                    app.clone(),
                    Instant::now(),
                    None,
                    None,
                    None,
                )
            })
            .unwrap_err();
            assert!(matches!(
                error,
                InvestigationStartError::Rejected {
                    code: "invalid_input",
                    ..
                }
            ));
            assert!(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .existing_desktop_investigation_completion(&request)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let descriptor = runtime::descriptor(provider.as_ref(), request.provider_mode).unwrap();
        // Seed an authentic historical @26 task using the explicit captured lane.
        let saved = state
            .jobs
            .lock()
            .unwrap()
            .start_local_finish_investigation(&request, Some(&descriptor), Some((600, 180_000)))
            .unwrap()
            .detail;
        provider.available.store(false, Ordering::SeqCst);
        let profiles = provider.profiles.load(Ordering::SeqCst);
        let before = serde_json::to_vec(&saved).unwrap();
        let recovered =
            completion_start::start_access(request, app.clone(), Instant::now(), None, None, None)
                .unwrap();
        assert_eq!(recovered, saved.summary);
        assert_eq!(
            serde_json::to_vec(&detail(app, &recovered.investigation_id)).unwrap(),
            before
        );
        assert_eq!(provider.profiles.load(Ordering::SeqCst), profiles);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    });
}

fn set_pool(app: &tauri::AppHandle<tauri::test::MockRuntime>, paused: bool, local_limit: u32) {
    app.state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .set_agent_pool_controls(&crate::agent_pool::AgentPoolControls {
            paused,
            local_limit,
            cloud_limit: 0,
        })
        .unwrap();
}

// AC-1706 (T-1706): a specialist investigation the agent pool does not
// admit stays queued without a provider call, then runs when resumed.
#[test]
fn a_held_investigation_runs_when_the_pool_is_resumed() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, request, _| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        set_pool(app, true, 0);
        let started =
            completion_start::start_access(request, app.clone(), Instant::now(), None, None, None)
                .unwrap();
        assert_eq!(started.status, InvestigationStatus::Queued);
        std::thread::sleep(Duration::from_millis(600));
        let held = detail(app, &started.investigation_id);
        assert_eq!(held.summary.status, InvestigationStatus::Queued);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        set_pool(app, false, 1);
        let saved = finished(app, &started.investigation_id);
        assert_eq!(
            saved.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            saved.error
        );
        assert!(provider.calls.load(Ordering::SeqCst) > 0);
    });
}

// AC-1706 (T-1706): a held investigation stays cancellable from Jobs; cancelling it
// ends the wait, it never calls the provider, and its live place is freed.
#[test]
fn a_held_investigation_can_be_cancelled_before_it_starts() {
    let provider = Arc::new(PublicProvider::new(false));
    worker::with_public_start_fixture(provider.clone(), false, |app, request, _| {
        let state = app.state::<AppState>();
        state
            .investigations
            .set_test_provider(InvestigationProviderMode::Local, provider.clone());
        set_pool(app, true, 0);
        let started =
            completion_start::start_access(request, app.clone(), Instant::now(), None, None, None)
                .unwrap();
        // Cancelled from Jobs, as a held job is.
        state.jobs.lock().unwrap().cancel(started.job_id).unwrap();
        let saved = finished(app, &started.investigation_id);
        assert_eq!(saved.summary.status, InvestigationStatus::Cancelled);
        wait(|| state.investigations.insert(&started.investigation_id).ok());
        set_pool(app, false, 0);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    });
}
