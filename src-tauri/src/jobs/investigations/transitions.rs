use super::*;

const SOURCE_REPLAY_FEEDBACK: &str = "A source-replay finish was discarded.";

// Stream the existing bounded journal; summaries never authorize another call.
pub(super) fn source_replay_state(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<(String, bool)>, InvestigationStoreError> {
    if task.completion_intent.is_some()
        || task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
    {
        return Ok(None);
    }
    let id = &task.detail.summary.investigation_id;
    let limit = journal_event_limit(task)?;
    let mut statement = connection.prepare(
        "SELECT sequence, CASE WHEN typeof(body)='blob' AND length(body)<=16384 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_events WHERE task_id=?1 ORDER BY sequence LIMIT ?2",
    )?;
    let rows = statement.query_map(params![id, signed(limit + 1)?], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut count = 0_u64;
    let mut starts = 0_u32;
    let mut refusal: Option<String> = None;
    let mut correction: Option<PendingInvestigationStep> = None;
    let mut last_started: Option<String> = None;
    let mut revision = 0;
    for row in rows {
        let (sequence, Some(body), Some(hash)) = row? else {
            return Err(InvestigationStoreError::Invalid);
        };
        count += 1;
        if count > limit {
            return Err(InvestigationStoreError::Invalid);
        }
        let event: InvestigationEvent = decode(&body, &hash)?;
        if event.investigation_id != *id
            || event.sequence != count
            || sequence != signed(count)?
            || event.revision < revision
            || event.revision > task.detail.summary.revision
        {
            return Err(InvestigationStoreError::Invalid);
        }
        revision = event.revision;
        match event.kind {
            InvestigationEventKind::ModelStarted => {
                starts = starts
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Invalid)?;
                let step_id = event.step_id.ok_or(InvestigationStoreError::Invalid)?;
                if let Some(refused_step) = refusal.as_deref() {
                    if correction.is_some() || step_id == refused_step {
                        return Err(InvestigationStoreError::Invalid);
                    }
                    correction = Some(source_replay_step(connection, task, &step_id)?);
                }
                last_started = Some(step_id);
            }
            InvestigationEventKind::SourceReplayRefused => {
                let refused_step = event.step_id.ok_or(InvestigationStoreError::Invalid)?;
                if refusal.is_some()
                    || event.tool.is_some()
                    || last_started.as_deref() != Some(refused_step.as_str())
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                source_replay_step(connection, task, &refused_step)?;
                refusal = Some(refused_step);
            }
            _ if refusal.is_some()
                && (event.tool.is_some()
                    || matches!(
                        event.kind,
                        InvestigationEventKind::ToolStarted
                            | InvestigationEventKind::ToolCompleted
                            | InvestigationEventKind::ContextPrepared
                            | InvestigationEventKind::ReadUnavailable
                            | InvestigationEventKind::ReadFirewallRefused
                            | InvestigationEventKind::EvidenceValidationReserved
                    )) =>
            {
                return Err(InvestigationStoreError::Invalid);
            }
            _ => {}
        }
    }
    if count != task.detail.summary.last_event_sequence {
        return Err(InvestigationStoreError::Invalid);
    }
    let invocations: u32 = connection.query_row(
        "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if invocations != starts || starts != task.detail.usage.model_invocations {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(refused_step) = refusal.as_ref()
        && (!matches!(
            task.phase,
            Phase::Ready | Phase::Consent | Phase::Invocation | Phase::Terminal
        ) || (task.phase == Phase::Invocation && task.pending.as_ref() != correction.as_ref())
            || (task.phase == Phase::Consent
                && task.pending.as_ref().is_none_or(|step| {
                    step.step_id == *refused_step
                        || task.ledger_hash.as_ref() != Some(&step.input_ledger_hash)
                }))
            || (correction.is_some() && !matches!(task.phase, Phase::Invocation | Phase::Terminal))
            || (task.detail.summary.has_result && correction.is_none()))
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(refusal.map(|step| (step, correction.is_some())))
}

fn source_replay_step(
    connection: &Connection,
    task: &StoredTask,
    step_id: &str,
) -> Result<PendingInvestigationStep, InvestigationStoreError> {
    let step: PendingInvestigationStep = read_body(
        connection,
        "investigation_invocations",
        "step_id",
        &task.detail.summary.investigation_id,
        &step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    validate_step(&step, &task.detail.limits)?;
    validate_step_basis(connection, task, &step)?;
    if step.step_id != step_id || task.ledger_hash.as_ref() != Some(&step.input_ledger_hash) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(step)
}

fn validate_source_replay_begin(
    connection: &Connection,
    task: &StoredTask,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    if let Some((refused_step, consumed)) = source_replay_state(connection, task)?
        && (consumed || step.step_id == refused_step)
    {
        return Err(InvestigationStoreError::Stale);
    }
    Ok(())
}

impl JobStore {
    pub fn advance_investigation(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        transition: InvestigationTransition,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = task_id_for_job(&tx, execution.id())?.ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_task(&tx, &id)?;
        check_identity(&task, execution)?;
        let late_finish = matches!(
            &transition,
            InvestigationTransition::ActionAdmitted {
                result: Some(_),
                ..
            } | InvestigationTransition::FinishRefused { .. }
                | InvestigationTransition::SourceReplayRefused { .. }
        );
        let stopping = matches!(
            &transition,
            InvestigationTransition::StopWithElapsed { .. }
                | InvestigationTransition::StopRejected { .. }
        );
        if task.detail.summary.revision != expected_revision {
            return Err(InvestigationStoreError::Stale);
        }
        check_job(&tx, execution, late_finish || stopping)?;
        if task.phase == Phase::Terminal {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested && !late_finish && !stopping {
            return Err(InvestigationStoreError::Cancelled);
        }
        match transition {
            InvestigationTransition::InputPrepared {
                ledger,
                usage,
                coverage_archive,
            } => {
                if task.phase != Phase::Preparing || task.ledger_revision.is_some() {
                    return Err(InvestigationStoreError::Stale);
                }
                if task
                    .expected_graph_revision
                    .as_ref()
                    .is_some_and(|expected| expected != &ledger.graph_snapshot_id)
                {
                    return Err(InvestigationStoreError::Conflict);
                }
                match (&ledger.coverage, coverage_archive) {
                    (None, None) => {}
                    (Some(proof), Some(archive)) if proof == archive.proof() => {
                        coverage::insert(&tx, &task.detail.summary.investigation_id, &archive)?;
                    }
                    _ => return Err(InvestigationStoreError::Invalid),
                }
                admit_input(&tx, &mut task, &ledger, usage, true)?;
            }
            InvestigationTransition::InputAdmitted { ledger, usage } => {
                if task.phase != Phase::Tool {
                    return Err(InvestigationStoreError::Stale);
                }
                admit_input(&tx, &mut task, &ledger, usage, false)?;
            }
            InvestigationTransition::AwaitConsent { step } => {
                check_ready(&task, &step)?;
                validate_step_basis(&tx, &task, &step)?;
                if task.detail.provider.mode != InvestigationProviderMode::Cloud {
                    return Err(InvestigationStoreError::Invalid);
                }
                task.phase = Phase::Consent;
                task.detail.summary.status = InvestigationStatus::AwaitingConsent;
                task.pending = Some(step.clone());
                task.approved = false;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ConsentRequired,
                    Some(&step.step_id),
                    None,
                    "Exact step consent is required; no provider call has started.",
                )?;
            }
            InvestigationTransition::BeginInvocation { step } => {
                validate_source_replay_begin(&tx, &task, &step)?;
                completion::reserve_invocation(&tx, &task)?;
                validate_step(&step, &task.detail.limits)?;
                validate_step_basis(&tx, &task, &step)?;
                if task.ledger_hash.as_ref() != Some(&step.input_ledger_hash) {
                    return Err(InvestigationStoreError::Conflict);
                }
                match task.detail.provider.mode {
                    InvestigationProviderMode::Local => check_ready(&task, &step)?,
                    InvestigationProviderMode::Cloud => {
                        if task.phase != Phase::Consent
                            || !task.approved
                            || task.pending.as_ref() != Some(&step)
                        {
                            return Err(InvestigationStoreError::Stale);
                        }
                    }
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.model_invocations = usage
                    .model_invocations
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?;
                usage.generated_token_reservations = usage
                    .generated_token_reservations
                    .checked_add(step.generated_tokens)
                    .ok_or(InvestigationStoreError::Capacity)?;
                validate_usage(&usage, &task.detail.limits)?;
                if read_body::<PendingInvestigationStep>(
                    &tx,
                    "investigation_invocations",
                    "step_id",
                    &id,
                    &step.step_id,
                )?
                .is_some()
                {
                    return Err(InvestigationStoreError::Conflict);
                }
                insert_body(
                    &tx,
                    "investigation_invocations",
                    &id,
                    &step.step_id,
                    &step,
                    MAX_INVOCATION_BYTES,
                )?;
                task.detail.usage = usage;
                task.pending = Some(step.clone());
                task.approved = false;
                task.phase = Phase::Invocation;
                task.detail.summary.status = InvestigationStatus::Running;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ModelStarted,
                    Some(&step.step_id),
                    None,
                    "Invocation boundary committed; the provider outcome is pending.",
                )?;
            }
            InvestigationTransition::ActionAdmitted {
                response,
                tool,
                result,
            } => {
                if task.phase != Phase::Invocation || tool.is_some() == result.is_some() {
                    return Err(InvestigationStoreError::Stale);
                }
                if source_replay_state(&tx, &task)?.is_some_and(|(_, consumed)| consumed)
                    && tool.is_some()
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                if tool == Some(InvestigationTool::ReadCoverageInventory)
                    && !task
                        .detail
                        .summary
                        .specialist_id
                        .uses_coverage_qualification()
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                let (invoked, usage) = validate_response(&tx, &task, &response)?;
                if let Some(result) = &result {
                    let ledger =
                        load_ledger(&tx, &task)?.ok_or(InvestigationStoreError::Invalid)?;
                    validate_result_basis(result, &ledger, &invoked, &task.detail.limits, None)?;
                    validate_inspection_result(&task, result)?;
                    completion::accepted(&tx, &mut task, result, &ledger, &invoked, None)?;
                    if result.investigation_id != id
                        || result.observed_response_model != response.observed_model
                    {
                        return Err(InvestigationStoreError::Invalid);
                    }
                    let body = encode(result, MAX_RESULT_BYTES)?;
                    let result_hash = core_prov::content_hash(&body);
                    one(tx.execute(
                        "INSERT INTO investigation_results VALUES (?1,?2,?3)",
                        params![id, body, result_hash],
                    )?)?;
                    super::chat::insert_result_reference(
                        &tx,
                        &id,
                        &result.result_id,
                        &result_hash,
                    )?;
                    if task.completion_intent.is_none() {
                        task.detail.inspection_assessment = result.inspection_assessment.clone();
                        if let Some(assessment) = &result.inspection_assessment {
                            task.detail.summary.inspection_status =
                                Some(if assessment.requirements.iter().all(|r| r.supplied) {
                                    InvestigationInspectionStatus::Supplied
                                } else {
                                    InvestigationInspectionStatus::Unmet
                                });
                        }
                    }
                    task.detail.summary.status = InvestigationStatus::Completed;
                    task.detail.summary.has_result = true;
                    task.phase = Phase::Terminal;
                } else {
                    task.phase = Phase::Action;
                }
                insert_body(
                    &tx,
                    "investigation_responses",
                    &id,
                    &response.step_id,
                    &response,
                    4096,
                )?;
                task.detail.usage = usage;
                task.pending = None;
                task.approved = false;
                task.action_tool = tool;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ModelCompleted,
                    Some(&response.step_id),
                    tool,
                    "A complete provider response was admitted and its identity persisted.",
                )?;
                event(
                    &tx,
                    &mut task,
                    if result.is_some() {
                        InvestigationEventKind::ResultPersisted
                    } else {
                        InvestigationEventKind::ActionAdmitted
                    },
                    Some(&response.step_id),
                    tool,
                    if result.is_some() {
                        "Cited findings were persisted against the original input ledger."
                    } else {
                        "A bounded tool action was admitted; its tool has not started."
                    },
                )?;
                if result.is_some() {
                    sync_terminal_job(&tx, &task)?;
                }
            }
            InvestigationTransition::SourceReplayRefused {
                step_id,
                reported_input_tokens,
                reported_output_tokens,
                reported_cache_read_tokens,
                active_milliseconds,
            } => {
                if task.phase != Phase::Invocation
                    || task.completion_intent.is_some()
                    || task
                        .detail
                        .summary
                        .specialist_id
                        .uses_next_prompt_admission()
                    || active_milliseconds < task.detail.usage.active_milliseconds
                    || task
                        .pending
                        .as_ref()
                        .is_none_or(|step| step.step_id != step_id)
                    || source_replay_state(&tx, &task)?.is_some()
                {
                    return Err(InvestigationStoreError::Stale);
                }
                let invoked: PendingInvestigationStep =
                    read_body(&tx, "investigation_invocations", "step_id", &id, &step_id)?
                        .ok_or(InvestigationStoreError::Invalid)?;
                if task.pending.as_ref() != Some(&invoked) {
                    return Err(InvestigationStoreError::Invalid);
                }
                let mut usage = task.detail.usage.clone();
                usage.reported_input_tokens = add_reported(
                    usage.reported_input_tokens,
                    reported_input_tokens,
                    usage.model_invocations == 1,
                )?;
                usage.reported_output_tokens = add_reported(
                    usage.reported_output_tokens,
                    reported_output_tokens,
                    usage.model_invocations == 1,
                )?;
                usage.reported_cache_read_tokens = add_reported(
                    usage.reported_cache_read_tokens,
                    reported_cache_read_tokens,
                    usage.model_invocations == 1,
                )?;
                usage.active_milliseconds = active_milliseconds;
                validate_usage(&usage, &task.detail.limits)?;
                task.detail.usage = usage;
                task.pending = None;
                task.approved = false;
                task.action_tool = None;
                task.phase = Phase::Ready;
                task.detail.summary.status = InvestigationStatus::Running;
                let policy = super::policy_for_detail(&task.detail)?;
                let has_invocation_budget =
                    task.detail.usage.model_invocations < task.detail.limits.model_invocations;
                let has_token_budget = task
                    .detail
                    .usage
                    .generated_token_reservations
                    .checked_add(policy.limits().generated_tokens_per_invocation)
                    .is_some_and(|reserved| {
                        reserved <= task.detail.limits.generated_token_reservations
                    });
                let has_time_budget = task.detail.usage.active_milliseconds
                    < task.detail.limits.active_seconds.saturating_mul(1000);
                let settled = task.clone();
                if !task.detail.summary.cancel_requested
                    && has_invocation_budget
                    && has_token_budget
                    && has_time_budget
                {
                    // A failed continuation must leave room for the known refusal's
                    // terminal receipt. Roll back its nonterminal reservation first.
                    tx.execute_batch("SAVEPOINT source_replay_ready")?;
                    let ready = (|| {
                        bump(&tx, &mut task)?;
                        event(
                            &tx,
                            &mut task,
                            InvestigationEventKind::SourceReplayRefused,
                            Some(&step_id),
                            None,
                            SOURCE_REPLAY_FEEDBACK,
                        )?;
                        save_task(&tx, &task, expected_revision, false)
                    })();
                    match ready {
                        Ok(()) => {
                            tx.execute_batch("RELEASE source_replay_ready")?;
                            tx.commit()?;
                            return Ok(task.detail);
                        }
                        Err(InvestigationStoreError::Capacity) => {
                            tx.execute_batch(
                                "ROLLBACK TO source_replay_ready; RELEASE source_replay_ready",
                            )?;
                            task = settled;
                        }
                        Err(error) => return Err(error),
                    }
                }
                // Both terminal events use the already reserved terminal slots/bytes.
                task.phase = Phase::Terminal;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::SourceReplayRefused,
                    Some(&step_id),
                    None,
                    SOURCE_REPLAY_FEEDBACK,
                )?;
                let reason = if task.detail.summary.cancel_requested {
                    InvestigationStopReason::Cancelled
                } else {
                    InvestigationStopReason::InvalidFinish(InvestigationError::SourceReplay)
                };
                stop(&tx, &mut task, reason)?;
            }
            InvestigationTransition::FinishRefused {
                request,
                response,
                finish,
                input,
            } => {
                completion::refused(
                    &tx,
                    &self.namespace,
                    execution,
                    &mut task,
                    &request,
                    &input,
                    &finish,
                    response,
                    None,
                    None,
                )?;
            }
            InvestigationTransition::ReadUnavailable { read, reason } => {
                completion::unavailable(&tx, &mut task, read, reason)?;
            }
            InvestigationTransition::QueryRefused { record } => {
                query_refusal::publish(&tx, &mut task, *record)?;
            }
            InvestigationTransition::BeginTool { tool } => {
                if task.phase != Phase::Action || task.action_tool != Some(tool) {
                    return Err(InvestigationStoreError::Stale);
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.tool_actions = usage
                    .tool_actions
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?;
                if tool == InvestigationTool::ReadEvidence {
                    usage.evidence_requests = usage
                        .evidence_requests
                        .checked_add(1)
                        .ok_or(InvestigationStoreError::Capacity)?;
                }
                validate_usage(&usage, &task.detail.limits)?;
                task.detail.usage = usage;
                task.phase = Phase::Tool;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ToolStarted,
                    None,
                    Some(tool),
                    "Tool attempt budget reserved before execution.",
                )?;
            }
            InvestigationTransition::ChargeEvidence { validation_bytes } => {
                if task.phase != Phase::Tool
                    || task.action_tool != Some(InvestigationTool::ReadEvidence)
                    || validation_bytes == 0
                    || validation_bytes > 16 * 1024 * 1024
                {
                    return Err(InvestigationStoreError::Stale);
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.captured_validation_bytes = usage
                    .captured_validation_bytes
                    .checked_add(validation_bytes)
                    .ok_or(InvestigationStoreError::Capacity)?;
                validate_usage(&usage, &task.detail.limits)?;
                task.detail.usage = usage;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::EvidenceValidationReserved,
                    None,
                    Some(InvestigationTool::ReadEvidence),
                    "Full captured file validation bytes reserved before reading.",
                )?;
            }
            InvestigationTransition::StopWithElapsed {
                reason,
                active_milliseconds,
            } => {
                if active_milliseconds < task.detail.usage.active_milliseconds {
                    return Err(InvestigationStoreError::Invalid);
                }
                task.detail.usage.active_milliseconds = active_milliseconds;
                stop(&tx, &mut task, reason)?;
            }
            InvestigationTransition::StopRejected {
                reason,
                active_milliseconds,
                rejected,
            } => {
                if active_milliseconds < task.detail.usage.active_milliseconds {
                    return Err(InvestigationStoreError::Invalid);
                }
                task.detail.usage.active_milliseconds = active_milliseconds;
                stop(&tx, &mut task, reason)?;
                super::rejected_responses::record_on_stop(&tx, &task, reason, Some(&rejected))?;
            }
        }
        let terminal = task.phase == Phase::Terminal;
        save_task(&tx, &task, expected_revision, terminal)?;
        tx.commit()?;
        Ok(task.detail)
    }

    pub fn approve_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.consent_decision(id, revision, step_id, payload_hash, true)
    }

    pub fn decline_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.consent_decision(id, revision, step_id, payload_hash, false)
    }

    fn consent_decision(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
        approved: bool,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_task(&tx, id)?;
        if task.detail.summary.revision != revision
            || task.phase != Phase::Consent
            || task.approved
            || task
                .pending
                .as_ref()
                .is_none_or(|step| step.step_id != step_id || step.payload_hash != payload_hash)
        {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if approved {
            task.approved = true;
            bump(&tx, &mut task)?;
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ConsentApproved,
                Some(step_id),
                None,
                "Exact step approved; authorization will be consumed at invocation start.",
            )?;
        } else {
            task.detail.summary.cancel_requested = true;
            stop(&tx, &mut task, InvestigationStopReason::ConsentDeclined)?;
        }
        save_task(&tx, &task, revision, task.phase == Phase::Terminal)?;
        tx.commit()?;
        Ok(task.detail)
    }

    pub fn cancel_investigation(
        &mut self,
        id: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.cancel_investigation_access(id, false, None)
    }

    /// Next-family cancellation retains its existing terminal reserve and
    /// generation fencing; owner app and MCP use the same transition.
    pub fn cancel_next_prompt_investigation(
        &mut self,
        id: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.cancel_investigation_access(id, true, None)
    }

    pub fn cancel_investigation_access(
        &mut self,
        id: &str,
        next_prompt: bool,
        session: Option<&McpTaskSession>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        with_task_access(session, |client| {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            validate(&tx, &self.namespace)?;
            require_request_owner(&tx, id, client)?;
            let mut task = if client.is_some() {
                load_execution_task(&tx, id)?
            } else if next_prompt {
                load_next_prompt_task_base(&tx, id)?
            } else {
                load_task(&tx, id)?
            };
            let next_prompt = task
                .detail
                .summary
                .specialist_id
                .uses_next_prompt_admission();
            if task.phase != Phase::Terminal && !task.detail.summary.cancel_requested {
                let expected = task.detail.summary.revision;
                request_cancel(&tx, &mut task)?;
                if next_prompt {
                    save_next_prompt_task(&tx, &task, expected, true)?;
                } else {
                    save_task(&tx, &task, expected, true)?;
                }
            }
            tx.commit()?;
            Ok(task.detail)
        })
    }

    /// Internal terminal path for failures outside a pending source read.
    /// Reservations must settle their durable charge and disposition instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stop_next_prompt_investigation(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        reason: InvestigationStopReason,
        active_milliseconds: u64,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.stop_next_prompt_investigation_keeping(
            execution,
            revision,
            reason,
            active_milliseconds,
            None,
        )
    }

    /// The same stop, also keeping the rejected response that caused it (AC-1403).
    pub fn stop_next_prompt_investigation_keeping(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        reason: InvestigationStopReason,
        active_milliseconds: u64,
        rejected: Option<&super::RejectedCompletion>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = task_id_for_job(&tx, execution.id())?.ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        check_identity(&task, execution)?;
        if task.detail.summary.revision != revision || task.phase == Phase::Terminal {
            return Err(InvestigationStoreError::Stale);
        }
        check_job(&tx, execution, true)?;
        if task.phase == Phase::Reservation || task.pending_source_read.is_some() {
            return Err(InvestigationStoreError::Invalid);
        }
        if active_milliseconds < task.detail.usage.active_milliseconds {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(audit) = &task.semantic_audit {
            audit.validate_saved(&tx, &task)?;
        }
        task.detail.usage.active_milliseconds = active_milliseconds;
        if task.phase == Phase::Consent {
            // This prospective call was never invoked; it has no invocation row.
            task.admission_state
                .as_mut()
                .ok_or(InvestigationStoreError::Invalid)?
                .current_call = None;
        }
        stop(&tx, &mut task, reason)?;
        super::rejected_responses::record_on_stop(&tx, &task, reason, rejected)?;
        save_next_prompt_task(&tx, &task, revision, true)?;
        if task.semantic_audit.is_some() {
            load_next_prompt_task(&tx, &id)?;
        }
        tx.commit()?;
        Ok(task.detail)
    }

    pub fn investigation_recovery_candidates(
        &self,
    ) -> Result<Vec<InvestigationRecoveryCandidate>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let ids=tx.prepare("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE live=1 ORDER BY ordinal LIMIT 3")?
            .query_map([],|r|r.get::<_,Option<String>>(0))?.collect::<Result<Vec<_>,_>>()?;
        if ids.len() > MAX_LIVE_TASKS {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut candidates = Vec::new();
        for id in ids {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let task = load_execution_task(&tx, &id)?;
            // No owner was ever recorded for a queued task. A concurrent start
            // may still be moving toward claim, so an absent/new lock cannot
            // prove its process died. Such rows remain explicitly cancellable.
            if task.execution.is_none() {
                continue;
            }
            candidates.push(InvestigationRecoveryCandidate {
                id,
                revision: task.detail.summary.revision,
                identity: task.execution.clone(),
                target: JobLockTarget {
                    namespace: self.namespace.clone(),
                    id: task.detail.summary.job_id,
                    existing: task.execution.is_some(),
                },
            });
        }
        tx.commit()?;
        Ok(candidates)
    }

    /// AC-1707: live tasks the agent pool held and never claimed. A held
    /// waiter keeps its job's lock file reserved while it waits, so only an
    /// existing, free lock proves the waiting process stopped; a task whose
    /// lock file does not exist yet stays queued and cancellable.
    pub fn held_investigation_candidates(
        &self,
    ) -> Result<Vec<InvestigationRecoveryCandidate>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let ids=tx.prepare("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE live=1 ORDER BY ordinal LIMIT 3")?
            .query_map([],|r|r.get::<_,Option<String>>(0))?.collect::<Result<Vec<_>,_>>()?;
        if ids.len() > MAX_LIVE_TASKS {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut candidates = Vec::new();
        for id in ids {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let task = load_execution_task(&tx, &id)?;
            if task.execution.is_some() || task.phase != Phase::Queued {
                continue;
            }
            candidates.push(InvestigationRecoveryCandidate {
                id,
                revision: task.detail.summary.revision,
                identity: None,
                target: JobLockTarget {
                    namespace: self.namespace.clone(),
                    id: task.detail.summary.job_id,
                    existing: true,
                },
            });
        }
        tx.commit()?;
        Ok(candidates)
    }

    pub fn recover_investigation(
        &mut self,
        candidate: &InvestigationRecoveryCandidate,
        reservation: ExecutionReservation,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        reservation.verify()?;
        if reservation.target.namespace != self.namespace
            || reservation.target.id != candidate.target.id
            || reservation.target.existing != candidate.target.existing
            || candidate.target.namespace != self.namespace
        {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_execution_task(&tx, &candidate.id)?;
        if task.phase == Phase::Terminal
            || task.detail.summary.revision != candidate.revision
            || task.execution != candidate.identity
        {
            tx.commit()?;
            return Ok(None);
        }
        let next_family = task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission();
        if next_family && task.phase == Phase::Reservation {
            // Keep the recovered OS lock. The source writer rechecks this exact
            // candidate inside its own immediate transaction and records the
            // unknown read without refunding its charge or reserved accounting.
            tx.commit()?;
            return self
                .settle_orphaned_next_prompt_source(candidate, reservation)
                .map(Some);
        }
        let expected = task.detail.summary.revision;
        if task.pending_patch().is_some() {
            // The persisted hashes authenticate the pending correction, but
            // never reconstruct its memory-only candidate. Do not dispatch.
            super::volatile_patch::authenticate(&tx, &task)?;
            if task.phase == Phase::Invocation {
                clear_unmeasured_usage(&mut task);
            }
            super::volatile_patch::abandon(&mut task);
            mark_inspection_not_assessed(&mut task);
            bump(&tx, &mut task)?;
            let kind = if task.detail.summary.cancel_requested {
                InvestigationEventKind::Cancelled
            } else {
                InvestigationEventKind::Failed
            };
            event(
                &tx,
                &mut task,
                kind,
                None,
                None,
                super::volatile_patch::LOST_CANDIDATE,
            )?;
            completion::refresh_observation(&tx, &mut task)?;
            sync_terminal_job(&tx, &task)?;
            save_next_prompt_task(&tx, &task, expected, true)?;
            tx.commit()?;
            return Ok(Some(task.detail));
        }
        let unknown = matches!(task.phase, Phase::Invocation | Phase::AuditInvocation);
        // AC-1707: held by the agent pool and never claimed, so nothing ran.
        let held = task.execution.is_none() && task.phase == Phase::Queued;
        if next_family && task.phase == Phase::Consent {
            // An uninvoked consent stamp is not a provider call. Preserve all
            // earlier measured usage while removing only that prospective call.
            task.admission_state
                .as_mut()
                .ok_or(InvestigationStoreError::Invalid)?
                .current_call = None;
        }
        if task.phase == Phase::AuditInvocation {
            super::semantic_audit_state::interrupt_started_audit(&mut task)?;
        } else if unknown {
            clear_unmeasured_usage(&mut task);
        }
        task.phase = Phase::Terminal;
        mark_inspection_not_assessed(&mut task);
        task.pending = None;
        task.approved = false;
        task.action_tool = None;
        task.detail.summary.status = if unknown {
            InvestigationStatus::OutcomeUnknown
        } else {
            InvestigationStatus::Interrupted
        };
        task.detail.error=Some(if unknown{"A started invocation has no durable admitted outcome; it was not replayed."}else if held{"The app stopped before this held investigation started; it was not started."}else{"The execution owner stopped and its transient input was lost; work was not resumed."}.into());
        bump(&tx, &mut task)?;
        event(
            &tx,
            &mut task,
            if unknown {
                InvestigationEventKind::OutcomeUnknown
            } else {
                InvestigationEventKind::Interrupted
            },
            None,
            None,
            if unknown {
                "Owner death confirmed; the started invocation outcome is unknown."
            } else if held {
                "The app stopped before this held investigation started."
            } else {
                "Owner death confirmed; transient preparation was interrupted."
            },
        )?;
        completion::refresh_observation(&tx, &mut task)?;
        sync_terminal_job(&tx, &task)?;
        if next_family {
            save_next_prompt_task(&tx, &task, expected, true)?;
        } else {
            save_task(&tx, &task, expected, true)?;
        }
        tx.commit()?;
        Ok(Some(task.detail))
    }
}

pub(super) fn add_reported(
    previous: Option<u64>,
    current: Option<u64>,
    first: bool,
) -> Result<Option<u64>, InvestigationStoreError> {
    if first {
        return Ok(current);
    }
    match (previous, current) {
        (Some(previous), Some(current)) => Ok(Some(
            previous
                .checked_add(current)
                .ok_or(InvestigationStoreError::Capacity)?,
        )),
        _ => Ok(None),
    }
}

fn check_ready(
    task: &StoredTask,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    validate_step(step, &task.detail.limits)?;
    if task.phase != Phase::Ready || task.ledger_hash.as_ref() != Some(&step.input_ledger_hash) {
        return Err(InvestigationStoreError::Stale);
    }
    available_time(task)
}

pub(super) fn available_time(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    if task.detail.usage.active_milliseconds >= task.detail.limits.active_seconds * 1000 {
        Err(InvestigationStoreError::Capacity)
    } else {
        Ok(())
    }
}

pub(super) fn admit_input(
    connection: &Connection,
    task: &mut StoredTask,
    ledger: &InvestigationInputLedger,
    usage: InvestigationUsage,
    first: bool,
) -> Result<(), InvestigationStoreError> {
    let policy = validate_input_candidate(connection, task, ledger, &usage)?;
    persist_input_rows(connection, task, ledger)?;
    task.ledger_revision = Some(ledger.revision);
    task.ledger_hash = Some(ledger.fingerprint_with_policy(&policy)?);
    task.detail.summary.graph_snapshot_id = Some(ledger.graph_snapshot_id.clone());
    task.detail.scope_snapshot_id = Some(ledger.scope_snapshot_id.clone());
    task.detail.coverage_context =
        ledger
            .coverage
            .as_ref()
            .map(|proof| InvestigationCoverageContext {
                context_id: proof.context_id.clone(),
                coverage: proof.qualification.clone(),
            });
    task.detail.citations = ledger.citations.clone();
    task.detail.usage = usage;
    let tool = task.action_tool.take();
    task.phase = Phase::Ready;
    task.detail.summary.status = InvestigationStatus::Running;
    bump(connection, task)?;
    event(
        connection,
        task,
        if first {
            InvestigationEventKind::ContextPrepared
        } else {
            InvestigationEventKind::ToolCompleted
        },
        None,
        tool,
        if first {
            "Frozen context and original input metadata committed."
        } else {
            "Tool input and its actual receipt references committed before lease release."
        },
    )?;
    if !first {
        completion::record_acquisition(connection, task, ledger, tool)?;
    }
    completion::refresh_observation(connection, task)
}

/// Read-only validation shared with prospective admission. The caller retains
/// the authoritative transaction and must revalidate its task/owner basis.
pub(super) fn validate_input_candidate(
    connection: &Connection,
    task: &StoredTask,
    ledger: &InvestigationInputLedger,
    usage: &InvestigationUsage,
) -> Result<InvestigationPolicy, InvestigationStoreError> {
    let policy = super::policy_for_detail(&task.detail)?;
    ledger.validate_for_specialist(task.detail.summary.specialist_id, &policy)?;
    let archive = coverage::load(connection, &task.detail.summary.investigation_id)?;
    match (&ledger.coverage, archive.as_ref()) {
        (None, None) => {}
        (Some(proof), Some(archive)) if proof == archive.proof() => {
            archive.validate_reads_with_policy(&ledger.coverage_reads, &policy)?;
        }
        _ => return Err(InvestigationStoreError::Invalid),
    }
    validate_usage(usage, &task.detail.limits)?;
    let prior = &task.detail.usage;
    if usage.model_invocations != prior.model_invocations
        || usage.tool_actions != prior.tool_actions
        || usage.evidence_requests != prior.evidence_requests
        || usage.captured_validation_bytes != prior.captured_validation_bytes
        || usage.generated_token_reservations != prior.generated_token_reservations
        || usage.reported_input_tokens != prior.reported_input_tokens
        || usage.reported_output_tokens != prior.reported_output_tokens
        || usage.reported_cache_read_tokens != prior.reported_cache_read_tokens
        || usage.active_milliseconds < prior.active_milliseconds
        || usage.selected_facts < prior.selected_facts
        || usage.evidence_items < prior.evidence_items
        || usage.evidence_bytes < prior.evidence_bytes
        || usage.selected_facts != ledger.selected_facts.len()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let evidence = ledger
        .citations
        .iter()
        .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata));
    let mut items = 0usize;
    let mut bytes = 0u64;
    let mut captured = 0u64;
    for citation in evidence {
        items += 1;
        let source = citation
            .source
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        bytes = bytes
            .checked_add(source.byte_end - source.byte_start)
            .ok_or(InvestigationStoreError::Capacity)?;
        if let InvestigationEvidenceOrigin::CapturedPrimarySource { captured: span, .. } =
            &citation.origin
        {
            captured = captured
                .checked_add(span.file.byte_len)
                .ok_or(InvestigationStoreError::Capacity)?;
        }
    }
    if usage.evidence_items != items
        || u64::try_from(usage.evidence_bytes).ok() != Some(bytes)
        || captured > usage.captured_validation_bytes
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(previous) = load_ledger(connection, task)?
        && (ledger.schema_version != previous.schema_version
            || ledger.coverage != previous.coverage
            || ledger.revision <= previous.revision
            || ledger.graph_snapshot_id != previous.graph_snapshot_id
            || ledger.scope_snapshot_id != previous.scope_snapshot_id
            || ledger.supplied_history_hash != previous.supplied_history_hash
            || ledger.supplied_history_bytes != previous.supplied_history_bytes
            || !previous
                .selected_facts
                .iter()
                .all(|fact| ledger.selected_facts.contains(fact))
            || !previous
                .citations
                .iter()
                .all(|citation| ledger.citations.contains(citation))
            || !ledger.coverage_reads.starts_with(&previous.coverage_reads)
            || !ledger.queries.starts_with(&previous.queries)
            || !previous
                .receipt_references
                .iter()
                .all(|reference| ledger.receipt_references.contains(reference)))
    {
        return Err(InvestigationStoreError::Conflict);
    }
    if ledger.schema_version == 2 {
        InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(&policy)?,
            previous_ledger: load_ledger(connection, task)?,
            completed_tool: task.action_tool,
        }
        .validate_with_policy(ledger, &policy)?;
    }
    Ok(policy)
}

