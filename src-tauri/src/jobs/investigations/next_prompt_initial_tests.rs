//! AC-0287: production initial publication and first call, beginning with a
//! genuinely unprepared owner claim; no seeded invocation or model execution.
use super::*;
use completion::{InitialFixture, initial_fixture, next_prompt_snapshot::NextPromptSnapshot};
use llm::bounded::{AuthorizedBoundedCompletion, PreparedBoundedCompletion};
use next_prompt_invocation::tests::{firewall, limits, provider};

fn fixture() -> InitialFixture {
    let profile = provider(false).0;
    initial_fixture(
        InvestigationProviderMode::Local,
        InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: profile.provider_id,
            model: profile.requested_model,
            endpoint: profile.endpoint_id,
            deployment: None,
            protocol_version: Some(profile.protocol_version),
            available: true,
            unavailable_reason: None,
        },
    )
}
fn build(
    f: &InitialFixture,
) -> (
    InvestigationInput,
    PreparedBoundedCompletion,
    AuthorizedBoundedCompletion,
    u64,
) {
    build_current(f, None)
}
fn build_current(
    f: &InitialFixture,
    snapshot: Option<&NextPromptSnapshot>,
) -> (
    InvestigationInput,
    PreparedBoundedCompletion,
    AuthorizedBoundedCompletion,
    u64,
) {
    let policy = &INVESTIGATION_V2_POLICY;
    let cloud = f.task.detail.provider.mode == InvestigationProviderMode::Cloud;
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
        snapshot.map_or(CompletionCallMode::Choice, NextPromptSnapshot::mode),
        snapshot.map_or(0, NextPromptSnapshot::capacity_refusal_count),
        snapshot.and_then(NextPromptSnapshot::capacity_feedback),
        policy,
    )
    .unwrap();
    let input = if let Some(snapshot) = snapshot {
        snapshot.attach_acquisition(input).unwrap()
    } else if f.task.detail.summary.specialist_id == SpecialistId::DomainAnalystV18 {
        let choices = agents::investigation::acquisition_choices::AcquisitionChoices::derive(
            &f.task.completion_intent.as_ref().unwrap().contract(),
            &f.metadata,
            &f.task.detail.summary.investigation_id,
            &f.ledger,
            &[],
            policy,
        )
        .unwrap();
        input
            .with_acquisition_choices(std::sync::Arc::new(choices))
            .unwrap()
    } else {
        input
    };
    let action = input
        .action(format!(
            "{}:step-{}",
            f.task.detail.summary.investigation_id,
            f.task.detail.usage.model_invocations + 1
        ))
        .unwrap();
    let prepared = firewall()
        .prepare_bounded(&provider(cloud), &action, &limits())
        .unwrap();
    let grant = llm::ConsentGrant::from_preview(&prepared.preview().egress);
    let authorized = firewall()
        .authorize_bounded(&provider(cloud), &prepared, cloud.then_some(&grant))
        .unwrap();
    (input, prepared, authorized, usage.active_milliseconds)
}
fn snapshot(f: &InitialFixture) -> Vec<Vec<u8>> {
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &f.task.detail.summary.investigation_id).unwrap();
    let mut result = vec![encode(&task, MAX_RECORD_BYTES).unwrap()];
    let mut statement = tx
        .prepare("SELECT name FROM sqlite_schema ORDER BY name")
        .unwrap();
    for row in statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
    {
        result.push(row.unwrap().into_bytes());
    }
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_invocations",
    ] {
        let mut statement = tx
            .prepare(&format!(
                "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
            ))
            .unwrap();
        result.extend(
            statement
                .query_map([&f.task.detail.summary.investigation_id], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    result
}

#[test]
fn next_prompt_initial_publication_and_first_call_use_production_owner_boundaries() {
    let mut f = fixture();
    let (input, prepared, authorized, active) = build(&f);
    let invocation = NextPromptInvocation {
        input: &input,
        prepared: &prepared,
        active_milliseconds: active,
    };
    let detail = f
        .store
        .admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &invocation,
            &f.archive,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(detail.usage.model_invocations, 0);
    assert_eq!(detail.usage.generated_token_reservations, 0);
    assert_eq!(detail.usage.active_milliseconds, active);
    assert_eq!(detail.summary.status, InvestigationStatus::Running);
    {
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &detail.summary.investigation_id).unwrap();
        assert_eq!(task.phase, Phase::Ready);
        assert!(
            task.admission_state
                .as_ref()
                .unwrap()
                .current_call
                .is_none()
        );
        assert_eq!(
            storage::load_ledger(&tx, &task).unwrap().as_ref(),
            Some(&f.ledger)
        );
        assert_eq!(
            coverage::load(&tx, &detail.summary.investigation_id)
                .unwrap()
                .as_ref(),
            Some(&f.archive)
        );
        assert!(
            completion::next_prompt_invocations(&tx, &task)
                .unwrap()
                .is_empty()
        );
        assert!(
            reopened
                .investigation(&detail.summary.investigation_id)
                .is_err()
        );
    }
    let after = f
        .store
        .begin_next_prompt_invocation(
            &f.execution,
            detail.summary.revision,
            &invocation,
            &authorized,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(after.usage.model_invocations, 1);
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let saved = load_next_prompt_task_base(&tx, &detail.summary.investigation_id).unwrap();
    let call = saved
        .admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap();
    assert_eq!(call.invocation_ordinal, 1);
    call.validate_saved(&tx, &saved).unwrap();
}

#[test]
fn next_prompt_initial_callback_and_final_write_failures_rollback_archive_namespace_too() {
    let mut f = fixture();
    let (input, prepared, _, active) = build(&f);
    let invocation = NextPromptInvocation {
        input: &input,
        prepared: &prepared,
        active_milliseconds: active,
    };
    let before = snapshot(&f);
    let called = std::cell::Cell::new(0);
    assert_eq!(
        f.store.admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &invocation,
            &f.archive,
            |_| {
                called.set(called.get() + 1);
                Err(InvestigationStoreError::Cancelled)
            }
        ),
        Err(InvestigationStoreError::Cancelled)
    );
    assert_eq!(called.get(), 1);
    assert_eq!(snapshot(&f), before);
    FAIL_BEFORE_INITIAL_COMMIT.with(|flag| flag.set(true));
    assert_eq!(
        f.store.admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &invocation,
            &f.archive,
            |_| Ok(())
        ),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(snapshot(&f), before);
    LOSE_INITIAL_ACK.with(|flag| flag.set(true));
    assert_eq!(
        f.store.admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &invocation,
            &f.archive,
            |_| Ok(())
        ),
        Err(InvestigationStoreError::Storage)
    );
    let committed = snapshot(&f);
    assert_ne!(committed, before);
    assert_eq!(
        f.store.admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &invocation,
            &f.archive,
            |_| Ok(())
        ),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(snapshot(&f), committed);
}

