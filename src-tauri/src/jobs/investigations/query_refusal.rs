//! A charged query attempt can finish without publishing a successful page.
use super::*;
use agents::investigation::query_refusal::QueryRefusalRecord;

pub(super) fn validate_event(
    connection: &Connection,
    task: &StoredTask,
    event: &InvestigationEvent,
) -> Result<(), InvestigationStoreError> {
    event.validate_query_refusal_envelope()?;
    let Some(record) = event.query_refusal.as_ref() else {
        return Ok(());
    };
    let id = &task.detail.summary.investigation_id;
    let policy = completion::saved_policy(task)?;
    let ledger: InvestigationInputLedger = read_body(
        connection,
        "investigation_inputs",
        "revision",
        id,
        &signed(record.input_revision)?,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    let step: PendingInvestigationStep = read_body(
        connection,
        "investigation_invocations",
        "step_id",
        id,
        &record.step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    let response: InvestigationResponseMeta = read_body(
        connection,
        "investigation_responses",
        "step_id",
        id,
        &record.step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    record.validate_binding(
        id,
        &step.step_id,
        &response.action_hash,
        &ledger,
        task.detail.summary.specialist_id,
        &policy,
    )?;
    if step.input_ledger_hash != record.input_ledger_hash || response.step_id != step.step_id {
        return Err(InvestigationStoreError::Invalid);
    }
    // BeginTool has no step field; the preceding ActionAdmitted event binds it.
    // This journal protocol requires adjacent ActionAdmitted -> ToolStarted ->
    // QueryRefused events, with exactly one task revision after ToolStarted.
    // Inserting progress/heartbeat events here requires a protocol migration;
    // replay tests reject rehashed sequence or revision changes.
    let sequence = event
        .sequence
        .checked_sub(1)
        .ok_or(InvestigationStoreError::Invalid)?;
    let started: InvestigationEvent = read_body(
        connection,
        "investigation_events",
        "sequence",
        id,
        &signed(sequence)?,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    let admitted: InvestigationEvent = read_body(
        connection,
        "investigation_events",
        "sequence",
        id,
        &signed(
            sequence
                .checked_sub(1)
                .ok_or(InvestigationStoreError::Invalid)?,
        )?,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    if started.investigation_id != *id
        || started.sequence != sequence
        || started.kind != InvestigationEventKind::ToolStarted
        || started.tool != Some(InvestigationTool::QueryContext)
        || started.query_refusal.is_some()
        || admitted.investigation_id != *id
        || admitted.sequence.checked_add(1) != Some(sequence)
        || admitted.kind != InvestigationEventKind::ActionAdmitted
        || admitted.step_id.as_deref() != Some(record.step_id.as_str())
        || admitted.tool != Some(InvestigationTool::QueryContext)
        || admitted.query_refusal.is_some()
        || started.revision.checked_add(1) != Some(event.revision)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

pub(super) fn publish(
    connection: &Connection,
    task: &mut StoredTask,
    record: QueryRefusalRecord,
) -> Result<(), InvestigationStoreError> {
    if task.detail.summary.cancel_requested {
        return Err(InvestigationStoreError::Cancelled);
    }
    if task.phase != Phase::Tool
        || task.action_tool != Some(InvestigationTool::QueryContext)
        || task.ledger_hash.as_deref() != Some(record.input_ledger_hash.as_str())
        || task.ledger_revision != Some(record.input_revision)
    {
        return Err(InvestigationStoreError::Stale);
    }
    transitions::available_time(task)?;
    let step = record.step_id.clone();
    task.phase = Phase::Ready;
    task.action_tool = None;
    bump(connection, task)?;
    let mut event = prepare_event(
        task,
        InvestigationEventKind::QueryRefused,
        Some(&step),
        Some(InvestigationTool::QueryContext),
        "The query was refused; no evidence was acquired.",
        timestamp(connection)?,
    )?;
    event.query_refusal = Some(Box::new(record));
    validate_event(connection, task, &event)?;
    // Existing event/store caps apply. The enclosing transition rolls back any
    // failure and retains the already charged tool attempt without refund.
    persist_event(connection, &event)
}

/// The most recent refusal survives invocation/consent events until a successful
/// acquisition supersedes it. Historical bindings are validated before use.
pub(super) fn latest(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<QueryRefusalRecord>, InvestigationStoreError> {
    // Consent/model-start/recovery events do not erase feedback. Only a
    // successful acquisition supersedes the refused query's input basis.
    for sequence in (1..=task.detail.summary.last_event_sequence).rev() {
        let event: InvestigationEvent = read_body(
            connection,
            "investigation_events",
            "sequence",
            &task.detail.summary.investigation_id,
            &signed(sequence)?,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        validate_event(connection, task, &event)?;
        if event.kind == InvestigationEventKind::ToolCompleted {
            return Ok(None);
        }
        if let Some(record) = event.query_refusal {
            return Ok(Some(*record));
        }
    }
    Ok(None)
}

/// Exact active invocation witnesses; the worker classifies the frozen query
/// error before entering this owner-authenticated transaction.
pub struct QueryRefusalAdmission<'a> {
    pub response: &'a InvestigationResponseMeta,
    pub original_action: &'a serde_json::Value,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a llm::bounded::PreparedBoundedCompletion,
    pub record: QueryRefusalRecord,
    pub active_milliseconds: u64,
}
impl JobStore {
    pub fn refuse_next_query(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        input: QueryRefusalAdmission<'_>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != expected_revision || task.phase != Phase::Invocation {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if !task.detail.summary.specialist_id.uses_acquisition_choices()
            || input
                .original_action
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("query_context")
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let authenticated = metadata_admission::authenticate_response(
            &tx,
            &task,
            input.response,
            input.original_action,
            input.current,
            input.current_prepared,
            input.active_milliseconds,
        )?;
        input.record.validate_binding(
            &id,
            &authenticated.call.step_id,
            &input.response.action_hash,
            &authenticated.core.ledger,
            task.detail.summary.specialist_id,
            &super::policy_for_detail(&task.detail)?,
        )?;
        if !input
            .current
            .permits_acquisition(&QualifiedInvestigationAction::QueryContext {
                context_id: input
                    .record
                    .context_id
                    .clone()
                    .ok_or(InvestigationStoreError::Invalid)?,
                query: input.record.query.clone(),
            })
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut usage = authenticated.admitted_usage;
        usage.active_milliseconds = input.active_milliseconds;
        usage.tool_actions = usage
            .tool_actions
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        validate_usage(&usage, &task.detail.limits)?;
        task.detail.usage = usage;
        insert_body(
            &tx,
            "investigation_responses",
            &id,
            &input.response.step_id,
            input.response,
            4096,
        )?;
        bump(&tx, &mut task)?;
        for (kind, summary) in [
            (
                InvestigationEventKind::ModelCompleted,
                "A complete provider response was recorded.",
            ),
            (
                InvestigationEventKind::ActionAdmitted,
                "The query selection was admitted.",
            ),
            (
                InvestigationEventKind::ToolStarted,
                "The query attempt was charged.",
            ),
        ] {
            event(
                &tx,
                &mut task,
                kind,
                Some(&input.response.step_id),
                Some(InvestigationTool::QueryContext),
                summary,
            )?;
        }
        task.phase = Phase::Tool;
        task.action_tool = Some(InvestigationTool::QueryContext);
        publish(&tx, &mut task, input.record)?;
        task.pending = None;
        task.approved = false;
        task.admission_state
            .as_mut()
            .ok_or(InvestigationStoreError::Invalid)?
            .current_call = None;
        completion::refresh_observation(&tx, &mut task)?;
        admission_journal::derive_current(&tx, &task)?;
        save_next_prompt_task(&tx, &task, expected_revision, false)?;
        tx.commit()?;
        Ok(task.detail)
    }
}