/// Persist only bounded input and newly referenced receipts, in caller order.
/// The caller must validate the candidate and owns transaction/rollback plus
/// task, event and completion-attempt finalization; this helper never commits.
pub(super) fn persist_input_rows(
    connection: &Connection,
    task: &StoredTask,
    ledger: &InvestigationInputLedger,
) -> Result<(), InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    insert_body(
        connection,
        "investigation_inputs",
        id,
        &signed(ledger.revision)?,
        ledger,
        MAX_MANIFEST_BYTES,
    )?;
    for reference in &ledger.receipt_references {
        let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM investigation_refs WHERE task_id=?1 AND source_id=?2 AND repo_key=?3 AND receipt_id=?4)",params![id,reference.source_id,reference.repo_key,reference.receipt_id],|r|r.get(0))?;
        if !exists {
            one(connection.execute(
                "INSERT INTO investigation_refs VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    signed(ledger.revision)?,
                    reference.source_id,
                    reference.repo_key,
                    reference.receipt_id
                ],
            )?)?;
        }
    }
    Ok(())
}

pub(super) fn check_identity(
    task: &StoredTask,
    execution: &JobExecution,
) -> Result<(), InvestigationStoreError> {
    let identity = task
        .execution
        .as_ref()
        .ok_or(InvestigationStoreError::Ownership)?;
    if identity.namespace != execution.inner.namespace.value
        || identity.job_id != execution.id()
        || identity.generation != execution.inner.generation
        || identity.owner != execution.inner.owner
    {
        return Err(InvestigationStoreError::Ownership);
    }
    Ok(())
}