#[test]
fn next_prompt_initial_rejects_archive_action_and_clock_rebinding_without_publication() {
    let mut f = fixture();
    let (input, prepared, _, active) = build(&f);
    let before = snapshot(&f);
    let mut archive = serde_json::to_value(&f.archive).unwrap();
    archive["inventories"][0]["inventory_hash"] = serde_json::json!("f".repeat(64));
    let archive: InvestigationCoverageArchive = serde_json::from_value(archive).unwrap();
    assert!(
        f.store
            .admit_next_prompt_initial_context(
                &f.execution,
                f.task.detail.summary.revision,
                &NextPromptInvocation {
                    input: &input,
                    prepared: &prepared,
                    active_milliseconds: active
                },
                &archive,
                |_| Ok(())
            )
            .is_err()
    );
    assert_eq!(snapshot(&f), before);
    let wrong = firewall()
        .prepare_bounded(
            &provider(false),
            &input.action("wrong-task:step-1".into()).unwrap(),
            &limits(),
        )
        .unwrap();
    assert!(
        f.store
            .admit_next_prompt_initial_context(
                &f.execution,
                f.task.detail.summary.revision,
                &NextPromptInvocation {
                    input: &input,
                    prepared: &wrong,
                    active_milliseconds: active
                },
                &f.archive,
                |_| Ok(())
            )
            .is_err()
    );
    assert_eq!(snapshot(&f), before);
    for sample in [active - 1, f.task.detail.limits.active_seconds * 1000] {
        assert!(
            f.store
                .admit_next_prompt_initial_context(
                    &f.execution,
                    f.task.detail.summary.revision,
                    &NextPromptInvocation {
                        input: &input,
                        prepared: &prepared,
                        active_milliseconds: sample
                    },
                    &f.archive,
                    |_| Ok(())
                )
                .is_err()
        );
        assert_eq!(snapshot(&f), before);
    }
}

