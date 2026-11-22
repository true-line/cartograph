//! AC-0287: actual internal cloud start/claim, complete input preparation,
//! pending preview and user decision. No provider dispatch or public activation.
use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use completion::{InitialFixture, initial_fixture};
use llm::{ConsentGrant, bounded::PreparedBoundedCompletion};
use next_prompt_invocation::tests::{OfflineProvider, firewall, limits, provider};
use serde_json::json;

fn selected_provider() -> InvestigationProvider {
    let profile = provider(true).0;
    InvestigationProvider {
        mode: InvestigationProviderMode::Cloud,
        provider_id: profile.provider_id,
        model: profile.requested_model,
        endpoint: profile.endpoint_id,
        deployment: None,
        protocol_version: Some(profile.protocol_version),
        available: true,
        unavailable_reason: None,
    }
}
struct Fixture {
    f: InitialFixture,
    input: InvestigationInput,
    prepared: PreparedBoundedCompletion,
    active: u64,
}
impl Fixture {
    fn new() -> Self {
        Self::prepare(initial_fixture(
            InvestigationProviderMode::Cloud,
            selected_provider(),
        ))
    }
    fn prepare(f: InitialFixture) -> Self {
        Self::prepare_with(f, &provider(true))
    }
    /// Prepares under the task's frozen effective policy and matching limits.
    fn prepare_with(mut f: InitialFixture, bounded: &OfflineProvider) -> Self {
        let effective = policy_for_detail(&f.task.detail).unwrap();
        let policy = &effective;
        let mut usage = f.task.detail.usage.clone();
        usage.active_milliseconds += 5;
        let input = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            f.task.detail.summary.specialist_id,
            f.ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
            &InvestigationTurnBasis {
                input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
                previous_ledger: None,
                completed_tool: None,
            },
            &f.request,
            &f.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            policy,
        )
        .unwrap();
        let mut values = limits().values();
        values.max_output_tokens = policy.limits().generated_tokens_per_invocation;
        let prepared = firewall()
            .prepare_bounded(
                bounded,
                &input
                    .action(format!("{}:step-1", f.task.detail.summary.investigation_id))
                    .unwrap(),
                &llm::bounded::CompletionLimits::for_investigation(values).unwrap(),
            )
            .unwrap();
        let invocation = NextPromptInvocation {
            input: &input,
            prepared: &prepared,
            active_milliseconds: usage.active_milliseconds,
        };
        let ready = f
            .store
            .admit_next_prompt_initial_context(
                &f.execution,
                f.task.detail.summary.revision,
                &invocation,
                &f.archive,
                |_| Ok(()),
            )
            .unwrap();
        f.store
            .await_next_prompt_consent(
                &f.execution,
                ready.summary.revision,
                &invocation,
                |_| Ok(()),
            )
            .unwrap();
        {
            let tx = f.store.conn.unchecked_transaction().unwrap();
            f.task =
                load_next_prompt_task_base(&tx, &f.task.detail.summary.investigation_id).unwrap();
        }
        Self {
            f,
            input,
            prepared,
            active: usage.active_milliseconds,
        }
    }
    fn decide(&mut self, approve: bool) -> Result<InvestigationDetail, InvestigationStoreError> {
        if approve {
            self.f.store.approve_next_prompt_investigation(
                &self.f.task.detail.summary.investigation_id,
                self.f.task.detail.summary.revision,
                "step-1",
                &self.prepared.preview().egress.payload_hash,
            )
        } else {
            self.f.store.decline_next_prompt_investigation(
                &self.f.task.detail.summary.investigation_id,
                self.f.task.detail.summary.revision,
                "step-1",
                &self.prepared.preview().egress.payload_hash,
            )
        }
    }
    fn snapshot(&self) -> Vec<Vec<u8>> {
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        let id = &self.f.task.detail.summary.investigation_id;
        let task = load_next_prompt_task_base(&tx, id).unwrap();
        let mut value = vec![encode(&task, MAX_RECORD_BYTES).unwrap()];
        for table in [
            "investigation_events",
            "investigation_invocations",
            "investigation_responses",
            "investigation_inputs",
        ] {
            value.extend(
                tx.prepare(&format!(
                    "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
                ))
                .unwrap()
                .query_map([id], |row| row.get::<_, Vec<u8>>(0))
                .unwrap()
                .map(Result::unwrap),
            );
        }
        value.push(
            tx.query_row(
                "SELECT status FROM jobs WHERE id=?1",
                [task.detail.summary.job_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .into_bytes(),
        );
        value
    }
}

#[test]
fn investigation_active_history_includes_durable_awaiting_consent() {
    // AC-0372: consent-waiting tasks remain active in persisted selection.
    let fixture = Fixture::new();
    let id = fixture.f.task.detail.summary.investigation_id.clone();
    let page = fixture
        .f
        .store
        .investigation_history_selection(None, true, None)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].investigation_id, id);
    assert_eq!(page.items[0].status, InvestigationStatus::AwaitingConsent);
}

