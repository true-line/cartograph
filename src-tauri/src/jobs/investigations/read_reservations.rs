//! Source-read reservation records for the @10 boundary (AC-0287).
//!
//! Storage validation alone supplies no owner-loss proof, source access, task
//! transition, claim sizing certificate or public-family routing. Callers must authenticate saved task/response/ledger authority
//! and account these rows and outstanding claims in the same writer transaction.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const OWNED: &str = "name GLOB 'investigation_read_*' OR (tbl_name GLOB 'investigation_read_*' AND name NOT GLOB 'sqlite_*')";
const MAX_RESERVATIONS_PER_TASK: usize = 64;
const MAX_RESERVATIONS: usize = MAX_TASKS * MAX_RESERVATIONS_PER_TASK;
// Logical capacity allowances following the existing store's bookkeeping
// convention, not measured SQLite physical page/index usage. These estimates
// are not a proven upper bound on physical disk allocation.
const READ_ROW_ALLOWANCE_BYTES: usize = 256;
const READ_NAMESPACE_ALLOWANCE_BYTES: usize = 256;
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
    /// Frozen host-derived limits; absent historical bindings retain V2 bytes.
    /// Structural validity alone never authorizes a reservation against a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_limits: Option<InvestigationLimits>,
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
    /// Immutable usage at reservation admission, before its validation charge.
    /// Load validates this saved base, never substitutes current task usage.
    pub base_usage: InvestigationUsage,
    pub prior_completion_hash: String,
    pub certification_basis_hash: String,
}

