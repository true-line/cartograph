use super::super::{admission_journal::tests as fixture, completion::internal_next_prompt_fixture};
use super::*;

#[test]
fn capacity_owner_transition_commits_once_and_rejects_stale_revision() {
    // AC-0287: only the retained execution owner can publish refusal effects.
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let revision = f.task.detail.summary.revision;
    let next = f
        .store
        .refuse_next_prompt_capacity(
            &f._execution_guard,
            revision,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
    assert!(next.summary.revision > revision);
    assert!(matches!(
        f.store.refuse_next_prompt_capacity(
            &f._execution_guard,
            revision,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        ),
        Err(InvestigationStoreError::Stale)
    ));
    let tx = f.store.conn.unchecked_transaction().unwrap();
    let saved = storage::load_next_prompt_task_base(&tx, &f.basis.task_id).unwrap();
    assert_eq!(saved.phase, Phase::Ready);
    assert_eq!(
        admission_journal::derive_current(&tx, &saved)
            .unwrap()
            .count,
        1
    );
}

#[test]
fn capacity_owner_transition_rejects_foreign_owner_and_cancelled_job() {
    for foreign in [true, false] {
        let mut f = internal_next_prompt_fixture();
        let other = internal_next_prompt_fixture();
        let proposal = fixture::proposal(&f, 1);
        let response = fixture::response(&f, &proposal);
        if !foreign {
            // Exercise the durable job fence even when a stale task snapshot
            // has not yet observed cancellation.
            f.store
                .conn
                .execute(
                    "UPDATE jobs SET status='cancelled' WHERE id=?1",
                    [f.task.detail.summary.job_id],
                )
                .unwrap();
        }
        let owner = if foreign {
            &other._execution_guard
        } else {
            &f._execution_guard
        };
        assert!(matches!(
            f.store.refuse_next_prompt_capacity(
                owner,
                f.task.detail.summary.revision,
                &response,
                &proposal,
                CapacityRefusalReason::PromptCapacity,
            ),
            Err(InvestigationStoreError::Ownership)
        ));
        let responses: i64 = f
            .store
            .conn
            .query_row("SELECT count(*) FROM investigation_responses", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(responses, 0);
        assert_eq!(fixture::state(&f).count, 0);
    }
}

#[test]
fn capacity_owner_transition_honors_saved_task_cancellation() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    f.task.detail.summary.cancel_requested = true;
    f.task.detail.summary.actions.can_cancel = false;
    {
        let tx = f.store.conn.transaction().unwrap();
        fixture::save_task(&tx, &f.task);
        tx.commit().unwrap();
    }
    assert!(matches!(
        f.store.refuse_next_prompt_capacity(
            &f._execution_guard,
            f.task.detail.summary.revision,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        ),
        Err(InvestigationStoreError::Cancelled)
    ));
    assert_eq!(fixture::state(&f).count, 0);
}