#[test]
fn next_prompt_consent_exact_approval_then_real_begin_preserves_single_invocation() {
    let mut f = Fixture::new();
    let before = f.snapshot();
    assert!(
        firewall()
            .authorize_bounded(&provider(true), &f.prepared, None)
            .is_err()
    );
    let id = f.f.task.detail.summary.investigation_id.clone();
    let revision = f.f.task.detail.summary.revision;
    let hash = f.prepared.preview().egress.payload_hash.clone();
    assert!(
        f.f.store
            .approve_investigation(&id, revision, "step-1", &hash)
            .is_err()
    );
    for (revision, step, hash) in [
        (revision + 1, "step-1", hash.clone()),
        (revision, "step-2", hash.clone()),
        (revision, "step-1", "f".repeat(64)),
    ] {
        assert_eq!(
            f.f.store
                .approve_next_prompt_investigation(&id, revision, step, &hash),
            Err(InvestigationStoreError::Stale)
        );
        assert_eq!(f.snapshot(), before);
    }
    let approved = f.decide(true).unwrap();
    assert_eq!(approved.usage, f.f.task.detail.usage);
    assert_eq!(
        approved.summary.status,
        InvestigationStatus::AwaitingConsent
    );
    assert_eq!(f.decide(true), Err(InvestigationStoreError::Stale));
    assert_eq!(
        f.f.store.approve_next_prompt_investigation(
            &id,
            approved.summary.revision,
            "step-1",
            &hash
        ),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(
        f.f.store.decline_next_prompt_investigation(
            &id,
            approved.summary.revision,
            "step-1",
            &hash
        ),
        Err(InvestigationStoreError::Stale)
    );
    {
        let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &id).unwrap();
        assert!(task.approved);
        #[cfg(unix)]
        {
            // AC-0327: these actual original witnesses survive nested consent replay.
            let ledger = storage::load_ledger(&tx, &task).unwrap().unwrap();
            let metadata = completion::metadata_current_next_prompt(&tx, &task).unwrap();
            let originals = serde_json::to_vec(&ledger).unwrap().len()
                + serde_json::to_vec(&metadata).unwrap().len();
            assert!(
                next_prompt_saved_validation_workspace().unwrap().1
                    >= originals + next_prompt_capacity_replay_workspace().unwrap().1
            );
        }
        task.admission_state
            .as_ref()
            .unwrap()
            .current_call
            .as_ref()
            .unwrap()
            .validate_pending_consent(&tx, &task)
            .unwrap();
        assert!(
            completion::next_prompt_invocations(&tx, &task)
                .unwrap()
                .is_empty()
        );
    }
    let grant = ConsentGrant::from_preview(&f.prepared.preview().egress);
    let authorized = firewall()
        .authorize_bounded(&provider(true), &f.prepared, Some(&grant))
        .unwrap();
    let invoked =
        f.f.store
            .begin_next_prompt_invocation(
                &f.f.execution,
                approved.summary.revision,
                &NextPromptInvocation {
                    input: &f.input,
                    prepared: &f.prepared,
                    active_milliseconds: f.active,
                },
                &authorized,
                |_| Ok(()),
            )
            .unwrap();
    assert_eq!(invoked.usage.model_invocations, 1);
    assert_eq!(
        invoked.usage.generated_token_reservations,
        limits().values().max_output_tokens
    );
    let tx = f.f.store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &id).unwrap();
    assert!(!task.approved);
    assert_eq!(
        completion::next_prompt_invocations(&tx, &task)
            .unwrap()
            .len(),
        1
    );
    task.admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap()
        .validate_saved(&tx, &task)
        .unwrap();
}

