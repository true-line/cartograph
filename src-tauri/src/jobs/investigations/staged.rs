//! Optional internal staging journal. No production caller or task transition.
//! Slice 3 must combine TerminalRequired with host terminal publication in this
//! same caller-owned transaction; this helper never commits or clears a phase.
use super::*;
use agents::investigation::{
    obligations::*, staged_attempt::*, staged_finish::*, staged_refusal::*,
};
use rusqlite::Transaction;

fn schema() -> Vec<String> {
    let mut sql=vec![
        "CREATE TABLE staged_completion_intents (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL) STRICT".into(),
        "CREATE TABLE staged_completion_refusals (task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 2), body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT".into(),
        "CREATE TABLE staged_completion_feedback (task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 2), body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT".into()];
    for table in [
        "staged_completion_intents",
        "staged_completion_refusals",
        "staged_completion_feedback",
    ] {
        for verb in ["UPDATE", "DELETE"] {
            sql.push(format!("CREATE TRIGGER {table}_no_{} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable staged completion record'); END",verb.to_lowercase()));
        }
    }
    sql
}
fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'staged_completion_*'",
        [],
        |row| row.get(0),
    )?;
    if count == 0 {
        for sql in schema() {
            tx.execute_batch(&sql)?;
        }
    }
    validate_schema(tx)
}
fn validate_schema(connection: &Connection) -> Result<(), InvestigationStoreError> {
    let mut actual = connection
        .prepare("SELECT sql FROM sqlite_schema WHERE name GLOB 'staged_completion_*'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = schema();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    legacy_intent_hash: String,
    request_hash: String,
    envelope: ObligationEnvelope,
    expected_graph: Option<String>,
    expected_context: String,
    redacted_question_hash: String,
}
impl Binding {
    fn validate(&self, task: &StoredTask) -> Result<(), InvestigationStoreError> {
        self.envelope.normalized()?;
        let sources = self.envelope.sources()?;
        let sources = if sources.is_empty() {
            vec![]
        } else {
            normalize_inspection_targets(&sources)?
        };
        if self.legacy_intent_hash != task.intent_hash
            || !hash_valid(&self.request_hash)
            || self.expected_graph != task.expected_graph_revision
            || self.redacted_question_hash
                != core_prov::content_hash(task.detail.summary.question.as_bytes())
            || sources
                != task
                    .detail
                    .inspection_requirements
                    .clone()
                    .unwrap_or_default()
            || task
                .detail
                .coverage_context
                .as_ref()
                .is_none_or(|context| context.context_id != self.expected_context)
            || !task
                .detail
                .summary
                .specialist_id
                .uses_coverage_qualification()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        for item in &self.envelope.obligations {
            if let Obligation::Inventory { target, .. } = item {
                let coverage = &task
                    .detail
                    .coverage_context
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?
                    .coverage;
                if target.context_id != self.expected_context
                    || !coverage.repos.iter().any(|repo| {
                        repo.repo == target.repo
                            && repo.status == context_hub::coverage::CoverageStatus::Recorded
                            && repo.inventory_hash.as_deref()
                                == Some(target.inventory_hash.as_str())
                    })
                {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
        }
        Ok(())
    }
}
fn bind(
    tx: &Transaction<'_>,
    task: &StoredTask,
    request: &StagedObligationRequest,
) -> Result<Binding, InvestigationStoreError> {
    initialize(tx)?;
    let request = request.normalized()?;
    let sources = request.obligations.sources()?;
    let projected = StartInvestigationRequest {
        schema_version: 3,
        request_nonce: request.request_nonce.clone(),
        specialist_id: if request.specialist_id == "domain-analyst@7" {
            SpecialistId::DomainAnalystV6
        } else {
            SpecialistId::EvidenceAuditorV6
        },
        inspection_requirements: if sources.is_empty() {
            None
        } else {
            Some(normalize_inspection_targets(&sources)?)
        },
        question: request.question.clone(),
        scope: request.scope.clone(),
        provider_mode: request.provider_mode,
        limit_profile: request.limit_profile.clone(),
        expected_graph_revision: request.expected_graph_revision.clone(),
        expected_context_id: Some(request.expected_context_id.clone()),
        conversation_id: request.conversation_id.clone(),
        parent_id: request.parent_id.clone(),
    };
    if projected.intent_hash() != Ok(task.intent_hash.clone()) {
        return Err(InvestigationStoreError::Invalid);
    }
    let expected_role = if request.specialist_id == "domain-analyst@7" {
        SpecialistId::DomainAnalystV6
    } else {
        SpecialistId::EvidenceAuditorV6
    };
    if task.nonce != request.request_nonce
        || task.detail.summary.specialist_id != expected_role
        || task.detail.summary.scope != request.scope
        || task.detail.summary.provider_mode != request.provider_mode
        || task.detail.summary.parent_id != request.parent_id
        || request
            .conversation_id
            .as_ref()
            .is_some_and(|id| id != &task.detail.summary.conversation_id)
        || redacted_history_text(&request.question, MAX_QUESTION_BYTES)?
            != task.detail.summary.question
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let binding = Binding {
        legacy_intent_hash: task.intent_hash.clone(),
        request_hash: request.intent_hash()?,
        envelope: request.obligations,
        expected_graph: request.expected_graph_revision,
        expected_context: request.expected_context_id,
        redacted_question_hash: core_prov::content_hash(task.detail.summary.question.as_bytes()),
    };
    binding.validate(task)?;
    let id = &task.detail.summary.investigation_id;
    if let Some(existing) = read::<Binding>(tx, "staged_completion_intents", id, None, 32 * 1024)? {
        if existing.request_hash != binding.request_hash || existing.envelope != binding.envelope {
            return Err(InvestigationStoreError::Conflict);
        }
        existing.validate(task)?;
        return Ok(existing);
    }
    insert(
        tx,
        "staged_completion_intents",
        id,
        None,
        &binding,
        32 * 1024,
    )?;
    Ok(binding)
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalEntry {
    entry_id: String,
    pub record: RefusalRecord,
    pub response: InvestigationResponseMeta,
    pub usage: InvestigationUsage,
    pub attempts: Vec<AttemptRecord>,
    pub ledger_revision: u64,
}
impl JournalEntry {
    fn content_id(&self) -> Result<String, InvestigationStoreError> {
        encode(self, MAX_RECORD_BYTES)?;
        let value = serde_json::json!({"record":self.record,"response":self.response,"usage":self.usage,"attempts":self.attempts,"ledger_revision":self.ledger_revision});
        let mut bytes = b"cartograph:staged-refusal-journal-entry:v1\0".to_vec();
        bytes.extend(encode(&value, MAX_RECORD_BYTES)?);
        Ok(core_prov::content_hash(&bytes))
    }
}
fn insert(
    tx: &Transaction<'_>,
    table: &str,
    id: &str,
    sequence: Option<u32>,
    value: &impl Serialize,
    max: usize,
) -> Result<(), InvestigationStoreError> {
    let body = encode(value, max)?;
    let hash = core_prov::content_hash(&body);
    let sql = if sequence.is_some() {
        format!("INSERT INTO {table} VALUES (?1,?2,?3,?4)")
    } else {
        format!("INSERT INTO {table} VALUES (?1,?3,?4)")
    };
    one(tx.execute(&sql, params![id, sequence, body, hash])?)?;
    Ok(())
}
fn read<T: serde::de::DeserializeOwned + Serialize>(
    connection: &Connection,
    table: &str,
    id: &str,
    sequence: Option<u32>,
    max: usize,
) -> Result<Option<T>, InvestigationStoreError> {
    let predicate = if sequence.is_some() {
        " AND sequence=?2"
    } else {
        " AND ?2 IS NULL"
    };
    let sql = format!(
        "SELECT CASE WHEN typeof(body)='blob' AND length(body)<=?3 THEN body END,CASE WHEN typeof(body_hash)='text' AND length(body_hash)=64 THEN body_hash END FROM {table} WHERE task_id=?1{predicate}"
    );
    let row: Option<(Option<Vec<u8>>, Option<String>)> = connection
        .query_row(&sql, params![id, sequence, max as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((Some(body), Some(hash))) = row else {
        return if row.is_none() {
            Ok(None)
        } else {
            Err(InvestigationStoreError::Invalid)
        };
    };
    if core_prov::content_hash(&body) != hash {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(Some(decode_record(
        &String::from_utf8(body).map_err(|_| InvestigationStoreError::Invalid)?,
    )?))
}
fn saved_ledger(
    connection: &Connection,
    id: &str,
    revision: u64,
) -> Result<InvestigationInputLedger, InvestigationStoreError> {
    read_body(
        connection,
        "investigation_inputs",
        "revision",
        id,
        &signed(revision)?,
    )?
    .ok_or(InvestigationStoreError::Invalid)
}
fn completed_attempts(
    connection: &Connection,
    id: &str,
    attempts: &[AttemptRecord],
) -> Result<Vec<InvestigationInputLedger>, InvestigationStoreError> {
    if attempts.len() > MAX_TOOL_ACTIONS as usize {
        return Err(InvestigationStoreError::Capacity);
    }
    let mut ledgers = Vec::new();
    for attempt in attempts {
        // Unavailable/firewall completion persistence is slice 3. A claimed
        // fixed code or observation hash is never accepted as durable evidence.
        let expected_tool = match attempt.outcome {
            AttemptOutcome::SourceAcquired { .. } => InvestigationTool::ReadEvidence,
            AttemptOutcome::InventoryAcquired { .. } => InvestigationTool::ReadCoverageInventory,
            _ => return Err(InvestigationStoreError::Invalid),
        };
        let response: InvestigationResponseMeta = read_body(
            connection,
            "investigation_responses",
            "step_id",
            id,
            &attempt.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let read_step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &attempt.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        validate_step(&read_step)?;
        let previous_revision = attempt
            .input_revision
            .checked_sub(1)
            .ok_or(InvestigationStoreError::Invalid)?;
        let before = saved_ledger(connection, id, previous_revision)?;
        let after = saved_ledger(connection, id, attempt.input_revision)?;
        if before.fingerprint()? != read_step.input_ledger_hash {
            return Err(InvestigationStoreError::Invalid);
        }
        InvestigationTurnBasis {
            input_ledger_hash: after.fingerprint()?,
            previous_ledger: Some(before.clone()),
            completed_tool: Some(expected_tool),
        }
        .validate(&after)?;
        if match &attempt.outcome {
            AttemptOutcome::SourceAcquired { citation_id } => after
                .citations
                .get(before.citations.len())
                .is_none_or(|citation| &citation.citation_id != citation_id),
            AttemptOutcome::InventoryAcquired { read_index } => {
                *read_index != before.coverage_reads.len()
            }
            _ => true,
        } {
            return Err(InvestigationStoreError::Invalid);
        }
        if response.action_hash != read_action_hash(&attempt.read)? {
            return Err(InvestigationStoreError::Invalid);
        }
        let task = load_task(connection, id)?;
        if task.detail.summary.last_event_sequence > MAX_EVENTS {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut admitted = false;
        let mut completed = false;
        let mut completed_revision = 0u64;
        for sequence in 1..=task.detail.summary.last_event_sequence {
            let event: InvestigationEvent = read_body(
                connection,
                "investigation_events",
                "sequence",
                id,
                &signed(sequence)?,
            )?
            .ok_or(InvestigationStoreError::Invalid)?;
            if event.sequence != sequence || event.investigation_id != id {
                return Err(InvestigationStoreError::Invalid);
            }
            if event.kind == InvestigationEventKind::ToolCompleted {
                completed_revision += 1;
            }
            if event.kind == InvestigationEventKind::ActionAdmitted {
                if admitted {
                    return Err(InvestigationStoreError::Invalid);
                }
                admitted = event.step_id.as_deref() == Some(attempt.step_id.as_str())
                    && event.tool == Some(expected_tool);
            } else if admitted && event.kind == InvestigationEventKind::ToolCompleted {
                completed = event.tool == Some(expected_tool)
                    && completed_revision == attempt.input_revision;
                break;
            }
        }
        if !completed {
            return Err(InvestigationStoreError::Invalid);
        }
        ledgers.push(saved_ledger(connection, id, attempt.input_revision)?);
    }
    Ok(ledgers)
}
fn read_action_hash(read: &ReadIntent) -> Result<String, InvestigationStoreError> {
    let value = match read {
        ReadIntent::Source { fact, role, index } => {
            serde_json::json!({"type":"read_evidence","fact":fact,"role":role,"index":index})
        }
        ReadIntent::Inventory {
            context_id,
            repo,
            inventory_hash,
            max_bytes,
        } => {
            serde_json::json!({"type":"read_coverage_inventory","context_id":context_id,"repo":repo,"inventory_hash":inventory_hash,"max_bytes":max_bytes})
        }
    };
    Ok(core_prov::content_hash(&encode(&value, MAX_ACTION_BYTES)?))
}
/// Replay only immutable, ordered journal and actual saved production witnesses.
/// Rejected finding prose/raw responses are deliberately not stored or replayed.
pub(super) fn load(
    connection: &Connection,
    id: &str,
) -> Result<Vec<JournalEntry>, InvestigationStoreError> {
    if connection.is_autocommit() {
        let tx = connection.unchecked_transaction()?;
        let entries = load(&tx, id)?;
        tx.commit()?;
        return Ok(entries);
    }
    validate_schema(connection)?;
    let task = load_task(connection, id)?;
    let binding = read::<Binding>(connection, "staged_completion_intents", id, None, 32 * 1024)?
        .ok_or(InvestigationStoreError::Invalid)?;
    binding.validate(&task)?;
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM staged_completion_refusals WHERE task_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let feedback_count: i64 = connection.query_row(
        "SELECT count(*) FROM staged_completion_feedback WHERE task_id=?1",
        [id],
        |row| row.get(0),
    )?;
    if !(0..=2).contains(&count) || count != feedback_count {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut entries: Vec<JournalEntry> = Vec::new();
    for sequence in 1..=count as u32 {
        let entry = read::<JournalEntry>(
            connection,
            "staged_completion_refusals",
            id,
            Some(sequence),
            MAX_RECORD_BYTES,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let feedback = read::<RefusalFeedback>(
            connection,
            "staged_completion_feedback",
            id,
            Some(sequence),
            2048,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let ledger = saved_ledger(connection, id, entry.ledger_revision)?;
        let step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &entry.record.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        validate_step(&step)?;
        let disclosure = step
            .disclosure
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let attempt_ledgers = completed_attempts(connection, id, &entry.attempts)?;
        let witnesses = entry
            .attempts
            .iter()
            .zip(&attempt_ledgers)
            .map(|(record, ledger)| AttemptEvidence { record, ledger })
            .collect::<Vec<_>>();
        let assessment = derive_assessment_for_binding(
            &binding.request_hash,
            &binding.envelope,
            id,
            &step.step_id,
            &ledger,
            disclosure,
            entry.record.outcome,
            &witnesses,
        )?;
        entry.record.validate_stored_basis(
            &binding.request_hash,
            &binding.envelope,
            &ledger,
            disclosure,
            entries.last().map(|entry| &entry.record),
        )?;
        if entry.record.task_id != id
            || entry.record.refusal_count != sequence
            || entry.record.assessment != assessment
            || entry.record.feedback != feedback
            || entry.response.step_id != step.step_id
            || entry.response.response_hash != entry.record.response_hash
            || !hash_valid(&entry.response.action_hash)
            || step.input_ledger_hash != ledger.fingerprint()?
            || disclosure.payload_hash != step.payload_hash
            || entry.usage.model_invocations > task.detail.usage.model_invocations
            || entry.usage.active_milliseconds < entry.response.active_milliseconds
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if entry.entry_id != entry.content_id()?
            || entry.usage.active_milliseconds != entry.response.active_milliseconds
            || entry
                .response
                .observed_model
                .as_ref()
                .is_some_and(|model| model.is_empty() || model.len() > 256)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(model) = &entry.response.observed_model
            && redacted_history_text(model, 256)? != *model
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let invocation_count:i64=connection.query_row("SELECT count(*) FROM investigation_invocations WHERE task_id=?1 AND rowid<=(SELECT rowid FROM investigation_invocations WHERE task_id=?1 AND step_id=?2)",params![id,entry.record.step_id],|row|row.get(0))?;
        if invocation_count != i64::from(entry.usage.model_invocations) {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(previous) = entries.last()
            && (previous.usage.model_invocations >= entry.usage.model_invocations
                || previous.usage.tool_actions > entry.usage.tool_actions
                || previous.usage.active_milliseconds > entry.usage.active_milliseconds
                || previous.usage.generated_token_reservations
                    > entry.usage.generated_token_reservations
                || previous.usage.evidence_requests > entry.usage.evidence_requests
                || previous.usage.evidence_bytes > entry.usage.evidence_bytes
                || previous.usage.captured_validation_bytes > entry.usage.captured_validation_bytes
                || [
                    (
                        &previous.usage.reported_input_tokens,
                        &entry.usage.reported_input_tokens,
                    ),
                    (
                        &previous.usage.reported_output_tokens,
                        &entry.usage.reported_output_tokens,
                    ),
                ]
                .iter()
                .any(|(old, new)| {
                    matches!((old, new), (None, Some(_)))
                        || matches!((old,new),(Some(a),Some(b)) if a>b)
                }))
        {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_usage(&entry.usage, &task.detail.limits)?;
        entries.push(entry);
    }
    Ok(entries)
}
/// Caller owns this transaction and commit. No production caller until slice 3.
#[allow(clippy::too_many_arguments)]
pub(super) fn append(
    tx: &Transaction<'_>,
    namespace: &ExecutionNamespace,
    execution: &JobExecution,
    request: &StagedObligationRequest,
    input: &InvestigationInput,
    finish: &StagedFinish,
    response: InvestigationResponseMeta,
    attempts: &[AttemptRecord],
) -> Result<RefusalDisposition, InvestigationStoreError> {
    execution.verify()?;
    validate(tx, namespace)?;
    let id = transitions::task_id_for_job(tx, execution.id())?
        .ok_or(InvestigationStoreError::Missing)?;
    let task = load_task(tx, &id)?;
    transitions::check_identity(&task, execution)?;
    transitions::check_job(tx, execution, false)?;
    if execution.inner.namespace != *namespace
        || task.phase != Phase::Invocation
        || task.detail.summary.cancel_requested
    {
        return Err(InvestigationStoreError::Stale);
    }
    let binding = bind(tx, &task, request)?;
    let previous = load(tx, &id)?;
    if previous.len() >= 2 {
        return Err(InvestigationStoreError::Capacity);
    }
    let (step, usage) = transitions::validate_response(tx, &task, &response)?;
    let ledger = load_ledger(tx, &task)?.ok_or(InvestigationStoreError::Invalid)?;
    if input.ledger() != &ledger {
        return Err(InvestigationStoreError::Invalid);
    }
    let disclosure = step
        .disclosure
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let attempt_ledgers = completed_attempts(tx, &id, attempts)?;
    let evidence = attempts
        .iter()
        .zip(&attempt_ledgers)
        .map(|(record, ledger)| AttemptEvidence { record, ledger })
        .collect::<Vec<_>>();
    let FinishDecision::Refused {
        assessment,
        feedback,
    } = admit_finish(
        request,
        &id,
        &step.step_id,
        input,
        disclosure,
        finish,
        &evidence,
        response.observed_model.clone(),
        "staged-admission".into(),
    )?
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    let canonical =
        serde_json::to_value(&finish.action).map_err(|_| InvestigationStoreError::Invalid)?;
    if response.action_hash != core_prov::content_hash(&encode(&canonical, MAX_ACTION_BYTES)?) {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut record = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: id.clone(),
        request_hash: binding.request_hash,
        obligations_hash: binding.envelope.fingerprint()?,
        step_id: step.step_id.clone(),
        input_ledger_hash: ledger.fingerprint()?,
        disclosure_hash: disclosure.fingerprint(&ledger)?,
        finish_hash: finish.fingerprint()?,
        outcome: finish.outcome,
        response_hash: response.response_hash.clone(),
        previous_refusal: previous.last().map(|entry| entry.record.refusal_id.clone()),
        refusal_count: previous.len() as u32 + 1,
        assessment,
        feedback,
    };
    record.refusal_id = record.content_id()?;
    let disposition = record.validate(
        request,
        &ledger,
        disclosure,
        finish,
        previous.last().map(|entry| &entry.record),
    )?;
    let mut entry = JournalEntry {
        entry_id: String::new(),
        record,
        response,
        usage,
        attempts: attempts.to_vec(),
        ledger_revision: ledger.revision,
    };
    entry.entry_id = entry.content_id()?;
    let bytes =
        encode(&entry, MAX_RECORD_BYTES)?.len() + entry.record.feedback.bytes()?.len() + 1024;
    let task_bytes = journal_bytes(tx, Some(&id))?
        .checked_add(stored_bytes(tx, Some(&id))?)
        .and_then(|n| n.checked_add(bytes))
        .ok_or(InvestigationStoreError::Capacity)?;
    let global = journal_bytes(tx, None)?
        .checked_add(stored_bytes(tx, None)?)
        .and_then(|n| n.checked_add(bytes))
        .ok_or(InvestigationStoreError::Capacity)?;
    if task_bytes > TASK_CAPACITY - TERMINAL_RESERVE || global > STORE_CAPACITY {
        return Err(InvestigationStoreError::Capacity);
    }
    insert(
        tx,
        "staged_completion_refusals",
        &id,
        Some(entry.record.refusal_count),
        &entry,
        MAX_RECORD_BYTES,
    )?;
    insert(
        tx,
        "staged_completion_feedback",
        &id,
        Some(entry.record.refusal_count),
        &entry.record.feedback,
        2048,
    )?;
    Ok(disposition)
}
fn journal_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    let mut total = 0usize;
    for table in [
        "staged_completion_intents",
        "staged_completion_refusals",
        "staged_completion_feedback",
    ] {
        let bytes:i64=connection.query_row(&format!("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256),0) FROM {table} WHERE (?1 IS NULL OR task_id=?1)"),[id],|row|row.get(0))?;
        total = total
            .checked_add(usize::try_from(bytes).map_err(|_| InvestigationStoreError::Capacity)?)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    Ok(total)
}

#[cfg(test)]
#[path = "staged_tests.rs"]
mod tests;
