//! Immutable task-keyed qualified coverage archives (AC-0239).
//! Helpers use the caller's transaction and never consult the current graph.
use super::*;

// Exact per-row charge in storage::stored_bytes: body_hash plus scalar/index allowance.
const ARCHIVE_ROW_BYTES: usize = 64 + 256;

fn available_archive_bytes(
    connection: &Connection,
    task_id: &str,
) -> Result<usize, InvestigationStoreError> {
    remaining_task_capacity(connection, task_id)?
        .min(remaining_store_capacity(connection)?)
        .checked_sub(ARCHIVE_ROW_BYTES)
        .filter(|bytes| *bytes != 0)
        .map(|bytes| bytes.min(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES))
        .ok_or(InvestigationStoreError::Capacity)
}

impl JobStore {
    /// Read-only preparation ceiling; publication rechecks capacity transactionally.
    /// This measures stored lengths without copying any stored bodies.
    pub(crate) fn investigation_coverage_archive_capacity(
        &self,
        task_id: &str,
    ) -> Result<usize, InvestigationStoreError> {
        if !id_valid(task_id) {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM investigation_tasks WHERE id=?1)",
            [task_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(InvestigationStoreError::Missing);
        }
        let capacity = available_archive_bytes(&tx, task_id)?;
        tx.commit()?;
        Ok(capacity)
    }
}

pub(super) fn schema() -> Vec<(String, String, String)> {
    let mut objects = vec![
        ("investigation_coverage_meta".into(), "table".into(), "CREATE TABLE investigation_coverage_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1)) STRICT".into()),
        ("investigation_coverage_archives".into(), "table".into(), "CREATE TABLE investigation_coverage_archives (task_id TEXT PRIMARY KEY NOT NULL REFERENCES investigation_tasks(id), body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND 16777216), body_hash TEXT NOT NULL CHECK(length(CAST(body_hash AS BLOB))=64)) STRICT".into()),
    ];
    for verb in ["UPDATE", "DELETE"] {
        objects.push((
            format!("investigation_coverage_archives_no_{}", verb.to_lowercase()),
            "trigger".into(),
            format!("CREATE TRIGGER investigation_coverage_archives_no_{} BEFORE {verb} ON investigation_coverage_archives BEGIN SELECT RAISE(ABORT, 'immutable investigation coverage archive'); END", verb.to_lowercase()),
        ));
    }
    objects
}

pub(super) fn validate_version(connection: &Connection) -> Result<(), InvestigationStoreError> {
    let (rows, valid): (i64, bool) = connection.query_row(
        "SELECT count(*), coalesce(min(singleton=1 AND version=1),0) FROM investigation_coverage_meta",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if rows != 1 || !valid {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

/// Insert once, inside the same caller-owned transaction as InputPrepared.
/// The archive's 16 MiB format ceiling does not widen the task's existing 2 MiB
/// cap or its terminal reserve. Capacity is checked before the single write.
/// Caller remains responsible for checking execution and binding the ledger.
pub(super) fn insert(
    connection: &Connection,
    task_id: &str,
    archive: &InvestigationCoverageArchive,
) -> Result<(), InvestigationStoreError> {
    require_transaction(connection, task_id)?;
    validate_archive_schema(connection)?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM investigation_tasks WHERE id=?1)",
        [task_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(InvestigationStoreError::Missing);
    }
    let duplicate: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM investigation_coverage_archives WHERE task_id=?1)",
        [task_id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(InvestigationStoreError::Conflict);
    }
    archive.validate()?;
    let body_limit = available_archive_bytes(connection, task_id)?;
    let body = encode(archive, body_limit)?;
    let hash = core_prov::content_hash(&body);
    one(connection.execute(
        "INSERT INTO investigation_coverage_archives(task_id,body,body_hash) VALUES (?1,?2,?3)",
        params![task_id, body, hash],
    )?)
}

/// Read this task's original archive. The caller's read/write transaction keeps
/// schema, type/length preflight, capacity admission and body/hash on one revision.
pub(super) fn load(
    connection: &Connection,
    task_id: &str,
) -> Result<Option<InvestigationCoverageArchive>, InvestigationStoreError> {
    require_transaction(connection, task_id)?;
    validate_archive_schema(connection)?;
    let terminal: Option<bool> = connection
        .query_row(
            "SELECT live=0 FROM investigation_tasks WHERE id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let terminal = terminal.ok_or(InvestigationStoreError::Missing)?;
    check_task_capacity(connection, task_id, terminal)?;
    // CASE runs before rusqlite copies any oversized or incorrectly typed body.
    let row = connection.query_row(
        "SELECT CASE WHEN typeof(body)='blob' AND length(body) BETWEEN 1 AND 16777216 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_coverage_archives WHERE task_id=?1",
        [task_id], |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    row.map(|(body, hash)| {
        let (Some(body), Some(hash)) = (body, hash) else {
            return Err(InvestigationStoreError::Invalid);
        };
        if !hash_valid(&hash) || core_prov::content_hash(&body) != hash {
            return Err(InvestigationStoreError::Invalid);
        }
        let text = std::str::from_utf8(&body).map_err(|_| InvestigationStoreError::Invalid)?;
        let archive = InvestigationCoverageArchive::decode(text)?;
        if encode(&archive, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES)? != body {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(archive)
    })
    .transpose()
}

fn require_transaction(
    connection: &Connection,
    task_id: &str,
) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit() || !id_valid(task_id) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
