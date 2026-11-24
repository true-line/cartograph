use super::*;
use std::io::Write;

fn legacy_schema() -> Vec<(String, String, String)> {
    let mut objects = vec![
        ("investigation_meta", "CREATE TABLE investigation_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), namespace TEXT NOT NULL) STRICT"),
        ("investigation_conversations", "CREATE TABLE investigation_conversations (id TEXT PRIMARY KEY NOT NULL, created_at TEXT NOT NULL) STRICT"),
        ("investigation_tasks", "CREATE TABLE investigation_tasks (ordinal INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL UNIQUE, conversation_id TEXT NOT NULL, job_id INTEGER NOT NULL UNIQUE, revision INTEGER NOT NULL CHECK(revision>=0), live INTEGER NOT NULL CHECK(live IN (0,1)), record BLOB NOT NULL, record_hash TEXT NOT NULL) STRICT"),
        ("investigation_events", "CREATE TABLE investigation_events (task_id TEXT NOT NULL, sequence INTEGER NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT"),
        ("investigation_inputs", "CREATE TABLE investigation_inputs (task_id TEXT NOT NULL, revision INTEGER NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,revision)) STRICT"),
        ("investigation_invocations", "CREATE TABLE investigation_invocations (task_id TEXT NOT NULL, step_id TEXT NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,step_id)) STRICT"),
        ("investigation_responses", "CREATE TABLE investigation_responses (task_id TEXT NOT NULL, step_id TEXT NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,step_id)) STRICT"),
        ("investigation_results", "CREATE TABLE investigation_results (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL) STRICT"),
        ("investigation_refs", "CREATE TABLE investigation_refs (task_id TEXT NOT NULL, input_revision INTEGER NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, receipt_id TEXT NOT NULL, PRIMARY KEY(task_id,source_id,repo_key,receipt_id)) STRICT"),
    ].into_iter().map(|(name, sql)| (name.to_owned(), "table".to_owned(), sql.to_owned())).collect::<Vec<_>>();
    objects.push(("investigation_refs_source".into(), "index".into(), "CREATE INDEX investigation_refs_source ON investigation_refs(source_id,task_id,repo_key,receipt_id)".into()));
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_invocations",
        "investigation_responses",
        "investigation_results",
        "investigation_refs",
    ] {
        for verb in ["UPDATE", "DELETE"] {
            let name = format!("{table}_no_{}", verb.to_lowercase());
            objects.push((name.clone(), "trigger".into(), format!("CREATE TRIGGER {name} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable investigation record'); END")));
        }
    }
    objects
}

fn version_one_schema() -> Vec<(String, String, String)> {
    let mut objects = legacy_schema();
    objects.extend(super::coverage::schema());
    objects
}

const META_V2: &str = "CREATE TABLE investigation_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2), namespace TEXT NOT NULL) STRICT";

fn version_two_schema() -> Vec<(String, String, String)> {
    let mut objects = version_one_schema();
    objects[0].2 = META_V2.into();
    objects.extend(super::finding_reviews::schema());
    objects
}

const META_V3: &str = "CREATE TABLE investigation_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=3), namespace TEXT NOT NULL) STRICT";
const META_V4: &str = "CREATE TABLE investigation_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=4), namespace TEXT NOT NULL) STRICT";
const MCP_CLIENTS_V3: &str = "CREATE TABLE investigation_mcp_clients (id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32), created_at TEXT NOT NULL) STRICT";
const TASKS_V3: &str = "CREATE TABLE investigation_tasks (ordinal INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL, request_namespace TEXT NOT NULL DEFAULT 'app', conversation_id TEXT NOT NULL, job_id INTEGER NOT NULL UNIQUE, revision INTEGER NOT NULL CHECK(revision>=0), live INTEGER NOT NULL CHECK(live IN (0,1)), record BLOB NOT NULL, record_hash TEXT NOT NULL, UNIQUE(request_namespace,nonce)) STRICT";

fn version_three_schema() -> Vec<(String, String, String)> {
    let mut objects = version_two_schema();
    objects[0].2 = META_V3.into();
    objects
        .iter_mut()
        .find(|entry| entry.0 == "investigation_tasks")
        .unwrap()
        .2 = TASKS_V3.into();
    objects.push((
        "investigation_mcp_clients".into(),
        "table".into(),
        MCP_CLIENTS_V3.into(),
    ));
    objects
}

fn schema() -> Vec<(String, String, String)> {
    let mut objects = version_three_schema();
    objects[0].2 = META_V4.into();
    objects.extend(super::chat::schema());
    objects
}

const OWNED: &str =
    "name GLOB 'investigation_*' OR (tbl_name GLOB 'investigation_*' AND name NOT GLOB 'sqlite_*')";

pub(super) fn initialize(
    connection: &mut Connection,
    namespace: &ExecutionNamespace,
) -> Result<(), InvestigationStoreError> {
    initialize_with_hook(connection, namespace, |_| Ok(()))
}

fn initialize_with_hook(
    connection: &mut Connection,
    namespace: &ExecutionNamespace,
    mut hook: impl FnMut(u8) -> Result<(), InvestigationStoreError>,
) -> Result<(), InvestigationStoreError> {
    connection.pragma_update(None, "synchronous", "FULL")?;
    let foreign_keys: bool = connection.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    let legacy_alter: bool =
        connection.pragma_query_value(None, "legacy_alter_table", |r| r.get(0))?;
    // SQLite's documented table-rebuild mode: preserve child references to the
    // final table name, then validate referential integrity before commit.
    connection.pragma_update(None, "foreign_keys", false)?;
    if let Err(error) = connection.pragma_update(None, "legacy_alter_table", true) {
        connection.pragma_update(None, "foreign_keys", foreign_keys)?;
        return Err(error.into());
    }
    let result = initialize_transaction(connection, namespace, &mut hook);
    let restore_legacy = connection.pragma_update(None, "legacy_alter_table", legacy_alter);
    let restore_foreign = connection.pragma_update(None, "foreign_keys", foreign_keys);
    restore_legacy?;
    restore_foreign?;
    result
}

