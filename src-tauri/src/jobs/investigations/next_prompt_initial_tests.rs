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