#[test]
fn next_prompt_initial_factory_nonce_recovery_and_public_gate() {
    let f = fixture();
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    assert!(matches!(
        store.start_completion_investigation(&f.request, Some(&f.task.detail.provider)),
        Err(InvestigationStoreError::Invalid)
    ));
    let count: i64 = store
        .conn
        .query_row("SELECT count(*) FROM investigation_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let first = store
        .start_next_prompt_investigation(&f.request, Some(&f.task.detail.provider))
        .unwrap();
    assert!(first.created);
    assert_eq!(first.detail.summary.schema_version, 7);
    let replay = store
        .start_next_prompt_investigation(&f.request, None)
        .unwrap();
    assert!(!replay.created);
    assert_eq!(first.detail, replay.detail);
    let mut changed = f.request.clone();
    changed.question.push_str(" changed");
    assert!(matches!(
        store.start_next_prompt_investigation(&changed, None),
        Err(InvestigationStoreError::Conflict)
    ));
    assert_eq!(
        store
            .investigation(&first.detail.summary.investigation_id)
            .unwrap(),
        first.detail
    );
    assert_eq!(
        store.existing_investigation_completion(&f.request).unwrap(),
        Some(first.detail.clone())
    );
    assert!(matches!(
        store.existing_investigation_completion(&changed),
        Err(InvestigationStoreError::Conflict)
    ));
    let mut reopened = JobStore::open(directory.path().join("state.db")).unwrap();
    assert!(
        !reopened
            .start_next_prompt_investigation(&f.request, None)
            .unwrap()
            .created
    );
}

#[test]
fn next_prompt_initial_terminal_writer_preserves_charges_and_rejects_stale_retries() {
    // AC-0287: stopping uses the actual new-family owner and invocation boundary.
    for invoked in [false, true] {
        let mut f = fixture();
        let (input, prepared, authorized, active) = build(&f);
        let invocation = NextPromptInvocation {
            input: &input,
            prepared: &prepared,
            active_milliseconds: active,
        };
        let mut detail = f
            .store
            .admit_next_prompt_initial_context(
                &f.execution,
                f.task.detail.summary.revision,
                &invocation,
                &f.archive,
                |_| Ok(()),
            )
            .unwrap();
        if invoked {
            detail = f
                .store
                .begin_next_prompt_invocation(
                    &f.execution,
                    detail.summary.revision,
                    &invocation,
                    &authorized,
                    |_| Ok(()),
                )
                .unwrap();
        }
        let before = snapshot(&f);
        assert!(matches!(
            f.store.stop_next_prompt_investigation(
                &f.execution,
                detail.summary.revision + 1,
                InvestigationStopReason::Operational,
                active + 1,
            ),
            Err(InvestigationStoreError::Stale)
        ));
        assert!(matches!(
            f.store.stop_next_prompt_investigation(
                &f.execution,
                detail.summary.revision,
                InvestigationStopReason::Operational,
                active - 1,
            ),
            Err(InvestigationStoreError::Invalid)
        ));
        assert_eq!(snapshot(&f), before);
        let reason = if invoked {
            InvestigationStopReason::OutcomeUnknown
        } else {
            InvestigationStopReason::Operational
        };
        let stopped = f
            .store
            .stop_next_prompt_investigation(
                &f.execution,
                detail.summary.revision,
                reason,
                active + 1,
            )
            .unwrap();
        assert_eq!(
            stopped.usage.model_invocations,
            detail.usage.model_invocations
        );
        assert_eq!(
            stopped.usage.generated_token_reservations,
            detail.usage.generated_token_reservations
        );
        assert_eq!(stopped.usage.tool_actions, 0);
        assert_eq!(stopped.usage.active_milliseconds, active + 1);
        assert_eq!(
            stopped.summary.status,
            if invoked {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Failed
            }
        );
        let tx = f.store.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &stopped.summary.investigation_id).unwrap();
        assert_eq!(saved.phase, Phase::Terminal);
        drop(tx);
        assert!(matches!(
            f.store.stop_next_prompt_investigation(
                &f.execution,
                stopped.summary.revision,
                reason,
                active + 1,
            ),
            Err(InvestigationStoreError::Stale)
        ));
    }
}

