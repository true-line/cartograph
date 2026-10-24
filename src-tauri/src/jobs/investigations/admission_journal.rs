//! Immutable @10 capacity refusals. The caller owns one writer transaction,
//! execution/lease checks, response/event/phase publication and central capacity
//! admission. This module never commits, advances input or creates a read attempt.
use super::admission_contract::{SavedCallBasis, SavedCallCore};
use super::*;
use agents::investigation::staged_refusal::CompletionCallMode;
use rusqlite::Transaction;
use serde_json::Value;
use std::collections::BTreeMap;

const TABLE: &str = "investigation_admission_refusals";
const OWNED: &str = "name GLOB 'investigation_admission_*' OR (tbl_name GLOB 'investigation_admission_*' AND name NOT GLOB 'sqlite_*')";
const MAX_ENTRY_BYTES: usize = 64 * 1024;
const ROW_ALLOWANCE: usize = 256;

pub(super) fn schema() -> Vec<String> {
    let mut sql = vec!["CREATE TABLE investigation_admission_refusals (task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 64), step_id TEXT NOT NULL, invocation_ordinal INTEGER NOT NULL CHECK(invocation_ordinal BETWEEN 1 AND 64), body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence), UNIQUE(task_id,step_id), UNIQUE(task_id,invocation_ordinal)) STRICT".into()];
    for verb in ["UPDATE", "DELETE"] {
        sql.push(format!("CREATE TRIGGER investigation_admission_refusals_no_{} BEFORE {verb} ON investigation_admission_refusals BEGIN SELECT RAISE(ABORT, 'immutable admission refusal'); END",verb.to_lowercase()));
    }
    sql
}
fn require_transaction(connection: &Connection) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
pub(super) fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
    require_transaction(tx)?;
    if namespace_count(tx)? == 0 {
        for sql in schema() {
            tx.execute_batch(&sql)?;
        }
    }
    preflight(tx).map(|_| ())
}
/// Canonical optional namespace for the parent schema allowlist. Legacy
/// absence needs no transaction and performs no initialization or migration.
type SchemaObject = (String, String, String);
pub(super) fn validate_optional_schema(
    connection: &Connection,
) -> Result<Option<Vec<SchemaObject>>, InvestigationStoreError> {
    if !preflight(connection)? {
        return Ok(None);
    }
    let sql = schema();
    Ok(Some(vec![
        (TABLE.into(), "table".into(), sql[0].clone()),
        (
            format!("{TABLE}_no_update"),
            "trigger".into(),
            sql[1].clone(),
        ),
        (
            format!("{TABLE}_no_delete"),
            "trigger".into(),
            sql[2].clone(),
        ),
    ]))
}

