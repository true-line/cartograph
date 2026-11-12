//! Optional copied-fixture witness. Scripted provider only; not acceptance evidence.
use super::*;
struct Replay {
    calls: AtomicUsize,
    provider_ns: std::sync::atomic::AtomicU64,
    replay_text: String,
    forced_capacity: bool,
    prompts: std::sync::Mutex<Vec<Value>>,
}
impl LlmProvider for Replay {
    fn id(&self) -> &str {
        "openai:gpt-6.1-sol"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        NextProvider::new(false).capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("offline")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        llm::openai::OpenAiProvider::new("gpt-6.1-sol", "offline")
            .unwrap()
            .bounded_profile()
    }
    fn complete_bounded(
        &self,
        r: &AuthorizedBoundedCompletion,
        c: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        // Entire scripted provider boundary, including host control/dispatch
        // callbacks. No network/model exists; report that distinction explicitly.
        struct ProviderTimer<'a>(&'a std::sync::atomic::AtomicU64, Instant);
        impl Drop for ProviderTimer<'_> {
            fn drop(&mut self) {
                self.0
                    .fetch_add(self.1.elapsed().as_nanos() as u64, Ordering::SeqCst);
            }
        }
        let _timer = ProviderTimer(&self.provider_ns, Instant::now());
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        let role = std::env::var("REPRO_ROLE").unwrap();
        if i > 20 || (role == "evidence-auditor@13" && i > 2) {
            return Err(BoundedCallError {
                code: llm::bounded::FailureCode::Cancelled,
                outcome: llm::bounded::InvocationOutcome::NotDispatched,
            });
        }
        assert_eq!((c.check)(), CallDirective::Continue);
        assert!(c.dispatching.unwrap()(100));
        let p: Value = serde_json::from_str(&r.payload().prompt).unwrap();
        let entries = p["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap();
        let ids: Vec<_> = p["citations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["origin"]["kind"] == "captured_primary_source")
            .map(|c| c["citation_id"].clone())
            .collect();
        self.prompts.lock().unwrap().push(p.clone());
        let a = if self.forced_capacity && entries.is_empty() {
            json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["Mandatory evidence remains unaffordable; no unsupported conclusion is offered."]}})
        } else if ids.len() == 7 {
            let mut finish = json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"Offline feasibility probe","statement":"The supplied source ranges were acquired for this mechanical probe.","citation_ids":ids,"limitations":["This scripted probe is not a semantic review."]}],"knowledge_completeness":"partial","limitations":["Offline mechanical feasibility only."]}});
            if i == 15 {
                finish["action"]["limitations"] = json!([self.replay_text]);
            }
            finish
        } else if i == 0 {
            entries[0]["action"].clone()
        } else {
            entries
                .iter()
                .map(|e| &e["action"])
                .find(|a| a.to_string().contains("orderProcess.transitions"))
                .or_else(|| entries.first().map(|e| &e["action"]))
                .expect("pending source choices")
                .clone()
        };
        let required = serde_json::to_vec(r.payload()).unwrap().len();
        println!(
            "auditor14_step_wire call={} required={} allowed={} headroom={}",
            i + 1,
            required,
            r.limits().values().input_bytes,
            r.limits()
                .values()
                .input_bytes
                .saturating_sub(required as u64)
        );
        println!(
            "probe_call={} prompt_bytes={} action={} query_bytes={}",
            i + 1,
            r.payload().prompt.len(),
            a["type"].as_str().unwrap_or("finish"),
            a["query"]["max_bytes"]
        );
        Ok(BoundedCompletion {
            text: a.to_string(),
            requested_model: "gpt-6.1-sol".into(),
            response_model: Some("gpt-6.1-sol".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}
#[test]
#[ignore]
fn auditor14_copied_real_parent_whole_run() {
    let base = std::path::PathBuf::from(std::env::var("CARTOGRAPH_OFFLINE_REPRO").unwrap());
    assert!(base.starts_with("/tmp/cartograph-codex/auditor14/probes"));
    let root = std::path::Path::new("/tmp/cartograph-codex/auditor14/probes")
        .canonicalize()
        .unwrap();
    assert!(base.canonicalize().unwrap().starts_with(&root));
    assert!(
        base.join("profile/data")
            .canonicalize()
            .unwrap()
            .starts_with(&root)
    );

    let mut config = tauri::test::mock_context(tauri::test::noop_assets());
    config.config_mut().identifier = "com.qwtm.cartograph.acceptance598.frontierf1".into();
    config.config_mut().app.app_directories_override=Some(serde_json::from_value(json!({"data":base.join("profile/data"),"config":base.join("profile/config"),"localData":base.join("profile/local-data"),"cache":base.join("profile/cache"),"log":base.join("profile/logs")})).unwrap());
    let app = tauri::test::mock_builder().build(config).unwrap();
    app.manage(crate::registered_source_tests::app_state(
        &base.join("profile/data"),
    ));
    let state = app.state::<AppState>();
    let def: Value =
        serde_json::from_slice(&std::fs::read(base.join("definition.json")).unwrap()).unwrap();
    let mut request: StagedObligationRequest =
        serde_json::from_value(def["request"].clone()).unwrap();
    request.specialist_id = std::env::var("REPRO_ROLE").unwrap();
    request.request_nonce = format!("offline-probe-{}", std::env::var("REPRO_CASE").unwrap());
    let connection = rusqlite::Connection::open(base.join("profile/data/state.db")).unwrap();
    let parent: Vec<u8> = connection
        .query_row(
            "SELECT body FROM investigation_results WHERE task_id=?1",
            [request.parent_id.as_deref().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let parent: Value = serde_json::from_slice(&parent).unwrap();
    let replay_text: String = parent["findings"][0]["statement"]
        .as_str()
        .unwrap()
        .chars()
        .take(160)
        .collect();
    assert!(replay_text.chars().filter(|c| !c.is_whitespace()).count() >= 48);
    drop(connection);
    let forced_capacity = std::env::var_os("REPRO_FORCE_CAPACITY").is_some();
    if forced_capacity {
        // Valid adversarial question bytes, not a limit override. Parent,
        // capture and obligation ledger remain the same copied fixture.
        request
            .question
            .push_str(&"\\".repeat(2048 - request.question.len()));
    }
    let provider = Arc::new(Replay {
        calls: AtomicUsize::new(0),
        provider_ns: std::sync::atomic::AtomicU64::new(0),
        replay_text,
        forced_capacity,
        prompts: std::sync::Mutex::new(Vec::new()),
    });
    {
        let mut settings = state.settings.lock().unwrap();
        settings.set_enabled("T3", true).unwrap();
        settings.set_provider("T3", "cloud").unwrap();
        settings
            .grant_consent(
                "T3",
                r#"{"provider":"offline-scripted-sol","network":false}"#,
            )
            .unwrap();
    }
    let mut jobs = state.jobs.lock().unwrap();
    let start = jobs
        .start_next_prompt_investigation(
            &request,
            Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Cloud).unwrap()),
        )
        .unwrap();
    let id = start.detail.summary.investigation_id;
    let plan = jobs
        .claim_plan(
            start.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let lock = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = jobs.claim_execution(&plan, lock).unwrap();
    drop(jobs);
    let live = state.investigations.insert(&id).unwrap();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let done = &done;
        let approval = live.clone();
        let aid = id.clone();
        let state = &*state;
        scope.spawn(move || {
            let mut approved = String::new();
            while !done.load(Ordering::SeqCst) {
                if let Some(p) = approval.pending.lock().unwrap().clone()
                    && p.step_id != approved
                {
                    state
                        .jobs
                        .lock()
                        .unwrap()
                        .approve_next_prompt_investigation(
                            &aid,
                            p.revision,
                            &p.step_id,
                            &p.preview.payload_hash,
                        )
                        .unwrap();
                    approved = p.step_id;
                    state.investigations.wake(&aid);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        struct Done<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _done = Done(done);
        run_completion(
            app.handle().clone(),
            id.clone(),
            request.clone(),
            execution.clone(),
            provider.clone(),
            live,
            Instant::now(),
        );
        done.store(true, Ordering::SeqCst);
    });
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_detail(&execution, &id)
        .unwrap();

    if forced_capacity {
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.terminal_diagnostic, Some(agents::investigation::InvestigationTerminalDiagnostic::CapacityCompletion { reason: agents::investigation::CapacityCompletionReason::UnmetObligationsAfterCapacityExclusion }));
        assert!(detail.usage.evidence_items < 7);
        assert!(provider.calls.load(Ordering::SeqCst) < 22);
        let prompts = provider.prompts.lock().unwrap();
        let mut refusals = 0;
        for pair in prompts.windows(2) {
            if pair[1]["turn_progress"]["capacity_feedback"]["reason"] != "prompt_capacity" {
                continue;
            }
            refusals += 1;
            let before = pair[0]["turn_progress"]["acquisition_choices"]["entries"]
                .as_array()
                .unwrap();
            let after = pair[1]["turn_progress"]["acquisition_choices"]["entries"]
                .as_array()
                .unwrap();
            if let Some(refused) = before
                .iter()
                .map(|e| &e["action"])
                .find(|a| a.to_string().contains("orderProcess.transitions"))
                .or_else(|| before.first().map(|e| &e["action"]))
            {
                assert!(!after.iter().any(|e| &e["action"] == refused));
            }
            assert_eq!(pair[0]["compact_context"], pair[1]["compact_context"]);
            assert_eq!(pair[0]["citations"], pair[1]["citations"]);
        }
        assert!(refusals > 0);
        assert!(
            prompts.last().unwrap()["turn_progress"]["acquisition_choices"]["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        println!("auditor14_forced_capacity refusals={refusals} terminal_honest=true");
    } else if request.specialist_id == "evidence-auditor@14" {
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "{:?}",
            detail.error
        );
        assert_eq!(detail.usage.evidence_items, 7);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 17);
    }
    let reopened = crate::jobs::JobStore::open(base.join("profile/data/state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&execution, &id)
            .unwrap(),
        detail
    );
    let provider_ms = provider
        .provider_ns
        .load(Ordering::SeqCst)
        .div_ceil(1_000_000);
    println!(
        "auditor14_timing active_ms={} scripted_provider_boundary_ms={} host_remainder_ms={} network_ms=0",
        detail.usage.active_milliseconds,
        provider_ms,
        detail.usage.active_milliseconds.saturating_sub(provider_ms)
    );
    println!(
        "probe_terminal status={:?} error={:?} usage={:?} obligations={:?}",
        detail.summary.status, detail.error, detail.usage, detail.summary.obligation_status
    );
    std::fs::write(
        base.join(format!(
            "result-{}.json",
            std::env::var("REPRO_CASE").unwrap()
        )),
        serde_json::to_vec_pretty(&detail).unwrap(),
    )
    .unwrap();
}
