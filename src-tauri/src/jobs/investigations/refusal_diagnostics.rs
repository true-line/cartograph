//! Separate diagnostic allowance: never included in task/store occupancy.
use super::*;
use agents::investigation::diagnostic_sidecar::{MAX_REFUSAL_DIAGNOSTIC_BYTES, Sidecar};
use rusqlite::Transaction;

pub(super) fn collect(
    input: &InvestigationInput,
    finish: &agents::investigation::staged_finish::StagedFinish,
    refusal_hash: String,
    finish_hash: String,
) -> Sidecar {
    // Identities come from the already authenticated refusal/projection. A
    // diagnostic collector cannot veto that independent outcome.
    let collected = input.refusal_diagnostic_sidecar(finish, refusal_hash.clone());
    #[cfg(any(test, feature = "test-support"))]
    let collected = if COLLECTOR_REJECTED.get() {
        Err(agents::investigation::InvestigationError::InvalidInput)
    } else {
        collected
    };
    let sidecar = collected.unwrap_or(Sidecar {
        schema_version: 1,
        refusal_hash,
        finish_hash,
        retention: agents::investigation::diagnostic_sidecar::Retention::Unrecorded {
            reason: agents::investigation::diagnostic_sidecar::UnrecordedReason::CollectorRejected,
            omitted_entries: 0, // Unknown: collection may have stopped before traversal.
        },
    });
    #[cfg(any(test, feature = "test-support"))]
    let sidecar = if NO_DETAIL_ROOM.get() {
        use agents::investigation::diagnostic_sidecar::{Retention, UnrecordedReason};
        let mut sidecar = sidecar;
        if let Retention::Recorded {
            entries,
            omitted_entries,
        } = &sidecar.retention
        {
            let omitted_entries = entries.len() + omitted_entries;
            if omitted_entries > 0 {
                sidecar.retention = Retention::Unrecorded {
                    reason: UnrecordedReason::Capacity,
                    omitted_entries,
                };
            }
        }
        sidecar
    } else {
        sidecar
    };
    sidecar
}

#[cfg(any(test, feature = "test-support"))]
thread_local! { static COLLECTOR_REJECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(any(test, feature = "test-support"))]
pub(super) fn with_collector_rejected<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            COLLECTOR_REJECTED.set(self.0);
        }
    }
    let _restore = Restore(COLLECTOR_REJECTED.replace(true));
    run()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! { static NO_DETAIL_ROOM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(any(test, feature = "test-support"))]
pub(super) fn without_detail_room<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            NO_DETAIL_ROOM.set(self.0);
        }
    }
    let _restore = Restore(NO_DETAIL_ROOM.replace(true));
    run()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! { static TEST_TIMESTAMP: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) }; }