pub(super) fn check_job(
    connection: &Connection,
    execution: &JobExecution,
    allow_cancelled_or_missing: bool,
) -> Result<(), InvestigationStoreError> {
    let row=connection.query_row("SELECT CASE WHEN typeof(j.status)='text' AND length(CAST(j.status AS BLOB))<=32 THEN j.status END,a.generation,CASE WHEN typeof(a.owner)='text' AND length(CAST(a.owner AS BLOB))=32 THEN a.owner END FROM jobs j LEFT JOIN job_attempts a ON a.job_id=j.id WHERE j.id=?1",[execution.id()],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<String>>(2)?))).optional()?;
    match row {
        None if allow_cancelled_or_missing => Ok(()),
        Some((Some(status), generation, owner))
            if generation == Some(execution.inner.generation)
                && owner.as_deref() == Some(execution.inner.owner.as_str())
                && (status == "running"
                    || (allow_cancelled_or_missing && status == "cancelled")) =>
        {
            Ok(())
        }
        _ => Err(InvestigationStoreError::Ownership),
    }
}

pub(super) fn stop(
    connection: &Connection,
    task: &mut StoredTask,
    reason: InvestigationStopReason,
) -> Result<(), InvestigationStoreError> {
    let terminal = reason.terminal_class();
    if terminal == InvestigationStopReason::OutcomeUnknown
        && !matches!(task.phase, Phase::Invocation | Phase::AuditInvocation)
    {
        return Err(InvestigationStoreError::Stale);
    }
    if task.phase == Phase::AuditInvocation {
        super::semantic_audit_state::stop_started_audit(task)?;
    } else if task.phase == Phase::Invocation {
        clear_unmeasured_usage(task);
        if let Some(audit) = task.semantic_audit.as_mut() {
            audit.account = audit
                .account
                .stop_unmeasured_analyst(&task.detail.usage, &task.detail.limits)?;
        }
    }
    let cancelled = task.detail.summary.cancel_requested
        || matches!(
            terminal,
            InvestigationStopReason::Cancelled | InvestigationStopReason::ConsentDeclined
        );
    let unknown = terminal == InvestigationStopReason::OutcomeUnknown;
    task.pending_correction = None;
    task.phase = Phase::Terminal;
    mark_inspection_not_assessed(task);
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    if cancelled {
        task.detail.summary.cancel_requested = true;
    }
    task.detail.summary.status = if unknown {
        InvestigationStatus::OutcomeUnknown
    } else if cancelled {
        InvestigationStatus::Cancelled
    } else {
        InvestigationStatus::Failed
    };
    // A cap is still the terminal cause. Preserve only the last authenticated
    // refused page's numeric diagnostic, never the query or response text.
    let query_cap = if terminal == InvestigationStopReason::LimitExceeded
        && task
            .detail
            .summary
            .specialist_id
            .uses_query_budget_refusals()
    {
        use agents::investigation::InvestigationTerminalDiagnostic;
        // Generic host/store failures can coincide with exhausted counters.
        // Only the exact worker cap check supplies this causal witness; never
        // infer ActiveTime (or a tool/model cap) from an unrelated failure.
        let termination = query_budget_cap(reason);
        match termination {
            Some(termination) => super::query_refusal::latest(connection, task)?
                .and_then(|record| record.budget)
                .map(|budget| InvestigationTerminalDiagnostic::QueryBudget {
                    termination,
                    budget,
                }),
            None => None,
        }
    } else {
        None
    };
    task.detail.terminal_diagnostic = match reason {
        InvestigationStopReason::OperationalAt { phase, error_class } => Some(
            agents::investigation::InvestigationTerminalDiagnostic::Operational {
                phase,
                error_class,
            },
        ),
        InvestigationStopReason::HostLimitAt {
            limit: HostLimitKind::QueryPageBudget(budget),
            ..
        } => Some(
            agents::investigation::InvestigationTerminalDiagnostic::QueryBudget {
                termination: agents::investigation::QueryBudgetTermination::PageBudget,
                budget,
            },
        ),
        // AC-0725: already-classified closed causes become machine-readable;
        // the rejected response and the refused input are never retained.
        InvestigationStopReason::ActionDecode(failure) => Some(failure.into()),
        InvestigationStopReason::PreparationAt(phase) => Some(
            agents::investigation::InvestigationTerminalDiagnostic::Preparation {
                phase: phase.diagnostic_phase(),
            },
        ),
        _ => query_cap,
    };
    task.detail.error = Some(reason.message().into());
    bump(connection, task)?;
    event(
        connection,
        task,
        if unknown {
            InvestigationEventKind::OutcomeUnknown
        } else if reason == InvestigationStopReason::ConsentDeclined {
            InvestigationEventKind::ConsentDeclined
        } else if cancelled {
            InvestigationEventKind::Cancelled
        } else {
            InvestigationEventKind::Failed
        },
        None,
        None,
        reason.message(),
    )?;
    completion::refresh_observation(connection, task)?;
    sync_terminal_job(connection, task)
}

