//! Source-read reservation records for the proposed @10 boundary (AC-0287).
//!
//! Isolated storage foundation, not runtime admission: no owner-loss proof, source
//! access, task transition, claim sizing certificate or public-family routing is
//! supplied here. Callers must authenticate saved task/response/ledger authority
//! and account these rows and outstanding claims in the same writer transaction.
use super::*;

const OWNED: &str = "name GLOB 'investigation_read_*' OR (tbl_name GLOB 'investigation_read_*' AND name NOT GLOB 'sqlite_*')";
const MAX_RESERVATIONS_PER_TASK: usize = 64;
const MAX_RESERVATIONS: usize = MAX_TASKS * MAX_RESERVATIONS_PER_TASK;
const TABLES: [&str; 3] = [
    "investigation_read_reservations",
    "investigation_read_validation_charges",
    "investigation_read_dispositions",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadBinding {
    pub task_id: String,
    pub step_id: String,
    pub request_hash: String,
    pub specialist_id: SpecialistId,
    pub policy_profile: String,
    pub execution_job: i64,
    pub execution_generation: i64,
    /// Digest only; never persist the execution owner token in these bodies.
    pub owner_basis_hash: String,
    pub original_response_hash: String,
    pub original_action_hash: String,
    pub original_read: ReadIntent,
    pub resolved_read_hash: String,
    pub base_ledger_hash: String,
    pub base_ledger_revision: u64,
    pub context_id: String,
    pub archive_hash: String,
    pub base_usage: InvestigationUsage,
    pub prior_completion_hash: String,
    pub certification_basis_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reservation {
    pub schema_version: u32,
    pub binding: ReadBinding,
    /// Exact retained descriptor; public validation proves consistency only.
    pub source: InvestigationInspectionTarget,
    /// Checked by admission against a production-derived maximal settlement
    /// plan. This format validator bounds it; it does not certify that plan.
    pub storage_claim_bytes: usize,
}
impl Reservation {
    pub(super) fn validate(&self) -> Result<(), InvestigationStoreError> {
        let b = &self.binding;
        let policy = &INVESTIGATION_V2_POLICY;
        let hashes = [
            &b.request_hash,
            &b.owner_basis_hash,
            &b.original_response_hash,
            &b.original_action_hash,
            &b.resolved_read_hash,
            &b.base_ledger_hash,
            &b.archive_hash,
            &b.prior_completion_hash,
            &b.certification_basis_hash,
        ];
        let read_matches = matches!(&b.original_read,
            ReadIntent::Source { fact, role, index }
            if fact == &self.source.fact && role == &self.source.role && index == &self.source.index);
        if self.schema_version != 1
            || !b.specialist_id.uses_next_prompt_admission()
            || b.policy_profile != policy.profile()
            || !id_valid(&b.task_id)
            || !id_valid(&b.step_id)
            || !b
                .context_id
                .strip_prefix("context-v2:")
                .is_some_and(hash_valid)
            || b.execution_job <= 0
            || b.execution_generation <= 0
            || i64::try_from(b.base_ledger_revision).is_err()
            || hashes.iter().any(|h| !hash_valid(h))
            || !read_matches
            || self.storage_claim_bytes == 0
            || self.storage_claim_bytes > TASK_CAPACITY
            || b.original_action_hash != staged::read_action_hash(&b.original_read)?
            || b.resolved_read_hash != b.original_read.fingerprint()?
        {
            return Err(InvestigationStoreError::Invalid);
        }
        self.source.validate()?;
        validate_usage(&b.base_usage, &policy.limits())?;
        // Reserve eventual tool debits before accepting the source action.
        if b.base_usage.tool_actions >= policy.limits().tool_actions
            || b.base_usage.evidence_requests >= policy.limits().evidence_requests
            || b.base_usage
                .captured_validation_bytes
                .checked_add(self.source.captured.file.byte_len)
                .is_none_or(|n| n > policy.limits().captured_validation_bytes)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        encode(self, MAX_MANIFEST_BYTES)?;
        Ok(())
    }
    pub(super) fn fingerprint(&self) -> Result<String, InvestigationStoreError> {
        self.validate()?;
        domain_hash(b"cartograph:source-read-reservation:v1\0", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ValidationCharge {
    pub schema_version: u32,
    pub reservation_hash: String,
    pub source_descriptor_hash: String,
    pub validation_bytes: u64,
}
impl ValidationCharge {
    pub(super) fn for_reservation(r: &Reservation) -> Result<Self, InvestigationStoreError> {
        Ok(Self {
            schema_version: 1,
            reservation_hash: r.fingerprint()?,
            source_descriptor_hash: r.source.content_id()?,
            validation_bytes: r.source.captured.file.byte_len,
        })
    }
    pub(super) fn validate(&self, r: &Reservation) -> Result<(), InvestigationStoreError> {
        if self != &Self::for_reservation(r)? {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    pub(super) fn fingerprint(&self) -> Result<String, InvestigationStoreError> {
        domain_hash(b"cartograph:source-read-validation-charge:v1\0", self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DispositionKind {
    Acquired,
    ReadUnavailable,
    ReleasedBeforeRead,
    KnownReadDiscarded,
    BoundInvariantDefect,
    OutcomeUnknown,
}

/// Deltas relative to the reservation's saved base_usage. Unknown is neither a
/// released slot nor a claim that source access actually executed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadAccounting {
    pub executed_tool_actions: u32,
    pub executed_evidence_requests: u32,
    pub reserved_tool_actions: u32,
    pub reserved_evidence_requests: u32,
    pub reserved_unknown_tool_actions: u32,
    pub reserved_unknown_evidence_requests: u32,
    pub validation_bytes: u64,
}
impl ReadAccounting {
    fn derive(kind: Option<DispositionKind>, owner_lost: bool, bytes: u64) -> Self {
        let mut value = Self {
            validation_bytes: bytes,
            ..Self::default()
        };
        match kind {
            Some(DispositionKind::ReleasedBeforeRead) => {}
            Some(DispositionKind::OutcomeUnknown) => {
                value.reserved_unknown_tool_actions = 1;
                value.reserved_unknown_evidence_requests = 1;
            }
            Some(_) => {
                value.executed_tool_actions = 1;
                value.executed_evidence_requests = 1;
            }
            None if owner_lost => {
                value.reserved_unknown_tool_actions = 1;
                value.reserved_unknown_evidence_requests = 1;
            }
            None => {
                value.reserved_tool_actions = 1;
                value.reserved_evidence_requests = 1;
            }
        }
        value
    }
    fn add(&mut self, rhs: &Self) -> Result<(), InvestigationStoreError> {
        macro_rules! add { ($($field:ident),*) => { $(self.$field = self.$field.checked_add(rhs.$field)
            .ok_or(InvestigationStoreError::Capacity)?;)* }; }
        add!(
            executed_tool_actions,
            executed_evidence_requests,
            reserved_tool_actions,
            reserved_evidence_requests,
            reserved_unknown_tool_actions,
            reserved_unknown_evidence_requests,
            validation_bytes
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Disposition {
    pub schema_version: u32,
    pub reservation_hash: String,
    pub validation_charge_hash: Option<String>,
    pub kind: DispositionKind,
    /// Explicitly retained even after the live slot/storage claim is freed.
    pub accounting: ReadAccounting,
}
impl Disposition {
    pub(super) fn for_reservation(
        r: &Reservation,
        charge: Option<&ValidationCharge>,
        kind: DispositionKind,
    ) -> Result<Self, InvestigationStoreError> {
        if let Some(c) = charge {
            c.validate(r)?;
        }
        if charge.is_none()
            && !matches!(
                kind,
                DispositionKind::ReleasedBeforeRead | DispositionKind::OutcomeUnknown
            )
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(Self {
            schema_version: 1,
            reservation_hash: r.fingerprint()?,
            validation_charge_hash: charge.map(ValidationCharge::fingerprint).transpose()?,
            kind,
            accounting: ReadAccounting::derive(
                Some(kind),
                false,
                charge.map_or(0, |c| c.validation_bytes),
            ),
        })
    }
    pub(super) fn validate(
        &self,
        r: &Reservation,
        charge: Option<&ValidationCharge>,
    ) -> Result<(), InvestigationStoreError> {
        if self != &Self::for_reservation(r, charge, self.kind)? {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

/// Owner-loss input is a caller attestation backed by the OS execution lock, not
/// a boolean that establishes loss here. No projection writes or resumes work.
pub(super) fn project_accounting(
    r: &Reservation,
    charge: Option<&ValidationCharge>,
    disposition: Option<&Disposition>,
    owner_lost: bool,
) -> Result<ReadAccounting, InvestigationStoreError> {
    r.validate()?;
    if let Some(c) = charge {
        c.validate(r)?;
    }
    if let Some(d) = disposition {
        d.validate(r, charge)?;
        return Ok(d.accounting.clone());
    }
    Ok(ReadAccounting::derive(
        None,
        owner_lost,
        charge.map_or(0, |c| c.validation_bytes),
    ))
}

fn domain_hash(domain: &[u8], value: &impl Serialize) -> Result<String, InvestigationStoreError> {
    let mut bytes = domain.to_vec();
    bytes.extend(encode(value, MAX_MANIFEST_BYTES)?);
    Ok(core_prov::content_hash(&bytes))
}

pub(super) fn schema() -> Vec<(String, String, String)> {
    let mut objects = vec![("investigation_read_meta".into(), "table".into(),
        "CREATE TABLE investigation_read_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1)) STRICT".into())];
    for table in TABLES {
        let foreign = if table == TABLES[0] {
            "FOREIGN KEY(task_id) REFERENCES investigation_tasks(id)"
        } else {
            "FOREIGN KEY(task_id,step_id) REFERENCES investigation_read_reservations(task_id,step_id)"
        };
        objects.push((table.into(), "table".into(), format!("CREATE TABLE {table} (task_id TEXT NOT NULL CHECK(length(CAST(task_id AS BLOB)) BETWEEN 1 AND 128), step_id TEXT NOT NULL CHECK(length(CAST(step_id AS BLOB)) BETWEEN 1 AND 128), body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND 131072), body_hash TEXT NOT NULL CHECK(length(CAST(body_hash AS BLOB))=64), PRIMARY KEY(task_id,step_id), {foreign}) STRICT")));
        for verb in ["UPDATE", "DELETE"] {
            let name = format!("{table}_no_{}", verb.to_lowercase());
            objects.push((name.clone(), "trigger".into(), format!("CREATE TRIGGER {name} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable investigation read record'); END")));
        }
    }
    objects
}

/// Explicit additive namespace creation only. Caller owns migration/transaction;
/// current global investigation schema must be integrated before runtime use.
pub(super) fn initialize(connection: &Connection) -> Result<(), InvestigationStoreError> {
    require_transaction(connection)?;
    if namespace_count(connection)? == 0 {
        for (_, _, sql) in schema() {
            connection.execute_batch(&sql)?;
        }
        one(connection.execute("INSERT INTO investigation_read_meta VALUES (1,1)", [])?)?;
    }
    validate_schema(connection)
}
fn namespace_count(connection: &Connection) -> Result<i64, InvestigationStoreError> {
    Ok(connection.query_row(
        &format!("SELECT count(*) FROM sqlite_schema WHERE {OWNED}"),
        [],
        |row| row.get(0),
    )?)
}
pub(super) fn validate_schema(connection: &Connection) -> Result<(), InvestigationStoreError> {
    require_transaction(connection)?;
    let expected = schema();
    let (count, bounded): (i64, bool) = connection.query_row(&format!(
        "SELECT count(*), coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=128 AND typeof(type)='text' AND length(CAST(type AS BLOB))<=16 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),0) FROM sqlite_schema WHERE {OWNED}"),
        [], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if usize::try_from(count).ok() != Some(expected.len()) || !bounded {
        return Err(InvestigationStoreError::Invalid);
    }
    let actual = connection
        .prepare(&format!(
            "SELECT name,type,sql FROM sqlite_schema WHERE {OWNED}"
        ))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if expected.iter().any(|item| !actual.contains(item)) {
        return Err(InvestigationStoreError::Invalid);
    }
    let (rows, valid): (i64, bool) = connection.query_row(
        "SELECT count(*), coalesce(min(singleton=1 AND version=1),0) FROM investigation_read_meta",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if rows != 1 || !valid {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
fn require_transaction(connection: &Connection) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() {
        Err(InvestigationStoreError::Invalid)
    } else {
        Ok(())
    }
}

fn preflight(connection: &Connection) -> Result<(), InvestigationStoreError> {
    validate_schema(connection)?;
    for table in TABLES {
        let (count, valid, per_task): (i64, bool, i64) = connection.query_row(&format!(
            "SELECT count(*), coalesce(min(typeof(task_id)='text' AND length(CAST(task_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(step_id)='text' AND length(CAST(step_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(body)='blob' AND length(body) BETWEEN 1 AND 131072 AND typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64),1), (SELECT coalesce(max(n),0) FROM (SELECT count(*) AS n FROM {table} GROUP BY task_id)) FROM {table}"),
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        if !valid
            || usize::try_from(count)
                .ok()
                .is_none_or(|n| n > MAX_RESERVATIONS)
            || usize::try_from(per_task)
                .ok()
                .is_none_or(|n| n > MAX_RESERVATIONS_PER_TASK)
        {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    let orphan: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM investigation_read_reservations r LEFT JOIN investigation_tasks t ON t.id=r.task_id WHERE t.id IS NULL) OR EXISTS(SELECT 1 FROM investigation_read_validation_charges c LEFT JOIN investigation_read_reservations r USING(task_id,step_id) WHERE r.task_id IS NULL) OR EXISTS(SELECT 1 FROM investigation_read_dispositions d LEFT JOIN investigation_read_reservations r USING(task_id,step_id) WHERE r.task_id IS NULL)",
        [], |row| row.get(0))?;
    if orphan {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

fn read<T: serde::de::DeserializeOwned + Serialize>(
    connection: &Connection,
    table: &str,
    task: &str,
    step: &str,
) -> Result<Option<T>, InvestigationStoreError> {
    let row = connection.query_row(&format!("SELECT CASE WHEN typeof(body)='blob' AND length(body) BETWEEN 1 AND 131072 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM {table} WHERE task_id=?1 AND step_id=?2"),
        params![task, step], |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Option<String>>(1)?))).optional()?;
    let Some((Some(body), Some(hash))) = row else {
        return if row.is_some() {
            Err(InvestigationStoreError::Invalid)
        } else {
            Ok(None)
        };
    };
    if !hash_valid(&hash) || core_prov::content_hash(&body) != hash {
        return Err(InvestigationStoreError::Invalid);
    }
    let value: T = serde_json::from_slice(&body).map_err(|_| InvestigationStoreError::Invalid)?;
    if encode(&value, MAX_MANIFEST_BYTES)? != body {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(Some(value))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SavedRead {
    pub reservation: Reservation,
    pub charge: Option<ValidationCharge>,
    pub disposition: Option<Disposition>,
}
/// Reads only immutable saved records; never task_text or the current graph.
pub(super) fn load(
    connection: &Connection,
    task: &str,
    step: &str,
) -> Result<Option<SavedRead>, InvestigationStoreError> {
    preflight(connection)?;
    load_validated(connection, task, step)
}
fn load_validated(
    connection: &Connection,
    task: &str,
    step: &str,
) -> Result<Option<SavedRead>, InvestigationStoreError> {
    if !id_valid(task) || !id_valid(step) {
        return Err(InvestigationStoreError::Invalid);
    }
    let r: Option<Reservation> = read(connection, TABLES[0], task, step)?;
    let charge: Option<ValidationCharge> = read(connection, TABLES[1], task, step)?;
    let disposition: Option<Disposition> = read(connection, TABLES[2], task, step)?;
    let Some(reservation) = r else {
        return if charge.is_some() || disposition.is_some() {
            Err(InvestigationStoreError::Invalid)
        } else {
            Ok(None)
        };
    };
    if reservation.binding.task_id != task || reservation.binding.step_id != step {
        return Err(InvestigationStoreError::Invalid);
    }
    project_accounting(&reservation, charge.as_ref(), disposition.as_ref(), false)?;
    Ok(Some(SavedRead {
        reservation,
        charge,
        disposition,
    }))
}

fn insert(
    connection: &Connection,
    table: &str,
    task: &str,
    step: &str,
    value: &impl Serialize,
) -> Result<(), InvestigationStoreError> {
    let exists: bool = connection.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE task_id=?1 AND step_id=?2)"),
        params![task, step],
        |row| row.get(0),
    )?;
    if exists {
        return Err(InvestigationStoreError::Conflict);
    }
    let body = encode(value, MAX_MANIFEST_BYTES)?;
    let hash = core_prov::content_hash(&body);
    one(connection.execute(
        &format!("INSERT INTO {table}(task_id,step_id,body,body_hash) VALUES (?1,?2,?3,?4)"),
        params![task, step, body, hash],
    )?)
}
/// Writes a validated record only. Does not update task usage/phase, capacity,
/// execution, journal events or immutable response authority; caller must do all
/// those atomically. At most one pending source reservation per task.
pub(super) fn insert_reservation(
    connection: &Connection,
    r: &Reservation,
) -> Result<(), InvestigationStoreError> {
    preflight(connection)?;
    r.validate()?;
    let b = &r.binding;
    let (count, pending): (i64, bool) = connection.query_row(
        "SELECT count(*), EXISTS(SELECT 1 FROM investigation_read_reservations r LEFT JOIN investigation_read_dispositions d USING(task_id,step_id) WHERE r.task_id=?1 AND d.task_id IS NULL) FROM investigation_read_reservations WHERE task_id=?1",
        [&b.task_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if pending {
        return Err(InvestigationStoreError::Conflict);
    }
    let global: i64 = connection.query_row(
        "SELECT count(*) FROM investigation_read_reservations",
        [],
        |row| row.get(0),
    )?;
    if usize::try_from(global)
        .ok()
        .is_none_or(|n| n >= MAX_RESERVATIONS)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    if usize::try_from(count)
        .ok()
        .is_none_or(|n| n >= MAX_RESERVATIONS_PER_TASK)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    insert(connection, TABLES[0], &b.task_id, &b.step_id, r)
}
pub(super) fn insert_charge(
    connection: &Connection,
    task: &str,
    step: &str,
    charge: &ValidationCharge,
) -> Result<(), InvestigationStoreError> {
    let saved = load(connection, task, step)?.ok_or(InvestigationStoreError::Missing)?;
    if saved.disposition.is_some() {
        return Err(InvestigationStoreError::Conflict);
    }
    charge.validate(&saved.reservation)?;
    insert(connection, TABLES[1], task, step, charge)
}
/// Caller proves actual read/no-access/owner-loss disposition authority. A kind
/// tag alone never establishes that source access did or did not happen.
pub(super) fn insert_disposition(
    connection: &Connection,
    task: &str,
    step: &str,
    disposition: &Disposition,
) -> Result<(), InvestigationStoreError> {
    let saved = load(connection, task, step)?.ok_or(InvestigationStoreError::Missing)?;
    disposition.validate(&saved.reservation, saved.charge.as_ref())?;
    insert(connection, TABLES[2], task, step, disposition)
}

/// Actual stored rows only. Root integration must add this to ALL subsequent
/// task/global admission, and add pending_storage_claims separately exactly once.
pub(super) fn stored_bytes(
    connection: &Connection,
    task: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    require_transaction(connection)?;
    if namespace_count(connection)? == 0 {
        return Ok(0);
    }
    preflight(connection)?;
    let mut bytes = 256usize; // schema metadata allowance, charged once globally
    if task.is_some() {
        bytes = 0;
    }
    for table in TABLES {
        let sum: i64 = connection.query_row(&format!("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+256),0) FROM {table} WHERE (?1 IS NULL OR task_id=?1)"),
            [task], |row| row.get(0))?;
        bytes = bytes
            .checked_add(usize::try_from(sum).map_err(|_| InvestigationStoreError::Capacity)?)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    Ok(bytes)
}

/// Validate each bounded saved body before trusting its claim. Settled unknown
/// retains unknown tool debits but releases the storage claim, just like all
/// other durable dispositions. No independent mutable claim counter can drift.
pub(super) fn pending_storage_claims(
    connection: &Connection,
    task: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    require_transaction(connection)?;
    if namespace_count(connection)? == 0 {
        return Ok(0);
    }
    preflight(connection)?;
    let mut keys = connection.prepare("SELECT task_id,step_id FROM investigation_read_reservations WHERE (?1 IS NULL OR task_id=?1) ORDER BY task_id,step_id")?;
    let rows = keys.query_map([task], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut total = 0usize;
    let mut prior_task = None;
    for row in rows {
        let (id, step) = row?;
        let saved =
            load_validated(connection, &id, &step)?.ok_or(InvestigationStoreError::Invalid)?;
        if saved.disposition.is_none() {
            if prior_task.as_ref() == Some(&id) {
                return Err(InvestigationStoreError::Invalid);
            }
            prior_task = Some(id.clone());
            total = total
                .checked_add(saved.reservation.storage_claim_bytes)
                .ok_or(InvestigationStoreError::Capacity)?;
        }
    }
    Ok(total)
}

/// Audit deltas for one task, including permanently retained unknown debits.
/// This is not saved task usage reconciliation or an owner-loss proof.
pub(super) fn task_accounting(
    connection: &Connection,
    task: &str,
    owner_lost: bool,
) -> Result<ReadAccounting, InvestigationStoreError> {
    preflight(connection)?;
    if !id_valid(task) {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut keys = connection.prepare(
        "SELECT step_id FROM investigation_read_reservations WHERE task_id=?1 ORDER BY step_id",
    )?;
    let mut total = ReadAccounting::default();
    for step in keys.query_map([task], |row| row.get::<_, String>(0))? {
        let saved =
            load_validated(connection, task, &step?)?.ok_or(InvestigationStoreError::Invalid)?;
        total.add(&project_accounting(
            &saved.reservation,
            saved.charge.as_ref(),
            saved.disposition.as_ref(),
            owner_lost,
        )?)?;
    }
    Ok(total)
}

#[cfg(test)]
#[path = "read_reservation_tests.rs"]
mod tests;