fn initialize_transaction(
    connection: &mut Connection,
    namespace: &ExecutionNamespace,
    hook: &mut impl FnMut(u8) -> Result<(), InvestigationStoreError>,
) -> Result<(), InvestigationStoreError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        &format!("SELECT count(*) FROM sqlite_schema WHERE {OWNED}"),
        [],
        |r| r.get(0),
    )?;
    if count == 0 {
        for (_, _, sql) in legacy_schema() {
            tx.execute_batch(&sql)?;
        }
        one(tx.execute(
            "INSERT INTO investigation_meta VALUES (1,1,?1)",
            [&namespace.value],
        )?)?;
    }
    let count: i64 = tx.query_row(
        &format!("SELECT count(*) FROM sqlite_schema WHERE {OWNED}"),
        [],
        |r| r.get(0),
    )?;
    if usize::try_from(count).ok() == Some(legacy_schema().len()) {
        execution::validate(&tx, namespace)?;
        validate_layout(&tx, &legacy_schema())?;
        validate_legacy_metadata(&tx, namespace)?;
        for (_, _, sql) in super::coverage::schema() {
            tx.execute_batch(&sql)?;
        }
        one(tx.execute("INSERT INTO investigation_coverage_meta VALUES (1,1)", [])?)?;
    }
    let mut version: i64 = tx.query_row(
        "SELECT version FROM investigation_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if version == 1 {
        execution::validate(&tx, namespace)?;
        validate_version_schema(&tx, 1)?;
        validate_metadata(&tx, namespace, 1)?;
        tx.execute_batch("DROP TABLE investigation_meta")?;
        tx.execute_batch(META_V2)?;
        one(tx.execute(
            "INSERT INTO investigation_meta VALUES (1,2,?1)",
            [&namespace.value],
        )?)?;
        hook(1)?;
        for (_, _, sql) in super::finding_reviews::schema() {
            tx.execute_batch(&sql)?;
        }
        hook(2)?;
    } else if version != 2 && version != 3 && version != 4 {
        return Err(InvestigationStoreError::Invalid);
    }
    if version <= 2 {
        validate_version_schema(&tx, 2)?;
        validate_metadata(&tx, namespace, 2)?;
        // Preserve immutable record bytes/hashes and all existing scalar identity.
        // No caller-supplied namespace is introduced by migration.
        tx.execute_batch("ALTER TABLE investigation_tasks RENAME TO investigation_tasks_v2")?;
        tx.execute_batch(TASKS_V3)?;
        hook(4)?;
        tx.execute_batch("INSERT INTO investigation_tasks(ordinal,id,nonce,request_namespace,conversation_id,job_id,revision,live,record,record_hash) SELECT ordinal,id,nonce,'app',conversation_id,job_id,revision,live,record,record_hash FROM investigation_tasks_v2; UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT seq FROM sqlite_sequence WHERE name='investigation_tasks_v2'),0)) WHERE name='investigation_tasks'; DROP TABLE investigation_tasks_v2; DROP TABLE investigation_meta")?;
        tx.execute_batch(META_V3)?;
        tx.execute_batch(MCP_CLIENTS_V3)?;
        one(tx.execute(
            "INSERT INTO investigation_meta VALUES (1,3,?1)",
            [&namespace.value],
        )?)?;
        hook(5)?;
        version = 3;
    }
    if version == 3 {
        validate_version_schema(&tx, 3)?;
        validate_metadata(&tx, namespace, 3)?;
        tx.execute_batch("DROP TABLE investigation_meta")?;
        tx.execute_batch(META_V4)?;
        one(tx.execute(
            "INSERT INTO investigation_meta VALUES (1,4,?1)",
            [&namespace.value],
        )?)?;
        for (_, _, sql) in super::chat::schema() {
            tx.execute_batch(&sql)?;
        }
        hook(6)?;
    }
    validate(&tx, namespace)?;
    hook(3)?;
    if tx
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn validate(
    connection: &Connection,
    namespace: &ExecutionNamespace,
) -> Result<(), InvestigationStoreError> {
    execution::validate(connection, namespace)?;
    validate_archive_schema(connection)?;
    validate_metadata(connection, namespace, 4)
}

/// Logical owners in version-3-and-later schema validation, including both optional
/// namespaces. Trusted DDL construction is bounded independently of stored SQL.
#[cfg(unix)]
pub(super) fn archive_schema_workspace() -> Option<usize> {
    use context_hub::logical::{slots, sum};
    let base = schema();
    let reads = super::read_reservations::schema();
    let admission = super::admission_journal::schema();
    if base
        .iter()
        .chain(&reads)
        .any(|(name, kind, sql)| name.len() > 128 || kind.len() > 16 || sql.len() > 4096)
        || admission.iter().any(|sql| sql.len() > 4096)
    {
        return None;
    }
    let rows = base
        .len()
        .checked_add(reads.len())?
        .checked_add(admission.len())?;
    let layout = sum(&[
        rows.checked_mul(128 + 16 + 4096)?,
        slots::<(String, String, String)>(rows)?,
        slots::<Vec<(String, String, String)>>(1)?,
    ])?;
    // Parent expected layout remains live during optional namespace checks.
    // Charge parent + optional expected + actual (or returned clone), using
    // the full inventory for each. Sorting needs at most one element array.
    sum(&[
        layout.checked_mul(3)?,
        slots::<(String, String, String)>(rows)?,
        4096, // one fixed SQL format buffer; no untrusted interpolation
        slots::<String>(1)?,
    ])
}

/// Validate the complete extended SQL layout before archive helpers use it.
/// Namespace/owner checks remain the caller's existing JobStore admission.
pub(super) fn validate_archive_schema(
    connection: &Connection,
) -> Result<(), InvestigationStoreError> {
    validate_version_schema(connection, 4)
}

fn validate_version_schema(
    connection: &Connection,
    version: i64,
) -> Result<(), InvestigationStoreError> {
    let mut expected = if version == 1 {
        version_one_schema()
    } else if version == 2 {
        version_two_schema()
    } else if version == 3 {
        version_three_schema()
    } else {
        schema()
    };
    // Read reservations are optional: old stores do not acquire empty tables or
    // new charges on open. Only a complete, exact versioned additive namespace
    // is recognized; partial/foreign schemas fail before any body is read.
    if super::read_reservations::validate_optional_schema(connection)? {
        expected.extend(super::read_reservations::schema());
    }
    if let Some(admission) = super::admission_journal::validate_optional_schema(connection)? {
        expected.extend(admission);
    }
    validate_layout(connection, &expected)?;
    super::coverage::validate_version(connection)
}