#[test]
fn next_prompt_consent_decline_is_terminal_without_charging_uninvoked_preview() {
    let mut f = Fixture::new();
    let prior = f.f.task.detail.usage.clone();
    let declined = f.decide(false).unwrap();
    assert_eq!(declined.summary.status, InvestigationStatus::Cancelled);
    assert!(declined.summary.cancel_requested);
    assert_eq!(declined.usage, prior);
    assert!(!declined.summary.has_result);
    assert!(declined.summary.completion_outcome.is_none());
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &declined.summary.investigation_id).unwrap();
    assert_eq!(task.phase, Phase::Terminal);
    assert!(task.pending.is_none());
    assert!(
        task.admission_state
            .as_ref()
            .unwrap()
            .current_call
            .is_none()
    );
    assert_eq!(
        storage::load_ledger(&tx, &task).unwrap().as_ref(),
        Some(&f.f.ledger)
    );
    assert_eq!(
        coverage::load_for_task(&tx, &task).unwrap().as_ref(),
        Some(&f.f.archive)
    );
    assert!(
        completion::next_prompt_invocations(&tx, &task)
            .unwrap()
            .is_empty()
    );
    let status: String = tx
        .query_row(
            "SELECT status FROM jobs WHERE id=?1",
            [task.detail.summary.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "cancelled");
    assert_eq!(
        pending_event(
            &tx,
            &declined.summary.investigation_id,
            declined.summary.last_event_sequence
        )
        .unwrap()
        .kind,
        InvestigationEventKind::ConsentDeclined
    );
    assert!(
        reopened
            .investigation(&declined.summary.investigation_id)
            .is_err()
    );
}

#[test]
fn next_prompt_consent_cancellation_rollback_and_lost_ack_never_duplicate_decisions() {
    for approve in [false, true] {
        let mut f = Fixture::new();
        let before = f.snapshot();
        FAIL_BEFORE_CONSENT_COMMIT.with(|flag| flag.set(true));
        assert_eq!(f.decide(approve), Err(InvestigationStoreError::Capacity));
        assert_eq!(f.snapshot(), before);
        LOSE_CONSENT_ACK.with(|flag| flag.set(true));
        assert_eq!(f.decide(approve), Err(InvestigationStoreError::Storage));
        let committed = f.snapshot();
        assert_ne!(committed, before);
        assert_eq!(f.decide(approve), Err(InvestigationStoreError::Stale));
        assert_eq!(f.snapshot(), committed);
        let tx = f.f.store.conn.unchecked_transaction().unwrap();
        let task =
            load_next_prompt_task_base(&tx, &f.f.task.detail.summary.investigation_id).unwrap();
        assert_eq!(task.detail.usage.model_invocations, 0);
        assert_eq!(task.detail.usage.generated_token_reservations, 0);
    }
    let mut f = Fixture::new();
    f.f.store
        .cancel_next_prompt_investigation(&f.f.task.detail.summary.investigation_id)
        .unwrap();
    {
        let tx = f.f.store.conn.unchecked_transaction().unwrap();
        f.f.task =
            load_next_prompt_task_base(&tx, &f.f.task.detail.summary.investigation_id).unwrap();
    }
    let before = f.snapshot();
    assert_eq!(f.decide(true), Err(InvestigationStoreError::Cancelled));
    assert_eq!(f.decide(false), Err(InvestigationStoreError::Cancelled));
    assert_eq!(f.snapshot(), before);
}

// A policy-maximal explicit inventory contract makes terminal assessment grow
// while real retained bodies fill storage. No fake history, task counters,
// pending allocations, or caller-selected numeric policy is introduced.
fn large_fixture(padding: usize) -> Fixture {
    let seed = initial_fixture(InvestigationProviderMode::Cloud, selected_provider());
    let snapshot = context_hub::ContextSnapshot::new(vec![], vec![]).unwrap();
    let mut records = vec![];
    let mut repos = vec![];
    let count = INVESTIGATION_V2_POLICY.max_completion_obligations();
    for index in 0..count {
        let bytes = padding / count + usize::from(index < padding % count);
        let repo = format!("fixture/retained-{index}");
        let record = core_graph::coverage::CoverageRecord { repo_key:repo.clone(), registered_source_id:Some("src:fixture".into()), ingest_id:"fixture-ingest".into(), config_hash:"fixture-config".into(), inventory_json:json!({"inventory":context_hub::coverage::INVENTORY_VERSION,"repo":repo,"commit":"workdir","layer_hints":[],"producers":[],"unsupported":[],"limited":false,"retained_note":"a".repeat(bytes)}).to_string() };
        assert!(record.inventory_json.len() <= core_graph::coverage::MAX_INVENTORY_BYTES);
        repos.push(context_hub::coverage::ScopedRepo {
            repo_key: record.repo_key.clone(),
            registered_source_id: record.registered_source_id.clone(),
            config_hash: record.config_hash.clone(),
            layer_hints: None,
        });
        records.push(core_graph::coverage::StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        });
    }
    let proof = context_hub::coverage::CoverageQualification::new(&repos, &records)
        .unwrap()
        .durable_proof(snapshot.id())
        .unwrap();
    let archive = InvestigationCoverageArchive::new(proof.clone(), records.clone()).unwrap();
    let mut request = seed.request.clone();
    request.expected_context_id = proof.context_id.clone();
    request.obligations.obligations = records
        .iter()
        .map(|record| {
            let mut obligation = Obligation::Inventory {
                obligation_id: String::new(),
                target: InventoryTarget {
                    context_id: proof.context_id.clone(),
                    repo: record.record.repo_key.clone(),
                    inventory_hash: record.inventory_hash.clone(),
                },
            };
            let id = obligation.content_id().unwrap();
            if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
                *obligation_id = id;
            }
            obligation
        })
        .collect();
    let request = request
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let started = store
        .start_next_prompt_investigation(&request, Some(&selected_provider()))
        .unwrap();
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let plan = store
        .claim_plan(started.detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &started.detail.summary.investigation_id).unwrap();
    let metadata = completion::derive_protected_metadata(&task, vec![], &[]).unwrap();
    tx.commit().unwrap();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(proof),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: snapshot.id().into(),
        scope_snapshot_id: snapshot.id().into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    Fixture::prepare(InitialFixture {
        directory,
        store,
        execution,
        task,
        request,
        archive,
        ledger,
        metadata,
    })
}