fn mark_inspection_not_assessed(task: &mut StoredTask) {
    if task.detail.inspection_requirements.is_some() && task.detail.inspection_assessment.is_none()
    {
        task.detail.summary.inspection_status = Some(InvestigationInspectionStatus::NotAssessed);
    }
}

fn clear_unmeasured_usage(task: &mut StoredTask) {
    // A prior measured response cannot establish the complete total when a
    // later invocation has no admitted usage report, including owner death.
    task.detail.usage.reported_input_tokens = None;
    task.detail.usage.reported_output_tokens = None;
    task.detail.usage.reported_cache_read_tokens = None;
}

fn request_cancel(
    connection: &Connection,
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    task.detail.summary.cancel_requested = true;
    if task.phase == Phase::Queued {
        task.phase = Phase::Terminal;
        mark_inspection_not_assessed(task);
        task.detail.summary.status = InvestigationStatus::Cancelled;
    }
    bump(connection, task)?;
    event(
        connection,
        task,
        InvestigationEventKind::CancelRequested,
        None,
        None,
        "Cancellation requested; outstanding provider work retains its execution lease.",
    )?;
    connection.execute("UPDATE jobs SET status='cancelled',updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1 AND status IN ('queued','running')",[task.detail.summary.job_id])?;
    completion::refresh_observation(connection, task)
}

