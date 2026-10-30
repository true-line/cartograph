//! AC-0287: actual prompt9/firewall seals and real owner transactions, without
//! provider dispatch. Invocation1 is explicitly seeded by the internal fixture;
//! this suite exercises production next-call publication, not public activation.
use super::*;
use completion::{InternalFixture, internal_fixture_with_step_preparer};
use llm::bounded::{BoundedCallError, ProviderProfile};
use llm::{AnalysisTier, EgressFirewall, EgressPolicy, LlmProvider, Locality, ProviderCaps};

pub(in crate::jobs::investigations) struct OfflineProvider(pub ProviderProfile);
impl LlmProvider for OfflineProvider {
    fn id(&self) -> &str {
        &self.0.provider_id
    }
    fn locality(&self) -> Locality {
        self.0.locality
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        panic!("no provider dispatch permitted")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(self.0.clone())
    }
}
pub(in crate::jobs::investigations) fn provider(cloud: bool) -> OfflineProvider {
    OfflineProvider(ProviderProfile {
        protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
        provider_id: "offline-invocation-fixture".into(),
        locality: if cloud {
            Locality::Cloud
        } else {
            Locality::Local
        },
        endpoint_id: "http://127.0.0.1:11434/".into(),
        requested_model: "test-model".into(),
    })
}
pub(in crate::jobs::investigations) fn firewall() -> EgressFirewall {
    EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]))
}
pub(in crate::jobs::investigations) fn limits() -> CompletionLimits {
    let mut values = CompletionLimits::default().values();
    values.max_output_tokens = INVESTIGATION_V2_POLICY
        .limits()
        .generated_tokens_per_invocation;
    values.max_spans = INVESTIGATION_V2_POLICY
        .limits()
        .evidence_items
        .try_into()
        .unwrap();
    CompletionLimits::new(values).unwrap()
}
struct Fixture {
    saved: InternalFixture,
    pages: Vec<String>,
    previous: Option<InvestigationInputLedger>,
    cloud: bool,
}
impl Fixture {
    fn new(cloud: bool, query: bool) -> Self {
        let p = provider(cloud);
        let mut saved = internal_fixture_with_step_preparer(
            |_, task, _, request| {
                let mode = if cloud {
                    InvestigationProviderMode::Cloud
                } else {
                    InvestigationProviderMode::Local
                };
                task.detail.summary.provider_mode = mode;
                request.provider_mode = mode;
                task.detail.provider = InvestigationProvider {
                    mode,
                    provider_id: p.0.provider_id.clone(),
                    model: p.0.requested_model.clone(),
                    endpoint: p.0.endpoint_id.clone(),
                    deployment: None,
                    protocol_version: Some(p.0.protocol_version.clone()),
                    available: true,
                    unavailable_reason: None,
                };
                vec![]
            },
            |task, request, ledger, metadata, pages| {
                assert!(pages.is_empty());
                let basis = InvestigationTurnBasis {
                    input_ledger_hash: ledger
                        .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap(),
                    previous_ledger: None,
                    completed_tool: None,
                };
                let input = InvestigationInput::prepare_v9_with_policy(
                    &request.question,
                    task.detail.summary.specialist_id,
                    ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &task.detail.usage,
                    &basis,
                    request,
                    metadata,
                    &[],
                    CompletionCallMode::Choice,
                    0,
                    None,
                    &INVESTIGATION_V2_POLICY,
                )
                .unwrap();
                let action = input
                    .action(format!("{}:step-1", task.detail.summary.investigation_id))
                    .unwrap();
                let prepared = firewall().prepare_bounded(&p, &action, &limits()).unwrap();
                PendingInvestigationStep {
                    step_id: "step-1".into(),
                    payload_hash: prepared.preview().egress.payload_hash.clone(),
                    input_ledger_hash: basis.input_ledger_hash,
                    input_bytes: encode(&action.payload, MAX_INPUT_BYTES).unwrap().len(),
                    generated_tokens: limits().values().max_output_tokens,
                    disclosure: input
                        .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                        .unwrap(),
                }
            },
        );
        let (pages, previous) = if query {
            let (raw, previous) = admission_journal::tests::accept_query(&mut saved);
            (vec![raw], Some(previous))
        } else {
            admission_journal::tests::refuse(&mut saved, 12);
            (vec![], None)
        };
        Self {
            saved,
            pages,
            previous,
            cloud,
        }
    }
    fn build(
        &self,
        forged_capacity: bool,
    ) -> (
        InvestigationInput,
        PreparedBoundedCompletion,
        AuthorizedBoundedCompletion,
        u64,
    ) {
        let policy = &INVESTIGATION_V2_POLICY;
        let capacity = admission_journal::tests::state(&self.saved);
        let mut usage = self.saved.task.detail.usage.clone();
        usage.active_milliseconds += 5;
        let basis = InvestigationTurnBasis {
            input_ledger_hash: self.saved.ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: self.previous.clone(),
            completed_tool: self
                .previous
                .as_ref()
                .map(|_| InvestigationTool::QueryContext),
        };
        let input = InvestigationInput::prepare_v9_with_policy(
            &self.saved.request.question,
            self.saved.task.detail.summary.specialist_id,
            self.saved.ledger.clone(),
            &self.pages.iter().map(String::as_str).collect::<Vec<_>>(),
            &[],
            &[],
            &usage,
            &basis,
            &self.saved.request,
            &self.saved.metadata,
            &[],
            self.saved
                .metadata
                .next_call_mode_with_policy(&usage, policy)
                .unwrap(),
            if forged_capacity { 0 } else { capacity.count },
            if forged_capacity {
                None
            } else {
                capacity.feedback.as_ref()
            },
            policy,
        )
        .unwrap();
        let action = input
            .action(format!("{}:step-2", self.saved.basis.task_id))
            .unwrap();
        let prepared = firewall()
            .prepare_bounded(&provider(self.cloud), &action, &limits())
            .unwrap();
        let grant = llm::ConsentGrant::from_preview(&prepared.preview().egress);
        let authorized = firewall()
            .authorize_bounded(
                &provider(self.cloud),
                &prepared,
                self.cloud.then_some(&grant),
            )
            .unwrap();
        (input, prepared, authorized, usage.active_milliseconds)
    }
    fn snapshot(&self) -> Vec<Vec<u8>> {
        let tx = self.saved.store.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &self.saved.basis.task_id).unwrap();
        let mut result = vec![encode(&task, MAX_RECORD_BYTES).unwrap()];
        for table in [
            "investigation_invocations",
            "investigation_events",
            "investigation_inputs",
            "investigation_responses",
        ] {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
                ))
                .unwrap();
            result.extend(
                statement
                    .query_map([&self.saved.basis.task_id], |row| row.get::<_, Vec<u8>>(0))
                    .unwrap()
                    .map(Result::unwrap),
            );
        }
        result
    }
    fn begin(
        &mut self,
        input: &NextPromptInvocation<'_>,
        authorized: &AuthorizedBoundedCompletion,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.saved.store.begin_next_prompt_invocation(
            &self.saved._execution_guard,
            self.saved.task.detail.summary.revision,
            input,
            authorized,
            |_| Ok(()),
        )
    }
}

