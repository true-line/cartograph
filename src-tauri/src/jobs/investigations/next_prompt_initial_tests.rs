//! AC-0287: production initial publication and first call, beginning with a
//! genuinely unprepared owner claim; no seeded invocation or model execution.
use super::*;
use completion::{InitialFixture, initial_fixture};
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
    let policy = &INVESTIGATION_V2_POLICY;
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
    let action = input
        .action(format!("{}:step-1", f.task.detail.summary.investigation_id))
        .unwrap();
    let prepared = firewall()
        .prepare_bounded(&provider(false), &action, &limits())
        .unwrap();
    let authorized = firewall()
        .authorize_bounded(&provider(false), &prepared, None)
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
    let (detail, metadata) = f
        .store
        .investigation_next_prompt_initial_state(&f.execution, revision, &f.request)
        .unwrap();
    assert_eq!(detail, f.task.detail);
    assert_eq!(metadata, f.metadata);
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