#[test]
fn next_prompt_initial_owned_detail_requires_owner_alongside_public_history() {
    // AC-0287: runtime reads authenticate the held execution, not only a task id.
    let mut f = fixture();
    let id = f.task.detail.summary.investigation_id.clone();
    assert_eq!(
        f.store
            .investigation_owned_detail(&f.execution, &id)
            .unwrap(),
        f.task.detail
    );
    assert_eq!(f.store.investigation(&id).unwrap(), f.task.detail);
    let foreign = fixture();
    assert!(matches!(
        f.store.investigation_owned_detail(&foreign.execution, &id),
        Err(InvestigationStoreError::Ownership)
    ));
    let cancelled = f.store.cancel_next_prompt_investigation(&id).unwrap();
    assert!(cancelled.summary.cancel_requested);
    assert_eq!(
        f.store
            .investigation_owned_detail(&f.execution, &id)
            .unwrap(),
        cancelled
    );
}

#[test]
fn next_prompt_initial_state_binds_original_request_owner_and_revision() {
    let mut f = fixture();
    let revision = f.task.detail.summary.revision;
    let (detail, metadata, contract) = f
        .store
        .investigation_next_prompt_initial_state(&f.execution, revision, &f.request)
        .unwrap();
    assert_eq!(detail, f.task.detail);
    assert_eq!(metadata, f.metadata);
    assert!(contract.is_none()); // historical initial state creates no choices owner
    let mut changed = f.request.clone();
    changed.question.push_str(" Different intent.");
    assert!(matches!(
        f.store
            .investigation_next_prompt_initial_state(&f.execution, revision, &changed,),
        Err(InvestigationStoreError::Conflict)
    ));
    assert!(matches!(
        f.store
            .investigation_next_prompt_initial_state(&f.execution, revision + 1, &f.request,),
        Err(InvestigationStoreError::Stale)
    ));
    let foreign = fixture();
    assert!(matches!(
        f.store
            .investigation_next_prompt_initial_state(&foreign.execution, revision, &f.request,),
        Err(InvestigationStoreError::Ownership)
    ));
    let cancelled = f
        .store
        .cancel_next_prompt_investigation(&detail.summary.investigation_id)
        .unwrap();
    assert!(matches!(
        f.store.investigation_next_prompt_initial_state(
            &f.execution,
            cancelled.summary.revision,
            &f.request,
        ),
        Err(InvestigationStoreError::Cancelled)
    ));
}