/// Stage allocations, additional to the existing terminal byte/event reserves.
/// Shape validation is not sizing authority: admission derives these from the
/// production validation/settlement projections before storing a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageClaim {
    pub validation_bytes: usize,
    pub settlement_bytes: usize,
    pub validation_events: usize,
    pub settlement_events: usize,
    pub settlement_attempts: usize,
}
impl StorageClaim {
    pub(super) fn validate(&self) -> Result<(), InvestigationStoreError> {
        if self.validation_bytes == 0
            || self.settlement_bytes == 0
            || self.validation_events != 1
            || self.settlement_events != 2
            || self.settlement_attempts > INVESTIGATION_V2_POLICY.max_completed_attempts()
            || self.bytes(false)? > TASK_CAPACITY
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    pub(super) fn bytes(&self, charged: bool) -> Result<usize, InvestigationStoreError> {
        self.settlement_bytes
            .checked_add(if charged { 0 } else { self.validation_bytes })
            .ok_or(InvestigationStoreError::Capacity)
    }
    pub(super) fn events(&self, charged: bool) -> usize {
        self.settlement_events + if charged { 0 } else { self.validation_events }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reservation {
    pub schema_version: u32,
    pub binding: ReadBinding,
    /// Exact retained descriptor; public validation proves consistency only.
    pub source: InvestigationInspectionTarget,
    pub storage_claim: StorageClaim,
}
impl Reservation {
    pub(super) fn policy(&self) -> Result<InvestigationPolicy, InvestigationStoreError> {
        match &self.binding.effective_limits {
            Some(limits) => InvestigationPolicy::for_effective_limits(limits)
                .ok_or(InvestigationStoreError::Invalid),
            None => Ok(INVESTIGATION_V2_POLICY.clone()),
        }
    }

    pub(super) fn validate_task_policy(
        &self,
        detail: &InvestigationDetail,
    ) -> Result<(), InvestigationStoreError> {
        let policy = super::policy_for_detail(detail)?;
        let expected =
            (policy.limits() != INVESTIGATION_V2_POLICY.limits()).then(|| policy.limits());
        // V2 predates effective_limits and hosted tasks could write absent fields.
        // V3 writers bind every non-static policy; absence is not a legacy escape.
        if (self.schema_version == 3 && self.binding.effective_limits != expected)
            || self
                .binding
                .effective_limits
                .as_ref()
                .is_some_and(|limits| *limits != policy.limits())
            || ((policy.local_finish_generation() || policy.limits().history_policy.is_some())
                && self.binding.effective_limits.is_none())
        {
            return Err(InvestigationStoreError::Invalid);
        }
        self.validate()
    }

    pub(super) fn validate(&self) -> Result<(), InvestigationStoreError> {
        let b = &self.binding;
        let policy = self.policy()?;
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
        if !matches!(self.schema_version, 2 | 3)
            || !b.specialist_id.uses_next_prompt_admission()
            || policy.profile() != INVESTIGATION_V2_POLICY.profile()
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
            || b.original_action_hash != staged::read_action_hash(&b.original_read)?
            || b.resolved_read_hash != b.original_read.fingerprint()?
        {
            return Err(InvestigationStoreError::Invalid);
        }
        self.storage_claim.validate()?;
        self.source.validate()?;
        validate_usage(&b.base_usage, &policy.limits())?;
        // Reserve eventual tool debits against the immutable admission base.
        // Repeating this check on load must not use later current task usage.
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
        domain_hash(b"cartograph:source-read-reservation:v2\0", self)
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
    #[cfg(any(test, feature = "test-support"))]
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
/// Absent namespace preserves legacy autocommit metadata/capacity reads. A
/// present namespace is inspected only in a caller transaction, including its
/// exact DDL and singleton version. Never creates or repairs schema on a read.
pub(super) fn validate_optional_schema(
    connection: &Connection,
) -> Result<bool, InvestigationStoreError> {
    if namespace_count(connection)? == 0 {
        return Ok(false);
    }
    validate_schema(connection)?;
    Ok(true)
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
    // Bound aggregate history before allocating any body, including malformed
    // stores populated outside the supported writers. These are logical store
    // limits, not claims about SQLite physical file/page allocation.
    let distinct: i64 = connection.query_row(
        "SELECT count(DISTINCT task_id) FROM investigation_read_reservations",
        [],
        |row| row.get(0),
    )?;
    if usize::try_from(distinct).ok().is_none_or(|n| n > MAX_TASKS) {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut global = READ_NAMESPACE_ALLOWANCE_BYTES;
    let mut tasks = BTreeMap::<String, usize>::new();
    for table in TABLES {
        let mut statement = connection.prepare(&format!("SELECT task_id,sum(length(body)+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+{READ_ROW_ALLOWANCE_BYTES}) FROM {table} GROUP BY task_id"))?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (task, bytes) = row?;
            let bytes = usize::try_from(bytes).map_err(|_| InvestigationStoreError::Capacity)?;
            global = global
                .checked_add(bytes)
                .ok_or(InvestigationStoreError::Capacity)?;
            let task_bytes = tasks.entry(task).or_default();
            *task_bytes = task_bytes
                .checked_add(bytes)
                .ok_or(InvestigationStoreError::Capacity)?;
            if *task_bytes > TASK_CAPACITY || global > STORE_CAPACITY {
                return Err(InvestigationStoreError::Capacity);
            }
        }
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
/// Reads only immutable saved records; compatibility wrapper with a fresh
/// preflight scope per call. Retain ReadTransaction for repeated operations.
pub(super) fn load(
    connection: &Connection,
    task: &str,
    step: &str,
) -> Result<Option<SavedRead>, InvestigationStoreError> {
    ReadTransaction::compatible(connection)?.load(task, step)
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

/// The same logical row accounting used by preflight and capacity snapshots.
/// This does not estimate physical SQLite pages or allocate a storage claim.
pub(super) fn logical_row_bytes(
    task: &str,
    step: &str,
    value: &impl Serialize,
) -> Result<usize, InvestigationStoreError> {
    encode(value, MAX_MANIFEST_BYTES)?
        .len()
        .checked_add(64)
        .and_then(|n| n.checked_add(task.len()))
        .and_then(|n| n.checked_add(step.len()))
        .and_then(|n| n.checked_add(READ_ROW_ALLOWANCE_BYTES))
        .ok_or(InvestigationStoreError::Capacity)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MutationStamp {
    changes: u64,
    schema_version: i64,
}
impl MutationStamp {
    fn read(connection: &Connection) -> Result<Self, InvestigationStoreError> {
        Ok(Self {
            changes: connection.total_changes(),
            schema_version: connection.query_row("PRAGMA schema_version", [], |row| row.get(0))?,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CapacityCharge {
    pub rows: usize,
    pub claims: usize,
    pub event_claims: usize,
}
impl CapacityCharge {
    pub(super) fn occupied(self) -> Result<usize, InvestigationStoreError> {
        self.rows
            .checked_add(self.claims)
            .ok_or(InvestigationStoreError::Capacity)
    }
}
#[derive(Default)]
struct CapacitySnapshot {
    global: CapacityCharge,
    tasks: BTreeMap<String, CapacityCharge>,
}

/// Transaction-borrowed preflight and coherent accounting scope. No Clone,
/// Deserialize, owned cache export or connection accessor. A fresh transaction
/// gets a fresh handle and revalidates historical settlements: O(history),
/// bounded by logical store caps, once per unchanged handle snapshot. Body hashes
/// and bindings fail closed on corrupt/inconsistent rows; they do not detect a
/// consistent forgery by a DDL-capable local writer.
///
/// Controlled writes invalidate first. total_changes also detects direct SQL
/// writes (including rolled-back changes), and schema_version detects DDL. This
/// deliberately invalidates on unrelated writes too. Another connection cannot
/// alter this transaction's pinned read snapshot. Callers must not manually end
/// and restart transactions through raw SQL while a handle is borrowed.
pub(super) struct ReadTransaction<'tx> {
    connection: &'tx Connection,
    stamp: Option<MutationStamp>,
    present: bool,
    capacity: Option<CapacitySnapshot>,
    #[cfg(any(test, feature = "test-support"))]
    preflight_runs: usize,
    #[cfg(any(test, feature = "test-support"))]
    snapshot_runs: usize,
    #[cfg(any(test, feature = "test-support"))]
    validated_reads: usize,
}
impl<'tx> ReadTransaction<'tx> {
    pub(super) fn new(
        transaction: &'tx rusqlite::Transaction<'_>,
    ) -> Result<Self, InvestigationStoreError> {
        require_transaction(transaction)?;
        Self::compatible(transaction)
    }
    /// Only compatibility helpers use this constructor. They preserve old
    /// absent-namespace autocommit metadata reads and drop the handle per call.
    fn compatible(connection: &'tx Connection) -> Result<Self, InvestigationStoreError> {
        let mut value = Self {
            connection,
            stamp: None,
            present: false,
            capacity: None,
            #[cfg(any(test, feature = "test-support"))]
            preflight_runs: 0,
            #[cfg(any(test, feature = "test-support"))]
            snapshot_runs: 0,
            #[cfg(any(test, feature = "test-support"))]
            validated_reads: 0,
        };
        value.ensure_preflight()?;
        Ok(value)
    }
    pub(super) fn matches_connection(&self, connection: &Connection) -> bool {
        std::ptr::eq(self.connection, connection)
    }
    fn invalidate(&mut self) {
        self.stamp = None;
        self.capacity = None;
    }
    fn ensure_preflight(&mut self) -> Result<(), InvestigationStoreError> {
        let stamp = MutationStamp::read(self.connection)?;
        if self.stamp == Some(stamp) {
            if self.present {
                require_transaction(self.connection)?;
            }
            return Ok(());
        }
        self.invalidate();
        self.present = namespace_count(self.connection)? != 0;
        if self.present {
            require_transaction(self.connection)?;
            #[cfg(any(test, feature = "test-support"))]
            {
                self.preflight_runs += 1;
            }
            preflight(self.connection)?;
        }
        self.stamp = Some(stamp);
        Ok(())
    }
    pub(super) fn initialize(&mut self) -> Result<(), InvestigationStoreError> {
        self.invalidate();
        initialize(self.connection)
    }
    pub(super) fn load(
        &mut self,
        task: &str,
        step: &str,
    ) -> Result<Option<SavedRead>, InvestigationStoreError> {
        self.ensure_preflight()?;
        if !self.present {
            return Err(InvestigationStoreError::Missing);
        }
        load_validated(self.connection, task, step)
    }
    /// Writes no task usage/phase, capacity admission, lease, journal event or
    /// saved-response authority. Caller performs those atomically in this Tx.
    pub(super) fn insert_reservation(
        &mut self,
        r: &Reservation,
    ) -> Result<(), InvestigationStoreError> {
        self.ensure_preflight()?;
        if !self.present {
            return Err(InvestigationStoreError::Missing);
        }
        r.validate()?;
        let b = &r.binding;
        let (count, pending): (i64, bool) = self.connection.query_row(
            "SELECT count(*), EXISTS(SELECT 1 FROM investigation_read_reservations r LEFT JOIN investigation_read_dispositions d USING(task_id,step_id) WHERE r.task_id=?1 AND d.task_id IS NULL) FROM investigation_read_reservations WHERE task_id=?1",
            [&b.task_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if pending {
            return Err(InvestigationStoreError::Conflict);
        }
        let global: i64 = self.connection.query_row(
            "SELECT count(*) FROM investigation_read_reservations",
            [],
            |row| row.get(0),
        )?;
        if usize::try_from(global)
            .ok()
            .is_none_or(|n| n >= MAX_RESERVATIONS)
            || usize::try_from(count)
                .ok()
                .is_none_or(|n| n >= MAX_RESERVATIONS_PER_TASK)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        self.invalidate();
        insert(self.connection, TABLES[0], &b.task_id, &b.step_id, r)
    }
    pub(super) fn insert_charge(
        &mut self,
        task: &str,
        step: &str,
        charge: &ValidationCharge,
    ) -> Result<(), InvestigationStoreError> {
        let saved = self
            .load(task, step)?
            .ok_or(InvestigationStoreError::Missing)?;
        if saved.disposition.is_some() {
            return Err(InvestigationStoreError::Conflict);
        }
        charge.validate(&saved.reservation)?;
        self.invalidate();
        insert(self.connection, TABLES[1], task, step, charge)
    }
    /// Kind is a caller attestation, not proof of read/no-access/owner loss.
    pub(super) fn insert_disposition(
        &mut self,
        task: &str,
        step: &str,
        disposition: &Disposition,
    ) -> Result<(), InvestigationStoreError> {
        let saved = self
            .load(task, step)?
            .ok_or(InvestigationStoreError::Missing)?;
        disposition.validate(&saved.reservation, saved.charge.as_ref())?;
        self.invalidate();
        insert(self.connection, TABLES[2], task, step, disposition)
    }
    fn derive_capacity(&mut self) -> Result<CapacitySnapshot, InvestigationStoreError> {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.snapshot_runs += 1;
        }
        let mut snapshot = CapacitySnapshot::default();
        if !self.present {
            return Ok(snapshot);
        }
        snapshot.global.rows = READ_NAMESPACE_ALLOWANCE_BYTES;
        for table in TABLES {
            let mut statement = self.connection.prepare(&format!("SELECT task_id,sum(length(body)+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(step_id AS BLOB))+{READ_ROW_ALLOWANCE_BYTES}) FROM {table} GROUP BY task_id"))?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (task, bytes) = row?;
                let bytes =
                    usize::try_from(bytes).map_err(|_| InvestigationStoreError::Capacity)?;
                let task_rows = &mut snapshot.tasks.entry(task).or_default().rows;
                *task_rows = task_rows
                    .checked_add(bytes)
                    .ok_or(InvestigationStoreError::Capacity)?;
                snapshot.global.rows = snapshot
                    .global
                    .rows
                    .checked_add(bytes)
                    .ok_or(InvestigationStoreError::Capacity)?;
            }
        }
        // Do not trust disposition row presence to release a claim. Validate
        // every settlement once in this snapshot; a pending-only index awaits
        // the exact task3 pending-state integration.
        let mut statement = self.connection.prepare(
            "SELECT task_id,step_id FROM investigation_read_reservations ORDER BY task_id,step_id",
        )?;
        let mut pending_tasks = BTreeSet::new();
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (task, step) = row?;
            let saved = load_validated(self.connection, &task, &step)?
                .ok_or(InvestigationStoreError::Invalid)?;
            #[cfg(any(test, feature = "test-support"))]
            {
                self.validated_reads += 1;
            }
            if saved.disposition.is_none() {
                if !pending_tasks.insert(task.clone()) {
                    return Err(InvestigationStoreError::Invalid);
                }
                let bytes = saved
                    .reservation
                    .storage_claim
                    .bytes(saved.charge.is_some())?;
                let events = saved
                    .reservation
                    .storage_claim
                    .events(saved.charge.is_some());
                snapshot.tasks.entry(task.clone()).or_default().event_claims = events;
                snapshot.global.event_claims = snapshot
                    .global
                    .event_claims
                    .checked_add(events)
                    .ok_or(InvestigationStoreError::Capacity)?;
                snapshot.tasks.entry(task).or_default().claims = bytes;
                snapshot.global.claims = snapshot
                    .global
                    .claims
                    .checked_add(bytes)
                    .ok_or(InvestigationStoreError::Capacity)?;
            }
        }
        Ok(snapshot)
    }
    pub(super) fn capacity_charge(
        &mut self,
        task: Option<&str>,
    ) -> Result<CapacityCharge, InvestigationStoreError> {
        self.ensure_preflight()?;
        if self.capacity.is_none() {
            self.capacity = Some(self.derive_capacity()?);
        }
        let snapshot = self
            .capacity
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        Ok(match task {
            None => snapshot.global,
            Some(task) => snapshot.tasks.get(task).copied().unwrap_or_default(),
        })
    }
}

/// Compatibility scopes cannot export an owned handle/cache. Their fresh
/// preflight is intentionally per call; retain new(&Transaction) in new code.
pub(super) fn with_compatibility_scope<T>(
    connection: &Connection,
    body: impl for<'scope> FnOnce(&mut ReadTransaction<'scope>) -> Result<T, InvestigationStoreError>,
) -> Result<T, InvestigationStoreError> {
    let mut reads = ReadTransaction::compatible(connection)?;
    body(&mut reads)
}

/// Compatibility wrappers create a fresh transaction-bound scope per call.
/// Production atomic transitions retain ReadTransaction across operations.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn insert_reservation(
    connection: &Connection,
    r: &Reservation,
) -> Result<(), InvestigationStoreError> {
    ReadTransaction::compatible(connection)?.insert_reservation(r)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn insert_charge(
    connection: &Connection,
    task: &str,
    step: &str,
    charge: &ValidationCharge,
) -> Result<(), InvestigationStoreError> {
    ReadTransaction::compatible(connection)?.insert_charge(task, step, charge)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn insert_disposition(
    connection: &Connection,
    task: &str,
    step: &str,
    disposition: &Disposition,
) -> Result<(), InvestigationStoreError> {
    ReadTransaction::compatible(connection)?.insert_disposition(task, step, disposition)
}
/// Actual logical row charges only. Shared storage adds rows and claims once.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn stored_bytes(
    connection: &Connection,
    task: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    Ok(ReadTransaction::compatible(connection)?
        .capacity_charge(task)?
        .rows)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn pending_storage_claims(
    connection: &Connection,
    task: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    Ok(ReadTransaction::compatible(connection)?
        .capacity_charge(task)?
        .claims)
}

pub(super) fn pending_event_claims(
    connection: &Connection,
    task: &str,
) -> Result<usize, InvestigationStoreError> {
    Ok(ReadTransaction::compatible(connection)?
        .capacity_charge(Some(task))?
        .event_claims)
}

/// Audit deltas for one task, including permanently retained unknown debits.
/// This is not saved task usage reconciliation or an owner-loss proof.
#[cfg(any(test, feature = "test-support"))]
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

#[cfg(any(test, feature = "test-support"))]
#[path = "read_reservation_tests.rs"]
mod tests;

#[cfg(unix)]
#[path = "source_replay_workspace.rs"]
pub(super) mod workspace;
