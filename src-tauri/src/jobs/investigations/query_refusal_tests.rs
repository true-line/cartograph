use super::*;
use agents::investigation::query_refusal::{QueryRefusalRecord, QueryRejection};

#[test]
fn query_refusal_journal_reopens_without_new_evidence_or_refunding_attempt() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("query-refusal", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let initial = load_task(&tx, &id).unwrap();
        assert_eq!(load_ledger(&tx, &initial).unwrap(), Some(ledger.clone()));
        tx.commit().unwrap();
    }
    let pending = step(&ledger, 1);
    let started = begin(&mut fixture, &id, &execution, pending.clone());
    let mut record = QueryRefusalRecord {
        budget: None,
        schema_version: 1,
        record_id: String::new(),
        task_id: id.clone(),
        step_id: pending.step_id.clone(),
        action_hash: String::new(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_revision: ledger.revision,
        context_id: None,
        query: InvestigationQuery {
            scope: InvestigationScope::Neighborhood {
                anchor: "absent".into(),
                hops: 1,
            },
            kind: None,
            labels: vec![],
            max_facts: 1,
            max_bytes: 16384,
            cursor: None,
        },
        reason: QueryRejection::UnknownAnchor,
    };
    record.action_hash = record.query_action_hash().unwrap();
    record.record_id = record.content_id().unwrap();
    let mut response = response(&pending.step_id, 1);
    response.action_hash = record.action_hash.clone();
    let admitted = fixture
        .store
        .advance_investigation(
            &execution,
            started.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response,
                tool: Some(InvestigationTool::QueryContext),
                result: None,
            },
        )
        .unwrap();
    let charged = fixture
        .store
        .advance_investigation(
            &execution,
            admitted.summary.revision,
            InvestigationTransition::BeginTool {
                tool: InvestigationTool::QueryContext,
            },
        )
        .unwrap();
    let mut wrong = record.clone();
    wrong.query.max_facts += 1;
    wrong.action_hash = wrong.query_action_hash().unwrap();
    wrong.record_id = wrong.content_id().unwrap();
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                charged.summary.revision,
                InvestigationTransition::QueryRefused {
                    record: Box::new(wrong)
                }
            )
            .is_err()
    );
    assert_eq!(fixture.store.investigation(&id).unwrap(), charged);
    let refused = fixture
        .store
        .advance_investigation(
            &execution,
            charged.summary.revision,
            InvestigationTransition::QueryRefused {
                record: Box::new(record.clone()),
            },
        )
        .unwrap();
    assert_eq!(refused.usage, charged.usage);
    assert_eq!(refused.usage.tool_actions, 1);
    assert_eq!(refused.usage.model_invocations, 1);
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                refused.summary.revision,
                InvestigationTransition::QueryRefused {
                    record: Box::new(record.clone())
                }
            )
            .is_err()
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), refused);
    let page = reopened.investigation_events(&id, 0).unwrap();
    let event = page.items.last().unwrap();
    assert_eq!(event.kind, InvestigationEventKind::QueryRefused);
    assert_eq!(event.query_refusal.as_deref(), Some(&record));
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let saved = load_task(&tx, &id).unwrap();
    assert_eq!(load_ledger(&tx, &saved).unwrap(), Some(ledger.clone()));
    // A self-consistent event hash is insufficient: replay must bind to the
    // original admitted response, input ledger, and charged tool sequence.
    for mutation in 0..6 {
        let mut forged = event.clone();
        let record = forged.query_refusal.as_mut().unwrap();
        match mutation {
            0 => {
                record.query.max_facts += 1;
                record.action_hash = record.query_action_hash().unwrap();
            }
            1 => record.input_ledger_hash = "a".repeat(64),
            2 => record.input_revision += 1,
            3 => {
                record.step_id = "substituted-step".into();
                forged.step_id = Some(record.step_id.clone());
            }
            4 => forged.sequence += 1,
            5 => forged.revision += 1,
            _ => unreachable!(),
        }
        record.record_id = record.content_id().unwrap();
        forged.validate_query_refusal_envelope().unwrap();
        assert!(
            query_refusal::validate_event(&tx, &saved, &forged).is_err(),
            "accepted self-consistent forged refusal case {mutation}"
        );
    }
    query_refusal::validate_event(&tx, &saved, event).unwrap();
    tx.commit().unwrap();
    let count: i64 = reopened
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    // A genuine second invocation must not erase the feedback needed to
    // reconstruct the pending input after reopen.
    let pending = step(&ledger, 2);
    begin(&mut fixture, &id, &execution, pending);
    let resumed = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let tx = resumed.conn.unchecked_transaction().unwrap();
    let task = load_task(&tx, &id).unwrap();
    assert_eq!(query_refusal::latest(&tx, &task).unwrap(), Some(record));
    let mut cancelled = task.clone();
    cancelled.detail.summary.cancel_requested = true;
    let record = query_refusal::latest(&tx, &task).unwrap().unwrap();
    assert_eq!(
        query_refusal::publish(&tx, &mut cancelled, record),
        Err(InvestigationStoreError::Cancelled)
    );
    tx.commit().unwrap();
}