// AC-0324: the scoped path uses the same next-family terminal reservation.
#[test]
fn principal_task_access_next_family_cancellation_preserves_owner_parity() {
    let mut f = fixture();
    let id = f.task.detail.summary.investigation_id.clone();
    let client = McpClientId::from_host_bytes([3; 16]);
    f.task.detail.origin = InvestigationOrigin::Mcp {
        client_id: client.clone(),
    };
    f.task.mcp_intent_hash = Some("a".repeat(64));
    let bytes = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    f.store.conn.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3,request_namespace=?4 WHERE id=?1",
        params![id, bytes, core_prov::content_hash(&bytes), f.task.detail.origin.request_namespace()],
    ).unwrap();
    let activation =
        McpTaskActivation::granted(client.clone(), std::num::NonZeroUsize::new(4).unwrap());
    let session = activation.session();
    let before = f.store.investigation(&id).unwrap();
    assert_eq!(
        f.store.investigation_access(&id, Some(&session)).unwrap(),
        before
    );
    let cancelled = f
        .store
        .cancel_investigation_access(&id, false, Some(&session))
        .unwrap();
    assert!(cancelled.summary.cancel_requested);
    // The execution owner still holds the claim; cancellation is requested,
    // not falsely reported as settled before that worker exits.
    assert_eq!(cancelled.summary.status, before.summary.status);
    assert!(matches!(
        f.store.investigation_next_prompt_initial_state(
            &f.execution,
            cancelled.summary.revision,
            &f.request,
        ),
        Err(InvestigationStoreError::Cancelled)
    ));
    assert_eq!(f.store.investigation(&id).unwrap(), cancelled);
    assert_eq!(
        f.store.cancel_next_prompt_investigation(&id).unwrap(),
        cancelled
    );
}

// AC-0322/0327: the completion factory persists attribution and replays before
// provider/capacity admission. This queues metadata only; no worker is launched.
#[test]
fn mcp_nonce_completion_factory_replays_without_provider_or_new_capacity() {
    let mut f = fixture();
    let mut identity = McpRequestIdentity {
        client: f
            .store
            .create_mcp_client(std::num::NonZeroUsize::new(1).unwrap())
            .unwrap(),
        normalized_intent_hash: core_prov::content_hash(b"normalized completion request"),
        maximum_identities: 1,
    };
    let created = f
        .store
        .start_completion_family(
            &f.request,
            Some(&f.task.detail.provider),
            true,
            Some(&identity),
        )
        .unwrap();
    assert!(created.created);
    assert_ne!(
        created.detail.summary.investigation_id,
        f.task.detail.summary.investigation_id
    );
    assert_eq!(
        created.detail.origin,
        InvestigationOrigin::Mcp {
            client_id: identity.client.clone()
        }
    );
    identity.maximum_identities = 0;
    let replay = f
        .store
        .start_completion_family(&f.request, None, true, Some(&identity))
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.detail, created.detail);
    assert_eq!(
        f.store
            .investigation(&created.detail.summary.investigation_id)
            .unwrap(),
        created.detail
    );
}

// AC-0327: direct loading enforces metadata byte bounds without schema preflight.
#[test]
fn next_prompt_base_load_rejects_namespace_byte_overflow_without_schema_preflight() {
    let f = fixture();
    let id = &f.task.detail.summary.investigation_id;
    for invalid in [
        "x".repeat(31),
        "é".repeat(32),
        "x".repeat(MAX_RECORD_BYTES + 1),
    ] {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE investigation_meta SET namespace=?1 WHERE singleton=1",
            [&invalid],
        )
        .unwrap();
        assert!(matches!(
            load_next_prompt_task_base(&tx, id),
            Err(InvestigationStoreError::Invalid)
        ));
        tx.rollback().unwrap();
    }
    let tx = f.store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        load_next_prompt_task_base(&tx, id).unwrap().detail,
        f.task.detail
    );
}

// Coherently rehash the saved task and, after ModelStarted, its exact named
// invocation. Thus these negatives reach prefix authentication, not a mismatch
// between a caller-only clone and the task's current-call/disclosure mirrors.
fn reject_feedback(f: &InitialFixture, hash: Option<String>) {
    let id = &f.task.detail.summary.investigation_id;
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let mut task = load_next_prompt_task(&tx, id).unwrap();
    let call = task
        .admission_state
        .as_mut()
        .unwrap()
        .current_call
        .as_mut()
        .unwrap();
    call.admission
        .acquisition
        .as_mut()
        .unwrap()
        .action_feedback_hash = hash;
    let admission = call.admission.clone();
    let step = task.pending.as_mut().unwrap();
    step.disclosure.as_mut().unwrap().admission = Some(admission);
    if task.phase == Phase::Invocation {
        completion::rewrite_test_body(
            &tx,
            "investigation_invocations",
            id,
            "step_id",
            &step.step_id,
            &encode(step, MAX_INVOCATION_BYTES).unwrap(),
        );
    }
    admission_journal::tests::save_task(&tx, &task);
    let call = task
        .admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap();
    if task.phase == Phase::Consent {
        assert!(call.validate_pending_consent(&tx, &task).is_err());
    } else {
        assert!(call.validate_saved(&tx, &task).is_err());
    }
}