fn validate_layout(
    connection: &Connection,
    expected: &[(String, String, String)],
) -> Result<(), InvestigationStoreError> {
    let (count, bounded): (i64, bool) = connection.query_row(
        &format!("SELECT count(*), coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=128 AND typeof(type)='text' AND length(CAST(type AS BLOB))<=16 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),0) FROM sqlite_schema WHERE {OWNED}"),
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if usize::try_from(count).ok() != Some(expected.len()) || !bounded {
        return Err(InvestigationStoreError::Invalid);
    }
    let actual = connection
        .prepare(&format!(
            "SELECT name,type,sql FROM sqlite_schema WHERE {OWNED}"
        ))?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if expected.iter().any(|item| !actual.contains(item)) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

fn validate_legacy_metadata(
    connection: &Connection,
    namespace: &ExecutionNamespace,
) -> Result<(), InvestigationStoreError> {
    validate_metadata(connection, namespace, 1)
}

fn validate_metadata(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    version: i64,
) -> Result<(), InvestigationStoreError> {
    let rows: i64 =
        connection.query_row("SELECT count(*) FROM investigation_meta", [], |r| r.get(0))?;
    let valid: bool = connection.query_row("SELECT coalesce(min(singleton=1 AND typeof(version)='integer' AND version=?2 AND typeof(namespace)='text' AND length(CAST(namespace AS BLOB))=32 AND namespace=?1),0) FROM investigation_meta", params![&namespace.value, version], |r| r.get(0))?;
    if rows != 1 || !valid {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("bounded investigation record"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(
    value: &impl Serialize,
    maximum: usize,
) -> Result<Vec<u8>, InvestigationStoreError> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| InvestigationStoreError::Capacity)?;
    Ok(writer.bytes)
}

pub(super) fn decode<T: serde::de::DeserializeOwned + Serialize>(
    bytes: &[u8],
    hash: &str,
) -> Result<T, InvestigationStoreError> {
    decode_bounded(bytes, hash, MAX_RECORD_BYTES)
}

pub(super) fn decode_bounded<T: serde::de::DeserializeOwned + Serialize>(
    bytes: &[u8],
    hash: &str,
    maximum: usize,
) -> Result<T, InvestigationStoreError> {
    if bytes.len() > maximum {
        return Err(InvestigationStoreError::Invalid);
    }
    if !hash_valid(hash) || core_prov::content_hash(bytes) != hash {
        return Err(InvestigationStoreError::Invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InvestigationStoreError::Invalid)?;
    let value = decode_record(text)?;
    if encode(&value, maximum)? != bytes {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(value)
}

pub(super) fn timestamp(connection: &Connection) -> Result<String, InvestigationStoreError> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(value) = super::refusal_diagnostics::fixed_timestamp() {
        return Ok(value);
    }
    Ok(
        connection.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| {
            r.get(0)
        })?,
    )
}

pub(super) fn new_id(
    connection: &Connection,
    prefix: &str,
) -> Result<String, InvestigationStoreError> {
    let suffix: String =
        connection.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
    Ok(format!("{prefix}:{suffix}"))
}

pub(super) fn signed(value: u64) -> Result<i64, InvestigationStoreError> {
    i64::try_from(value).map_err(|_| InvestigationStoreError::Invalid)
}

pub(super) fn validate_provider(
    provider: &InvestigationProvider,
) -> Result<(), InvestigationStoreError> {
    if !provider.available || provider.unavailable_reason.is_some() {
        return Err(InvestigationStoreError::Unavailable);
    }
    for value in [&provider.provider_id, &provider.model, &provider.endpoint]
        .into_iter()
        .chain(provider.deployment.iter())
        .chain(provider.protocol_version.iter())
    {
        if value.is_empty() || value.len() > 2048 || redacted_history_text(value, 2048)? != *value {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}

pub(super) fn validate_usage(
    usage: &InvestigationUsage,
    limits: &InvestigationLimits,
) -> Result<(), InvestigationStoreError> {
    if usage.model_invocations > limits.model_invocations
        || usage.tool_actions > limits.tool_actions
        || usage.selected_facts > limits.selected_facts
        || usage.evidence_requests > limits.evidence_requests
        || usage.evidence_items > limits.evidence_items
        || usage.evidence_bytes > limits.evidence_bytes
        || usage.captured_validation_bytes > limits.captured_validation_bytes
        || usage.generated_token_reservations > limits.generated_token_reservations
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

fn validate_task(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    validate_task_contract(task, false)
}

/// Structural task3 validation. Public and owned readers additionally validate
/// the exact saved call journal, steps and ledgers before exposing task history.
pub(super) fn validate_next_prompt_task(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    admission_contract::validate_pairing(task)?;
    validate_task_contract(task, true)
}

/// Pure @10 validation only: borrowed task, row decoding and nested durable
/// replay remain separate owners. Pairing rejects legacy families first.
#[cfg(unix)]
pub fn next_prompt_contract_workspace() -> Option<usize> {
    let encoding = MAX_RECORD_BYTES
        .max(MAX_INVOCATION_BYTES)
        .max(MAX_DISCLOSURE_BYTES)
        .max(4096);
    context_hub::logical::sum(&[
        next_prompt_validation_workspace()?,
        encoding,
        context_hub::logical::slots::<Vec<u8>>(1)?,
    ])
}

fn validate_task_contract(
    task: &StoredTask,
    allow_next_prompt: bool,
) -> Result<(), InvestigationStoreError> {
    let next = task
        .detail
        .summary
        .specialist_id
        .uses_next_prompt_admission();
    if next && !allow_next_prompt {
        return Err(InvestigationStoreError::Invalid);
    }
    let detail = &task.detail;
    match (&detail.origin, &task.mcp_intent_hash) {
        (InvestigationOrigin::App, None) => {}
        (InvestigationOrigin::Mcp { .. }, Some(hash)) if hash_valid(hash) => {}
        _ => return Err(InvestigationStoreError::Invalid),
    }
    let policy = super::policy_for_detail(detail)?;
    let summary = &detail.summary;
    let v7 = summary.specialist_id.uses_completion_obligations();
    if task.schema_version
        != if next {
            3
        } else if v7 {
            2
        } else {
            1
        }
        || task.admission_state.is_some() != next
        || task.pending_source_read.is_some() != (task.phase == Phase::Reservation)
        || (task.phase == Phase::Reservation && !next)
        || task.completion_intent.is_some() != v7
        || summary.schema_version
            != if next {
                7
            } else if summary.specialist_id.uses_query_history_disclosure() {
                6
            } else if summary.specialist_id.uses_policy_selection() {
                5
            } else if v7 {
                4
            } else if summary.specialist_id.uses_coverage_qualification() {
                3
            } else if summary.specialist_id.uses_inspection_requirements() {
                2
            } else {
                1
            }
        || !id_valid(&summary.investigation_id)
        || !id_valid(&summary.conversation_id)
        || !id_valid(&task.nonce)
        || !hash_valid(&task.intent_hash)
        || summary.job_id <= 0
        || summary.parent_id.as_ref().is_some_and(|v| !id_valid(v))
        || detail.context_owner.len() != 32
        || !detail
            .context_owner
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !saved_specialist_matches(task)?
        || detail.provider.mode != summary.provider_mode
        || summary.specialist_id.uses_policy_selection()
            != (policy.profile() == INVESTIGATION_V2_POLICY.profile())
        || redacted_history_text(&summary.question, MAX_QUESTION_BYTES)? != summary.question
        || summary.last_event_sequence > journal_event_limit(task)?
        || summary.actions.can_cancel
            != (task.phase != Phase::Terminal && !summary.cancel_requested)
        || !summary.actions.can_follow_up
        || summary.invocation_pending
            != matches!(task.phase, Phase::Invocation | Phase::AuditInvocation)
        || task.ledger_hash.is_some() != task.ledger_revision.is_some()
        || detail.coverage_context.is_some()
            != (summary.specialist_id.uses_coverage_qualification()
                && task.ledger_revision.is_some())
        || task.ledger_hash.as_ref().is_some_and(|v| !hash_valid(v))
        || detail.citations.len()
            > detail
                .limits
                .selected_facts
                .checked_add(detail.limits.evidence_items)
                .ok_or(InvestigationStoreError::Capacity)?
        || summary.created_at.len() > 32
        || summary.updated_at.len() > 32
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(admission) = &task.admission_state {
        admission.validate_task_binding(task)?;
    }
    if let Some(audit) = &task.semantic_audit {
        audit.validate(task)?;
    } else if task.detail.advisory_audit.is_some()
        || matches!(task.phase, Phase::AuditPending | Phase::AuditInvocation)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(pending) = &task.pending_source_read {
        pending.validate()?;
        if task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
            .is_none_or(|call| call.step_id != pending.step_id)
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    if let Some(intent) = &task.completion_intent {
        intent.validate(task)?;
        let detail = task
            .detail
            .completion
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        if detail.request_hash != intent.request_hash
            || detail.obligations != intent.obligations
            || detail.obligations_hash != intent.obligations_hash
            || Some(detail.validate_with_policy(&policy)?) != summary.obligation_status
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(
            task,
            if policy.profile() == INVESTIGATION_V1_POLICY.profile() {
                64 * 1024
            } else {
                MAX_RECORD_BYTES
            },
        )?;
    } else if task.detail.completion.is_some()
        || summary.completion_outcome.is_some()
        || summary.obligation_status.is_some()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    summary.scope.validate()?;
    validate_provider(&detail.provider)?;
    validate_usage(&detail.usage, &detail.limits)?;
    match &detail.inspection_requirements {
        Some(targets)
            if summary.specialist_id.uses_inspection_requirements()
                || summary.specialist_id.uses_coverage_qualification() =>
        {
            if normalize_inspection_targets(targets)? != *targets
                || summary.inspection_requirement_count != Some(targets.len())
                || detail.inspection_assessment.is_some() != summary.has_result
            {
                return Err(InvestigationStoreError::Invalid);
            }
            let status = detail.inspection_assessment.as_ref().map_or(
                if task.phase == Phase::Terminal {
                    InvestigationInspectionStatus::NotAssessed
                } else {
                    InvestigationInspectionStatus::Pending
                },
                |a| {
                    if a.requirements.iter().all(|r| r.supplied) {
                        InvestigationInspectionStatus::Supplied
                    } else {
                        InvestigationInspectionStatus::Unmet
                    }
                },
            );
            if summary.inspection_status != Some(status) {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        None if !summary.specialist_id.uses_inspection_requirements()
            && detail.inspection_assessment.is_none()
            && summary.inspection_status.is_none()
            && summary.inspection_requirement_count.is_none() => {}
        _ => return Err(InvestigationStoreError::Invalid),
    }
    let valid_status = match task.phase {
        Phase::Queued => summary.status == InvestigationStatus::Queued && task.execution.is_none(),
        Phase::Preparing => {
            summary.status == InvestigationStatus::Preparing && task.execution.is_some()
        }
        Phase::Consent => {
            summary.status == InvestigationStatus::AwaitingConsent && task.pending.is_some()
        }
        Phase::Ready
        | Phase::Invocation
        | Phase::Action
        | Phase::Tool
        | Phase::Reservation
        | Phase::AuditPending
        | Phase::AuditInvocation => summary.status == InvestigationStatus::Running,
        Phase::Terminal => matches!(
            summary.status,
            InvestigationStatus::Completed
                | InvestigationStatus::Failed
                | InvestigationStatus::Cancelled
                | InvestigationStatus::Interrupted
                | InvestigationStatus::OutcomeUnknown
        ),
    };
    if !valid_status || (summary.has_result != (summary.status == InvestigationStatus::Completed)) {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(identity) = &task.execution {
        if identity.namespace != detail.context_owner
            || identity.job_id != summary.job_id
            || identity.generation <= 0
            || identity.owner.len() != 32
            || !identity
                .owner
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvestigationStoreError::Invalid);
        }
    } else if !matches!(task.phase, Phase::Queued | Phase::Terminal) {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(pending) = &task.pending {
        validate_step(pending, &detail.limits)?;
    }
    if task.pending.is_some() != matches!(task.phase, Phase::Consent | Phase::Invocation)
        || (task.approved && task.phase != Phase::Consent)
        || task.action_tool.is_some() != matches!(task.phase, Phase::Action | Phase::Tool)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    encode(summary, 4096)?;
    Ok(())
}

pub(super) fn validate_step(
    step: &PendingInvestigationStep,
    limits: &InvestigationLimits,
) -> Result<(), InvestigationStoreError> {
    let policy = InvestigationPolicy::for_effective_limits(limits)
        .ok_or(InvestigationStoreError::Invalid)?;
    if !id_valid(&step.step_id)
        || !hash_valid(&step.payload_hash)
        || !hash_valid(&step.input_ledger_hash)
        || step.input_bytes == 0
        || step.input_bytes > MAX_INPUT_BYTES
        || step.generated_tokens == 0
        || step.generated_tokens
            > policy.output_tokens_for_mode(
                step.disclosure
                    .as_ref()
                    .and_then(|d| d.admission.as_ref())
                    .filter(|s| {
                        !policy.local_finish_generation()
                            || s.acquisition
                                .as_ref()
                                .is_some_and(|a| a.finish_policy_complete || a.capacity_exhausted)
                    })
                    .map_or(
                        agents::investigation::staged_refusal::CompletionCallMode::Choice,
                        |s| s.mode,
                    ),
            )
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(disclosure) = &step.disclosure {
        if disclosure.payload_hash != step.payload_hash
            || disclosure.input_ledger_hash != step.input_ledger_hash
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(disclosure, MAX_DISCLOSURE_BYTES)?;
    }
    encode(step, MAX_INVOCATION_BYTES)?;
    Ok(())
}

pub(super) fn validate_step_basis(
    connection: &Connection,
    task: &StoredTask,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    let policy = super::policy_for_detail(&task.detail)?;
    if task.detail.summary.specialist_id.uses_compact_disclosure() != step.disclosure.is_some() {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(disclosure) = &step.disclosure {
        let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
        disclosure.validate_saved_role_shape(
            &ledger,
            &policy,
            task.detail.summary.specialist_id,
        )?;
        let completion_sources = task
            .completion_intent
            .as_ref()
            .map(|intent| intent.obligations.sources_with_policy(&policy))
            .transpose()?;
        let expected = match &completion_sources {
            Some(sources) if !sources.is_empty() => Some(inspection_requirements_hash_with_policy(
                &normalize_inspection_targets_with_policy(sources, &policy)?,
                &policy,
            )?),
            Some(_) => None,
            None => task
                .detail
                .inspection_requirements
                .as_ref()
                .map(|targets| inspection_requirements_hash_with_policy(targets, &policy))
                .transpose()?,
        };
        if task.completion_intent.is_some() {
            super::completion::validate_step_metadata(connection, task, step)?;
        } else if matches!(disclosure.schema_version, 4 | 5) {
            return Err(InvestigationStoreError::Invalid);
        }
        if disclosure.inspection_requirements_hash != expected {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}

pub(super) fn validate_result_basis(
    result: &InvestigationResult,
    ledger: &InvestigationInputLedger,
    step: &PendingInvestigationStep,
    limits: &InvestigationLimits,
    basis: Option<&admission_journal::AuthenticatedCompletionBasis>,
) -> Result<(), InvestigationStoreError> {
    let policy = InvestigationPolicy::for_effective_limits(limits)
        .ok_or(InvestigationStoreError::Invalid)?;
    validate_step(step, limits)?;
    if let Some(basis) = basis {
        basis.check_step(ledger, step)?;
        basis.binding()?.validate_result(result)?;
        return Ok(());
    }
    match &step.disclosure {
        Some(disclosure) => result.validate_with_disclosure_and_policy(
            ledger,
            &step.step_id,
            disclosure,
            &policy,
        )?,
        None => result.validate_with_policy(ledger, &policy)?,
    }
    Ok(())
}

pub(super) fn validate_inspection_result(
    task: &StoredTask,
    result: &InvestigationResult,
) -> Result<(), InvestigationStoreError> {
    let policy = super::policy_for_detail(&task.detail)?;
    if let Some(intent) = &task.completion_intent {
        let sources = intent.obligations.sources_with_policy(&policy)?;
        if task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
        {
            admission_contract::validate_pairing(task)?;
        }
        if result.schema_version
            != if task.detail.summary.specialist_id.uses_cited_narrative() {
                8
            } else if task
                .detail
                .summary
                .specialist_id
                .uses_next_prompt_admission()
            {
                7
            } else if task
                .detail
                .summary
                .specialist_id
                .uses_query_history_disclosure()
            {
                6
            } else {
                5
            }
        {
            return Err(InvestigationStoreError::Invalid);
        }
        return match (&result.inspection_assessment, sources.is_empty()) {
            (None, true) => Ok(()),
            (Some(assessment), false)
                if assessment.targets_with_policy(&policy)?
                    == normalize_inspection_targets_with_policy(&sources, &policy)?
                    && assessment.requirements_hash
                        == inspection_requirements_hash_with_policy(
                            &normalize_inspection_targets_with_policy(&sources, &policy)?,
                            &policy,
                        )? =>
            {
                Ok(())
            }
            _ => Err(InvestigationStoreError::Invalid),
        };
    }
    match (
        &task.detail.inspection_requirements,
        &result.inspection_assessment,
    ) {
        (Some(targets), Some(assessment))
            if result.schema_version
                == if task
                    .detail
                    .summary
                    .specialist_id
                    .uses_completion_obligations()
                {
                    5
                } else if task
                    .detail
                    .summary
                    .specialist_id
                    .uses_coverage_qualification()
                {
                    4
                } else {
                    3
                }
                && assessment.targets()? == *targets
                && assessment.requirements_hash == inspection_requirements_hash(targets)? =>
        {
            Ok(())
        }
        (None, None)
            if !task
                .detail
                .summary
                .specialist_id
                .uses_inspection_requirements()
                && (!task
                    .detail
                    .summary
                    .specialist_id
                    .uses_coverage_qualification()
                    || result.schema_version
                        == if task
                            .detail
                            .summary
                            .specialist_id
                            .uses_completion_obligations()
                        {
                            5
                        } else {
                            4
                        }) =>
        {
            Ok(())
        }
        _ => Err(InvestigationStoreError::Invalid),
    }
}

pub(super) fn load_task(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    let task = load_task_base(connection, id)?;
    super::completion::validate_saved(connection, &task)?;
    Ok(task)
}

/// Owner app access is unrestricted; MCP must prove indexed ownership before
/// reading a body. Missing and foreign identifiers deliberately share an error.
pub(super) fn require_request_owner(
    connection: &Connection,
    id: &str,
    client: Option<&McpClientId>,
) -> Result<(), InvestigationStoreError> {
    if let Some(client) = client {
        if connection.is_autocommit() || !id_valid(id) {
            return Err(InvestigationStoreError::Missing);
        }
        let namespace = InvestigationOrigin::Mcp {
            client_id: client.clone(),
        }
        .request_namespace();
        let owned: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM investigation_tasks WHERE id=?1 AND request_namespace=?2)",
            params![id, namespace],
            |r| r.get(0),
        )?;
        if !owned {
            return Err(InvestigationStoreError::Missing);
        }
    }
    Ok(())
}

/// Family routing for execution and public reads. The bounded, checksummed
/// discriminator chooses a full validator; it never grants mutation authority.
pub(super) fn load_execution_task(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    if connection.is_autocommit() || !id_valid(id) {
        return Err(InvestigationStoreError::Invalid);
    }
    let row: Option<(Option<Vec<u8>>, Option<String>)> = connection.query_row(
        "SELECT CASE WHEN typeof(record)='blob' AND length(record)<=262144 THEN record END, CASE WHEN typeof(record_hash)='text' AND length(CAST(record_hash AS BLOB))=64 THEN record_hash END FROM investigation_tasks WHERE id=?1",
        [id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let (Some(bytes), Some(hash)) = row.ok_or(InvestigationStoreError::Missing)? else {
        return Err(InvestigationStoreError::Invalid);
    };
    let discriminator: StoredTask = decode(&bytes, &hash)?;
    let next_prompt = discriminator
        .detail
        .summary
        .specialist_id
        .uses_next_prompt_admission();
    // The full family reader acquires its own row and decoded task. Routing
    // owners must not overlap that second acquisition and saved-state validation.
    drop((discriminator, bytes, hash));
    if next_prompt {
        load_next_prompt_task(connection, id)
    } else {
        load_task(connection, id)
    }
}

/// Full internal reader for callers that are not themselves validating a
/// historical projection. Keep the base reader separate to avoid replay cycles.
pub(super) fn load_next_prompt_task(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    let task = load_next_prompt_task_base(connection, id)?;
    if task.detail.summary.specialist_id.uses_acquisition_choices() {
        if task.ledger_revision.is_some() {
            // Ready has no current call to trigger the current-call reader below.
            super::admission_journal::derive_current(connection, &task)?;
        } else {
            // No replay epoch exists before ContextPrepared, including a stop
            // during preparation. Never let absent context hide saved work.
            if task.ledger_hash.is_some()
                || task.detail.usage.model_invocations != 0
                || !matches!(
                    task.phase,
                    Phase::Queued | Phase::Preparing | Phase::Terminal
                )
            {
                return Err(InvestigationStoreError::Invalid);
            }
            let has_refusals = admission_journal::validate_optional_schema(connection)?.is_some();
            for table in [
                "investigation_inputs",
                "investigation_invocations",
                "investigation_responses",
                "investigation_admission_refusals",
            ] {
                if table == "investigation_admission_refusals" && !has_refusals {
                    continue;
                }
                let exists: bool = connection.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE task_id=?1)"),
                    [id],
                    |row| row.get(0),
                )?;
                if exists {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
        }
    }
    completion::validate_saved(connection, &task)?;
    super::volatile_patch::authenticate(connection, &task)?;
    if let Some(audit) = &task.semantic_audit {
        audit.validate_saved(connection, &task)?;
    }
    Ok(task)
}

pub(super) fn load_task_base(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    load_task_base_contract(connection, id, false)
}

/// Task3 reader in the caller's snapshot. Row bounds and every SQL scalar
/// parity check are shared with historical families, while saved @10 calls
/// use their exact journal/ordinal contract.
pub(super) fn load_next_prompt_task_base(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    load_task_base_contract(connection, id, true)
}

fn load_task_base_contract(
    connection: &Connection,
    id: &str,
    next_prompt: bool,
) -> Result<StoredTask, InvestigationStoreError> {
    if !id_valid(id) {
        return Err(InvestigationStoreError::Invalid);
    }
    let row = connection.query_row(
        "SELECT CASE WHEN typeof(record)='blob' AND length(record)<=262144 THEN record END, CASE WHEN typeof(record_hash)='text' AND length(CAST(record_hash AS BLOB))=64 THEN record_hash END, revision, live, job_id, CASE WHEN typeof(nonce)='text' AND length(CAST(nonce AS BLOB))<=128 THEN nonce END, CASE WHEN typeof(conversation_id)='text' AND length(CAST(conversation_id AS BLOB))<=128 THEN conversation_id END, CASE WHEN typeof(request_namespace)='text' AND length(CAST(request_namespace AS BLOB))<=36 THEN request_namespace END FROM investigation_tasks WHERE id=?1",
        [id], |r| Ok((r.get::<_,Option<Vec<u8>>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?)),
    ).optional()?.ok_or(InvestigationStoreError::Missing)?;
    let (
        Some(bytes),
        Some(hash),
        revision,
        live,
        job_id,
        Some(nonce),
        Some(conversation),
        Some(request_namespace),
    ) = row
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    let task: StoredTask = decode(&bytes, &hash)?;
    super::volatile_patch::validate_shape(&task)?;
    // Canonical decode owns the task; its row buffers need not overlap validation.
    drop((bytes, hash));
    if next_prompt {
        validate_next_prompt_task(&task)?;
    } else {
        validate_task(&task)?;
    }
    let namespace: Option<String> = connection.query_row(
        "SELECT CASE WHEN typeof(namespace)='text' AND length(CAST(namespace AS BLOB))=32 THEN namespace END FROM investigation_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let namespace = namespace.ok_or(InvestigationStoreError::Invalid)?;
    if task.detail.origin.request_namespace() != request_namespace
        || task.detail.summary.investigation_id != id
        || signed(task.detail.summary.revision)? != revision
        || live != i64::from(task.phase != Phase::Terminal)
        || task.detail.summary.job_id != job_id
        || task.nonce != nonce
        || task.detail.summary.conversation_id != conversation
        || task.detail.context_owner != namespace
    {
        return Err(InvestigationStoreError::Invalid);
    }
    // All scalar mirrors have been checked before entering nested saved reads.
    drop((nonce, conversation, request_namespace, namespace));
    if next_prompt {
        if task.pending_source_read.is_some() {
            source_validation::validate_saved_pending(connection, &task)?;
        } else if let Some(call) = task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
        {
            if task.phase == Phase::Consent {
                call.validate_pending_consent(connection, &task)?;
            } else {
                call.validate_saved(connection, &task)?;
            }
        }
    } else if let Some(pending) = &task.pending {
        validate_step_basis(connection, &task, pending)?;
        if task.completion_intent.is_some() && task.phase == Phase::Invocation {
            let invoked: PendingInvestigationStep = read_body(
                connection,
                "investigation_invocations",
                "step_id",
                id,
                &pending.step_id,
            )?
            .ok_or(InvestigationStoreError::Invalid)?;
            if &invoked != pending {
                return Err(InvestigationStoreError::Invalid);
            }
        }
    }
    super::transitions::source_replay_state(connection, &task)?;
    let qualified = task
        .detail
        .summary
        .specialist_id
        .uses_coverage_qualification();
    if qualified && task.ledger_revision.is_some() && !task.detail.summary.has_result {
        load_ledger(connection, &task)?.ok_or(InvestigationStoreError::Invalid)?;
    }
    // A changed specialist/schema must not disable validation of a completed
    // result and its ledger. Valid historical result versions use the same path.
    if task.detail.summary.has_result {
        load_result(connection, &task)?.ok_or(InvestigationStoreError::Invalid)?;
    }
    Ok(task)
}

pub(super) fn insert_task(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    validate_task(task)?;
    insert_validated_task(connection, task)
}

pub(super) fn insert_next_prompt_task(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_next_prompt_task(task)?;
    insert_validated_task(connection, task)
}

fn insert_validated_task(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let bytes = encode(task, MAX_RECORD_BYTES)?;
    one(connection.execute("INSERT INTO investigation_tasks(id,nonce,conversation_id,job_id,revision,live,record,record_hash,request_namespace) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![task.detail.summary.investigation_id,task.nonce,task.detail.summary.conversation_id,task.detail.summary.job_id,signed(task.detail.summary.revision)?,i64::from(task.phase!=Phase::Terminal),bytes,core_prov::content_hash(&bytes),task.detail.origin.request_namespace()])?)
}

pub(super) fn save_task(
    connection: &Connection,
    task: &StoredTask,
    expected_revision: u64,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    validate_task(task)?;
    save_validated_task(connection, task, expected_revision, terminal)
}

/// Task3 writer. The caller authenticates execution/task authority and owns
/// transaction/commit; historical families retain their separate serializer.
pub(super) fn save_next_prompt_task(
    connection: &Connection,
    task: &StoredTask,
    expected_revision: u64,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    if task.semantic_audit.is_some() {
        let mut accounted = task.clone();
        super::semantic_audit_state::account_analyst_save(&mut accounted)?;
        validate_next_prompt_task(&accounted)?;
        return save_validated_task(connection, &accounted, expected_revision, terminal);
    }
    validate_next_prompt_task(task)?;
    save_validated_task(connection, task, expected_revision, terminal)
}

/// Audited writer never feeds aggregate usage back into the analyst account.
/// The owning caller authenticates execution and commits the journal and debit.
pub(super) fn save_started_audit_task(
    connection: &Connection,
    task: &StoredTask,
    expected_revision: u64,
) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() || task.phase != Phase::AuditInvocation {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_next_prompt_task(task)?;
    super::semantic_audit_state::validate_invocation_journal(connection, task)?;
    save_validated_task(connection, task, expected_revision, false)
}

fn save_validated_task(
    connection: &Connection,
    task: &StoredTask,
    expected_revision: u64,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    let bytes = encode(task, MAX_RECORD_BYTES)?;
    one(connection.execute("UPDATE investigation_tasks SET revision=?2,live=?3,record=?4,record_hash=?5 WHERE id=?1 AND revision=?6 AND request_namespace=?7", params![task.detail.summary.investigation_id,signed(task.detail.summary.revision)?,i64::from(task.phase!=Phase::Terminal),bytes,core_prov::content_hash(&bytes),signed(expected_revision)?,task.detail.origin.request_namespace()])?)?;
    check_task_capacity(connection, &task.detail.summary.investigation_id, terminal)
}

pub(super) fn check_start_capacity(
    connection: &Connection,
    new_conversation: bool,
) -> Result<(), InvestigationStoreError> {
    let (tasks, live, conversations): (i64,i64,i64) = connection.query_row("SELECT (SELECT count(*) FROM investigation_tasks), (SELECT count(*) FROM investigation_tasks WHERE live=1), (SELECT count(*) FROM investigation_conversations)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let tasks = usize::try_from(tasks).map_err(|_| InvestigationStoreError::Invalid)?;
    let live = usize::try_from(live).map_err(|_| InvestigationStoreError::Invalid)?;
    let conversations =
        usize::try_from(conversations).map_err(|_| InvestigationStoreError::Invalid)?;
    if tasks >= MAX_TASKS
        || live >= MAX_LIVE_TASKS
        || (new_conversation && conversations >= MAX_TASKS)
        || tasks
            .checked_add(1)
            .and_then(|v| v.checked_mul(TASK_CAPACITY))
            .is_none_or(|v| v > STORE_CAPACITY)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

pub(super) fn check_task_capacity(
    connection: &Connection,
    id: &str,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    super::read_reservations::with_compatibility_scope(connection, |reads| {
        check_task_capacity_with_reads(connection, id, terminal, reads)
    })
}

/// Reuse one caller-transaction-borrowed read snapshot across both bounds and
/// repeated checks. Namespace writes invalidate/rederive it before the next use.
pub(super) fn check_task_capacity_with_reads(
    connection: &Connection,
    id: &str,
    terminal: bool,
    reads: &mut super::read_reservations::ReadTransaction<'_>,
) -> Result<(), InvestigationStoreError> {
    let maximum = if terminal {
        TASK_CAPACITY
    } else {
        TASK_CAPACITY - TERMINAL_RESERVE
    };
    if stored_bytes_with_reads(connection, Some(id), reads)? > maximum
        || stored_bytes_with_reads(connection, None, reads)? > STORE_CAPACITY
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

/// Remaining nonterminal storage, including all existing immutable archives.
pub(super) fn remaining_task_capacity(
    connection: &Connection,
    id: &str,
) -> Result<usize, InvestigationStoreError> {
    (TASK_CAPACITY - TERMINAL_RESERVE)
        .checked_sub(stored_bytes(connection, Some(id))?)
        .ok_or(InvestigationStoreError::Capacity)
}

/// Global remaining bytes under the unchanged store ceiling.
pub(super) fn remaining_store_capacity(
    connection: &Connection,
) -> Result<usize, InvestigationStoreError> {
    STORE_CAPACITY
        .checked_sub(stored_bytes(connection, None)?)
        .ok_or(InvestigationStoreError::Capacity)
}

/// Compatibility wrapper: a fresh short-lived read preflight/snapshot per
/// call. Existing absent-namespace autocommit metadata reads stay unchanged.
pub(super) fn stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    super::read_reservations::with_compatibility_scope(connection, |reads| {
        stored_bytes_with_reads(connection, id, reads)
    })
}

/// Actual logical occupancy excludes unspent allocations, using one coherent
/// read snapshot. Used to verify that an own-stage write fits its allocation.
pub(super) fn actual_stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    super::read_reservations::with_compatibility_scope(connection, |reads| {
        let occupied = stored_bytes_with_reads(connection, id, reads)?;
        let claims = reads.capacity_charge(id)?.claims;
        let chat_reservation = unspent_chat_reference_reservation(connection, id)?;
        occupied
            .checked_sub(claims)
            .and_then(|bytes| bytes.checked_sub(chat_reservation))
            .ok_or(InvestigationStoreError::Invalid)
    })
}

fn unspent_chat_reference_reservation(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM investigation_chats c JOIN investigation_tasks t ON t.id=c.task_id WHERE (?1 IS NULL OR c.task_id=?1) AND t.live=1 AND NOT EXISTS (SELECT 1 FROM investigation_chat_messages m WHERE m.chat_id=c.id AND m.ordinal=1)",
        [id],
        |row| row.get(0),
    )?;
    usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(super::chat::RESULT_REFERENCE_SLOT_BYTES))
        .ok_or(InvestigationStoreError::Capacity)
}

pub(super) fn stored_bytes_with_reads(
    connection: &Connection,
    id: Option<&str>,
    reads: &mut super::read_reservations::ReadTransaction<'_>,
) -> Result<usize, InvestigationStoreError> {
    if !reads.matches_connection(connection) {
        return Err(InvestigationStoreError::Invalid);
    }
    // Effective occupied capacity: fixed allowances, every stored body, and
    // additional pending source-settlement claims. Claims are beyond the
    // existing TERMINAL_RESERVE; do not add that reserve here a second time.
    // BLOB casts charge UTF-8 bytes even if a malformed row has SQL TEXT type.
    let (tasks, records): (i64, i64) = connection.query_row(
        "SELECT count(*), coalesce(sum(length(CAST(record AS BLOB))+length(CAST(record_hash AS BLOB))+length(CAST(id AS BLOB))+length(CAST(nonce AS BLOB))+length(CAST(conversation_id AS BLOB))+length(CAST(request_namespace AS BLOB))),0) FROM investigation_tasks WHERE (?1 IS NULL OR id=?1)",
        [id], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut bytes = usize::try_from(tasks)
        .ok()
        .and_then(|tasks| tasks.checked_mul(1024))
        .and_then(|bytes| {
            usize::try_from(records)
                .ok()
                .and_then(|records| bytes.checked_add(records))
        })
        .ok_or(InvestigationStoreError::Capacity)?;
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_invocations",
        "investigation_responses",
        "investigation_results",
        "investigation_coverage_archives",
    ] {
        let sum: i64 = connection.query_row(
            &format!("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256),0) FROM {table} WHERE (?1 IS NULL OR task_id=?1)"),
            [id], |r| r.get(0),
        )?;
        bytes = bytes
            .checked_add(usize::try_from(sum).map_err(|_| InvestigationStoreError::Invalid)?)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    let chat_metadata: i64 = connection.query_row(
        "SELECT coalesce(sum(length(CAST(id AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(created_at AS BLOB))+256),0) FROM investigation_chats WHERE (?1 IS NULL OR task_id=?1)",
        [id],
        |row| row.get(0),
    )?;
    let chat_messages: i64 = connection.query_row(
        "SELECT coalesce(sum(length(CAST(id AS BLOB))+length(CAST(chat_id AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256),0) FROM investigation_chat_messages WHERE (?1 IS NULL OR task_id=?1)",
        [id],
        |row| row.get(0),
    )?;
    bytes = bytes
        .checked_add(usize::try_from(chat_metadata).map_err(|_| InvestigationStoreError::Invalid)?)
        .and_then(|bytes| bytes.checked_add(usize::try_from(chat_messages).ok()?))
        .and_then(|bytes| {
            bytes.checked_add(unspent_chat_reference_reservation(connection, id).ok()?)
        })
        .ok_or(InvestigationStoreError::Capacity)?;
    // The optional journal is lazily created; legacy stores keep identical
    // accounting without creating or migrating its private schema.
    let journal_bytes = super::staged::journal_bytes(connection, id)?;
    let audit_bytes = super::semantic_audit_journal::stored_bytes(connection, id)?;
    let completion_bytes = super::completion::stored_bytes(connection, id)?;
    let admission_bytes = super::admission_journal::stored_bytes(connection, id)?;
    let review_bytes = super::finding_reviews::stored_bytes(connection, id)?;
    let read_charge = reads.capacity_charge(id)?.occupied()?;
    let refs: i64 = connection.query_row(
        "SELECT coalesce(sum(length(CAST(task_id AS BLOB))+length(CAST(source_id AS BLOB))+length(CAST(repo_key AS BLOB))+length(CAST(receipt_id AS BLOB))+128),0) FROM investigation_refs WHERE (?1 IS NULL OR task_id=?1)",
        [id], |r| r.get(0),
    )?;
    bytes
        .checked_add(usize::try_from(refs).map_err(|_| InvestigationStoreError::Invalid)?)
        .and_then(|bytes| bytes.checked_add(journal_bytes))
        .and_then(|bytes| bytes.checked_add(audit_bytes))
        .and_then(|bytes| bytes.checked_add(completion_bytes))
        .and_then(|bytes| bytes.checked_add(admission_bytes))
        .and_then(|bytes| bytes.checked_add(review_bytes))
        .and_then(|bytes| bytes.checked_add(read_charge))
        .ok_or(InvestigationStoreError::Capacity)
}

pub(super) fn journal_event_limit(task: &StoredTask) -> Result<u64, InvestigationStoreError> {
    super::policy_for_detail(&task.detail).map(|policy| policy.journal_events())
}

pub(super) fn event(
    connection: &Connection,
    task: &mut StoredTask,
    kind: InvestigationEventKind,
    step: Option<&str>,
    tool: Option<InvestigationTool>,
    summary: &'static str,
) -> Result<(), InvestigationStoreError> {
    let event = prepare_event(task, kind, step, tool, summary, timestamp(connection)?)?;
    persist_event(connection, &event)
}

/// Shared production event projection. Sizing callers can serialize this exact
/// record before a writer publishes it; this function performs no SQL mutation.
pub(super) fn prepare_event(
    task: &mut StoredTask,
    kind: InvestigationEventKind,
    step: Option<&str>,
    tool: Option<InvestigationTool>,
    summary: &'static str,
    created_at: String,
) -> Result<InvestigationEvent, InvestigationStoreError> {
    let next = task
        .detail
        .summary
        .last_event_sequence
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    let event_limit = journal_event_limit(task)?;
    let maximum = if task.phase == Phase::Terminal {
        event_limit
    } else if kind == InvestigationEventKind::CancelRequested {
        event_limit
            .checked_sub(2)
            .ok_or(InvestigationStoreError::Invalid)?
    } else {
        event_limit
            .checked_sub(3)
            .ok_or(InvestigationStoreError::Invalid)?
    };
    if next > maximum {
        return Err(InvestigationStoreError::Capacity);
    }
    task.detail.summary.last_event_sequence = next;
    Ok(InvestigationEvent {
        query_refusal: None,
        investigation_id: task.detail.summary.investigation_id.clone(),
        sequence: next,
        revision: task.detail.summary.revision,
        kind,
        step_id: step.map(str::to_owned),
        tool,
        summary: summary.into(),
        created_at,
    })
}

pub(super) fn persist_event(
    connection: &Connection,
    event: &InvestigationEvent,
) -> Result<(), InvestigationStoreError> {
    event.validate_query_refusal_envelope()?;
    let reserved =
        super::read_reservations::pending_event_claims(connection, &event.investigation_id)?;
    if reserved != 0 {
        let terminal_slots = if event.kind == InvestigationEventKind::CancelRequested {
            2
        } else {
            3
        };
        let maximum = INVESTIGATION_V2_POLICY
            .journal_events()
            .checked_sub(terminal_slots)
            .ok_or(InvestigationStoreError::Invalid)?;
        if event
            .sequence
            .checked_add(reserved as u64)
            .is_none_or(|n| n > maximum)
        {
            return Err(InvestigationStoreError::Capacity);
        }
    }
    let body = encode(event, 16 * 1024)?;
    one(connection.execute(
        "INSERT INTO investigation_events VALUES (?1,?2,?3,?4)",
        params![
            event.investigation_id,
            signed(event.sequence)?,
            body,
            core_prov::content_hash(&body)
        ],
    )?)
}

pub(super) fn bump(
    connection: &Connection,
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    task.detail.summary.revision = task
        .detail
        .summary
        .revision
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    task.detail.summary.updated_at = timestamp(connection)?;
    task.detail.summary.actions.can_cancel =
        task.phase != Phase::Terminal && !task.detail.summary.cancel_requested;
    task.detail.summary.invocation_pending =
        matches!(task.phase, Phase::Invocation | Phase::AuditInvocation);
    Ok(())
}

pub(super) fn body_read_sql(table: &str, key_column: &str) -> String {
    format!(
        "SELECT CASE WHEN typeof(body)='blob' AND length(body)<=262144 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM {table} WHERE task_id=?1 AND {key_column}=?2"
    )
}

pub(super) fn read_body<T: serde::de::DeserializeOwned + Serialize>(
    connection: &Connection,
    table: &str,
    key_column: &str,
    id: &str,
    key: &dyn rusqlite::ToSql,
) -> Result<Option<T>, InvestigationStoreError> {
    let sql = body_read_sql(table, key_column);
    let row = connection
        .query_row(&sql, params![id, key], |r| {
            Ok((
                r.get::<_, Option<Vec<u8>>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    row.map(|(body, hash)| {
        let (Some(body), Some(hash)) = (body, hash) else {
            return Err(InvestigationStoreError::Invalid);
        };
        decode(&body, &hash)
    })
    .transpose()
}

/// Next-family saved ledger load, including retained caller/archive through
/// inventory validation and fingerprinting. Other replay callers remain separate.
#[cfg(unix)]
pub fn next_prompt_ledger_workspace() -> Option<usize> {
    use context_hub::logical::{slots, sum};
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (ledger, decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (archive, _) = InvestigationCoverageArchive::decode_workspace_bytes()?;
    let (detail, read) = InvestigationCoverageArchive::read_workspace_bytes()?;
    let validation = ledger_validation_workspace(&INVESTIGATION_V2_POLICY, MAX_RECORD_BYTES)?;
    // read_body's static query identifiers cannot originate from the client.
    let acquisition = sum(&[
        task,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        128,
        slots::<String>(3)?,
        body_read_sql("investigation_inputs", "revision").len(),
        decode,
    ])?;
    let validate = sum(&[task, ledger, validation])?;
    // The archive helper already includes the caller task and saved witness.
    let load = ledger.checked_add(super::coverage::archive_acquisition_workspace()?)?;
    // Each read is sequential; its returned detail survives bounded encoding
    // and hashing. The archive remains live through ledger fingerprinting too.
    let read_encode = sum(&[
        detail,
        MAX_QUERY_BYTES,
        slots::<Vec<u8>>(1)?,
        64,
        slots::<String>(1)?,
    ])?;
    let replay = sum(&[
        task,
        ledger,
        archive,
        read.max(read_encode).max(validation),
        64,
        slots::<String>(1)?,
    ])?;
    Some(acquisition.max(validate).max(load).max(replay))
}

pub(super) fn load_ledger(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<InvestigationInputLedger>, InvestigationStoreError> {
    let policy = super::policy_for_detail(&task.detail)?;
    let Some(revision) = task.ledger_revision else {
        return Ok(None);
    };
    let ledger: InvestigationInputLedger = read_body(
        connection,
        "investigation_inputs",
        "revision",
        &task.detail.summary.investigation_id,
        &signed(revision)?,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    ledger.validate_for_specialist(task.detail.summary.specialist_id, &policy)?;
    match (&task.detail.coverage_context, &ledger.coverage) {
        (None, None) => {}
        (Some(context), Some(proof))
            if context.context_id == proof.context_id
                && context.coverage == proof.qualification => {}
        _ => return Err(InvestigationStoreError::Invalid),
    }
    if ledger.schema_version
        != if task
            .detail
            .summary
            .specialist_id
            .uses_coverage_qualification()
        {
            2
        } else {
            1
        }
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let archive = super::coverage::load_for_task(connection, task)?;
    match (&ledger.coverage, archive.as_ref()) {
        (None, None) => {}
        (Some(proof), Some(archive)) if proof == archive.proof() => {
            archive.validate_reads_with_policy(&ledger.coverage_reads, &policy)?;
        }
        _ => return Err(InvestigationStoreError::Invalid),
    }

    if Some(ledger.fingerprint_with_policy(&policy)?) != task.ledger_hash
        || ledger.revision != revision
        || Some(&ledger.graph_snapshot_id) != task.detail.summary.graph_snapshot_id.as_ref()
        || Some(&ledger.scope_snapshot_id) != task.detail.scope_snapshot_id.as_ref()
        || ledger.citations != task.detail.citations
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(Some(ledger))
}

pub(super) fn load_result(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<InvestigationResult>, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let result: Option<InvestigationResult> =
        read_body(connection, "investigation_results", "task_id", id, &id)?;
    if result.is_some() != task.detail.summary.has_result {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(result) = &result {
        let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
        if let Some(step_id) = &result.invocation_step_id {
            let step: PendingInvestigationStep = read_body(
                connection,
                "investigation_invocations",
                "step_id",
                id,
                step_id,
            )?
            .ok_or(InvestigationStoreError::Invalid)?;
            if step.step_id != *step_id || step.disclosure.is_none() {
                return Err(InvestigationStoreError::Invalid);
            }
            let basis =
                admission_journal::AuthenticatedCompletionBasis::load(connection, task, step_id)?;
            validate_step_basis(connection, task, &step)?;
            validate_result_basis(result, &ledger, &step, &task.detail.limits, basis.as_ref())?;
            super::completion::validate_result(
                connection,
                task,
                result,
                &ledger,
                &step,
                basis.as_ref(),
            )?;
        } else {
            if task.detail.summary.specialist_id.uses_compact_disclosure() {
                return Err(InvestigationStoreError::Invalid);
            }
            result.validate(&ledger)?;
        }
        validate_inspection_result(task, result)?;
        if task.completion_intent.is_none()
            && result.inspection_assessment != task.detail.inspection_assessment
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if result.investigation_id != *id {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(result)
}

#[cfg(any(test, feature = "test-support"))]
#[path = "qualified_storage_tests.rs"]
mod qualified_storage_tests;

pub(super) fn insert_body(
    connection: &Connection,
    table: &str,
    id: &str,
    key: &dyn rusqlite::ToSql,
    value: &impl Serialize,
    maximum: usize,
) -> Result<(), InvestigationStoreError> {
    let body = encode(value, maximum)?;
    one(connection.execute(
        &format!("INSERT INTO {table} VALUES (?1,?2,?3,?4)"),
        params![id, key, body, core_prov::content_hash(&body)],
    )?)
}

/// Construct an actual old layout for migration fixtures, never for runtime.
/// Construct an actual old layout for migration fixtures, never for runtime.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn version_two_fixture(connection: &Connection) {
    let foreign_keys: bool = connection
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    let legacy_alter: bool = connection
        .pragma_query_value(None, "legacy_alter_table", |r| r.get(0))
        .unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute_batch(
            "DROP TABLE investigation_chat_messages; DROP TABLE investigation_chats; DROP TABLE investigation_mcp_clients",
        )
        .unwrap();
    connection
        .pragma_update(None, "legacy_alter_table", true)
        .unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    tx.execute_batch("ALTER TABLE investigation_tasks RENAME TO investigation_tasks_v3")
        .unwrap();
    let old_tasks = legacy_schema()
        .into_iter()
        .find(|entry| entry.0 == "investigation_tasks")
        .unwrap()
        .2;
    tx.execute_batch(&old_tasks).unwrap();
    tx.execute_batch("INSERT INTO investigation_tasks (ordinal,id,nonce,conversation_id,job_id,revision,live,record,record_hash) SELECT ordinal,id,nonce,conversation_id,job_id,revision,live,record,record_hash FROM investigation_tasks_v3; DROP TABLE investigation_tasks_v3").unwrap();
    tx.commit().unwrap();
    connection
        .pragma_update(None, "legacy_alter_table", legacy_alter)
        .unwrap();
    connection
        .pragma_update(None, "foreign_keys", foreign_keys)
        .unwrap();
    let namespace: String = connection
        .query_row("SELECT namespace FROM investigation_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute_batch("DROP TABLE investigation_meta")
        .unwrap();
    connection.execute_batch(META_V2).unwrap();
    connection
        .execute(
            "INSERT INTO investigation_meta VALUES (1,2,?1)",
            [&namespace],
        )
        .unwrap();
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn version_three_fixture(connection: &Connection) {
    let foreign_keys: bool = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let namespace: String = connection
        .query_row("SELECT namespace FROM investigation_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute_batch("DROP TABLE investigation_chat_messages; DROP TABLE investigation_chats; DROP TABLE investigation_meta")
        .unwrap();
    connection.execute_batch(META_V3).unwrap();
    connection
        .execute(
            "INSERT INTO investigation_meta VALUES (1,3,?1)",
            [&namespace],
        )
        .unwrap();
    connection
        .pragma_update(None, "foreign_keys", foreign_keys)
        .unwrap();
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn version_one_fixture(connection: &Connection) {
    version_two_fixture(connection);
    for (name, kind, _) in super::finding_reviews::schema().into_iter().rev() {
        connection
            .execute_batch(&format!("DROP {} {name}", kind.to_uppercase()))
            .unwrap();
    }
    let namespace: String = connection
        .query_row("SELECT namespace FROM investigation_meta", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute_batch("DROP TABLE investigation_meta")
        .unwrap();
    connection.execute_batch(&legacy_schema()[0].2).unwrap();
    connection
        .execute(
            "INSERT INTO investigation_meta VALUES (1,1,?1)",
            [&namespace],
        )
        .unwrap();
}

#[cfg(any(test, feature = "test-support"))]
#[path = "finding_review_migration_tests.rs"]
mod finding_review_migration_tests;

/// History-only compatibility does not authorize old prompts for live work.
fn saved_specialist_matches(task: &StoredTask) -> Result<bool, InvestigationStoreError> {
    super::specialist_history::matches(
        &task.detail.specialist,
        task.detail.summary.specialist_id,
        task.phase == Phase::Terminal,
    )
}
