//! AC-0287: replay reuse preserves every historical cutoff and corruption check.
use super::*;
use completion::FinishFixture;

#[cfg(unix)]
#[test]
fn saved_metadata_history_workspace_includes_completion_refusal_feedback() {
    let mut f = FinishFixture::new(false);
    f.begin();
    assert_eq!(f.publish().unwrap().completion.unwrap().refusal_count, 1);
    f.next();
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let history = SavedMetadataHistory::load(&tx, &f.base.f.task).unwrap();
    assert_eq!(history.refusals.len(), 1);
    let step = &history.steps.last().unwrap().1;
    let metadata = history.metadata_at(&step.step_id).unwrap();
    assert_eq!(metadata.refusal_count, 1);
    assert_eq!(
        metadata,
        metadata_at_saved_step(&tx, &f.base.f.task, step).unwrap()
    );
    let (retained, project, peak) = next_prompt_metadata_workspace().unwrap();
    let refusal_bytes = serde_json::to_vec(&history.refusals).unwrap().len();
    assert!(retained >= refusal_bytes);
    assert!(project >= serde_json::to_vec(&metadata).unwrap().len());
    assert!(peak >= retained.checked_add(project).unwrap());
}

#[test]
fn saved_metadata_history_matches_standalone_cutoffs_with_one_sweep_per_timeline() {
    let mut f = FinishFixture::new(true);
    f.capacity_refusal();
    f.capacity_refusal();
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let task = &f.base.f.task;
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    assert_eq!(history.steps.len(), 4);
    // AC-0327: genuine history keeps all of these owners live during projection.
    #[cfg(unix)]
    let projection_bound = {
        let (retained, project, peak) = next_prompt_metadata_workspace().unwrap();
        let bytes = serde_json::to_vec(&history.steps).unwrap().len()
            + serde_json::to_vec(&history.ordinals).unwrap().len()
            + serde_json::to_vec(&history.attempts).unwrap().len()
            + serde_json::to_vec(&history.refusals).unwrap().len()
            + serde_json::to_vec(&history.archive).unwrap().len();
        assert!(retained >= bytes);
        assert!(peak >= retained.checked_add(project).unwrap());
        project
    };
    for (_, step) in &history.steps {
        #[cfg(unix)]
        assert!(
            projection_bound
                >= serde_json::to_vec(&history.metadata_at(&step.step_id).unwrap())
                    .unwrap()
                    .len()
        );
        assert_eq!(
            history.metadata_at(&step.step_id).unwrap(),
            metadata_at_saved_step(&tx, task, step).unwrap()
        );
    }
    assert!(matches!(
        history.metadata_at("missing"),
        Err(InvestigationStoreError::Missing)
    ));
    INVOCATION_SWEEPS.with(|count| count.set(0));
    let state = admission_journal::derive_current(&tx, task).unwrap();
    assert_eq!(state.count, 2);
    assert!(state.feedback.is_some());
    INVOCATION_SWEEPS.with(|count| assert_eq!(count.get(), 1));
    let call = task
        .admission_state
        .as_ref()
        .unwrap()
        .current_call
        .as_ref()
        .unwrap();
    let actual = admission_contract::load_saved_call_basis_core(&tx, task, call).unwrap();
    let reused = history
        .validate_timeline_call(call, &actual.ledger)
        .unwrap();
    assert_eq!(reused.step, actual.step);
    assert_eq!(reused.ledger, actual.ledger);
    assert_eq!(reused.metadata, actual.metadata);
    assert_eq!(reused.ordinal, actual.ordinal);
}

#[test]
fn saved_metadata_history_write_rollback_and_ddl_invalidate_same_transaction_view() {
    let mut f = FinishFixture::new(false);
    f.begin();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let task = &f.base.f.task;
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    tx.execute_batch("SAVEPOINT mutation").unwrap();
    tx.execute(
        "UPDATE jobs SET status=status WHERE id=?1",
        [task.detail.summary.job_id],
    )
    .unwrap();
    tx.execute_batch("ROLLBACK TO mutation; RELEASE mutation")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    tx.execute_batch("CREATE TEMP TABLE stale_history_marker (id INTEGER)")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
    let history = SavedMetadataHistory::load(&tx, task).unwrap();
    // Main DDL must also invalidate without an affected row/body count.
    tx.execute_batch("CREATE TABLE stale_main_history_marker (id INTEGER)")
        .unwrap();
    assert!(matches!(
        history.metadata_at("step-1"),
        Err(InvestigationStoreError::Stale)
    ));
}

#[test]
fn saved_metadata_history_fresh_replay_rejects_rehashed_earlier_call_at_same_row_count() {
    let mut f = FinishFixture::new(false);
    f.capacity_refusal();
    f.begin();
    let id = f.base.id().to_owned();
    {
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        let history = SavedMetadataHistory::load(&tx, &f.base.f.task).unwrap();
        let mut first = history.step("step-1").unwrap().clone();
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM investigation_invocations WHERE task_id=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        first
            .disclosure
            .as_mut()
            .unwrap()
            .admission
            .as_mut()
            .unwrap()
            .capacity_refusal_count = 1;
        let bytes = encode(&first, MAX_INVOCATION_BYTES).unwrap();
        let trigger: String = tx
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='investigation_invocations_no_update'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        tx.execute_batch("DROP TRIGGER investigation_invocations_no_update")
            .unwrap();
        tx.execute("UPDATE investigation_invocations SET body=?2,body_hash=?3 WHERE task_id=?1 AND step_id='step-1'",params![id,bytes,core_prov::content_hash(&bytes)]).unwrap();
        tx.execute_batch(&trigger).unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT COUNT(*) FROM investigation_invocations WHERE task_id=?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            count
        );
        assert!(matches!(
            history.metadata_at("step-1"),
            Err(InvestigationStoreError::Stale)
        ));
        drop(history);
        tx.commit().unwrap();
    }
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    assert!(admission_journal::derive_current(&tx, &f.base.f.task).is_err());
    assert!(load_next_prompt_task(&tx, &id).is_err());
}

#[test]
fn saved_metadata_history_reused_append_core_rejects_substituted_call_basis() {
    let mut f = completion::internal_next_prompt_fixture();
    let proposal = admission_journal::tests::proposal(&f, 12);
    let response = admission_journal::tests::response(&f, &proposal);
    let tx = f.store.conn.transaction().unwrap();
    admission_journal::initialize(&tx).unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.basis.step_id,
        &response,
        4096,
    )
    .unwrap();
    let mut changed = f.basis.clone();
    changed.completion_metadata_hash = "f".repeat(64);
    f.task.admission_state.as_mut().unwrap().current_call = Some(changed.clone());
    assert_eq!(
        admission_journal::append(
            &tx,
            &f.task,
            &changed,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity
        ),
        Err(InvestigationStoreError::Invalid)
    );
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM investigation_admission_refusals",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
