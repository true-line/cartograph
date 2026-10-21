use super::*;

/// Publish one exact query response through the shared candidate validator and
/// row writer, then derive/seal the next call from its real before/after ledgers.
/// No fixture stamps or mutable rowid are used as call authority.
pub(super) fn publish_query_and_start_source_call(
    f: &mut completion::InternalFixture,
    mut next: InvestigationInputLedger,
    raw: String,
) {
    let before = f.ledger.clone();
    next.revision = before.revision + 1;
    let query = &next.queries.last().unwrap().query;
    let proposal =
        json!({"type":"query_context","context_id":f.request.expected_context_id,"query":query});
    let response = admission_journal::tests::response(f, &proposal);
    let tx = f.store.conn.transaction().unwrap();
    let expected = f.task.detail.summary.revision;
    let (_, mut usage) = transitions::validate_response(&tx, &f.task, &response).unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.step.step_id,
        &response,
        4096,
    )
    .unwrap();
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelCompleted,
        Some(&f.step.step_id),
        Some(InvestigationTool::QueryContext),
        "Test exact query response.",
    )
    .unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ActionAdmitted,
        Some(&f.step.step_id),
        Some(InvestigationTool::QueryContext),
        "Test exact accepted query.",
    )
    .unwrap();
    f.task.phase = Phase::Tool;
    f.task.pending = None;
    f.task.action_tool = Some(InvestigationTool::QueryContext);
    usage.tool_actions += 1;
    f.task.detail.usage = usage.clone();
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ToolStarted,
        None,
        Some(InvestigationTool::QueryContext),
        "Test query publication.",
    )
    .unwrap();
    usage.selected_facts = next.selected_facts.len();
    transitions::validate_input_candidate(&tx, &f.task, &next, &usage).unwrap();
    transitions::persist_input_rows(&tx, &f.task, &next).unwrap();
    f.task.ledger_hash = Some(
        next.fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    f.task.ledger_revision = Some(next.revision);
    f.task.detail.citations = next.citations.clone();
    f.task.detail.usage = usage;
    f.task.phase = Phase::Ready;
    f.task.action_tool = None;
    f.task.admission_state.as_mut().unwrap().current_call = None;
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ToolCompleted,
        None,
        Some(InvestigationTool::QueryContext),
        "Test admitted exact query ledger and receipt references.",
    )
    .unwrap();
    save_next_prompt_task(&tx, &f.task, expected, false).unwrap();
    f.metadata = completion::metadata_current_next_prompt(&tx, &f.task).unwrap();
    tx.commit().unwrap();
    f.ledger = next;
    admission_journal::tests::start_call(f, &[raw], Some(before));
}

pub(super) fn initial_ledger_for_query(
    next: &InvestigationInputLedger,
) -> InvestigationInputLedger {
    let mut initial = next.clone();
    initial.revision = 0;
    initial.queries.clear();
    initial.selected_facts.clear();
    initial.receipt_references.clear();
    initial.citations.clear();
    initial
}
