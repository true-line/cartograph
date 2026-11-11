//! AC-0582: offline regression probes for the hosted auditor and versioned parent history.
use super::*;

struct HostedHistory {
    third: u8,
    large_parent: bool,
    calls: AtomicUsize,
}
impl LlmProvider for HostedHistory {
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
        panic!("offline fixture")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        llm::openai::OpenAiProvider::new("gpt-6.1-sol", "unused-scripted-key")
            .unwrap()
            .bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        assert_eq!(request.limits().values().max_output_tokens, 16384);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let p: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let acquisition = p["turn_progress"]["acquisition_choices"].is_object();
        if call
            >= if acquisition {
                32
            } else if self.third == 3 {
                4
            } else {
                3
            }
        {
            return Err(BoundedCallError {
                code: llm::bounded::FailureCode::Cancelled,
                outcome: llm::bounded::InvocationOutcome::NotDispatched,
            });
        }
        // Exercise main's pre-dispatch accounting hook, even though this fixture
        // never opens a socket. Failed scripted stops above are not dispatched.
        assert!(control.dispatching.expect("Cloud dispatch hook")(100));
        let history = serde_json::to_vec(&p["prior_findings_as_t3_history"]).unwrap();
        assert!(history.len() <= 32768);
        if self.large_parent {
            assert!(history.len() > 16384);
        }
        let offered = p["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .and_then(|entries| entries.first());
        let action = if let Some(entry) = offered {
            entry["action"].clone()
        } else if call == 0 {
            let target = &p["completion_contract"]["obligations"]["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["kind"] == "inventory")
                .unwrap()["target"];
            json!({"type":"read_coverage_inventory","context_id":target["context_id"],"repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768})
        } else if call == 1 || self.third == 1 {
            let cursor = if call == 2 {
                let cursor = p["turn_progress"]["last_completed_action"]["next_cursor"].clone();
                assert!(
                    cursor.is_object(),
                    "fixture must offer a real continuation cursor"
                );
                cursor
            } else {
                Value::Null
            };
            json!({"type":"query_context","context_id":p["context_id"],"query":{"scope":{"type":"all"},"kind":"node","labels":if self.third == 1 {json!([])} else {json!(["BusinessRule"])},"max_facts":1,"max_bytes":32768,"cursor":cursor}})
        } else if self.third == 0 || (self.third == 3 && call == 2) {
            let option = &p["compact_context"]["evidence_options"][0];
            let range = option["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["role"] == "definition_initializer")
                .unwrap();
            json!({"type":"read_evidence","fact":option["fact"],"role":range["role"],"index":range["index"]})
        } else if self.third == 3 {
            let citation = p["citations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"Independent source check","statement":"Separate inspection supports only this selected guard; surrounding paths were not acquired.","citation_ids":[citation["citation_id"]],"limitations":["Execution conditions were not exercised by this audit."]}],"knowledge_completeness":"partial","limitations":["Only the selected captured source and inventory were inspected."]}})
        } else {
            json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["Source evidence has not been independently acquired."]}})
        };
        if acquisition {
            println!(
                "auditor12_fixture_call={} action={} menu_entries={}",
                call + 1,
                action["type"].as_str().unwrap_or("finish"),
                p["turn_progress"]["acquisition_choices"]["entries"]
                    .as_array()
                    .map_or(0, Vec::len)
            );
        }
        Ok(BoundedCompletion {
            text: action.to_string(),
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
fn hosted_auditor_large_parent_third_actions() {
    for third in 0..3 {
        let mut scripted = NextProvider::new(false);
        scripted.source_scenario = Some(SourceScenario::Acquired);
        scripted.extra_inventory = true;
        scripted.large_finish = true;
        let parent_provider = Arc::new(scripted);
        let base = CompletionFixture::with_padding(
            parent_provider.as_ref(),
            SpecialistId::DomainAnalystV9,
            true,
            true,
            ORIGINAL,
            0,
        );
        let parent_request = base.request.clone();
        let parent = NextFixture::from_completion_base(
            parent_provider.as_ref(),
            SpecialistId::DomainAnalystV10,
            base,
            parent_request,
        );
        parent.run(parent_provider);
        assert_eq!(
            parent
                .base
                .base
                .app
                .state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .investigation_owned_detail(&parent.execution, &parent.id)
                .unwrap()
                .summary
                .status,
            InvestigationStatus::Completed
        );
        let mut request = request(&parent);
        request.specialist_id = "evidence-auditor@11".into();
        request.provider_mode = InvestigationProviderMode::Cloud;
        let provider = Arc::new(HostedHistory {
            third,
            large_parent: true,
            calls: AtomicUsize::new(0),
        });
        let state = parent.base.base.app.state::<AppState>();
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
        assert_eq!(start.detail.limits.generated_tokens_per_invocation, 16384);
        assert!(start.detail.limits.history_policy.is_some());
        assert_eq!(
            InvestigationPolicy::for_effective_limits(&start.detail.limits)
                .unwrap()
                .limits(),
            start.detail.limits
        );
        let expected_parent = jobs
            .investigation_owned_detail(&parent.execution, &parent.id)
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
            let approval_live = live.clone();
            let approval_id = id.clone();
            let state = &*state;
            scope.spawn(move || {
                let mut approved = String::new();
                while !done.load(Ordering::SeqCst) {
                    if let Some(p) = approval_live.pending.lock().unwrap().clone()
                        && p.step_id != approved
                    {
                        state
                            .jobs
                            .lock()
                            .unwrap()
                            .approve_next_prompt_investigation(
                                &approval_id,
                                p.revision,
                                &p.step_id,
                                &p.preview.payload_hash,
                            )
                            .unwrap();
                        approved = p.step_id;
                        state.investigations.wake(&approval_id);
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
                parent.base.base.app.handle().clone(),
                id.clone(),
                request,
                execution.clone(),
                provider.clone(),
                live,
                Instant::now(),
            );
            done.store(true, Ordering::SeqCst);
        });
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation_owned_detail(&execution, &id).unwrap();
        assert_ne!(
            detail.error.as_deref(),
            Some(InvestigationStopReason::Operational.test_message())
        );
        assert!(detail.terminal_diagnostic.is_none());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert_eq!(
            detail.completion.as_ref().unwrap().refusal_count,
            u32::from(third == 2)
        );
        if third == 0 {
            assert_eq!(detail.usage.evidence_items, 1);
            assert!(detail.usage.captured_validation_bytes > 0);
        }
        assert_eq!(
            jobs.investigation_owned_detail(&parent.execution, &parent.id)
                .unwrap(),
            expected_parent
        );
        drop(jobs);
        let reopened =
            crate::jobs::JobStore::open(parent.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_owned_detail(&execution, &id)
                .unwrap(),
            detail
        );
        println!(
            "third={third} calls={} status={:?} error={:?} diagnostic={:?}",
            provider.calls.load(Ordering::SeqCst),
            detail.summary.status,
            detail.error,
            detail.terminal_diagnostic
        );
    }
}

#[test]
fn terminal_diagnostic_persists_and_historical_stop_remains_omitted() {
    use agents::investigation::{
        InvestigationFailurePhase as Phase, InvestigationOperationalClass as Class,
        InvestigationTerminalDiagnostic as Diagnostic,
    };
    for reason in [
        InvestigationStopReason::Operational,
        InvestigationStopReason::OperationalAt {
            phase: Phase::SourceReservation,
            error_class: Class::StoreInvalid,
        },
        InvestigationStopReason::OperationalAt {
            phase: Phase::EgressRecording,
            error_class: Class::HostOperational,
        },
    ] {
        let provider = NextProvider::new(false);
        let f = NextFixture::new(&provider, SpecialistId::DomainAnalystV10);
        let state = f.base.base.app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        let revision = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap()
            .summary
            .revision;
        let detail = jobs
            .stop_next_prompt_investigation(&f.execution, revision, reason, 0)
            .unwrap();
        let expected = match reason {
            InvestigationStopReason::OperationalAt { phase, error_class } => {
                Some(Diagnostic::Operational { phase, error_class })
            }
            _ => None,
        };
        assert_eq!(detail.terminal_diagnostic, expected);
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(
            detail.error.as_deref(),
            Some(InvestigationStopReason::Operational.test_message())
        );
        let raw = serde_json::to_value(&detail).unwrap();
        assert_eq!(raw.get("terminal_diagnostic").is_some(), expected.is_some());
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_owned_detail(&f.execution, &f.id)
                .unwrap(),
            detail
        );
    }
}

pub(in crate::investigations) fn full_hosted_auditor_provider(
    large_parent: bool,
) -> Arc<dyn LlmProvider> {
    Arc::new(HostedHistory {
        third: 3,
        large_parent,
        calls: AtomicUsize::new(0),
    })
}
