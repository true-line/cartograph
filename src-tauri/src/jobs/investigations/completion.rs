//! Real generation-four completion intent and durable read observations.
use super::*;
use rusqlite::Transaction;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompletionIntent {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_profile: Option<String>,
    pub request_hash: String,
    pub obligations: ObligationEnvelope,
    pub obligations_hash: String,
    pub expected_context: String,
    pub expected_graph: Option<String>,
    nonce: String,
    specialist: SpecialistId,
    scope: InvestigationScope,
    provider_mode: InvestigationProviderMode,
    parent: Option<String>,
    conversation: Option<String>,
    question_hash: String,
}
impl CompletionIntent {
    fn new(request: &StagedObligationRequest) -> Result<Self, InvestigationStoreError> {
        let policy = InvestigationPolicy::for_profile(&request.limit_profile)
            .ok_or(InvestigationStoreError::Invalid)?;
        let request = request.normalized_with_policy(policy)?;
        let specialist: SpecialistId =
            serde_json::from_value(serde_json::Value::String(request.specialist_id.clone()))
                .map_err(|_| InvestigationStoreError::Invalid)?;
        Ok(Self {
            schema_version: if specialist.uses_query_history_disclosure() {
                3
            } else if policy.profile() == "investigation-v1" {
                1
            } else {
                2
            },
            limit_profile: (policy.profile() != "investigation-v1")
                .then(|| policy.profile().into()),
            request_hash: request.intent_hash_with_policy(policy)?,
            obligations: request.obligations.clone(),
            obligations_hash: request.obligations.fingerprint_with_policy(policy)?,
            expected_context: request.expected_context_id.clone(),
            expected_graph: request.expected_graph_revision.clone(),
            nonce: request.request_nonce.clone(),
            specialist,
            scope: request.scope.clone(),
            provider_mode: request.provider_mode,
            parent: request.parent_id.clone(),
            conversation: request.conversation_id.clone(),
            question_hash: core_prov::content_hash(
                redacted_history_text(&request.question, MAX_QUESTION_BYTES)?.as_bytes(),
            ),
        })
    }
    pub(super) fn validate(&self, task: &StoredTask) -> Result<(), InvestigationStoreError> {
        let policy = saved_policy(task)?;
        let legacy = policy.profile() == "investigation-v1";
        let summary = &task.detail.summary;
        if self.schema_version
            != if summary.specialist_id.uses_query_history_disclosure() {
                3
            } else if legacy {
                1
            } else {
                2
            }
            || self.limit_profile.as_deref() != if legacy { None } else { Some(policy.profile()) }
            || summary.specialist_id.uses_policy_selection() == legacy
            || !hash_valid(&self.request_hash)
            || self.request_hash != task.intent_hash
            || self.obligations != self.obligations.normalized_with_policy(policy)?
            || self.obligations.fingerprint_with_policy(policy)? != self.obligations_hash
            || self.expected_graph != task.expected_graph_revision
            || self.nonce != task.nonce
            || self.specialist != summary.specialist_id
            || self.scope != summary.scope
            || self.provider_mode != summary.provider_mode
            || self.parent != summary.parent_id
            || self
                .conversation
                .as_ref()
                .is_some_and(|id| id != &summary.conversation_id)
            || self.question_hash != core_prov::content_hash(summary.question.as_bytes())
            || !summary.specialist_id.uses_completion_obligations()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if task.detail.inspection_requirements.is_some()
            || task.detail.inspection_assessment.is_some()
            || summary.inspection_status.is_some()
            || summary.inspection_requirement_count.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if task.ledger_revision.is_some()
            && task
                .detail
                .coverage_context
                .as_ref()
                .is_none_or(|context| context.context_id != self.expected_context)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(detail) = &task.detail.completion {
            self.validate_assessment_basis(&detail.assessment_basis)?;
        }
        Ok(())
    }
    // Numeric policy ceilings are shared by @8 and @9, but their disclosure
    // identities retain the family admitted by the immutable completion intent.
    fn validate_assessment_basis(
        &self,
        basis: &CompletionAssessmentBasis,
    ) -> Result<(), InvestigationStoreError> {
        let disclosure_hash = match basis {
            CompletionAssessmentBasis::NotAssessed => None,
            CompletionAssessmentBasis::FinishingDisclosure {
                disclosure_hash, ..
            } => Some(disclosure_hash),
            CompletionAssessmentBasis::TerminalObservation {
                last_refusal_basis, ..
            } => last_refusal_basis
                .as_ref()
                .map(|basis| &basis.disclosure_hash),
        };
        let prefix = if self.schema_version == 3 {
            "investigation-disclosure-v5:"
        } else {
            "investigation-disclosure-v4:"
        };
        if disclosure_hash.is_some_and(|hash| !hash.strip_prefix(prefix).is_some_and(hash_valid)) {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    pub(super) fn contract(&self) -> InvestigationCompletionContract {
        InvestigationCompletionContract {
            schema_version: 1,
            request_hash: self.request_hash.clone(),
            obligations_hash: self.obligations_hash.clone(),
            obligations: self.obligations.clone(),
        }
    }
}
pub(super) fn initial_detail(intent: &CompletionIntent) -> InvestigationCompletionDetail {
    InvestigationCompletionDetail {
        schema_version: 1,
        request_hash: intent.request_hash.clone(),
        obligations_hash: intent.obligations_hash.clone(),
        obligations: intent.obligations.clone(),
        refusal_count: 0,
        assessment_basis: CompletionAssessmentBasis::NotAssessed,
        assessment: vec![],
    }
}
impl JobStore {
    pub(crate) fn start_completion_investigation(
        &mut self,
        request: &StagedObligationRequest,
        provider: Option<&InvestigationProvider>,
    ) -> Result<InvestigationStart, InvestigationStoreError> {
        let policy = InvestigationPolicy::for_profile(&request.limit_profile)
            .ok_or(InvestigationStoreError::Invalid)?;
        let request = request.normalized_with_policy(policy)?;
        let intent = CompletionIntent::new(&request)?;
        let sources = request.obligations.sources_with_policy(policy)?;
        let projected = StartInvestigationRequest {
            schema_version: request.schema_version,
            request_nonce: request.request_nonce.clone(),
            specialist_id: intent.specialist,
            inspection_requirements: if sources.is_empty() {
                None
            } else {
                Some(normalize_inspection_targets_with_policy(&sources, policy)?)
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
        self.start_investigation_internal(&projected, provider, Some(intent))
    }
    pub(crate) fn existing_investigation_completion(
        &self,
        request: &StagedObligationRequest,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        let policy = InvestigationPolicy::for_profile(&request.limit_profile)
            .ok_or(InvestigationStoreError::Invalid)?;
        let hash = request.intent_hash_with_policy(policy)?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM investigation_tasks WHERE nonce=?1",
                [&request.request_nonce],
                |row| row.get(0),
            )
            .optional()?;
        let detail = if let Some(id) = id {
            let task = load_task(&tx, &id)?;
            if task.schema_version != 2 || task.intent_hash != hash {
                return Err(InvestigationStoreError::Conflict);
            }
            Some(task.detail)
        } else {
            None
        };
        tx.commit()?;
        Ok(detail)
    }
    pub(crate) fn investigation_completion_attempts(
        &self,
        id: &str,
    ) -> Result<Vec<AttemptRecord>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_task(&tx, id)?;
        let rows = attempts(&tx, &task)?;
        tx.commit()?;
        Ok(rows.into_iter().map(|entry| entry.record).collect())
    }
    pub(crate) fn investigation_completion_attempt_ledgers(
        &self,
        id: &str,
    ) -> Result<Vec<InvestigationInputLedger>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let task = load_task(&tx, id)?;
        let rows = attempts(&tx, &task)?;
        let ledgers = rows
            .iter()
            .map(|entry| ledger_at(&tx, id, entry.record.input_revision))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(ledgers)
    }
    pub(crate) fn investigation_completion_metadata(
        &self,
        id: &str,
    ) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let task = load_task(&tx, id)?;
        let metadata = metadata(&tx, &task, None)?;
        tx.commit()?;
        Ok(metadata)
    }
}
const LEGACY_ATTEMPT_TABLE: &str = "CREATE TABLE completion_attempts(task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 32), body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT";
const ATTEMPT_SCHEMA: &[&str] = &[
    "CREATE TABLE completion_attempts(task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 112), body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT",
    "CREATE TRIGGER completion_attempts_no_update BEFORE UPDATE ON completion_attempts BEGIN SELECT RAISE(ABORT,'immutable completion attempt'); END",
    "CREATE TRIGGER completion_attempts_no_delete BEFORE DELETE ON completion_attempts BEGIN SELECT RAISE(ABORT,'immutable completion attempt'); END",
];
pub(super) fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'completion_*'",
        [],
        |row| row.get(0),
    )?;
    if count == 0 {
        for sql in ATTEMPT_SCHEMA {
            tx.execute_batch(sql)?;
        }
    }
    let legacy = validate_schema_version(tx)?;
    if legacy {
        // Migrate only the exact known schema, in the caller's transaction.
        // Copy opaque bodies and hashes byte-for-byte; policy remains authoritative.
        tx.execute_batch("DROP TRIGGER completion_attempts_no_update; DROP TRIGGER completion_attempts_no_delete; ALTER TABLE completion_attempts RENAME TO completion_attempts_legacy")?;
        for sql in ATTEMPT_SCHEMA {
            tx.execute_batch(sql)?;
        }
        tx.execute_batch("INSERT INTO completion_attempts(task_id,sequence,body,body_hash) SELECT task_id,sequence,body,body_hash FROM completion_attempts_legacy; DROP TABLE completion_attempts_legacy")?;
    }
    validate_schema(tx)
}
fn validate_schema(connection: &Connection) -> Result<(), InvestigationStoreError> {
    validate_schema_version(connection).map(|_| ())
}
/// Both historical and current schemas remain readable; only initialization
/// upgrades a historical table, atomically with the new investigation.
fn validate_schema_version(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    let (count,bounded):(i64,bool)=connection.query_row("SELECT count(*),coalesce(min(typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),0) FROM sqlite_schema WHERE name GLOB 'completion_*' OR (tbl_name='completion_attempts' AND name NOT GLOB 'sqlite_*')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count != ATTEMPT_SCHEMA.len() as i64 || !bounded {
        return Err(InvestigationStoreError::Invalid);
    }
    let actual=connection.prepare("SELECT sql FROM sqlite_schema WHERE name GLOB 'completion_*' OR (tbl_name='completion_attempts' AND name NOT GLOB 'sqlite_*')")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    let legacy = actual.iter().any(|sql| sql == LEGACY_ATTEMPT_TABLE);
    if ATTEMPT_SCHEMA.iter().enumerate().any(|(index, sql)| {
        let expected = if legacy && index == 0 {
            LEGACY_ATTEMPT_TABLE
        } else {
            sql
        };
        !actual.iter().any(|actual| actual == expected)
    }) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(legacy)
}
pub(super) fn stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name GLOB 'completion_*')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    validate_schema(connection)?;
    let bytes:i64=connection.query_row("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256),0) FROM completion_attempts WHERE (?1 IS NULL OR task_id=?1)",[id],|r|r.get(0))?;
    usize::try_from(bytes).map_err(|_| InvestigationStoreError::Capacity)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttemptEntry {
    pub record: AttemptRecord,
    response_hash: String,
    action_hash: String,
    event_sequence: u64,
}
fn ledger_at(
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
fn latest_step(connection: &Connection, id: &str) -> Result<String, InvestigationStoreError> {
    connection.query_row("SELECT CASE WHEN typeof(step_id)='text' AND length(CAST(step_id AS BLOB))<=128 THEN step_id END FROM investigation_invocations WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",[id],|row|row.get(0)).map_err(Into::into)
}
fn ordinal(connection: &Connection, id: &str, step: &str) -> Result<i64, InvestigationStoreError> {
    connection
        .query_row(
            "SELECT rowid FROM investigation_invocations WHERE task_id=?1 AND step_id=?2",
            params![id, step],
            |row| row.get(0),
        )
        .map_err(Into::into)
}
pub(super) fn attempts(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Vec<AttemptEntry>, InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let limit = policy.max_completed_attempts();
    let Some(intent) = &task.completion_intent else {
        return Ok(vec![]);
    };
    intent.validate(task)?;
    validate_schema(connection)?;
    let id = &task.detail.summary.investigation_id;
    let sequences = connection
        .prepare(
            "SELECT sequence FROM completion_attempts WHERE task_id=?1 ORDER BY sequence LIMIT ?2",
        )?
        .query_map(params![id, signed(limit as u64 + 1)?], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if sequences.len() > limit {
        return Err(InvestigationStoreError::Capacity);
    }
    let mut entries = Vec::new();
    for (index, sequence) in sequences.into_iter().enumerate() {
        if sequence != index as i64 + 1 {
            return Err(InvestigationStoreError::Invalid);
        }
        let entry: AttemptEntry =
            read_body(connection, "completion_attempts", "sequence", id, &sequence)?
                .ok_or(InvestigationStoreError::Invalid)?;
        let ledger = ledger_at(connection, id, entry.record.input_revision)?;
        entry.record.validate_binding_with_policy(
            &intent.request_hash,
            &intent.obligations,
            id,
            &ledger,
            policy,
        )?;
        let response: InvestigationResponseMeta = read_body(
            connection,
            "investigation_responses",
            "step_id",
            id,
            &entry.record.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let actual_step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &entry.record.step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        validate_step(&actual_step, &task.detail.limits)?;
        if actual_step.step_id != entry.record.step_id || response.step_id != entry.record.step_id {
            return Err(InvestigationStoreError::Invalid);
        }
        if response.response_hash != entry.response_hash
            || response.action_hash != entry.action_hash
            || staged::read_action_hash(&entry.record.read)? != entry.action_hash
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let event: InvestigationEvent = read_body(
            connection,
            "investigation_events",
            "sequence",
            id,
            &signed(entry.event_sequence)?,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        if event.investigation_id != *id
            || event.sequence != entry.event_sequence
            || event.sequence > task.detail.summary.last_event_sequence
        {
            return Err(InvestigationStoreError::Invalid);
        }
        match &entry.record.outcome {
            AttemptOutcome::SourceAcquired { .. } | AttemptOutcome::InventoryAcquired { .. } => {
                let expected_tool =
                    if matches!(entry.record.outcome, AttemptOutcome::SourceAcquired { .. }) {
                        InvestigationTool::ReadEvidence
                    } else {
                        InvestigationTool::ReadCoverageInventory
                    };
                if event.kind != InvestigationEventKind::ToolCompleted
                    || event.tool != Some(expected_tool)
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                let before = ledger_at(
                    connection,
                    id,
                    entry
                        .record
                        .input_revision
                        .checked_sub(1)
                        .ok_or(InvestigationStoreError::Invalid)?,
                )?;
                let step: PendingInvestigationStep = read_body(
                    connection,
                    "investigation_invocations",
                    "step_id",
                    id,
                    &entry.record.step_id,
                )?
                .ok_or(InvestigationStoreError::Invalid)?;
                if step.input_ledger_hash != before.fingerprint_with_policy(policy)? {
                    return Err(InvestigationStoreError::Invalid);
                }
                InvestigationTurnBasis {
                    input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
                    previous_ledger: Some(before.clone()),
                    completed_tool: Some(expected_tool),
                }
                .validate_with_policy(&ledger, policy)?;
                match &entry.record.outcome {
                    AttemptOutcome::SourceAcquired { citation_id } => {
                        if ledger
                            .citations
                            .get(before.citations.len())
                            .is_none_or(|citation| &citation.citation_id != citation_id)
                        {
                            return Err(InvestigationStoreError::Invalid);
                        }
                    }
                    AttemptOutcome::InventoryAcquired { read_index } => {
                        if *read_index != before.coverage_reads.len() {
                            return Err(InvestigationStoreError::Invalid);
                        }
                    }
                    _ => return Err(InvestigationStoreError::Invalid),
                }
            }
            AttemptOutcome::Unavailable { reason } => {
                if actual_step.input_ledger_hash != ledger.fingerprint_with_policy(policy)? {
                    return Err(InvestigationStoreError::Invalid);
                }
                if let ReadIntent::Inventory {
                    context_id,
                    repo,
                    inventory_hash,
                    max_bytes,
                } = &entry.record.read
                {
                    let archive = super::coverage::load(connection, id)?
                        .ok_or(InvestigationStoreError::Invalid)?;
                    if !matches!(
                        archive.read(&InvestigationCoverageRead {
                            context_id: context_id.clone(),
                            repo: repo.clone(),
                            inventory_hash: inventory_hash.clone(),
                            max_bytes: *max_bytes
                        }),
                        Err(InvestigationError::LimitExceeded)
                    ) {
                        return Err(InvestigationStoreError::Invalid);
                    }
                }
                if event.kind != InvestigationEventKind::ReadUnavailable
                    || event.step_id.as_deref() != Some(entry.record.step_id.as_str())
                    || event.summary != unavailable_message(reason)
                {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
            AttemptOutcome::FirewallRefused { .. } => return Err(InvestigationStoreError::Invalid),
        }
        entries.push(entry);
    }
    Ok(entries)
}
fn unavailable_message(reason: &UnavailableReason) -> &'static str {
    match reason {
        UnavailableReason::PinnedTargetAbsent => "Pinned completion target was unavailable.",
        UnavailableReason::RangeOutOfBounds => "Completion source range was unavailable.",
        UnavailableReason::InventoryExceedsMaxBytes => {
            "Retained completion inventory exceeds the requested response budget."
        }
    }
}
#[path = "prospective_attempt.rs"]
mod prospective_attempt;
pub(super) use prospective_attempt::derive_attempt_records;

fn append_attempt(
    connection: &Connection,
    task: &StoredTask,
    read: &ReadIntent,
    outcome: AttemptOutcome,
    revision: u64,
) -> Result<(), InvestigationStoreError> {
    saved_policy(task)?;
    // Reserved decode-only outcome: no current production firewall emits a
    // trustworthy per-read refusal witness, and redaction is still acquisition.
    if matches!(outcome, AttemptOutcome::FirewallRefused { .. }) {
        return Err(InvestigationStoreError::Invalid);
    }
    let intent = validated_intent(task)?;
    let id = &task.detail.summary.investigation_id;
    let step = latest_step(connection, id)?;
    let response: InvestigationResponseMeta =
        read_body(connection, "investigation_responses", "step_id", id, &step)?
            .ok_or(InvestigationStoreError::Invalid)?;
    if response.action_hash != staged::read_action_hash(read)? {
        return Err(InvestigationStoreError::Invalid);
    }
    let count = attempts(connection, task)?.len();
    // Incidental reads with no matching obligation historically require no
    // acquisition ledger lookup. Keep that behavior in the shared derivation.
    let ledger = intent
        .obligations
        .obligations
        .iter()
        .any(|obligation| read.matches(obligation))
        .then(|| ledger_at(connection, id, revision))
        .transpose()?;
    let records = derive_attempt_records(
        task,
        &step,
        &response,
        read,
        &outcome,
        ledger.as_ref(),
        count,
    )?;
    for (offset, record) in records.into_iter().enumerate() {
        let entry = AttemptEntry {
            record,
            response_hash: response.response_hash.clone(),
            action_hash: response.action_hash.clone(),
            event_sequence: task.detail.summary.last_event_sequence,
        };
        insert_body(
            connection,
            "completion_attempts",
            id,
            &signed((count + offset + 1) as u64)?,
            &entry,
            MAX_RECORD_BYTES,
        )?;
    }
    Ok(())
}
pub(super) fn record_acquisition(
    connection: &Connection,
    task: &StoredTask,
    ledger: &InvestigationInputLedger,
    tool: Option<InvestigationTool>,
) -> Result<(), InvestigationStoreError> {
    if task.completion_intent.is_none() {
        return Ok(());
    }
    let before = ledger_at(
        connection,
        &task.detail.summary.investigation_id,
        ledger
            .revision
            .checked_sub(1)
            .ok_or(InvestigationStoreError::Invalid)?,
    )?;
    match tool {
        Some(InvestigationTool::ReadEvidence) => {
            let citation = ledger
                .citations
                .get(before.citations.len())
                .ok_or(InvestigationStoreError::Invalid)?;
            let role = citation.role.ok_or(InvestigationStoreError::Invalid)?;
            let index = citation.index.ok_or(InvestigationStoreError::Invalid)?;
            append_attempt(
                connection,
                task,
                &ReadIntent::Source {
                    fact: citation.fact.clone(),
                    role,
                    index,
                },
                AttemptOutcome::SourceAcquired {
                    citation_id: citation.citation_id.clone(),
                },
                ledger.revision,
            )
        }
        Some(InvestigationTool::ReadCoverageInventory) => {
            let index = before.coverage_reads.len();
            let read = ledger
                .coverage_reads
                .get(index)
                .ok_or(InvestigationStoreError::Invalid)?;
            append_attempt(
                connection,
                task,
                &ReadIntent::Inventory {
                    context_id: read.request.context_id.clone(),
                    repo: read.request.repo.clone(),
                    inventory_hash: read.request.inventory_hash.clone(),
                    max_bytes: read.request.max_bytes,
                },
                AttemptOutcome::InventoryAcquired { read_index: index },
                ledger.revision,
            )
        }
        _ => Ok(()),
    }
}
pub(super) fn unavailable(
    connection: &Connection,
    task: &mut StoredTask,
    read: ReadIntent,
    reason: UnavailableReason,
) -> Result<(), InvestigationStoreError> {
    if task.phase != Phase::Tool || task.completion_intent.is_none() {
        return Err(InvestigationStoreError::Stale);
    }
    let tool = match read {
        ReadIntent::Source { .. } => InvestigationTool::ReadEvidence,
        ReadIntent::Inventory { .. } => InvestigationTool::ReadCoverageInventory,
    };
    if task.action_tool != Some(tool) {
        return Err(InvestigationStoreError::Invalid);
    }
    if let ReadIntent::Inventory {
        context_id,
        repo,
        inventory_hash,
        max_bytes,
    } = &read
    {
        if reason != UnavailableReason::InventoryExceedsMaxBytes {
            return Err(InvestigationStoreError::Invalid);
        }
        let archive = super::coverage::load(connection, &task.detail.summary.investigation_id)?
            .ok_or(InvestigationStoreError::Invalid)?;
        let request = InvestigationCoverageRead {
            context_id: context_id.clone(),
            repo: repo.clone(),
            inventory_hash: inventory_hash.clone(),
            max_bytes: *max_bytes,
        };
        if !matches!(
            archive.read(&request),
            Err(InvestigationError::LimitExceeded)
        ) {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    let step = latest_step(connection, &task.detail.summary.investigation_id)?;
    task.phase = Phase::Ready;
    task.action_tool = None;
    bump(connection, task)?;
    event(
        connection,
        task,
        InvestigationEventKind::ReadUnavailable,
        Some(&step),
        Some(tool),
        unavailable_message(&reason),
    )?;
    append_attempt(
        connection,
        task,
        &read,
        AttemptOutcome::Unavailable { reason },
        task.ledger_revision
            .ok_or(InvestigationStoreError::Invalid)?,
    )?;
    refresh_observation(connection, task)
}
fn raw_refusals(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Vec<staged::JournalEntry>, InvestigationStoreError> {
    saved_policy(task)?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='staged_completion_refusals')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(vec![]);
    }
    let id = &task.detail.summary.investigation_id;
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM staged_completion_refusals WHERE task_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if !(0..=2).contains(&count) {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut entries = vec![];
    for sequence in 1..=count {
        let entry: staged::JournalEntry = staged::read(
            connection,
            "staged_completion_refusals",
            id,
            Some(sequence as u32),
            MAX_RECORD_BYTES,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        entries.push(entry);
    }
    Ok(entries)
}
#[path = "prospective_metadata.rs"]
mod prospective_metadata;
pub(super) use prospective_metadata::derive_protected_metadata;

fn metadata(
    connection: &Connection,
    task: &StoredTask,
    step: Option<&str>,
) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
    saved_policy(task)?;
    validated_intent(task)?;
    let id = &task.detail.summary.investigation_id;
    let cutoff = step
        .map(|step| {
            connection
                .query_row(
                    "SELECT rowid FROM investigation_invocations WHERE task_id=?1 AND step_id=?2",
                    params![id, step],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let mut records = vec![];
    for entry in attempts(connection, task)? {
        if match cutoff {
            None => true,
            Some(cutoff) => ordinal(connection, id, &entry.record.step_id)? < cutoff,
        } {
            records.push(entry.record.attempt_id);
        }
    }
    let mut refusals = vec![];
    for entry in raw_refusals(connection, task)? {
        if match cutoff {
            None => true,
            Some(cutoff) => ordinal(connection, id, &entry.record.step_id)? < cutoff,
        } {
            refusals.push(entry);
        }
    }
    derive_protected_metadata(
        task,
        records,
        &refusals
            .iter()
            .map(|entry| &entry.record)
            .collect::<Vec<_>>(),
    )
}
pub(super) fn validate_step_metadata(
    connection: &Connection,
    task: &StoredTask,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let intent = validated_intent(task)?;
    let disclosure = step
        .disclosure
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let metadata = metadata(connection, task, Some(&step.step_id))?;
    let progressive = task
        .detail
        .summary
        .specialist_id
        .uses_query_history_disclosure();
    if disclosure.schema_version != if progressive { 5 } else { 4 }
        || disclosure
            .query_history
            .as_ref()
            .map(|binding| binding.request_hash.as_str())
            != progressive.then_some(intent.request_hash.as_str())
        || disclosure.completion_contract_hash.as_deref()
            != Some(intent.contract().fingerprint_with_policy(policy)?.as_str())
        || disclosure.completion_metadata_hash.as_deref()
            != Some(metadata.fingerprint_with_policy(policy)?.as_str())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
fn validated_intent(task: &StoredTask) -> Result<&CompletionIntent, InvestigationStoreError> {
    let intent = task
        .completion_intent
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    intent.validate(task)?;
    Ok(intent)
}
pub(super) fn saved_policy(
    task: &StoredTask,
) -> Result<&'static InvestigationPolicy, InvestigationStoreError> {
    InvestigationPolicy::for_limits(&task.detail.limits).ok_or(InvestigationStoreError::Invalid)
}
fn observed_assessment(
    intent: &CompletionIntent,
    entries: &[AttemptEntry],
) -> Vec<ObligationObservation> {
    intent
        .obligations
        .obligations
        .iter()
        .map(|obligation| {
            let matching = entries
                .iter()
                .filter(|entry| entry.record.obligation_id == obligation.key().1)
                .collect::<Vec<_>>();
            let acquired = matching.iter().any(|entry| {
                matches!(
                    entry.record.outcome,
                    AttemptOutcome::SourceAcquired { .. }
                        | AttemptOutcome::InventoryAcquired { .. }
                )
            });
            let last = matching.last().map(|entry| match entry.record.outcome {
                AttemptOutcome::SourceAcquired { .. }
                | AttemptOutcome::InventoryAcquired { .. } => CompletionAttemptOutcome::Acquired,
                AttemptOutcome::Unavailable { .. } => CompletionAttemptOutcome::Unavailable,
                AttemptOutcome::FirewallRefused { .. } => CompletionAttemptOutcome::FirewallRefused,
            });
            ObligationObservation {
                obligation_id: obligation.key().1.into(),
                kind: obligation.key().0.into(),
                attempted: !matching.is_empty(),
                acquired,
                exactly_supplied: false,
                unmet: Some(if matching.is_empty() {
                    RefusalReason::NotAttempted
                } else if acquired {
                    RefusalReason::AcquiredNotSupplied
                } else {
                    RefusalReason::Unavailable
                }),
                last_attempt_outcome: last,
            }
        })
        .collect()
}
fn observation_detail(
    connection: &Connection,
    task: &StoredTask,
) -> Result<InvestigationCompletionDetail, InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let intent = validated_intent(task)?;
    let entries = attempts(connection, task)?;
    let refusals = raw_refusals(connection, task)?;
    let last = refusals.last();
    if task.phase != Phase::Terminal && last.is_none() {
        return Ok(initial_detail(intent));
    }
    let basis = if task.phase != Phase::Terminal {
        let last = last.ok_or(InvestigationStoreError::Invalid)?;
        if last.attempts.len() > policy.max_completed_attempts() {
            return Err(InvestigationStoreError::Capacity);
        }
        let ids = last
            .attempts
            .iter()
            .map(|record| record.attempt_id.as_str())
            .collect::<Vec<_>>();
        CompletionAssessmentBasis::FinishingDisclosure {
            step_id: last.record.step_id.clone(),
            input_ledger_hash: last.record.input_ledger_hash.clone(),
            disclosure_hash: last.record.disclosure_hash.clone(),
            finishing_attempts_hash: completed_attempt_ids_hash_with_policy(&ids, policy)?,
        }
    } else {
        CompletionAssessmentBasis::TerminalObservation {
            finishing_attempts_hash: completed_attempt_ids_hash_with_policy(
                &entries
                    .iter()
                    .map(|entry| entry.record.attempt_id.as_str())
                    .collect::<Vec<_>>(),
                policy,
            )?,
            last_refusal_id: last.map(|entry| entry.record.refusal_id.clone()),
            last_refusal_basis: last.map(|entry| CompletionRefusalBasis {
                step_id: entry.record.step_id.clone(),
                input_ledger_hash: entry.record.input_ledger_hash.clone(),
                disclosure_hash: entry.record.disclosure_hash.clone(),
                finish_hash: entry.record.finish_hash.clone(),
            }),
        }
    };
    let same_attempts = last.is_some_and(|last| {
        last.attempts
            .iter()
            .map(|record| &record.attempt_id)
            .eq(entries.iter().map(|entry| &entry.record.attempt_id))
    });
    let assessment = if task.phase != Phase::Terminal || same_attempts {
        last.ok_or(InvestigationStoreError::Invalid)?
            .record
            .assessment
            .clone()
    } else {
        observed_assessment(intent, &entries)
    };
    Ok(InvestigationCompletionDetail {
        schema_version: 1,
        request_hash: intent.request_hash.clone(),
        obligations_hash: intent.obligations_hash.clone(),
        obligations: intent.obligations.clone(),
        refusal_count: refusals.len() as u32,
        assessment_basis: basis,
        assessment,
    })
}
pub(super) fn refresh_observation(
    connection: &Connection,
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    if task.completion_intent.is_none() {
        return Ok(());
    }
    let detail = observation_detail(connection, task)?;
    task.detail.summary.obligation_status = Some(detail.validate_with_policy(policy)?);
    task.detail.completion = Some(detail);
    Ok(())
}
pub(super) fn validate_result(
    connection: &Connection,
    task: &StoredTask,
    result: &InvestigationResult,
    ledger: &InvestigationInputLedger,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let Some(intent) = &task.completion_intent else {
        if result.completion.is_some() {
            return Err(InvestigationStoreError::Invalid);
        }
        return Ok(());
    };
    intent.validate(task)?;
    let records = attempts(connection, task)?;
    let ledgers = records
        .iter()
        .map(|entry| {
            ledger_at(
                connection,
                &task.detail.summary.investigation_id,
                entry.record.input_revision,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let evidence = records
        .iter()
        .zip(&ledgers)
        .map(|(entry, ledger)| AttemptEvidence {
            record: &entry.record,
            ledger,
        })
        .collect::<Vec<_>>();
    result.validate_completion_for_binding_with_policy(
        &intent.request_hash,
        &intent.obligations,
        &intent.expected_context,
        intent.expected_graph.as_deref(),
        &task.detail.summary.investigation_id,
        &step.step_id,
        ledger,
        step.disclosure
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?,
        &evidence,
        policy,
    )?;
    Ok(())
}
pub(super) fn accepted(
    connection: &Connection,
    task: &mut StoredTask,
    result: &InvestigationResult,
    ledger: &InvestigationInputLedger,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    if task.completion_intent.is_none() {
        return Ok(());
    }
    validate_result(connection, task, result, ledger, step)?;
    let intent = validated_intent(task)?;
    let completion = result
        .completion
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let refusals = raw_refusals(connection, task)?;
    let detail = InvestigationCompletionDetail {
        schema_version: 1,
        request_hash: intent.request_hash.clone(),
        obligations_hash: intent.obligations_hash.clone(),
        obligations: intent.obligations.clone(),
        refusal_count: refusals.len() as u32,
        assessment_basis: CompletionAssessmentBasis::FinishingDisclosure {
            step_id: step.step_id.clone(),
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            disclosure_hash: step
                .disclosure
                .as_ref()
                .ok_or(InvestigationStoreError::Invalid)?
                .fingerprint_with_policy(ledger, policy)?,
            finishing_attempts_hash: completion.finishing_attempts_hash.clone(),
        },
        assessment: completion.assessment.clone(),
    };
    task.detail.summary.completion_outcome = Some(match completion.outcome {
        CompletionOutcome::Supported => InvestigationCompletionOutcome::Supported,
        CompletionOutcome::InsufficientEvidence => {
            InvestigationCompletionOutcome::InsufficientEvidence
        }
    });
    task.detail.summary.obligation_status = Some(detail.validate_with_policy(policy)?);
    task.detail.completion = Some(detail);
    Ok(())
}
// Mutable task metadata cannot erase an already admitted completion contract.
// Legacy staged adapters are permitted, but their invocation disclosures are V6.
fn validate_legacy_evidence(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let id = &task.detail.summary.investigation_id;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='completion_attempts')",
        [],
        |row| row.get(0),
    )?;
    if exists {
        validate_schema(connection)?;
        let has_attempt: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM completion_attempts WHERE task_id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if has_attempt {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    let steps = connection
        .prepare("SELECT CASE WHEN typeof(step_id)='text' AND length(CAST(step_id AS BLOB))<=128 THEN step_id END FROM investigation_invocations WHERE task_id=?1 LIMIT ?2")?
        .query_map(params![id, policy.limits().model_invocations + 1], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if steps.len() > policy.limits().model_invocations as usize {
        return Err(InvestigationStoreError::Invalid);
    }
    for step_id in steps {
        let step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        if step
            .disclosure
            .as_ref()
            .is_some_and(|disclosure| matches!(disclosure.schema_version, 4 | 5))
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}
pub(super) fn validate_saved(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let Some(intent) = &task.completion_intent else {
        return validate_legacy_evidence(connection, task);
    };
    intent.validate(task)?;
    let _attempts = attempts(connection, task)?;
    let raw = raw_refusals(connection, task)?;
    let refusals = if raw.is_empty() {
        vec![]
    } else {
        staged::load(connection, &task.detail.summary.investigation_id)?
    };
    let detail = task
        .detail
        .completion
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if detail.refusal_count != refusals.len() as u32 {
        return Err(InvestigationStoreError::Invalid);
    }
    if task.detail.summary.has_result {
        let id = &task.detail.summary.investigation_id;
        let result: InvestigationResult =
            read_body(connection, "investigation_results", "task_id", id, id)?
                .ok_or(InvestigationStoreError::Invalid)?;
        let completion = result
            .completion
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let step_id = result
            .invocation_step_id
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        let step: PendingInvestigationStep = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            step_id,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
        let expected = InvestigationCompletionDetail {
            schema_version: 1,
            request_hash: intent.request_hash.clone(),
            obligations_hash: intent.obligations_hash.clone(),
            obligations: intent.obligations.clone(),
            refusal_count: refusals.len() as u32,
            assessment_basis: CompletionAssessmentBasis::FinishingDisclosure {
                step_id: step_id.clone(),
                input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
                disclosure_hash: step
                    .disclosure
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?
                    .fingerprint_with_policy(&ledger, policy)?,
                finishing_attempts_hash: completion.finishing_attempts_hash.clone(),
            },
            assessment: completion.assessment.clone(),
        };
        if detail != &expected
            || task.detail.summary.completion_outcome
                != Some(match completion.outcome {
                    CompletionOutcome::Supported => InvestigationCompletionOutcome::Supported,
                    CompletionOutcome::InsufficientEvidence => {
                        InvestigationCompletionOutcome::InsufficientEvidence
                    }
                })
        {
            return Err(InvestigationStoreError::Invalid);
        }
    } else {
        let host_cap = refusals.len() == 2
            && task.phase == Phase::Terminal
            && !task.detail.summary.cancel_requested;
        if task.detail.summary.completion_outcome
            != if host_cap {
                Some(InvestigationCompletionOutcome::ObligationsUnmet)
            } else {
                None
            }
            || (host_cap && task.detail.summary.status != InvestigationStatus::Failed)
            || (refusals.len() == 2 && task.phase != Phase::Terminal)
            || &observation_detail(connection, task)? != detail
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}
fn invocation_reservation_bytes(
    task: &StoredTask,
    policy: &InvestigationPolicy,
) -> Result<usize, InvestigationStoreError> {
    if policy.profile() == INVESTIGATION_V1_POLICY.profile() {
        return Ok(192 * 1024);
    }
    // Conservatively charge a whole replacement task rather than its growth.
    // The row, response and event caps already bound their actual serializers.
    // Reservation comes from nonterminal headroom, before any provider call;
    // terminal publication therefore does not rely on TERMINAL_RESERVE alone.
    let ordinary_row = 64 + 256;
    let task_row = MAX_RECORD_BYTES + 64 + 3 * 128 + 1024;
    [
        super::staged::journal_entry_bytes(policy) + ordinary_row,
        task_row,
        super::staged::binding_reservation_bytes(task, policy)?,
        RefusalFeedback::max_bytes_with_policy(policy)? + ordinary_row,
        4096 + ordinary_row,
        3 * (16 * 1024 + ordinary_row),
    ]
    .into_iter()
    .try_fold(0usize, |total, bytes| {
        total
            .checked_add(bytes)
            .ok_or(InvestigationStoreError::Capacity)
    })
}
pub(super) fn reserve_invocation(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let policy = saved_policy(task)?;
    if task.completion_intent.is_none() {
        return Ok(());
    }
    validated_intent(task)?;
    let count = raw_refusals(connection, task)?.len();
    if count >= 2 {
        return Err(InvestigationStoreError::Capacity);
    }
    check_invocation_capacity(connection, task, policy)
}
fn check_invocation_capacity(
    connection: &Connection,
    task: &StoredTask,
    policy: &InvestigationPolicy,
) -> Result<(), InvestigationStoreError> {
    let required = invocation_reservation_bytes(task, policy)?;
    if remaining_task_capacity(connection, &task.detail.summary.investigation_id)? < required
        || remaining_store_capacity(connection)? < required
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(super) fn refused(
    tx: &Transaction<'_>,
    namespace: &ExecutionNamespace,
    execution: &JobExecution,
    task: &mut StoredTask,
    request: &StagedObligationRequest,
    input: &InvestigationInput,
    finish: &StagedFinish,
    response: InvestigationResponseMeta,
) -> Result<(), InvestigationStoreError> {
    if task.phase != Phase::Invocation || task.completion_intent.is_none() {
        return Err(InvestigationStoreError::Stale);
    }
    let records = attempts(tx, task)?;
    let attempts = records
        .into_iter()
        .map(|entry| entry.record)
        .collect::<Vec<_>>();
    let disposition = staged::append(
        tx,
        namespace,
        execution,
        request,
        input,
        finish,
        response.clone(),
        &attempts,
    )?;
    let entries = raw_refusals(tx, task)?;
    let entry = entries.last().ok_or(InvestigationStoreError::Invalid)?;
    task.detail.usage = entry.usage.clone();
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    task.phase = Phase::Ready;
    task.detail.summary.status = InvestigationStatus::Running;
    task.detail.summary.completion_outcome = None;
    bump(tx, task)?;
    event(
        tx,
        task,
        InvestigationEventKind::ModelCompleted,
        Some(&response.step_id),
        None,
        "A complete refused finish response was durably admitted.",
    )?;
    event(
        tx,
        task,
        InvestigationEventKind::FinishRefused,
        Some(&response.step_id),
        None,
        "Completion obligations remain unmet; fixed feedback was retained.",
    )?;
    if task.detail.summary.cancel_requested || disposition == RefusalDisposition::TerminalRequired {
        task.phase = Phase::Terminal;
        task.detail.summary.status = if task.detail.summary.cancel_requested {
            InvestigationStatus::Cancelled
        } else {
            InvestigationStatus::Failed
        };
        task.detail.summary.completion_outcome = if task.detail.summary.cancel_requested {
            None
        } else {
            Some(InvestigationCompletionOutcome::ObligationsUnmet)
        };
        task.detail.error = Some(
            if task.detail.summary.cancel_requested {
                "Cancellation won over a late unmet finish."
            } else {
                "Two well-formed finishes left completion obligations unmet."
            }
            .into(),
        );
        bump(tx, task)?;
        event(
            tx,
            task,
            if task.detail.summary.cancel_requested {
                InvestigationEventKind::Cancelled
            } else {
                InvestigationEventKind::Failed
            },
            Some(&response.step_id),
            None,
            "No further completion invocation is authorized.",
        )?;
        transitions::sync_terminal_job(tx, task)?;
    }
    refresh_observation(tx, task)
}
#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