fn namespace_count(connection: &Connection) -> Result<i64, InvestigationStoreError> {
    connection
        .query_row(
            &format!("SELECT count(*) FROM sqlite_schema WHERE {OWNED}"),
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}
/// Optional namespace charge only. Absent legacy namespaces remain absent and
/// charge zero. Present schema/rows are checked before decoding any entry.
pub(super) fn stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    if namespace_count(connection)? == 0 {
        return Ok(0);
    }
    preflight(connection)?;
    let bytes:i64=connection.query_row("SELECT coalesce(sum(length(body)+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+256),0) FROM investigation_admission_refusals WHERE (?1 IS NULL OR task_id=?1)",[id],|row|row.get(0))?;
    usize::try_from(bytes).map_err(|_| InvestigationStoreError::Capacity)
}
fn preflight(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    if namespace_count(connection)? == 0 {
        return Ok(false);
    }
    require_transaction(connection)?;
    // Bound schema text before owning it, including unexpected objects attached
    // to the owned table. sqlite_autoindex entries have no user SQL.
    let invalid:i64=connection.query_row(&format!("SELECT count(*) FROM sqlite_schema WHERE ({OWNED}) AND (typeof(sql)!='text' OR length(CAST(sql AS BLOB))>4096)"),[],|row|row.get(0))?;
    let count = namespace_count(connection)?;
    if invalid != 0 || count != schema().len() as i64 {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut actual = connection
        .prepare(&format!("SELECT sql FROM sqlite_schema WHERE {OWNED}"))?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = schema();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(InvestigationStoreError::Invalid);
    }
    let maximum = INVESTIGATION_V2_POLICY.limits().model_invocations as usize;
    let (count,invalid,bytes):(i64,i64,i64)=connection.query_row("SELECT count(*),coalesce(sum(NOT(typeof(task_id)='text' AND length(CAST(task_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(step_id)='text' AND length(CAST(step_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(sequence)='integer' AND sequence BETWEEN 1 AND ?1 AND typeof(invocation_ordinal)='integer' AND invocation_ordinal BETWEEN 1 AND ?1 AND typeof(body)='blob' AND length(body) BETWEEN 1 AND ?2 AND typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64)),0),coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+256),0) FROM investigation_admission_refusals",params![maximum as i64,MAX_ENTRY_BYTES as i64],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    if invalid != 0
        || usize::try_from(count)
            .ok()
            .is_none_or(|n| n > MAX_TASKS * maximum)
        || usize::try_from(bytes)
            .ok()
            .is_none_or(|n| n > STORE_CAPACITY)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let bad_task:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM investigation_admission_refusals GROUP BY task_id HAVING count(*)>?1 OR sum(length(body)+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+256)>?2)",params![maximum as i64,TASK_CAPACITY as i64],|row|row.get(0))?;
    if bad_task {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub schema_version: u32,
    pub entry_id: String,
    pub sequence: u32,
    pub count: u32,
    pub call: SavedCallBasis,
    pub invocation_hash: String,
    pub response: InvestigationResponseMeta,
    /// Original decoded proposal, including a replay_query's original identity,
    /// never a differently resolved action. Its canonical bytes bind action_hash.
    pub proposal: Value,
    pub reason: CapacityRefusalReason,
    pub previous_entry_id: Option<String>,
}
impl Entry {
    fn content_id(&self) -> Result<String, InvestigationStoreError> {
        encode(self, MAX_ENTRY_BYTES)?;
        let mut value = self.clone();
        value.entry_id.clear();
        let bytes = encode(&value, MAX_ENTRY_BYTES)?;
        let mut framed = b"cartograph:investigation-admission-refusal:v1\0".to_vec();
        framed.extend_from_slice(&bytes);
        Ok(core_prov::content_hash(&framed))
    }
    pub(super) fn byte_charge(&self) -> Result<usize, InvestigationStoreError> {
        encode(self, MAX_ENTRY_BYTES)?
            .len()
            .checked_add(64 + ROW_ALLOWANCE)
            .and_then(|bytes| bytes.checked_add(self.call.task_id.len()))
            .and_then(|bytes| bytes.checked_add(self.call.step_id.len()))
            .ok_or(InvestigationStoreError::Capacity)
    }
    fn feedback(&self) -> AdmissionCapacityFeedback {
        AdmissionCapacityFeedback {
            schema_version: 1,
            action_hash: self.response.action_hash.clone(),
            reason: self.reason,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CapacityState {
    pub count: u32,
    pub feedback: Option<AdmissionCapacityFeedback>,
}
struct CallWitness {
    call: SavedCallBasis,
    core: SavedCallCore,
    action_admitted: bool,
}
struct Publication {
    ordinal: u32,
}
struct Timeline {
    calls: BTreeMap<u32, CallWitness>,
    publications: Vec<Publication>,
}

/// All ordering is through explicit within-task invocation ordinals from sealed
/// pre-call usage. Event sequence is used only inside its own log to associate
/// real accepted input publication with that invocation. No cross-table rowid,
/// timestamp or unrelated sequence is compared as an ordering authority.
fn timeline(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Timeline, InvestigationStoreError> {
    require_transaction(connection)?;
    admission_contract::validate_pairing(task)?;
    let policy = &INVESTIGATION_V2_POLICY;
    let id = &task.detail.summary.investigation_id;
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM investigation_events WHERE task_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let count = u64::try_from(count).map_err(|_| InvestigationStoreError::Invalid)?;
    if count != task.detail.summary.last_event_sequence || count > journal_event_limit(task)? {
        return Err(InvestigationStoreError::Invalid);
    }
    let invocations: u32 = connection.query_row(
        "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
        [id],
        |row| row.get(0),
    )?;
    if invocations > policy.limits().model_invocations
        || invocations != task.detail.usage.model_invocations
    {
        return Err(InvestigationStoreError::Invalid);
    }
    // Witness bodies must fit their own limits before generic read_body decodes.
    for (table, cap) in [
        ("investigation_events", 16 * 1024),
        ("investigation_inputs", MAX_MANIFEST_BYTES),
        ("investigation_invocations", MAX_INVOCATION_BYTES),
        ("investigation_responses", 4096),
    ] {
        let bad:bool=connection.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE task_id=?1 AND (typeof(body)!='blob' OR length(body)>?2 OR typeof(body_hash)!='text' OR length(CAST(body_hash AS BLOB))!=64))"),params![id,cap as i64],|row|row.get(0))?;
        if bad {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    // Local read-only replay epoch: all named invocation/attempt/refusal bodies
    // and the retained archive are validated once, then reused at exact cutoffs.
    let history = completion::SavedMetadataHistory::load(connection, task)?;
    let mut result = Timeline {
        calls: BTreeMap::new(),
        publications: vec![],
    };
    let mut current: Option<InvestigationInputLedger> = None;
    let mut active: Option<u32> = None;
    let mut accepted: Option<(u32, InvestigationTool, bool)> = None;
    let mut last_revision = 0;
    for sequence in 1..=count {
        let event: InvestigationEvent = read_body(
            connection,
            "investigation_events",
            "sequence",
            id,
            &signed(sequence)?,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        if event.sequence != sequence
            || event.investigation_id != *id
            || event.revision > task.detail.summary.revision
            || event.revision < last_revision
        {
            return Err(InvestigationStoreError::Invalid);
        }
        last_revision = event.revision;
        match event.kind {
            InvestigationEventKind::ContextPrepared => {
                if current.is_some() || event.tool.is_some() || !result.calls.is_empty() {
                    return Err(InvestigationStoreError::Invalid);
                }
                let ledger = ledger_at(connection, id, 0)?;
                InvestigationTurnBasis {
                    input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
                    previous_ledger: None,
                    completed_tool: None,
                }
                .validate_with_policy(&ledger, policy)?;
                current = Some(ledger);
                result.publications.push(Publication { ordinal: 0 });
            }
            InvestigationEventKind::ModelStarted => {
                if accepted.is_some() {
                    return Err(InvestigationStoreError::Invalid);
                }
                if let Some(previous_ordinal) = active {
                    let previous_call = &result.calls[&previous_ordinal].call;
                    let response: InvestigationResponseMeta = read_body(
                        connection,
                        "investigation_responses",
                        "step_id",
                        id,
                        &previous_call.step_id,
                    )?
                    .ok_or(InvestigationStoreError::Invalid)?;
                    validate_response_shape(&response, previous_call)?;
                }
                let step_id = event
                    .step_id
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?;
                let step = history.step(step_id)?;
                let ledger = current.as_ref().ok_or(InvestigationStoreError::Invalid)?;
                let disclosure = step
                    .disclosure
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?;
                let admission = disclosure
                    .admission
                    .clone()
                    .ok_or(InvestigationStoreError::Invalid)?;
                let ordinal = admission
                    .consumed_budgets
                    .model_invocations
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Invalid)?;
                if ordinal != result.calls.len() as u32 + 1 || step.step_id != *step_id {
                    return Err(InvestigationStoreError::Invalid);
                }
                let call = SavedCallBasis {
                    schema_version: 1,
                    task_id: id.clone(),
                    request_hash: task.intent_hash.clone(),
                    step_id: step_id.clone(),
                    invocation_ordinal: ordinal,
                    input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
                    input_revision: ledger.revision,
                    completion_metadata_hash: disclosure
                        .completion_metadata_hash
                        .clone()
                        .ok_or(InvestigationStoreError::Invalid)?,
                    admission,
                };
                let core = history.validate_timeline_call(&call, ledger)?;
                if core.step != *step || core.ledger != *ledger || core.ordinal != ordinal {
                    return Err(InvestigationStoreError::Invalid);
                }
                result.calls.insert(
                    ordinal,
                    CallWitness {
                        call,
                        core,
                        action_admitted: false,
                    },
                );
                active = Some(ordinal);
            }
            InvestigationEventKind::ActionAdmitted => {
                let ordinal = active.ok_or(InvestigationStoreError::Invalid)?;
                let witness = result
                    .calls
                    .get_mut(&ordinal)
                    .ok_or(InvestigationStoreError::Invalid)?;
                let call = &witness.call;
                if event.step_id.as_deref() != Some(call.step_id.as_str())
                    || accepted.is_some()
                    || witness.action_admitted
                    || call.mode() != CompletionCallMode::Choice
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                let response: InvestigationResponseMeta = read_body(
                    connection,
                    "investigation_responses",
                    "step_id",
                    id,
                    &call.step_id,
                )?
                .ok_or(InvestigationStoreError::Invalid)?;
                validate_response_shape(&response, call)?;
                witness.action_admitted = true;
                accepted = Some((
                    ordinal,
                    event.tool.ok_or(InvestigationStoreError::Invalid)?,
                    false,
                ));
            }
            InvestigationEventKind::ToolStarted => {
                let (ordinal, tool, started) =
                    accepted.as_mut().ok_or(InvestigationStoreError::Invalid)?;
                if *started || event.tool != Some(*tool) || Some(*ordinal) != active {
                    return Err(InvestigationStoreError::Invalid);
                }
                *started = true;
            }
            InvestigationEventKind::ToolCompleted => {
                let (ordinal, tool, started) =
                    accepted.take().ok_or(InvestigationStoreError::Invalid)?;
                if !started || event.tool != Some(tool) || active != Some(ordinal) {
                    return Err(InvestigationStoreError::Invalid);
                }
                let previous = current.take().ok_or(InvestigationStoreError::Invalid)?;
                let revision = previous
                    .revision
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Invalid)?;
                let ledger = ledger_at(connection, id, revision)?;
                validate_publication_response(
                    connection,
                    task,
                    &result.calls[&ordinal],
                    tool,
                    &previous,
                    &ledger,
                )?;
                InvestigationTurnBasis {
                    input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
                    previous_ledger: Some(previous),
                    completed_tool: Some(tool),
                }
                .validate_with_policy(&ledger, policy)?;
                current = Some(ledger);
                result.publications.push(Publication { ordinal });
            }
            InvestigationEventKind::ReadUnavailable => {
                let (ordinal, tool, started) =
                    accepted.take().ok_or(InvestigationStoreError::Invalid)?;
                if !started
                    || event.tool != Some(tool)
                    || event.step_id.as_deref()
                        != Some(result.calls[&ordinal].call.step_id.as_str())
                {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
            _ => {}
        }
    }
    let current = current.ok_or(InvestigationStoreError::Invalid)?;
    let input_count: i64 = connection.query_row(
        "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
        [id],
        |row| row.get(0),
    )?;
    if result.calls.len() != invocations as usize
        || u64::try_from(input_count).ok() != current.revision.checked_add(1)
        || task.ledger_revision != Some(current.revision)
        || task.ledger_hash.as_deref() != Some(current.fingerprint_with_policy(policy)?.as_str())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(result)
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

/// An input row plus a matching tool event is insufficient: the accepted
/// publication must bind the original saved model proposal at this ordinal.
fn validate_publication_response(
    connection: &Connection,
    task: &StoredTask,
    witness: &CallWitness,
    tool: InvestigationTool,
    before: &InvestigationInputLedger,
    after: &InvestigationInputLedger,
) -> Result<(), InvestigationStoreError> {
    let call = &witness.call;
    let response: InvestigationResponseMeta = read_body(
        connection,
        "investigation_responses",
        "step_id",
        &task.detail.summary.investigation_id,
        &call.step_id,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    validate_response_shape(&response, call)?;
    let expected = match tool {
        InvestigationTool::ReadEvidence => {
            let citation = after
                .citations
                .get(before.citations.len())
                .ok_or(InvestigationStoreError::Invalid)?;
            staged::read_action_hash(&ReadIntent::Source {
                fact: citation.fact.clone(),
                role: citation.role.ok_or(InvestigationStoreError::Invalid)?,
                index: citation.index.ok_or(InvestigationStoreError::Invalid)?,
            })?
        }
        InvestigationTool::ReadCoverageInventory => {
            let read = &after
                .coverage_reads
                .get(before.coverage_reads.len())
                .ok_or(InvestigationStoreError::Invalid)?
                .request;
            staged::read_action_hash(&ReadIntent::Inventory {
                context_id: read.context_id.clone(),
                repo: read.repo.clone(),
                inventory_hash: read.inventory_hash.clone(),
                max_bytes: read.max_bytes,
            })?
        }
        InvestigationTool::QueryContext => {
            let query = &after
                .queries
                .get(before.queries.len())
                .ok_or(InvestigationStoreError::Invalid)?
                .query;
            let context_id = &before
                .coverage
                .as_ref()
                .ok_or(InvestigationStoreError::Invalid)?
                .context_id;
            let direct =
                serde_json::json!({"type":"query_context","context_id":context_id,"query":query});
            let hash = core_prov::content_hash(&encode(&direct, MAX_ACTION_BYTES)?);
            if response.action_hash == hash {
                return Ok(());
            }
            for (index, prior) in before.queries.iter().enumerate() {
                if &prior.query != query {
                    continue;
                }
                let reference = QueryHistoryReference::from_ledger(
                    &call.request_hash,
                    before,
                    index,
                    &INVESTIGATION_V2_POLICY,
                )?;
                let replay = serde_json::json!({"type":"replay_query","context_id":context_id,
                    "query_index":reference.query_index,"query_id":reference.query_id});
                if response.action_hash
                    == core_prov::content_hash(&encode(&replay, MAX_ACTION_BYTES)?)
                {
                    return Ok(());
                }
            }
            return Err(InvestigationStoreError::Invalid);
        }
    };
    if response.action_hash != expected {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

fn read_entries(
    connection: &Connection,
    task: &StoredTask,
    line: &Timeline,
) -> Result<Vec<Entry>, InvestigationStoreError> {
    let present = preflight(connection)?;
    let id = &task.detail.summary.investigation_id;
    let policy = &INVESTIGATION_V2_POLICY;
    let mut entries: Vec<Entry> = vec![];
    if present {
        let rows=connection.prepare("SELECT sequence,step_id,invocation_ordinal FROM investigation_admission_refusals WHERE task_id=?1 ORDER BY sequence LIMIT ?2")?
            .query_map(params![id,policy.limits().model_invocations+1],|row|Ok((row.get::<_,u32>(0)?,row.get::<_,String>(1)?,row.get::<_,u32>(2)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        if rows.len() > policy.limits().model_invocations as usize {
            return Err(InvestigationStoreError::Invalid);
        }
        for (index, (sequence, step_id, ordinal)) in rows.into_iter().enumerate() {
            let entry: Entry = read_body(connection, TABLE, "sequence", id, &sequence)?
                .ok_or(InvestigationStoreError::Invalid)?;
            let witness = line
                .calls
                .get(&ordinal)
                .ok_or(InvestigationStoreError::Invalid)?;
            let before = project(&entries, line, Some(ordinal));
            if sequence != index as u32 + 1
                || entry.sequence != sequence
                || entry.count != sequence
                || entry.schema_version != 1
                || entry.call != witness.call
                || entry.call.step_id != step_id
                || entry.call.task_id != *id
                || entry.call.request_hash != task.intent_hash
                || entry.previous_entry_id != entries.last().map(|entry| entry.entry_id.clone())
                || entries
                    .last()
                    .is_some_and(|entry| entry.call.invocation_ordinal >= ordinal)
                || witness.call.admission.capacity_refusal_count != before.count
                || witness.call.admission.capacity_feedback != before.feedback
                || witness.call.mode() != CompletionCallMode::Choice
                || witness.action_admitted
                || line
                    .publications
                    .iter()
                    .any(|publication| publication.ordinal == ordinal)
                || entry.invocation_hash
                    != core_prov::content_hash(&encode(&witness.core.step, MAX_INVOCATION_BYTES)?)
                || !hash_valid(&entry.entry_id)
                || entry.entry_id != entry.content_id()?
            {
                return Err(InvestigationStoreError::Invalid);
            }
            validate_proposal(
                &entry.proposal,
                &entry.response,
                &witness.core.ledger,
                &entry.call,
            )?;
            let response: InvestigationResponseMeta = read_body(
                connection,
                "investigation_responses",
                "step_id",
                id,
                &step_id,
            )?
            .ok_or(InvestigationStoreError::Invalid)?;
            if response != entry.response {
                return Err(InvestigationStoreError::Invalid);
            }
            validate_response_shape(&response, &entry.call)?;
            entry.byte_charge()?;
            entries.push(entry);
        }
    }
    // Validate EVERY saved call against its historical prefix, not only refusal
    // rows: deleting a refusal cannot silently restore count/visible feedback.
    for (ordinal, witness) in &line.calls {
        let before = project(&entries, line, Some(*ordinal));
        if witness.call.admission.capacity_refusal_count != before.count
            || witness.call.admission.capacity_feedback != before.feedback
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(entries)
}
fn project(entries: &[Entry], line: &Timeline, cutoff: Option<u32>) -> CapacityState {
    let before = |ordinal: u32| cutoff.is_none_or(|cutoff| ordinal < cutoff);
    let prior = entries
        .iter()
        .filter(|entry| before(entry.call.invocation_ordinal))
        .collect::<Vec<_>>();
    let latest = prior.last();
    let cleared = latest.is_some_and(|entry| {
        line.publications.iter().any(|publication| {
            publication.ordinal > entry.call.invocation_ordinal && before(publication.ordinal)
        })
    });
    CapacityState {
        count: prior.len() as u32,
        feedback: latest.filter(|_| !cleared).map(|entry| entry.feedback()),
    }
}
fn validate_response_shape(
    response: &InvestigationResponseMeta,
    call: &SavedCallBasis,
) -> Result<(), InvestigationStoreError> {
    if response.step_id != call.step_id
        || !hash_valid(&response.response_hash)
        || !hash_valid(&response.action_hash)
        || response.active_milliseconds < call.admission.consumed_budgets.active_milliseconds
        || response.observed_model.as_ref().is_some_and(|model| {
            model.is_empty()
                || model.len() > 256
                || redacted_history_text(model, 256).ok().as_ref() != Some(model)
        })
    {
        return Err(InvestigationStoreError::Invalid);
    }
    encode(response, 4096)?;
    Ok(())
}
fn validate_proposal(
    proposal: &Value,
    response: &InvestigationResponseMeta,
    ledger: &InvestigationInputLedger,
    call: &SavedCallBasis,
) -> Result<(), InvestigationStoreError> {
    let raw = encode(proposal, MAX_ACTION_BYTES)?;
    let context = ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?
        .context_id
        .as_str();
    let action = ProgressiveInvestigationAction::decode_resolved(
        std::str::from_utf8(&raw).map_err(|_| InvestigationStoreError::Invalid)?,
        context,
        &call.request_hash,
        ledger,
        &INVESTIGATION_V2_POLICY,
    )?;
    if !matches!(action, CompletionInvestigationAction::Read(_))
        || call.mode() != CompletionCallMode::Choice
        || response.action_hash != core_prov::content_hash(&raw)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

/// Strict historical cutoff: a missing invocation is invalid, never "latest".
/// Its own refusal is excluded; later accepted publications cannot clear its
/// earlier feedback. All saved bodies/chain/call stamps are validated first.
pub(super) fn derive_at(
    connection: &Connection,
    task: &StoredTask,
    step_id: &str,
) -> Result<CapacityState, InvestigationStoreError> {
    let line = timeline(connection, task)?;
    let cutoff = line
        .calls
        .iter()
        .find(|(_, witness)| witness.call.step_id == step_id)
        .map(|(ordinal, _)| *ordinal)
        .ok_or(InvestigationStoreError::Missing)?;
    let entries = read_entries(connection, task, &line)?;
    Ok(project(&entries, &line, Some(cutoff)))
}
pub(super) fn derive_current(
    connection: &Connection,
    task: &StoredTask,
) -> Result<CapacityState, InvestigationStoreError> {
    let line = timeline(connection, task)?;
    let entries = read_entries(connection, task, &line)?;
    Ok(project(&entries, &line, None))
}

/// Append after the caller persisted the SAME ordinary response row inside this
/// transaction. Caller must atomically publish its refusal event/phase/usage and
/// enforce execution ownership plus whole-task/global capacity before commit.
/// No evidence, input revision, attempt, tool debit or validation charge is made.
pub(super) fn append(
    tx: &Transaction<'_>,
    task: &StoredTask,
    call: &SavedCallBasis,
    response: &InvestigationResponseMeta,
    proposal: &Value,
    reason: CapacityRefusalReason,
) -> Result<Entry, InvestigationStoreError> {
    require_transaction(tx)?;
    if !preflight(tx)? {
        return Err(InvestigationStoreError::Missing);
    }
    admission_contract::validate_pairing(task)?;
    if task.phase != Phase::Invocation
        || task
            .pending
            .as_ref()
            .is_none_or(|pending| pending.step_id != call.step_id)
        || task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
            != Some(call)
    {
        return Err(InvestigationStoreError::Stale);
    }
    let line = timeline(tx, task)?;
    let entries = read_entries(tx, task, &line)?;
    let witness = line
        .calls
        .get(&call.invocation_ordinal)
        .ok_or(InvestigationStoreError::Missing)?;
    if witness.call != *call {
        return Err(InvestigationStoreError::Invalid);
    }
    let core = &witness.core;
    let current = project(&entries, &line, None);
    if line.calls.last_key_value().map(|(ordinal, _)| *ordinal) != Some(call.invocation_ordinal)
        || entries
            .last()
            .is_some_and(|entry| entry.call.invocation_ordinal >= call.invocation_ordinal)
        || call.admission.capacity_refusal_count != current.count
        || call.admission.capacity_feedback != current.feedback
        || call.mode() != CompletionCallMode::Choice
        || line
            .calls
            .get(&call.invocation_ordinal)
            .is_none_or(|witness| witness.action_admitted)
    {
        return Err(InvestigationStoreError::Conflict);
    }
    let saved: InvestigationResponseMeta = read_body(
        tx,
        "investigation_responses",
        "step_id",
        &call.task_id,
        &call.step_id,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    if saved != *response {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_response_shape(response, call)?;
    let (pending, _) = transitions::validate_response(tx, task, response)?;
    if pending != core.step {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_proposal(proposal, response, &core.ledger, call)?;
    let sequence = current
        .count
        .checked_add(1)
        .filter(|count| *count <= INVESTIGATION_V2_POLICY.limits().model_invocations)
        .ok_or(InvestigationStoreError::Capacity)?;
    let mut entry = Entry {
        schema_version: 1,
        entry_id: String::new(),
        sequence,
        count: sequence,
        call: call.clone(),
        invocation_hash: core_prov::content_hash(&encode(&core.step, MAX_INVOCATION_BYTES)?),
        response: response.clone(),
        proposal: proposal.clone(),
        reason,
        previous_entry_id: entries.last().map(|entry| entry.entry_id.clone()),
    };
    entry.entry_id = entry.content_id()?;
    let charge = entry.byte_charge()?;
    // Journal-only ceilings complement central accounting; they do not reserve
    // space for the caller's response/task/event mutations.
    if stored_bytes(tx, Some(&call.task_id))?
        .checked_add(charge)
        .is_none_or(|bytes| bytes > TASK_CAPACITY - TERMINAL_RESERVE)
        || stored_bytes(tx, None)?
            .checked_add(charge)
            .is_none_or(|bytes| bytes > STORE_CAPACITY)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    let body = encode(&entry, MAX_ENTRY_BYTES)?;
    one(tx.execute("INSERT INTO investigation_admission_refusals(task_id,sequence,step_id,invocation_ordinal,body,body_hash) VALUES (?1,?2,?3,?4,?5,?6)",params![call.task_id,sequence,call.step_id,call.invocation_ordinal,body,core_prov::content_hash(&body)])?)?;
    Ok(entry)
}

#[cfg(test)]
#[path = "admission_journal_tests.rs"]
pub(super) mod tests;
