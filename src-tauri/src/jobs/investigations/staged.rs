//! Immutable completion refusal journal shared by the production transition and
//! test adapter. The caller publishes any required terminal outcome in this same
//! transaction; this helper never commits or clears a phase.
use super::*;
use agents::investigation::{staged_attempt::*, staged_finish::*, staged_refusal::*};
use rusqlite::Transaction;

#[cfg(unix)]
#[path = "staged_workspace.rs"]
mod workspace;
#[cfg(unix)]
pub use workspace::next_prompt_staged_workspace;

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
pub(super) fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
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
    let mut expected = schema();
    let max_bytes = expected.iter().map(String::len).max().unwrap_or(0);
    // Reject unexpected SQL before owning its text. One extra bounded row
    // distinguishes an oversized namespace from the exact canonical layout.
    let mut actual = connection
        .prepare("SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE name GLOB 'staged_completion_*' LIMIT ?2")?
        .query_map(params![max_bytes as i64, expected.len() as i64 + 1], |row| {
            row.get::<_, Option<String>>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(InvestigationStoreError::Invalid)?;
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
        let policy = super::completion::saved_policy(task)?;
        self.envelope.normalized_with_policy(&policy)?;
        if let Some(intent) = &task.completion_intent
            && (self.request_hash != intent.request_hash
                || self.envelope != intent.obligations
                || self.expected_context != intent.expected_context)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let sources = self.envelope.sources_with_policy(&policy)?;
        let sources = if sources.is_empty() {
            vec![]
        } else {
            normalize_inspection_targets_with_policy(&sources, &policy)?
        };
        if self.legacy_intent_hash != task.intent_hash
            || !hash_valid(&self.request_hash)
            || self.expected_graph != task.expected_graph_revision
            || self.redacted_question_hash
                != core_prov::content_hash(task.detail.summary.question.as_bytes())
            || sources
                != if let Some(intent) = &task.completion_intent {
                    let sources = intent.obligations.sources_with_policy(&policy)?;
                    if sources.is_empty() {
                        vec![]
                    } else {
                        normalize_inspection_targets_with_policy(&sources, &policy)?
                    }
                } else {
                    task.detail
                        .inspection_requirements
                        .clone()
                        .unwrap_or_default()
                }
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
pub(super) fn journal_entry_bytes(policy: &InvestigationPolicy) -> usize {
    if policy.profile() == INVESTIGATION_V1_POLICY.profile() {
        64 * 1024
    } else {
        MAX_RECORD_BYTES
    }
}
fn binding_bytes(policy: &InvestigationPolicy) -> usize {
    if policy.profile() == INVESTIGATION_V1_POLICY.profile() {
        32 * 1024
    } else {
        // The normalized envelope and fixed bounded bindings remain mandatory.
        MAX_RECORD_BYTES
    }
}
pub(super) fn binding_reservation_bytes(
    task: &StoredTask,
    policy: &InvestigationPolicy,
) -> Result<usize, InvestigationStoreError> {
    let intent = task
        .completion_intent
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let binding = Binding {
        legacy_intent_hash: task.intent_hash.clone(),
        request_hash: intent.request_hash.clone(),
        envelope: intent.obligations.normalized_with_policy(policy)?,
        expected_graph: intent.expected_graph.clone(),
        expected_context: intent.expected_context.clone(),
        redacted_question_hash: core_prov::content_hash(task.detail.summary.question.as_bytes()),
    };
    encode(&binding, binding_bytes(policy))?
        .len()
        .checked_add(64 + 256)
        .ok_or(InvestigationStoreError::Capacity)
}
fn bind(
    tx: &Transaction<'_>,
    task: &StoredTask,
    request: &StagedObligationRequest,
) -> Result<Binding, InvestigationStoreError> {
    let policy = super::completion::saved_policy(task)?;
    initialize(tx)?;
    let request = request.normalized_with_policy(&policy)?;
    if let Some(intent) = &task.completion_intent {
        intent.validate(task)?;
        if intent.request_hash != request.intent_hash_with_policy(&policy)?
            || intent.obligations != request.obligations
            || intent.expected_context != request.expected_context_id
        {
            return Err(InvestigationStoreError::Invalid);
        }
    } else {
        #[cfg(not(any(test, feature = "test-support")))]
        return Err(InvestigationStoreError::Invalid);
        #[cfg(any(test, feature = "test-support"))]
        {
            let sources = request.obligations.sources_with_policy(&policy)?;
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
                    Some(normalize_inspection_targets_with_policy(&sources, &policy)?)
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
        }
    }
    let binding = Binding {
        legacy_intent_hash: task.intent_hash.clone(),
        request_hash: request.intent_hash_with_policy(&policy)?,
        envelope: request.obligations,
        expected_graph: request.expected_graph_revision,
        expected_context: request.expected_context_id,
        redacted_question_hash: core_prov::content_hash(task.detail.summary.question.as_bytes()),
    };
    binding.validate(task)?;
    let id = &task.detail.summary.investigation_id;
    if let Some(existing) = read::<Binding>(
        tx,
        "staged_completion_intents",
        id,
        None,
        binding_bytes(&policy),
    )? {
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
        binding_bytes(&policy),
    )?;
    Ok(binding)
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::jobs::investigations) fn rehash_for_test(&mut self) {
        self.entry_id = self.content_id().unwrap();
    }
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
pub(super) fn read<T: serde::de::DeserializeOwned + Serialize>(
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
#[cfg(any(test, feature = "test-support"))]
fn completed_attempts(
    connection: &Connection,
    id: &str,
    attempts: &[AttemptRecord],
) -> Result<Vec<InvestigationInputLedger>, InvestigationStoreError> {
    let task = load_task_base(connection, id)?;
    completed_attempts_for_task(connection, &task, attempts)
}
fn completed_attempts_for_task(
    connection: &Connection,
    task: &StoredTask,
    attempts: &[AttemptRecord],
) -> Result<Vec<InvestigationInputLedger>, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let policy = super::completion::saved_policy(task)?;
    if task.completion_intent.is_some() {
        let durable = super::completion::attempts(connection, task)?;
        if durable.len() < attempts.len()
            || durable
                .iter()
                .take(attempts.len())
                .map(|entry| &entry.record)
                .collect::<Vec<_>>()
                != attempts.iter().collect::<Vec<_>>()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        return attempts
            .iter()
            .map(|attempt| saved_ledger(connection, id, attempt.input_revision))
            .collect();
    }
    if attempts.len() > policy.limits().tool_actions as usize {
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
        validate_step(&read_step, &task.detail.limits)?;
        let previous_revision = attempt
            .input_revision
            .checked_sub(1)
            .ok_or(InvestigationStoreError::Invalid)?;
        let before = saved_ledger(connection, id, previous_revision)?;
        let after = saved_ledger(connection, id, attempt.input_revision)?;
        if before.fingerprint_with_policy(&policy)? != read_step.input_ledger_hash {
            return Err(InvestigationStoreError::Invalid);
        }
        InvestigationTurnBasis {
            input_ledger_hash: after.fingerprint_with_policy(&policy)?,
            previous_ledger: Some(before.clone()),
            completed_tool: Some(expected_tool),
        }
        .validate_with_policy(&after, &policy)?;
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
        if task.detail.summary.last_event_sequence > journal_event_limit(task)? {
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
            if event.sequence != sequence || event.investigation_id != *id {
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
pub(super) fn read_action_hash(read: &ReadIntent) -> Result<String, InvestigationStoreError> {
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
struct ResponseUsage {
    response: InvestigationResponseMeta,
    invocations: u32,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
}

// Invocation order is authoritative: a missing response cannot be skipped to
// turn a later response into the first measured call, or restore unknown usage.
fn response_usage_for_task(
    connection: &Connection,
    task: &StoredTask,
    step_id: &str,
) -> Result<ResponseUsage, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let max_invocations = task.detail.limits.model_invocations;
    if !task
        .detail
        .summary
        .specialist_id
        .uses_next_prompt_admission()
    {
        return response_usage(connection, id, step_id, max_invocations);
    }
    let ordered = super::completion::next_prompt_invocations(connection, task)?;
    let cutoff = ordered
        .iter()
        .find(|(_, step)| step.step_id == step_id)
        .map(|(ordinal, _)| *ordinal)
        .ok_or(InvestigationStoreError::Missing)?;
    let steps = ordered
        .into_iter()
        .filter(|(ordinal, _)| *ordinal <= cutoff)
        .map(|(_, step)| Some(step.step_id))
        .collect::<Vec<_>>();
    response_usage_from_steps(connection, id, step_id, max_invocations, steps)
}
fn response_usage(
    connection: &Connection,
    id: &str,
    step_id: &str,
    max_invocations: u32,
) -> Result<ResponseUsage, InvestigationStoreError> {
    let steps = connection.prepare("SELECT CASE WHEN typeof(step_id)='text' AND length(CAST(step_id AS BLOB))<=128 THEN step_id END FROM investigation_invocations WHERE task_id=?1 AND rowid<=(SELECT rowid FROM investigation_invocations WHERE task_id=?1 AND step_id=?2) ORDER BY rowid LIMIT ?3")?
        .query_map(params![id, step_id, i64::from(max_invocations) + 1], |row| row.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    response_usage_from_steps(connection, id, step_id, max_invocations, steps)
}
fn response_usage_from_steps(
    connection: &Connection,
    id: &str,
    step_id: &str,
    max_invocations: u32,
    steps: Vec<Option<String>>,
) -> Result<ResponseUsage, InvestigationStoreError> {
    if steps.is_empty()
        || steps.len() > max_invocations as usize
        || steps.last().and_then(|step| step.as_deref()) != Some(step_id)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let count = u32::try_from(steps.len()).map_err(|_| InvestigationStoreError::Invalid)?;
    let (mut input_tokens, mut output_tokens) = (None, None);
    let mut cache_read_tokens = None;
    let mut last = None;
    for (ordinal, step) in steps.into_iter().enumerate() {
        let step = step.ok_or(InvestigationStoreError::Invalid)?;
        let response: InvestigationResponseMeta =
            read_body(connection, "investigation_responses", "step_id", id, &step)?
                .ok_or(InvestigationStoreError::Invalid)?;
        if response.step_id != step
            || !hash_valid(&response.response_hash)
            || !hash_valid(&response.action_hash)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(&response, 4096)?;
        input_tokens =
            transitions::add_reported(input_tokens, response.reported_input_tokens, ordinal == 0)?;
        output_tokens = transitions::add_reported(
            output_tokens,
            response.reported_output_tokens,
            ordinal == 0,
        )?;
        cache_read_tokens = transitions::add_reported(
            cache_read_tokens,
            response.reported_cache_read_tokens,
            ordinal == 0,
        )?;
        last = Some(response);
    }
    Ok(ResponseUsage {
        response: last.ok_or(InvestigationStoreError::Invalid)?,
        invocations: count,
        input_tokens,
        output_tokens,
        cache_read_tokens,
    })
}

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
    let task = load_task_base(connection, id)?;
    load_for_task(connection, &task)
}

pub(super) fn load_for_task(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Vec<JournalEntry>, InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_schema(connection)?;
    let id = &task.detail.summary.investigation_id;
    let policy = super::completion::saved_policy(task)?;
    let binding = read::<Binding>(
        connection,
        "staged_completion_intents",
        id,
        None,
        binding_bytes(&policy),
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    binding.validate(task)?;
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
            RefusalFeedback::max_bytes_with_policy(&policy)?,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        if feedback.citation_correction.is_some()
            && !task
                .detail
                .summary
                .specialist_id
                .uses_typed_citation_correction()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let ledger = saved_ledger(connection, id, entry.ledger_revision)?;
        let step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &entry.record.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        validate_step(&step, &task.detail.limits)?;
        let disclosure = step
            .disclosure
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        if task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
        {
            validate_next_prompt_refusal_basis(connection, task, &entry, &step, &ledger)?;
        }
        let basis =
            admission_journal::AuthenticatedRefusalBasis::load(connection, task, sequence, &entry)?;
        if let Some(basis) = &basis {
            basis.check_step(&ledger, &step)?;
        }
        let attempt_ledgers = completed_attempts_for_task(connection, task, &entry.attempts)?;
        let witnesses = entry
            .attempts
            .iter()
            .zip(&attempt_ledgers)
            .map(|(record, ledger)| AttemptEvidence { record, ledger })
            .collect::<Vec<_>>();
        let assessment = if let Some(basis) = &basis {
            basis.binding()?.validate_refusal(
                &entry.record,
                &binding.request_hash,
                &binding.envelope,
                &witnesses,
                entries.last().map(|entry| &entry.record),
            )?;
            entry.record.assessment.clone()
        } else {
            let assessment = derive_assessment_for_binding_with_policy(
                &binding.request_hash,
                &binding.envelope,
                id,
                &step.step_id,
                &ledger,
                disclosure,
                entry.record.outcome,
                &witnesses,
                &policy,
            )?;
            entry.record.validate_stored_basis_with_policy(
                &binding.request_hash,
                &binding.envelope,
                &ledger,
                disclosure,
                entries.last().map(|entry| &entry.record),
                &policy,
            )?;
            assessment
        };
        if entry.record.task_id != *id
            || entry.record.refusal_count != sequence
            || entry.record.assessment != assessment
            || entry.record.feedback != feedback
            || entry.response.step_id != step.step_id
            || entry.response.response_hash != entry.record.response_hash
            || !hash_valid(&entry.response.action_hash)
            || step.input_ledger_hash != ledger.fingerprint_with_policy(&policy)?
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
        let replay = response_usage_for_task(connection, task, &entry.record.step_id)?;
        if replay.response != entry.response
            || replay.invocations != entry.usage.model_invocations
            || replay.input_tokens != entry.usage.reported_input_tokens
            || replay.output_tokens != entry.usage.reported_output_tokens
            || replay.cache_read_tokens != entry.usage.reported_cache_read_tokens
        {
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
                    (
                        &previous.usage.reported_cache_read_tokens,
                        &entry.usage.reported_cache_read_tokens,
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

fn validate_next_prompt_refusal_basis(
    connection: &Connection,
    task: &StoredTask,
    entry: &JournalEntry,
    step: &PendingInvestigationStep,
    ledger: &InvestigationInputLedger,
) -> Result<(), InvestigationStoreError> {
    let disclosure = step
        .disclosure
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let admission = disclosure
        .admission
        .clone()
        .ok_or(InvestigationStoreError::Invalid)?;
    let metadata = super::completion::metadata_at_saved_step(connection, task, step)?;
    let call = admission_contract::SavedCallBasis {
        schema_version: 1,
        task_id: task.detail.summary.investigation_id.clone(),
        request_hash: task.intent_hash.clone(),
        step_id: step.step_id.clone(),
        invocation_ordinal: admission
            .consumed_budgets
            .model_invocations
            .checked_add(1)
            .ok_or(InvestigationStoreError::Invalid)?,
        input_ledger_hash: step.input_ledger_hash.clone(),
        input_revision: ledger.revision,
        completion_metadata_hash: disclosure
            .completion_metadata_hash
            .clone()
            .ok_or(InvestigationStoreError::Invalid)?,
        admission,
    };
    call.validate_core(task, step, ledger, &metadata)?;
    let capacity = admission_journal::derive_at(connection, task, &step.step_id)?;
    let mut expected = call.admission.consumed_budgets.clone();
    expected.model_invocations = call.invocation_ordinal;
    expected.generated_token_reservations = expected
        .generated_token_reservations
        .checked_add(step.generated_tokens)
        .ok_or(InvestigationStoreError::Invalid)?;
    expected.reported_input_tokens = entry.usage.reported_input_tokens;
    expected.reported_output_tokens = entry.usage.reported_output_tokens;
    expected.reported_cache_read_tokens = entry.usage.reported_cache_read_tokens;
    expected.active_milliseconds = entry.response.active_milliseconds;
    if call.admission.capacity_refusal_count != capacity.count
        || call.admission.capacity_feedback != capacity.feedback
        || expected != entry.usage
        || entry
            .attempts
            .iter()
            .map(|record| &record.attempt_id)
            .collect::<Vec<_>>()
            != metadata.completed_attempt_ids.iter().collect::<Vec<_>>()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
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
    let task = load_task_base(tx, &id)?;
    append_for_task(
        tx, namespace, execution, &task, request, input, finish, response, attempts, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_for_task(
    tx: &Transaction<'_>,
    namespace: &ExecutionNamespace,
    execution: &JobExecution,
    task: &StoredTask,
    request: &StagedObligationRequest,
    input: &InvestigationInput,
    finish: &StagedFinish,
    response: InvestigationResponseMeta,
    attempts: &[AttemptRecord],
    basis: Option<&admission_journal::AuthenticatedCompletionBasis>,
) -> Result<RefusalDisposition, InvestigationStoreError> {
    append_for_task_with_merge(
        tx, namespace, execution, task, request, input, finish, response, attempts, basis, None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_for_task_with_merge(
    tx: &Transaction<'_>,
    namespace: &ExecutionNamespace,
    execution: &JobExecution,
    task: &StoredTask,
    request: &StagedObligationRequest,
    input: &InvestigationInput,
    finish: &StagedFinish,
    mut response: InvestigationResponseMeta,
    attempts: &[AttemptRecord],
    basis: Option<&admission_journal::AuthenticatedCompletionBasis>,
    merged: Option<&super::volatile_patch::MergeAuthority>,
) -> Result<RefusalDisposition, InvestigationStoreError> {
    if response.finish_feedback_witness.is_some() {
        return Err(InvestigationStoreError::Invalid);
    }
    let id = task.detail.summary.investigation_id.clone();
    let policy = super::completion::saved_policy(task)?;
    transitions::check_identity(task, execution)?;
    transitions::check_job(tx, execution, task.completion_intent.is_some())?;
    if execution.inner.namespace != *namespace
        || task.phase != Phase::Invocation
        || (task.detail.summary.cancel_requested && task.completion_intent.is_none())
    {
        return Err(InvestigationStoreError::Stale);
    }
    let binding = bind(tx, task, request)?;
    if request.intent_hash_with_policy(&policy)? != binding.request_hash {
        return Err(InvestigationStoreError::Invalid);
    }
    let previous = load_for_task(tx, task)?;
    if previous.len() >= 2 {
        return Err(InvestigationStoreError::Capacity);
    }
    let (step, usage) = transitions::validate_response(tx, task, &response)?;
    let ledger = load_ledger(tx, task)?.ok_or(InvestigationStoreError::Invalid)?;
    if input.ledger() != &ledger {
        return Err(InvestigationStoreError::Invalid);
    }
    let disclosure = step
        .disclosure
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let attempt_ledgers = completed_attempts_for_task(tx, task, attempts)?;
    let evidence = attempts
        .iter()
        .zip(&attempt_ledgers)
        .map(|(record, ledger)| AttemptEvidence { record, ledger })
        .collect::<Vec<_>>();
    let decision = if let Some(basis) = basis {
        basis.check_step(&ledger, &step)?;
        match basis.binding()?.admit_finish(
            input,
            request,
            finish,
            &evidence,
            response.observed_model.clone(),
            "staged-admission".into(),
        )? {
            Err(decision) => decision,
            Ok(_) => return Err(InvestigationStoreError::Invalid),
        }
    } else {
        admit_finish_with_policy(
            request,
            &id,
            &step.step_id,
            input,
            disclosure,
            finish,
            &evidence,
            response.observed_model.clone(),
            "staged-admission".into(),
            &policy,
        )?
    };
    let FinishDecision::Refused {
        assessment,
        feedback,
    } = decision
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    let canonical = if task.completion_intent.is_some() {
        serde_json::to_value(finish)
    } else {
        serde_json::to_value(&finish.action)
    }
    .map_err(|_| InvestigationStoreError::Invalid)?;
    if let Some(merged) = merged {
        merged.validate(task, finish, &response)?;
    } else if task.pending_patch().is_some()
        || response.action_hash != core_prov::content_hash(&encode(&canonical, MAX_ACTION_BYTES)?)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if (feedback.citation_correction.is_some() || feedback.prose_correction.is_some())
        && request.specialist_id != "domain-analyst@20"
        && request.specialist_id != "domain-analyst@21"
        && request.specialist_id != "domain-analyst@22"
        && request.specialist_id != "evidence-auditor@13"
        && request.specialist_id != "evidence-auditor@14"
        && request.specialist_id != "domain-analyst@23"
        && !matches!(
            request.specialist_id.as_str(),
            "domain-analyst@24"
                | "domain-analyst@25"
                | "domain-analyst@26"
                | "domain-analyst@27"
                | "domain-analyst@28"
                | "domain-analyst@29"
        )
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut record = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: id.clone(),
        request_hash: binding.request_hash.clone(),
        obligations_hash: binding.envelope.fingerprint_with_policy(&policy)?,
        step_id: step.step_id.clone(),
        input_ledger_hash: ledger.fingerprint_with_policy(&policy)?,
        disclosure_hash: match basis {
            Some(basis) => basis.binding()?.fingerprint()?,
            None => disclosure.fingerprint_with_policy(&ledger, &policy)?,
        },
        finish_hash: finish.fingerprint()?,
        outcome: finish.outcome,
        response_hash: response.response_hash.clone(),
        previous_refusal: previous.last().map(|entry| entry.record.refusal_id.clone()),
        refusal_count: previous.len() as u32 + 1,
        assessment,
        feedback,
    };
    record.refusal_id = record.content_id()?;
    let sidecar = if task
        .detail
        .summary
        .specialist_id
        .uses_finish_feedback_witness()
    {
        let sidecar = refusal_diagnostics::collect(
            input,
            finish,
            record.refusal_id.clone(),
            record.finish_hash.clone(),
        );
        response.finish_feedback_witness =
            Some(record.diagnostic_witness_hash(&sidecar.content_hash()?)?);
        Some(sidecar)
    } else {
        None
    };

    let disposition = if let Some(basis) = basis {
        if record.finish_hash != finish.fingerprint()? || record.outcome != finish.outcome {
            return Err(InvestigationStoreError::Invalid);
        }
        basis.binding()?.validate_refusal(
            &record,
            &binding.request_hash,
            &binding.envelope,
            &evidence,
            previous.last().map(|entry| &entry.record),
        )?
    } else {
        record.validate_with_policy(
            request,
            &ledger,
            disclosure,
            finish,
            previous.last().map(|entry| &entry.record),
            &policy,
        )?
    };
    let mut entry = JournalEntry {
        entry_id: String::new(),
        record,
        response,
        usage,
        attempts: attempts.to_vec(),
        ledger_revision: ledger.revision,
    };
    entry.entry_id = entry.content_id()?;
    // Include the actual ordinary response body and its row allowance before
    // writing it. Shared accounting will charge that row once after commit.
    if task.completion_intent.is_some() {
        encode(&entry, journal_entry_bytes(&policy))?;
    }
    let response_body = encode(&entry.response, 4096)?;
    let bytes = encode(&entry, MAX_RECORD_BYTES)?.len()
        + entry.record.feedback.bytes_with_policy(&policy)?.len()
        + 1024
        + response_body.len()
        + 64
        + 256;
    let task_bytes = stored_bytes(tx, Some(&id))?
        .checked_add(bytes)
        .ok_or(InvestigationStoreError::Capacity)?;
    let global = stored_bytes(tx, None)?
        .checked_add(bytes)
        .ok_or(InvestigationStoreError::Capacity)?;
    let terminal = task.completion_intent.is_some()
        && (disposition == RefusalDisposition::TerminalRequired
            || task.detail.summary.cancel_requested);
    if task_bytes
        > if terminal {
            TASK_CAPACITY
        } else {
            TASK_CAPACITY - TERMINAL_RESERVE
        }
        || global > STORE_CAPACITY
    {
        return Err(InvestigationStoreError::Capacity);
    }
    if let Some(sidecar) = &sidecar {
        refusal_diagnostics::insert(tx, &id, entry.record.refusal_count, sidecar)?;
    }
    one(tx.execute(
        "INSERT INTO investigation_responses VALUES (?1,?2,?3,?4)",
        params![
            id,
            entry.response.step_id,
            response_body,
            core_prov::content_hash(&response_body)
        ],
    )?)?;
    let replay = response_usage_for_task(tx, task, &entry.record.step_id)?;
    if replay.response != entry.response
        || replay.invocations != entry.usage.model_invocations
        || replay.input_tokens != entry.usage.reported_input_tokens
        || replay.output_tokens != entry.usage.reported_output_tokens
        || replay.cache_read_tokens != entry.usage.reported_cache_read_tokens
    {
        return Err(InvestigationStoreError::Invalid);
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
        RefusalFeedback::max_bytes_with_policy(&policy)?,
    )?;
    Ok(disposition)
}
pub(super) fn journal_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name GLOB 'staged_completion_*')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    validate_schema(connection)?;
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

#[cfg(any(test, feature = "test-support"))]
#[path = "staged_tests.rs"]
mod tests;

/// The response must be independently loaded, not copied from the refusal row.
pub(super) fn validate_finish_feedback_witness(
    connection: &Connection,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    record: &RefusalRecord,
) -> Result<(), InvestigationStoreError> {
    if matches!(
        record.feedback.prose_correction,
        Some(agents::investigation::staged_finish::ProseCorrection::Combined(_))
    ) && !task
        .detail
        .summary
        .specialist_id
        .uses_combined_finish_feedback()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if task
        .detail
        .summary
        .specialist_id
        .uses_finish_feedback_witness()
    {
        let witness = response
            .finish_feedback_witness
            .as_deref()
            .ok_or(InvestigationStoreError::Missing)?;
        if let Some(sidecar) =
            refusal_diagnostics::load(connection, &record.task_id, record.refusal_count)?
        {
            if sidecar.refusal_hash != record.refusal_id
                || sidecar.finish_hash != record.finish_hash
                || response.response_hash != record.response_hash
                || response.step_id != record.step_id
                || record.diagnostic_witness_hash(&sidecar.content_hash()?)? != witness
            {
                return Err(InvestigationStoreError::Invalid);
            }
        } else {
            // Only an original historical digest can validate without a sidecar.
            // Deleting a new sidecar cannot downgrade its domain-separated hash.
            record.validate_feedback_witness(
                witness,
                &response.response_hash,
                &response.step_id,
            )?;
        }
    } else if response.finish_feedback_witness.is_some()
        || record.feedback.unmet.iter().any(|entry| {
            entry.reason == agents::investigation::staged_finish::RefusalReason::ExcludedByCapacity
        })
        || matches!(
            record.feedback.prose_correction,
            Some(agents::investigation::staged_finish::ProseCorrection::ParaphraseReplayAt(_))
        )
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