// AC-0484: real current producer, egress seal, pre-call consent and saved replay;
// only the cold Preparing task family is seeded; no provider is dispatched.
#[test]
fn gated_feedback_sealing_runs_real_invocation_consent_and_reopen() {
    let profile = provider(true).0;
    let mut f = initial_fixture(
        InvestigationProviderMode::Cloud,
        InvestigationProvider {
            mode: InvestigationProviderMode::Cloud,
            provider_id: profile.provider_id,
            model: profile.requested_model,
            endpoint: profile.endpoint_id,
            deployment: None,
            protocol_version: Some(profile.protocol_version),
            available: true,
            unavailable_reason: None,
        },
    );
    f.request.specialist_id = "domain-analyst@18".into();
    let intent = completion::host_fixture_intent(&f.request).unwrap();
    f.task.intent_hash = intent.request_hash.clone();
    f.task.completion_intent = Some(intent.clone());
    f.task.detail.completion = Some(completion::initial_detail(&intent));
    f.task.detail.summary.specialist_id = SpecialistId::DomainAnalystV18;
    f.task.detail.specialist = SpecialistId::DomainAnalystV18.definition().unwrap();
    f.metadata.request_hash = intent.request_hash;
    let id = f.task.detail.summary.investigation_id.clone();
    {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        admission_journal::tests::save_task(&tx, &f.task);
        tx.commit().unwrap();
    }
    for forged in ["input", "invocation", "response", "refusal", "usage"] {
        let tx = f.store.conn.unchecked_transaction().unwrap();
        load_next_prompt_task(&tx, &id).unwrap();
        match forged {
            "input" => insert_body(
                &tx,
                "investigation_inputs",
                &id,
                &0i64,
                &f.ledger,
                MAX_MANIFEST_BYTES,
            )
            .unwrap(),
            "invocation" | "response" => {
                let table = if forged == "invocation" {
                    "investigation_invocations"
                } else {
                    "investigation_responses"
                };
                insert_body(
                    &tx,
                    table,
                    &id,
                    &"orphan",
                    &serde_json::json!({}),
                    MAX_INVOCATION_BYTES,
                )
                .unwrap();
            }
            "refusal" => {
                admission_journal::initialize(&tx).unwrap();
                let body = b"{}";
                tx.execute(
                    "INSERT INTO investigation_admission_refusals VALUES (?1,1,'orphan',1,?2,?3)",
                    params![id, body.as_slice(), core_prov::content_hash(body)],
                )
                .unwrap();
            }
            "usage" => {
                let mut task = f.task.clone();
                task.detail.usage.model_invocations = 1;
                admission_journal::tests::save_task(&tx, &task);
            }
            _ => unreachable!(),
        }
        assert!(load_next_prompt_task(&tx, &id).is_err(), "cold {forged}");
    }
    for ordinal in 1..=2 {
        let current = (ordinal == 2).then(|| {
            f.store
                .investigation_next_prompt_snapshot(&id, &f.request)
                .unwrap()
        });
        let (input, prepared, authorized, active) = build_current(&f, current.as_ref());
        let invocation = NextPromptInvocation {
            input: &input,
            prepared: &prepared,
            active_milliseconds: active,
        };
        if ordinal == 1 {
            f.store
                .admit_next_prompt_initial_context(
                    &f.execution,
                    f.task.detail.summary.revision,
                    &invocation,
                    &f.archive,
                    |_| Ok(()),
                )
                .unwrap();
        }
        let refresh = |f: &mut InitialFixture| {
            let tx = f.store.conn.unchecked_transaction().unwrap();
            f.task = load_next_prompt_task(&tx, &id).unwrap();
            f.metadata = completion::metadata_current_next_prompt(&tx, &f.task).unwrap();
        };
        refresh(&mut f);
        let before = f.task.detail.usage.clone();
        let waiting = f
            .store
            .await_next_prompt_consent(
                &f.execution,
                f.task.detail.summary.revision,
                &invocation,
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(waiting.usage.model_invocations, ordinal - 1);
        let step_id = format!("step-{ordinal}");
        let hash = prepared.preview().egress.payload_hash.clone();
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        {
            let tx = reopened.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, &id).unwrap();
            let call = task
                .admission_state
                .as_ref()
                .unwrap()
                .current_call
                .as_ref()
                .unwrap();
            call.validate_pending_consent(&tx, &task).unwrap();
            assert_eq!(task.pending.as_ref().unwrap().payload_hash, hash);
            assert_eq!(
                call.admission.consumed_budgets.model_invocations,
                ordinal - 1
            );
            assert_eq!(
                call.admission
                    .acquisition
                    .as_ref()
                    .unwrap()
                    .action_feedback_hash
                    .is_some(),
                ordinal == 2
            );
            assert_eq!(
                completion::next_prompt_invocations(&tx, &task)
                    .unwrap()
                    .len(),
                (ordinal - 1) as usize
            );
        }
        let hashes = if ordinal == 1 {
            // No prior refusal exists: even a well-shaped Some is invalid.
            vec![Some("f".repeat(64))]
        } else {
            let tx = f.store.conn.unchecked_transaction().unwrap();
            let task = load_next_prompt_task(&tx, &id).unwrap();
            let feedback = admission_journal::derive_current(&tx, &task)
                .unwrap()
                .action_feedback
                .unwrap();
            let mut other_ordinal = feedback.clone();
            other_ordinal.invocation_ordinal += 1;
            let mut other_action = feedback;
            other_action.action_hash = core_prov::content_hash(b"another refused action");
            vec![
                None,
                Some(other_ordinal.stamp_hash().unwrap()),
                Some(other_action.stamp_hash().unwrap()),
            ]
        };
        for hash in &hashes {
            reject_feedback(&f, hash.clone());
        }
        let approved = f
            .store
            .approve_next_prompt_investigation(&id, waiting.summary.revision, &step_id, &hash)
            .unwrap();
        assert_eq!(approved.usage, waiting.usage);
        let started = f
            .store
            .begin_next_prompt_invocation(
                &f.execution,
                approved.summary.revision,
                &invocation,
                &authorized,
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(started.usage.model_invocations, ordinal);
        assert_eq!(started.usage.tool_actions, before.tool_actions);
        refresh(&mut f);
        {
            let tx = f.store.conn.unchecked_transaction().unwrap();
            let call = f
                .task
                .admission_state
                .as_ref()
                .unwrap()
                .current_call
                .as_ref()
                .unwrap();
            call.validate_saved(&tx, &f.task).unwrap();
            assert_eq!(
                completion::next_prompt_invocations(&tx, &f.task)
                    .unwrap()
                    .len(),
                ordinal as usize
            );
        }
        for hash in &hashes {
            reject_feedback(&f, hash.clone());
        }
        if ordinal == 1 {
            let original = serde_json::json!({"type":"query_context",
                "context_id":f.request.expected_context_id,
                "query":{"scope":{"type":"all"},"kind":null,"labels":[],
                    "max_facts":12,"max_bytes":16384,"cursor":null}});
            let response = InvestigationResponseMeta {
                finish_feedback_witness: None,
                step_id,
                response_hash: core_prov::content_hash(b"bounded scripted response"),
                action_hash: core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap()),
                observed_model: None,
                reported_input_tokens: None,
                reported_output_tokens: None,
                reported_cache_read_tokens: None,
                active_milliseconds: active + 1,
            };
            f.store
                .refuse_next_prompt(
                    &f.execution,
                    started.summary.revision,
                    &response,
                    &original,
                    admission_journal::RefusalReason::Action(
                        agents::investigation::ActionRefusalReason::ActionNotOffered,
                    ),
                )
                .unwrap();
            refresh(&mut f);
            assert_eq!(f.task.detail.usage.tool_actions, 0);
        }
    }
}
