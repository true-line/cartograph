// ADR-0072 slice 3 (AC-1070): append-only reconciliation results.
use super::*;
use crate::reconciliation::tests::{derive_v1, range, result_v1, v1_task};
use crate::reconciliation::{RecapturedCandidate, RecapturedItem, ReconciliationOutcome};
use crate::{AgentBroker, ContextStatus, ProposalDecision, RawProposal};

fn stage_v1(store: &mut ProposalStore) -> crate::StagedProposal {
    let task = v1_task();
    let proposal = AgentBroker::bounded_default()
        .validate_response(
            &task,
            RawProposal {
                target_id: "symbol:validate".into(),
                annotation: "Possible call target".into(),
                citations: vec!["source".into(), "target".into()],
            },
        )
        .unwrap();
    store
        .stage(
            &task,
            &proposal,
            1,
            &format!("context-v1:{}", "1".repeat(64)),
        )
        .unwrap()
}

fn missing_result() -> ReconciliationResultV1 {
    derive_v1(
        vec![
            range("cart.ts", "", b"cart source text"),
            RecapturedItem::FactAbsent,
        ],
        vec![RecapturedCandidate::Absent],
    )
}

fn staged_row(store: &ProposalStore, proposal_id: &str) -> (String, String) {
    store
        .conn
        .query_row(
            "SELECT immutable_json, created_at FROM staged_agent_proposals WHERE proposal_id = ?1",
            [proposal_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn results_append_survive_restart_and_never_change_the_staged_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proposals.sqlite");
    let mut store = ProposalStore::open(&path).unwrap();
    let staged = stage_v1(&mut store);
    let before = staged_row(&store, &staged.proposal_id);
    assert_eq!(
        store.latest_reconciliation(&staged.proposal_id).unwrap(),
        None
    );

    let reconciled = result_v1();
    assert_eq!(reconciled.outcome, ReconciliationOutcome::Reconciled);
    let first = store.record_reconciliation(&reconciled).unwrap();
    assert_eq!(first.result, reconciled);
    // The same result again is the first recording, not a second row.
    assert_eq!(store.record_reconciliation(&reconciled).unwrap(), first);
    let missing = missing_result();
    assert_eq!(missing.outcome, ReconciliationOutcome::Missing);
    store.record_reconciliation(&missing).unwrap();

    let history = store.reconciliations(&staged.proposal_id, 64).unwrap();
    let outcomes: Vec<_> = history.iter().map(|r| r.result.outcome).collect();
    assert_eq!(
        outcomes,
        [
            ReconciliationOutcome::Missing,
            ReconciliationOutcome::Reconciled
        ]
    );
    // Review leaves every result and the staged meaning as they were.
    let reviewed = store
        .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert_eq!(
        reviewed.context_status,
        ContextStatus::AwaitingReconciliation
    );
    assert_eq!(reviewed.evidence_binding, staged.evidence_binding);
    drop(store);

    let store = ProposalStore::open(&path).unwrap();
    assert_eq!(
        store.reconciliations(&staged.proposal_id, 64).unwrap(),
        history
    );
    assert_eq!(
        store
            .latest_reconciliation(&staged.proposal_id)
            .unwrap()
            .unwrap()
            .result,
        missing
    );
    assert_eq!(staged_row(&store, &staged.proposal_id), before);
    // Metadata only: no supplied source or summary text is stored.
    let raw: Vec<String> = store
        .conn
        .prepare("SELECT result_json FROM staged_reconciliation_results")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(raw.len(), 2);
    for json in raw {
        for text in [
            "cart source text",
            "validate target text",
            "Symbol: validate",
        ] {
            assert!(!json.contains(text));
        }
    }
}

#[test]
fn rows_are_append_only_and_bound_to_their_record() {
    let mut store = ProposalStore::open(":memory:").unwrap();
    // A result for a record this store never staged is refused.
    assert!(matches!(
        store.record_reconciliation(&result_v1()),
        Err(StagingError::UnknownProposal(_))
    ));
    let staged = stage_v1(&mut store);
    store.record_reconciliation(&result_v1()).unwrap();
    for sql in [
        "UPDATE staged_reconciliation_results SET recorded_at = 'x'",
        "DELETE FROM staged_reconciliation_results",
    ] {
        assert!(store.conn.execute(sql, []).is_err(), "{sql}");
    }
    // A result whose stored bytes no longer decode for the record is an
    // error for the whole read, never skipped.
    store
        .conn
        .execute_batch("DROP TRIGGER staged_reconciliation_results_immutable")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE staged_reconciliation_results SET result_json = replace(result_json, '\"job_id\":7', '\"job_id\":8')",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.reconciliations(&staged.proposal_id, 1),
        Err(StagingError::InvalidRecord)
    ));
    for limit in [0, MAX_RECONCILIATION_HISTORY + 1] {
        assert!(matches!(
            store.reconciliations(&staged.proposal_id, limit),
            Err(StagingError::InvalidLimit)
        ));
    }
}
