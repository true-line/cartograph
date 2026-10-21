//! AC-0287: the positive FinishOnly boundary requires real fixed-policy calls.
//! No synthetic debit/ordinal rows, providers or evidence acquisitions are used.
use super::*;
use std::time::Instant;

#[test]
fn next_prompt_finish_only_genuine_call_budget_stops_tool_without_acquisition() {
    let started = Instant::now();
    let mut f = Fixture::new(false);
    let policy = &INVESTIGATION_V2_POLICY;
    let mut refusals = 0;
    while f.base.snapshot().mode() == CompletionCallMode::Choice {
        assert!(refusals < policy.limits().model_invocations);
        let transition = Instant::now();
        f.capacity_refusal();
        refusals += 1;
        if refusals % 10 == 0 {
            eprintln!(
                "genuine finish-only: {refusals} refused calls, last transition {:?}, total {:?}",
                transition.elapsed(),
                started.elapsed()
            );
        }
    }
    let snapshot = f.base.snapshot();
    assert_eq!(snapshot.mode(), CompletionCallMode::FinishOnly);
    assert_eq!(snapshot.capacity_refusal_count(), refusals);
    let ledger = snapshot.ledger().clone();
    let feedback = snapshot.capacity_feedback().cloned();
    f.original = f.query_action();
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f.begin();
    let charged = f.base.f.task.detail.usage.clone();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some(InvestigationStopReason::FinishOnlyToolProposal.message())
    );
    assert_eq!(detail.usage.model_invocations, charged.model_invocations);
    assert_eq!(
        detail.usage.generated_token_reservations,
        charged.generated_token_reservations
    );
    assert_eq!(
        detail.usage.reported_input_tokens,
        Some(u64::from(charged.model_invocations) * 17)
    );
    assert_eq!(
        detail.usage.reported_output_tokens,
        Some(u64::from(charged.model_invocations) * 3)
    );
    assert_eq!(detail.usage.tool_actions, 0);
    assert_eq!(detail.usage.evidence_requests, 0);
    assert_eq!(detail.usage.evidence_bytes, 0);
    assert!(detail.citations.is_empty());
    assert!(!detail.summary.has_result);
    let saved = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    assert_eq!(load_ledger(&tx, &saved).unwrap(), Some(ledger));
    assert!(attempts(&tx, &saved).unwrap().is_empty());
    let capacity = admission_journal::derive_current(&tx, &saved).unwrap();
    assert_eq!(capacity.count, refusals);
    assert_eq!(capacity.feedback, feedback);
    eprintln!(
        "genuine FinishOnly writer completed after {refusals} real refusals in {:?}",
        started.elapsed()
    );
}