// AC-0585: reaching a cap is not evidence that any page was refused. Test the
// terminal projection with no query-refusal event; no numeric witness is invented.
#[test]
fn auditor12_caps_without_authenticated_budget_refusal_have_no_budget_diagnostic() {
    for model_cap in [false, true] {
        let mut fixture = Fixture::new();
        let (id, execution) =
            fixture.start("no-budget-refusal-cap", InvestigationProviderMode::Local);
        fixture.prepare(&id, &execution, false);
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let mut task = load_task(&tx, &id).unwrap();
        // Unit-test the stop projection, not admission; do not publish this
        // synthetic cap state or alter the authenticated historical request.
        task.detail.summary.specialist_id = SpecialistId::EvidenceAuditorV12;
        let reason = if model_cap {
            task.detail.usage.model_invocations = task.detail.limits.model_invocations;
            InvestigationStopReason::LimitExceeded
        } else {
            task.detail.usage.tool_actions = task.detail.limits.tool_actions;
            InvestigationStopReason::StoreCapacity
        };
        assert!(super::query_refusal::latest(&tx, &task).unwrap().is_none());
        super::transitions::stop(&tx, &mut task, reason).unwrap();
        assert!(task.detail.terminal_diagnostic.is_none());
        tx.rollback().unwrap();
    }
}

// AC-0585: the host's irreducible maximum-page failure is itself terminal;
// it requires neither a persisted recoverable refusal nor reaching any cap.
#[test]
fn auditor12_irreducible_page_budget_stops_without_refusal_record() {
    use agents::investigation::query_refusal::{
        QueryBudgetDiagnostic, QueryBudgetStage, RequiredBytesKind,
    };
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("irreducible-budget", InvestigationProviderMode::Local);
    fixture.prepare(&id, &execution, false);
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let mut task = load_task(&tx, &id).unwrap();
    task.detail.summary.specialist_id = SpecialistId::EvidenceAuditorV12;
    let budget = QueryBudgetDiagnostic {
        stage: QueryBudgetStage::CorePage,
        requested_bytes: MAX_QUERY_BYTES,
        effective_bytes: MAX_QUERY_BYTES,
        required_bytes: MAX_QUERY_BYTES + 1,
        required_bytes_kind: RequiredBytesKind::AtLeast,
    };
    let before = task.detail.usage.clone();
    super::transitions::stop(
        &tx,
        &mut task,
        InvestigationStopReason::HostLimitAt {
            phase: PreparationPhase::Query,
            limit: HostLimitKind::QueryPageBudget(budget),
        },
    )
    .unwrap();
    assert_eq!(task.detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(task.detail.usage, before);
    assert!(super::query_refusal::latest(&tx, &task).unwrap().is_none());
    assert_eq!(
        task.detail.terminal_diagnostic,
        Some(InvestigationTerminalDiagnostic::QueryBudget {
            termination: QueryBudgetTermination::PageBudget,
            budget,
        })
    );
    tx.rollback().unwrap();
}