pub(super) fn sync_terminal_job(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let status = if task.detail.summary.cancel_requested {
        "cancelled"
    } else {
        match task.detail.summary.status {
            InvestigationStatus::Completed => "done",
            InvestigationStatus::Interrupted | InvestigationStatus::OutcomeUnknown => "interrupted",
            _ => "failed",
        }
    };
    connection.execute("UPDATE jobs SET status=?2,progress=CASE WHEN ?2='done' THEN 100.0 ELSE progress END,error=?3,updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1 AND status IN ('queued','running')",params![task.detail.summary.job_id,status,task.detail.error])?;
    Ok(())
}

pub(super) fn task_id_for_job(
    connection: &Connection,
    job_id: i64,
) -> Result<Option<String>, InvestigationStoreError> {
    let id:Option<Option<String>>=connection.query_row("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE job_id=?1",[job_id],|r|r.get(0)).optional()?;
    id.map(|id| id.ok_or(InvestigationStoreError::Invalid))
        .transpose()
}

pub(in crate::jobs) fn attach_execution(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    job_id: i64,
    kind: &str,
    generation: i64,
    owner: &str,
) -> Result<(), InvestigationStoreError> {
    let id = task_id_for_job(connection, job_id)?;
    if !kind.starts_with(KIND_PREFIX) && id.is_none() {
        return Ok(());
    }
    validate(connection, namespace)?;
    let id = id.ok_or(InvestigationStoreError::Invalid)?;
    if kind != format!("{KIND_PREFIX}{id}") {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut task = load_execution_task(connection, &id)?;
    if task.phase != Phase::Queued
        || task.execution.is_some()
        || task.detail.summary.cancel_requested
        || generation != 1
    {
        return Err(InvestigationStoreError::Stale);
    }
    let revision = task.detail.summary.revision;
    task.execution = Some(StoredExecution {
        namespace: namespace.value.clone(),
        job_id,
        generation,
        owner: owner.into(),
    });
    task.phase = Phase::Preparing;
    task.detail.summary.status = InvestigationStatus::Preparing;
    bump(connection, &mut task)?;
    event(
        connection,
        &mut task,
        InvestigationEventKind::PreparationStarted,
        None,
        None,
        "Execution ownership attached atomically; preparation started.",
    )?;
    if task
        .detail
        .summary
        .specialist_id
        .uses_next_prompt_admission()
    {
        save_next_prompt_task(connection, &task, revision, false)
    } else {
        save_task(connection, &task, revision, false)
    }
}

pub(in crate::jobs) fn cancel_for_job(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    job_id: i64,
) -> Result<(), InvestigationStoreError> {
    let Some(id) = task_id_for_job(connection, job_id)? else {
        return Ok(());
    };
    validate(connection, namespace)?;
    let mut task = load_execution_task(connection, &id)?;
    if task.phase != Phase::Terminal && !task.detail.summary.cancel_requested {
        let revision = task.detail.summary.revision;
        request_cancel(connection, &mut task)?;
        if task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
        {
            save_next_prompt_task(connection, &task, revision, true)?;
        } else {
            save_task(connection, &task, revision, true)?;
        }
    }
    Ok(())
}

// Shared by normal action admission and the private staged refusal journal.
pub(super) fn validate_response(
    connection: &Connection,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
) -> Result<(PendingInvestigationStep, InvestigationUsage), InvestigationStoreError> {
    if let Some(telemetry) = &response.finish_telemetry {
        if !task.detail.summary.specialist_id.uses_finish_patch() {
            return Err(InvestigationStoreError::Invalid);
        }
        telemetry.validate()?;
    }
    let id = &task.detail.summary.investigation_id;
    let pending = task
        .pending
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if response.step_id != pending.step_id
        || !hash_valid(&response.response_hash)
        || !hash_valid(&response.action_hash)
        || response
            .observed_model
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 256)
        || response.active_milliseconds < task.detail.usage.active_milliseconds
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(model) = &response.observed_model
        && redacted_history_text(model, 256)? != *model
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let invoked: PendingInvestigationStep = read_body(
        connection,
        "investigation_invocations",
        "step_id",
        id,
        &response.step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    if &invoked != pending {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut usage = task.detail.usage.clone();
    usage.reported_input_tokens = add_reported(
        usage.reported_input_tokens,
        response.reported_input_tokens,
        usage.model_invocations == 1,
    )?;
    usage.reported_output_tokens = add_reported(
        usage.reported_output_tokens,
        response.reported_output_tokens,
        usage.model_invocations == 1,
    )?;
    usage.reported_cache_read_tokens = add_reported(
        usage.reported_cache_read_tokens,
        response.reported_cache_read_tokens,
        usage.model_invocations == 1,
    )?;
    usage.active_milliseconds = response.active_milliseconds;
    validate_usage(&usage, &task.detail.limits)?;
    Ok((invoked, usage))
}

// No usage counters enter this projection: a coincident exhausted counter is
// not evidence that an unrelated host/store error was caused by that cap.
pub(super) fn query_budget_cap(
    reason: InvestigationStopReason,
) -> Option<agents::investigation::QueryBudgetTermination> {
    match reason {
        InvestigationStopReason::BudgetCap(cap) => Some(cap),
        _ => None,
    }
}