#[cfg(any(test, feature = "test-support"))]
pub(super) struct FixedTimestamp(Option<String>);
#[cfg(any(test, feature = "test-support"))]
impl Drop for FixedTimestamp {
    fn drop(&mut self) {
        TEST_TIMESTAMP.replace(self.0.take());
    }
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn fix_timestamp(value: String) -> FixedTimestamp {
    FixedTimestamp(TEST_TIMESTAMP.replace(Some(value)))
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn fixed_timestamp() -> Option<String> {
    TEST_TIMESTAMP.with_borrow(Clone::clone)
}

// Bind host diagnostics in the response slot, never in the candidate whose
// fingerprint is exposed to the semantic auditor.
pub(super) fn withholding_witness(
    connection: &Connection,
    task: &str,
    witness_hash: String,
) -> Result<String, InvestigationStoreError> {
    let Some(sidecar) = load(connection, task, 2)? else {
        return Ok(witness_hash);
    };
    let mut bytes = b"cartograph:withholding-diagnostic-witness:v1\0".to_vec();
    bytes.extend_from_slice(witness_hash.as_bytes());
    bytes.extend_from_slice(sidecar.content_hash()?.as_bytes());
    Ok(core_prov::content_hash(&bytes))
}

const TABLE: &str = "refusal_diagnostics_records";
fn schema() -> Vec<String> {
    let mut statements = vec![format!(
        "CREATE TABLE {TABLE} (task_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence BETWEEN 1 AND 2), body BLOB NOT NULL CHECK(length(body)<={MAX_REFUSAL_DIAGNOSTIC_BYTES}), body_hash TEXT NOT NULL CHECK(length(body_hash)=64), PRIMARY KEY(task_id,sequence)) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!("CREATE TRIGGER {TABLE}_no_{} BEFORE {verb} ON {TABLE} BEGIN SELECT RAISE(ABORT, 'immutable refusal diagnostic'); END", verb.to_lowercase()));
    }
    statements
}

fn present(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    let mut expected = schema();
    let ceiling = expected.iter().map(String::len).max().unwrap_or(0);
    let mut actual: Vec<Option<String>> = connection.prepare(
        "SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE name GLOB 'refusal_diagnostics_*' OR (tbl_name GLOB 'refusal_diagnostics_*' AND name NOT GLOB 'sqlite_*') LIMIT ?2"
    )?.query_map(params![ceiling as i64, expected.len() as i64 + 1], |row| row.get(0))?.collect::<Result<_, _>>()?;
    if actual.is_empty() {
        return Ok(false);
    }
    actual.sort();
    expected.sort();
    if actual != expected.into_iter().map(Some).collect::<Vec<_>>() {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(true)
}

pub(super) fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
    if !present(tx)? {
        for statement in schema() {
            tx.execute_batch(&statement)?;
        }
    }
    Ok(())
}

pub(super) fn insert(
    tx: &Transaction<'_>,
    task: &str,
    sequence: u32,
    sidecar: &Sidecar,
) -> Result<(), InvestigationStoreError> {
    initialize(tx)?;
    let body = sidecar.bytes()?;
    one(tx.execute(
        "INSERT INTO refusal_diagnostics_records VALUES (?1,?2,?3,?4)",
        params![task, sequence, body, core_prov::content_hash(&body)],
    )?)?;
    Ok(())
}

pub(super) fn load(
    connection: &Connection,
    task: &str,
    sequence: u32,
) -> Result<Option<Sidecar>, InvestigationStoreError> {
    if !present(connection)? {
        return Ok(None);
    }
    let result: Option<Sidecar> = staged::read(
        connection,
        TABLE,
        task,
        Some(sequence),
        MAX_REFUSAL_DIAGNOSTIC_BYTES,
    )?;
    if let Some(sidecar) = &result {
        sidecar.bytes()?;
    }
    Ok(result)
}

// Development receipts consume the same authenticated snapshot as replay.
#[cfg(any(test, feature = "test-support"))]
impl JobStore {
    pub fn with_refusal_diagnostic_collector_rejected<T>(run: impl FnOnce() -> T) -> T {
        with_collector_rejected(run)
    }

    pub fn without_refusal_diagnostic_detail<T>(run: impl FnOnce() -> T) -> T {
        without_detail_room(run)
    }

    pub fn investigation_refusal_diagnostics(
        &self,
        id: &str,
    ) -> Result<Vec<Sidecar>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let sidecars = project(&tx, id)?;
        tx.commit()?;
        Ok(sidecars)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn project(
    connection: &Connection,
    id: &str,
) -> Result<Vec<Sidecar>, InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    let task = load_execution_task(connection, id)?;
    let refusals = completion::raw_refusals(connection, &task)?;
    for (index, refusal) in refusals.iter().enumerate() {
        admission_journal::AuthenticatedRefusalBasis::load(
            connection,
            &task,
            index as u32 + 1,
            refusal,
        )?;
    }
    let result = load_result(connection, &task)?;
    let withheld = result
        .as_ref()
        .and_then(|r| r.completion.as_ref())
        .and_then(|c| c.withholding.as_ref())
        .is_some();
    let mut sidecars = Vec::new();
    for sequence in 1..=2 {
        if let Some(sidecar) = load(connection, id, sequence)? {
            if !task
                .detail
                .summary
                .specialist_id
                .uses_finish_feedback_witness()
            {
                return Err(InvestigationStoreError::Invalid);
            }
            // Refuse orphan rows that neither authenticated journal nor
            // authenticated final withholding consumed.
            if sequence as usize > refusals.len() && !(sequence == 2 && withheld) {
                return Err(InvestigationStoreError::Invalid);
            }
            sidecars.push(sidecar);
        }
    }
    Ok(sidecars)
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;
    fn sidecar() -> Sidecar {
        Sidecar::collect("a".repeat(64), "b".repeat(64), []).unwrap()
    }
    // AC-0287 / #1707: new rows are immutable and share the caller transaction.
    #[test]
    fn sidecar_is_atomic_immutable_and_roundtrips() {
        let mut connection = Connection::open_in_memory().unwrap();
        assert!(load(&connection, "task", 1).unwrap().is_none());
        {
            let tx = connection.transaction().unwrap();
            insert(&tx, "task", 1, &sidecar()).unwrap();
        }
        assert!(load(&connection, "task", 1).unwrap().is_none());
        let tx = connection.transaction().unwrap();
        insert(&tx, "task", 1, &sidecar()).unwrap();
        tx.commit().unwrap();
        assert_eq!(load(&connection, "task", 1).unwrap(), Some(sidecar()));
        assert!(
            connection
                .execute("DELETE FROM refusal_diagnostics_records", [])
                .is_err()
        );
        assert!(
            connection
                .execute("UPDATE refusal_diagnostics_records SET body=x'00'", [])
                .is_err()
        );
    }
    #[test]
    fn sidecar_rejects_schema_and_rehashed_invalid_payload() {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.commit().unwrap();
        let mut invalid = sidecar();
        invalid.finish_hash = "not a hash".into();
        let body = serde_json::to_vec(&invalid).unwrap();
        connection
            .execute(
                "INSERT INTO refusal_diagnostics_records VALUES ('task',1,?1,?2)",
                params![body, core_prov::content_hash(&body)],
            )
            .unwrap();
        assert!(load(&connection, "task", 1).is_err());
        connection
            .execute_batch("DROP TRIGGER refusal_diagnostics_records_no_delete")
            .unwrap();
        assert!(load(&connection, "task", 1).is_err());
    }
}