#[test]
fn next_prompt_invocation_local_commits_exact_call_and_reopens_refusal_or_query_state() {
    for query in [false, true] {
        let mut f = Fixture::new(false, query);
        let (input, prepared, authorized, active) = f.build(false);
        let original_usage = f.saved.task.detail.usage.clone();
        let request = NextPromptInvocation {
            input: &input,
            prepared: &prepared,
            active_milliseconds: active,
        };
        let detail = f.begin(&request, &authorized).unwrap();
        assert_eq!(
            detail.usage.model_invocations,
            original_usage.model_invocations + 1
        );
        assert_eq!(
            detail.usage.generated_token_reservations,
            original_usage.generated_token_reservations + limits().values().max_output_tokens
        );
        assert_eq!(detail.usage.active_milliseconds, active);
        let reopened = JobStore::open(f.saved.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
        assert_eq!(saved.phase, Phase::Invocation);
        let call = saved
            .admission_state
            .as_ref()
            .unwrap()
            .current_call
            .as_ref()
            .unwrap();
        assert_eq!(call.step_id, "step-2");
        assert_eq!(call.invocation_ordinal, 2);
        assert_eq!(call.admission.capacity_refusal_count, u32::from(!query));
        call.validate_saved(&tx, &saved).unwrap();
        drop(tx);
        let before = f.snapshot();
        assert_eq!(
            f.begin(&request, &authorized),
            Err(InvestigationStoreError::Stale)
        );
        assert_eq!(f.snapshot(), before);
        assert_eq!(
            reopened.investigation(&f.saved.basis.task_id).unwrap(),
            saved.detail
        );
    }
}

#[test]
fn next_prompt_invocation_rejects_forged_journal_clock_binding_and_callback_without_effects() {
    let mut f = Fixture::new(false, false);
    let before = f.snapshot();
    let (forged, prepared, authorized, active) = f.build(true);
    assert!(
        f.begin(
            &NextPromptInvocation {
                input: &forged,
                prepared: &prepared,
                active_milliseconds: active
            },
            &authorized
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
    let (input, prepared, authorized, active) = f.build(false);
    for sample in [
        f.saved.task.detail.usage.active_milliseconds - 1,
        active - 1,
        f.saved.task.detail.limits.active_seconds * 1000,
    ] {
        assert!(
            f.begin(
                &NextPromptInvocation {
                    input: &input,
                    prepared: &prepared,
                    active_milliseconds: sample
                },
                &authorized
            )
            .is_err()
        );
        assert_eq!(f.snapshot(), before);
    }
    let action = input.action("wrong-task:step-2".into()).unwrap();
    let wrong = firewall()
        .prepare_bounded(&provider(false), &action, &limits())
        .unwrap();
    assert!(
        f.begin(
            &NextPromptInvocation {
                input: &input,
                prepared: &wrong,
                active_milliseconds: active
            },
            &authorized
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
    let called = std::cell::Cell::new(0);
    assert_eq!(
        f.saved.store.begin_next_prompt_invocation(
            &f.saved._execution_guard,
            f.saved.task.detail.summary.revision,
            &NextPromptInvocation {
                input: &input,
                prepared: &prepared,
                active_milliseconds: active
            },
            &authorized,
            |_| {
                called.set(called.get() + 1);
                Err(InvestigationStoreError::Cancelled)
            }
        ),
        Err(InvestigationStoreError::Cancelled)
    );
    assert_eq!(called.get(), 1);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn next_prompt_invocation_final_write_rollback_and_lost_ack_do_not_duplicate_call() {
    let mut f = Fixture::new(false, true);
    let (input, prepared, authorized, active) = f.build(false);
    let request = NextPromptInvocation {
        input: &input,
        prepared: &prepared,
        active_milliseconds: active,
    };
    let before = f.snapshot();
    FAIL_BEFORE_INVOCATION_COMMIT.with(|flag| flag.set(true));
    assert_eq!(
        f.begin(&request, &authorized),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(f.snapshot(), before);
    LOSE_INVOCATION_ACK.with(|flag| flag.set(true));
    assert_eq!(
        f.begin(&request, &authorized),
        Err(InvestigationStoreError::Storage)
    );
    assert_ne!(f.snapshot(), before);
    let committed = f.snapshot();
    assert_eq!(
        f.begin(&request, &authorized),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(f.snapshot(), committed);
    let reopened = JobStore::open(f.saved.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let saved = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
    assert_eq!(saved.detail.usage.model_invocations, 2);
    saved
        .admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap()
        .validate_saved(&tx, &saved)
        .unwrap();
}

#[test]
fn next_prompt_invocation_cloud_consent_is_exact_durable_and_not_worker_approval() {
    let mut f = Fixture::new(true, true);
    let (input, prepared, authorized, active) = f.build(false);
    let request = NextPromptInvocation {
        input: &input,
        prepared: &prepared,
        active_milliseconds: active,
    };
    let before = f.snapshot();
    assert_eq!(
        f.begin(&request, &authorized),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(f.snapshot(), before);
    let detail = f
        .saved
        .store
        .await_next_prompt_consent(
            &f.saved._execution_guard,
            f.saved.task.detail.summary.revision,
            &request,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::AwaitingConsent);
    assert_eq!(detail.usage.model_invocations, 1);
    assert_eq!(
        detail.usage.generated_token_reservations,
        f.saved.task.detail.usage.generated_token_reservations
    );
    {
        let tx = f.saved.store.conn.unchecked_transaction().unwrap();
        f.saved.task = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
        assert!(!f.saved.task.approved);
        assert_eq!(
            completion::next_prompt_invocations(&tx, &f.saved.task)
                .unwrap()
                .len(),
            1
        );
    }
    assert!(
        f.saved
            .store
            .approve_investigation(
                &f.saved.basis.task_id,
                detail.summary.revision,
                "step-2",
                &prepared.preview().egress.payload_hash
            )
            .is_err()
    );
    let awaiting = f.snapshot();
    assert_eq!(
        f.begin(&request, &authorized),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(f.snapshot(), awaiting);
    // Test-only explicit user decision. The owner writer never manufactures it.
    {
        let tx = f.saved.store.conn.transaction().unwrap();
        f.saved.task.approved = true;
        bump(&tx, &mut f.saved.task).unwrap();
        event(
            &tx,
            &mut f.saved.task,
            InvestigationEventKind::ConsentApproved,
            Some("step-2"),
            None,
            "Explicit test user approval of the exact preview.",
        )
        .unwrap();
        admission_journal::tests::save_task(&tx, &f.saved.task);
        tx.commit().unwrap();
    }
    let detail = f.begin(&request, &authorized).unwrap();
    assert_eq!(detail.usage.model_invocations, 2);
    assert_eq!(detail.summary.status, InvestigationStatus::Running);
}

#[test]
fn next_prompt_invocation_consent_reopen_rejects_uninvoked_bad_phase_and_stamp_tampering() {
    let mut f = Fixture::new(true, true);
    let (input, prepared, _, active) = f.build(false);
    f.saved
        .store
        .await_next_prompt_consent(
            &f.saved._execution_guard,
            f.saved.task.detail.summary.revision,
            &NextPromptInvocation {
                input: &input,
                prepared: &prepared,
                active_milliseconds: active,
            },
            |_| Ok(()),
        )
        .unwrap();
    let tx = f.saved.store.conn.unchecked_transaction().unwrap();
    let valid = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
    for mutation in 0..3 {
        let mut altered: StoredTask =
            serde_json::from_slice(&encode(&valid, MAX_RECORD_BYTES).unwrap()).unwrap();
        match mutation {
            0 => {
                altered.phase = Phase::Invocation;
                altered.detail.summary.status = InvestigationStatus::Running;
            }
            1 => {
                altered
                    .admission_state
                    .as_mut()
                    .unwrap()
                    .current_call
                    .as_mut()
                    .unwrap()
                    .admission
                    .capacity_refusal_count = 1;
            }
            2 => {
                altered.pending.as_mut().unwrap().payload_hash = "f".repeat(64);
            }
            _ => unreachable!(),
        }
        admission_journal::tests::save_task(&tx, &altered);
        assert!(load_next_prompt_task_base(&tx, &f.saved.basis.task_id).is_err());
    }
    admission_journal::tests::save_task(&tx, &valid);
    let restored = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
    assert_eq!(restored.detail.usage.model_invocations, 1);
    assert_eq!(
        completion::next_prompt_invocations(&tx, &restored)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn next_prompt_invocation_rejects_rebound_transport_foreign_owner_and_cancellation() {
    let mut f = Fixture::new(false, true);
    let (input, prepared, authorized, active) = f.build(false);
    let action = input
        .action(format!("{}:step-2", f.saved.basis.task_id))
        .unwrap();
    let before = f.snapshot();
    let mut changed_provider = provider(false);
    changed_provider.0.requested_model = "different-model".into();
    let wrong_profile = firewall()
        .prepare_bounded(&changed_provider, &action, &limits())
        .unwrap();
    let wrong_authorization = firewall()
        .authorize_bounded(&changed_provider, &wrong_profile, None)
        .unwrap();
    assert_eq!(
        f.begin(
            &NextPromptInvocation {
                input: &input,
                prepared: &wrong_profile,
                active_milliseconds: active
            },
            &wrong_authorization
        ),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(f.snapshot(), before);
    let mut smaller = limits().values();
    smaller.max_output_tokens = 1;
    let wrong_limits = firewall()
        .prepare_bounded(
            &provider(false),
            &action,
            &CompletionLimits::new(smaller).unwrap(),
        )
        .unwrap();
    let wrong_authorization = firewall()
        .authorize_bounded(&provider(false), &wrong_limits, None)
        .unwrap();
    assert_eq!(
        f.begin(
            &NextPromptInvocation {
                input: &input,
                prepared: &wrong_limits,
                active_milliseconds: active
            },
            &wrong_authorization
        ),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(f.snapshot(), before);
    let other = Fixture::new(false, true);
    let invocation = NextPromptInvocation {
        input: &input,
        prepared: &prepared,
        active_milliseconds: active,
    };
    assert_eq!(
        f.saved.store.begin_next_prompt_invocation(
            &other.saved._execution_guard,
            f.saved.task.detail.summary.revision,
            &invocation,
            &authorized,
            |_| Ok(())
        ),
        Err(InvestigationStoreError::Ownership)
    );
    assert_eq!(f.snapshot(), before);
    {
        let tx = f.saved.store.conn.transaction().unwrap();
        f.saved.task.detail.summary.cancel_requested = true;
        bump(&tx, &mut f.saved.task).unwrap();
        admission_journal::tests::save_task(&tx, &f.saved.task);
        tx.commit().unwrap();
    }
    let cancelled = f.snapshot();
    assert_eq!(
        f.begin(&invocation, &authorized),
        Err(InvestigationStoreError::Cancelled)
    );
    assert_eq!(f.snapshot(), cancelled);
}