#[test]
fn next_prompt_consent_decline_can_spend_terminal_reserve_at_real_retained_capacity() {
    let baseline = large_fixture(0);
    let used = stored_bytes(
        &baseline.f.store.conn,
        Some(&baseline.f.task.detail.summary.investigation_id),
    )
    .unwrap();
    let limit = TASK_CAPACITY - TERMINAL_RESERVE;
    let padding = limit - used - 8;
    drop(baseline);
    let mut f = large_fixture(padding);
    let used = stored_bytes(
        &f.f.store.conn,
        Some(&f.f.task.detail.summary.investigation_id),
    )
    .unwrap();
    assert_eq!(used, limit - 8);
    let declined = f.decide(false).unwrap();
    assert_eq!(declined.summary.status, InvestigationStatus::Cancelled);
    let after = stored_bytes(&f.f.store.conn, Some(&declined.summary.investigation_id)).unwrap();
    assert!(after > limit && after <= TASK_CAPACITY);
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &declined.summary.investigation_id).unwrap();
    assert_eq!(task.phase, Phase::Terminal);
    assert_eq!(task.detail.usage.model_invocations, 0);
}

// AC-0570: the real desktop start freezes the hosted output allowance only for
// the pinned Opus lane; the persisted task and its consented step carry it.
#[test]
fn hosted_opus_start_freezes_output_allowance_through_consent() {
    let opus = OfflineProvider(llm::bounded::ProviderProfile {
        protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
        provider_id: "anthropic:claude-opus-5-5".into(),
        locality: llm::Locality::Cloud,
        endpoint_id: "https://api.anthropic.com/v1/messages".into(),
        requested_model: "claude-opus-5-5".into(),
    });
    let descriptor = InvestigationProvider {
        mode: InvestigationProviderMode::Cloud,
        provider_id: opus.0.provider_id.clone(),
        model: opus.0.requested_model.clone(),
        endpoint: opus.0.endpoint_id.clone(),
        deployment: None,
        protocol_version: Some(opus.0.protocol_version.clone()),
        available: true,
        unavailable_reason: None,
    };
    let seed = initial_fixture(InvestigationProviderMode::Cloud, descriptor);
    let limits = &seed.task.detail.limits;
    assert_eq!(limits.generated_tokens_per_invocation, 16_384);
    assert_eq!(limits.generated_token_reservations, 1_048_576);
    let mut f = Fixture::prepare_with(seed, &opus);
    assert_eq!(f.prepared.limits().values().max_output_tokens, 16_384);
    // Reloaded from the store: the frozen limits and the pending step's charge.
    assert_eq!(
        f.f.task.detail.limits.generated_tokens_per_invocation,
        16_384
    );
    assert_eq!(f.f.task.pending.as_ref().unwrap().generated_tokens, 16_384);
    let detail = f.decide(true).unwrap();
    assert_eq!(detail.limits.generated_tokens_per_invocation, 16_384);

    // A Local or other cloud provider through the same start keeps 2048.
    for (mode, provider) in [
        (InvestigationProviderMode::Cloud, selected_provider()),
        (
            InvestigationProviderMode::Local,
            InvestigationProvider {
                mode: InvestigationProviderMode::Local,
                ..selected_provider()
            },
        ),
    ] {
        let seed = initial_fixture(mode, provider);
        assert_eq!(seed.task.detail.limits, INVESTIGATION_V2_POLICY.limits());
    }
}
