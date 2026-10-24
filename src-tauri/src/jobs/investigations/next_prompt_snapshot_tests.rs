//! AC-0287: coherent internal worker snapshots; no provider dispatch.
use super::*;
#[path = "next_prompt_snapshot_fixture.rs"]
pub(in crate::jobs::investigations) mod fixture;
use fixture::Fixture;

fn rewrite(
    tx: &Transaction<'_>,
    table: &str,
    task: &str,
    key: &str,
    value: &dyn rusqlite::ToSql,
    body: &[u8],
) {
    let trigger = format!("{table}_no_update");
    let sql: String = tx
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name=?1",
            [&trigger],
            |row| row.get(0),
        )
        .unwrap();
    tx.execute_batch(&format!("DROP TRIGGER {trigger}"))
        .unwrap();
    tx.execute(
        &format!("UPDATE {table} SET body=?1,body_hash=?2 WHERE task_id=?3 AND {key}=?4"),
        params![body, core_prov::content_hash(body), task, value],
    )
    .unwrap();
    tx.execute_batch(&sql).unwrap();
}

#[test]
fn next_prompt_snapshot_ready_reopens_exact_request_and_saved_inputs() {
    let f = Fixture::new(false);
    let saved = f.snapshot();
    assert_eq!(
        serde_json::to_value(saved.request()).unwrap(),
        serde_json::to_value(&f.f.request).unwrap()
    );
    assert_eq!(saved.detail(), &f.f.task.detail);
    assert_eq!(saved.ledger(), &f.f.ledger);
    assert_eq!(saved.metadata(), &f.f.metadata);
    assert_eq!(saved.mode(), CompletionCallMode::Choice);
    assert_eq!(saved.capacity_refusal_count(), 0);
    assert!(saved.capacity_feedback().is_none());
    assert!(saved.pending_step().is_none());
    assert!(saved.attempts().is_empty());
    assert!(saved.attempt_ledgers().is_empty());
    assert!(saved.basis().previous_ledger.is_none());
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_next_prompt_snapshot(f.id(), &f.f.request)
            .unwrap()
            .detail(),
        saved.detail()
    );
    assert_eq!(&reopened.investigation(f.id()).unwrap(), saved.detail());
    let mut changed = f.f.request.clone();
    changed.question.push_str(" changed");
    assert!(matches!(
        reopened.investigation_next_prompt_snapshot(f.id(), &changed),
        Err(InvestigationStoreError::Conflict)
    ));
}

#[test]
fn next_prompt_snapshot_cloud_consent_retains_exact_preusage_mode_and_capacity() {
    let mut f = Fixture::new(true);
    let ready = f.snapshot();
    f.f.store
        .await_next_prompt_consent(
            &f.f.execution,
            ready.detail().summary.revision,
            &NextPromptInvocation {
                input: &f.input,
                prepared: &f.prepared,
                active_milliseconds: ready.detail().usage.active_milliseconds,
            },
            |_| Ok(()),
        )
        .unwrap();
    f.refresh();
    let consent = f.snapshot();
    assert_eq!(consent.detail().usage, ready.detail().usage);
    assert_eq!(consent.mode(), ready.mode());
    assert_eq!(
        consent.capacity_refusal_count(),
        ready.capacity_refusal_count()
    );
    assert_eq!(consent.capacity_feedback(), ready.capacity_feedback());
    assert_eq!(consent.metadata(), ready.metadata());
    assert_eq!(consent.pending_step(), f.f.task.pending.as_ref());
    assert_eq!(
        consent
            .pending_step()
            .unwrap()
            .disclosure
            .as_ref()
            .unwrap()
            .admission
            .as_ref()
            .unwrap()
            .consumed_budgets,
        ready.detail().usage
    );
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_next_prompt_snapshot(f.id(), &f.f.request)
            .unwrap()
            .pending_step(),
        consent.pending_step()
    );
}

