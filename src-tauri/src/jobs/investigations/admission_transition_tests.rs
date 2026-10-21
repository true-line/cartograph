use super::super::{admission_journal::tests as fixture, completion::internal_next_prompt_fixture};
use super::*;

#[test]
fn capacity_transition_persists_response_feedback_and_ready_without_acquisition() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let before_usage = f.task.detail.usage.clone();
    let before_ledger = encode(&f.ledger, MAX_MANIFEST_BYTES).unwrap();
    let tx = f.store.conn.transaction().unwrap();
    let next = refuse_capacity(
        &tx,
        &f.task,
        &response,
        &proposal,
        CapacityRefusalReason::PromptCapacity,
    )
    .unwrap();
    assert_eq!(next.phase, Phase::Ready);
    assert!(next.pending.is_none());
    assert!(
        next.admission_state
            .as_ref()
            .unwrap()
            .current_call
            .is_none()
    );
    assert_eq!(
        next.detail.usage.model_invocations,
        before_usage.model_invocations
    );
    assert_eq!(
        next.detail.usage.generated_token_reservations,
        before_usage.generated_token_reservations
    );
    assert_eq!(next.detail.usage.tool_actions, before_usage.tool_actions);
    assert_eq!(
        next.detail.usage.evidence_requests,
        before_usage.evidence_requests
    );
    assert_eq!(
        next.detail.usage.evidence_items,
        before_usage.evidence_items
    );
    let state = admission_journal::derive_current(&tx, &next).unwrap();
    assert_eq!(state.count, 1);
    assert_eq!(state.feedback.unwrap().action_hash, response.action_hash);
    let ledger: InvestigationInputLedger = read_body(
        &tx,
        "investigation_inputs",
        "revision",
        &f.basis.task_id,
        &0i64,
    )
    .unwrap()
    .unwrap();
    assert_eq!(encode(&ledger, MAX_MANIFEST_BYTES).unwrap(), before_ledger);
    tx.commit().unwrap();
    f.task = next;
    assert_eq!(fixture::state(&f).count, 1);
}

#[test]
fn capacity_transition_failed_proposal_rolls_back_response_and_keeps_caller_state() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let before = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    {
        let tx = f.store.conn.transaction().unwrap();
        let mut changed = proposal.clone();
        changed["context_id"] = "context-v2:wrong".into();
        assert!(
            refuse_capacity(
                &tx,
                &f.task,
                &response,
                &changed,
                CapacityRefusalReason::PromptCapacity
            )
            .is_err()
        );
        // The caller drops the failed transaction; it never returns a successful
        // receipt or installs partially changed in-memory task state.
    }
    assert_eq!(encode(&f.task, MAX_RECORD_BYTES).unwrap(), before);
    assert_eq!(fixture::state(&f).count, 0);
    let count: i64 = f
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_responses WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn capacity_transition_uncommitted_success_has_no_reopen_effect() {
    let mut f = internal_next_prompt_fixture();
    let proposal = fixture::proposal(&f, 1);
    let response = fixture::response(&f, &proposal);
    let before = encode(&f.task, MAX_RECORD_BYTES).unwrap();
    {
        let tx = f.store.conn.transaction().unwrap();
        let _unacknowledged = refuse_capacity(
            &tx,
            &f.task,
            &response,
            &proposal,
            CapacityRefusalReason::PromptCapacity,
        )
        .unwrap();
        // Simulates failure before COMMIT, not power-loss durability or lost ACK.
    }
    assert_eq!(fixture::state(&f).count, 0);
    let saved: Vec<u8> = f
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, before);
}