#[test]
fn next_prompt_snapshot_refusal_state_and_rehashed_corruption_are_authenticated() {
    for kind in ["journal", "metadata"] {
        let mut f = Fixture::new(false);
        f.refuse();
        let saved = f.snapshot();
        assert_eq!(saved.capacity_refusal_count(), 1);
        assert_eq!(
            saved.capacity_feedback().unwrap().reason,
            CapacityRefusalReason::PromptCapacity
        );
        assert_eq!(saved.detail().usage.model_invocations, 1);
        let tx = f.f.store.conn.transaction().unwrap();
        if kind == "journal" {
            let mut entry: admission_journal::Entry = read_body(
                &tx,
                "investigation_admission_refusals",
                "sequence",
                &saved.detail().summary.investigation_id,
                &1i64,
            )
            .unwrap()
            .unwrap();
            entry.count = 2;
            let body = encode(&entry, MAX_MANIFEST_BYTES).unwrap();
            rewrite(
                &tx,
                "investigation_admission_refusals",
                &saved.detail().summary.investigation_id,
                "sequence",
                &1i64,
                &body,
            );
        } else {
            let mut step: PendingInvestigationStep = read_body(
                &tx,
                "investigation_invocations",
                "step_id",
                &saved.detail().summary.investigation_id,
                &"step-1",
            )
            .unwrap()
            .unwrap();
            step.disclosure.as_mut().unwrap().completion_metadata_hash = Some("a".repeat(64));
            let body = encode(&step, MAX_INVOCATION_BYTES).unwrap();
            rewrite(
                &tx,
                "investigation_invocations",
                &saved.detail().summary.investigation_id,
                "step_id",
                &"step-1",
                &body,
            );
        }
        tx.commit().unwrap();
        let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
        assert!(
            reopened
                .investigation_next_prompt_snapshot(f.id(), &f.f.request)
                .is_err(),
            "{kind}"
        );
    }
}

#[test]
fn next_prompt_snapshot_attempts_keep_exact_saved_ledgers_and_reject_mixed_history() {
    let mut f = Fixture::new(false);
    f.acquire_inventory();
    let saved = f.snapshot();
    assert_eq!(saved.attempts().len(), 1);
    assert_eq!(saved.attempt_ledgers(), std::slice::from_ref(&f.f.ledger));
    assert_eq!(
        saved.metadata().completed_attempt_ids,
        vec![saved.attempts()[0].attempt_id.clone()]
    );
    assert_eq!(
        saved.basis().completed_tool,
        Some(InvestigationTool::ReadCoverageInventory)
    );
    assert_eq!(saved.basis().previous_ledger.as_ref().unwrap().revision, 0);
    let mut other = saved.basis().previous_ledger.clone().unwrap();
    other.supplied_history_hash = Some(core_prov::content_hash(b"x"));
    other.supplied_history_bytes = 1;
    other
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let body = encode(&other, MAX_MANIFEST_BYTES).unwrap();
    let tx = f.f.store.conn.unchecked_transaction().unwrap();
    rewrite(
        &tx,
        "investigation_inputs",
        f.id(),
        "revision",
        &0i64,
        &body,
    );
    tx.commit().unwrap();
    let reopened = JobStore::open(f.f.directory.path().join("state.db")).unwrap();
    assert!(
        reopened
            .investigation_next_prompt_snapshot(f.id(), &f.f.request)
            .is_err()
    );
}

#[test]
fn next_prompt_snapshot_rejects_active_invocation_instead_of_using_charged_usage() {
    let mut f = Fixture::new(false);
    f.begin();
    assert!(matches!(
        f.f.store
            .investigation_next_prompt_snapshot(f.id(), &f.f.request),
        Err(InvestigationStoreError::Stale)
    ));
}

// AC-0327: acquisition includes malformed typed disclosure owners before replay
// rejects their references; use an actual durable invocation as the starting row.
#[test]
fn saved_attempt_acquisition_accounts_for_prevalidation_disclosure() {
    let mut f = Fixture::new(false);
    f.acquire_inventory();
    let saved = f.snapshot();
    let mut step: PendingInvestigationStep = read_body(
        &f.f.store.conn,
        "investigation_invocations",
        "step_id",
        f.id(),
        &saved.attempts()[0].step_id,
    )
    .unwrap()
    .unwrap();
    step.disclosure.as_mut().unwrap().queries = vec![
        DisclosedInvestigationQuery {
            query_index: usize::MAX,
            representation: DisclosureRepresentation::Omitted {
                reason: DisclosureOmissionReason::NotSelected,
            },
        };
        RECORD_MAX_ARRAY_ITEMS
    ];
    let body = encode(&step, MAX_RECORD_BYTES).unwrap();
    let decoded: PendingInvestigationStep =
        decode_record(std::str::from_utf8(&body).unwrap()).unwrap();
    assert_eq!(decoded, step);
    assert!(
        decoded
            .disclosure
            .as_ref()
            .unwrap()
            .validate_with_policy(saved.ledger(), &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    let actual = std::mem::size_of_val(&decoded)
        + body.len()
        + RECORD_MAX_ARRAY_ITEMS * std::mem::size_of::<DisclosedInvestigationQuery>();
    let peak = next_prompt_attempt_acquisition_workspace().unwrap();
    assert!(peak >= actual);
    assert!(
        peak >= InvestigationInputLedger::decode_workspace_bytes()
            .unwrap()
            .1
    );
    assert_eq!(f.snapshot().attempts(), saved.attempts());
}
