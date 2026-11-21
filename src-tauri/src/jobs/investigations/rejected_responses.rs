//! A failed task's rejected provider response (ADR-0076, AC-1403, #1616):
//! one immutable row per task, written in the terminal transaction and kept
//! outside task and store occupancy, like the refusal diagnostics.
use super::*;
use agents::investigation::rejected_response::{MAX_REJECTED_RESPONSE_BYTES, RejectedResponse};
use rusqlite::Transaction;

const TABLE: &str = "rejected_response_records";

fn schema() -> Vec<String> {
    let mut statements = vec![format!(
        "CREATE TABLE {TABLE} (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL CHECK(length(body)<={MAX_REJECTED_RESPONSE_BYTES}), body_hash TEXT NOT NULL CHECK(length(body_hash)=64)) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!("CREATE TRIGGER {TABLE}_no_{} BEFORE {verb} ON {TABLE} BEGIN SELECT RAISE(ABORT, 'immutable rejected response'); END", verb.to_lowercase()));
    }
    statements
}

fn present(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    let mut expected = schema();
    let ceiling = expected.iter().map(String::len).max().unwrap_or(0);
    let mut actual: Vec<Option<String>> = connection.prepare(
        "SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE (name GLOB 'rejected_response_records*' OR tbl_name GLOB 'rejected_response_records*') AND name NOT GLOB 'sqlite_*' LIMIT ?2"
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

fn initialize(tx: &Transaction<'_>) -> Result<(), InvestigationStoreError> {
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
    record: &RejectedResponse,
) -> Result<(), InvestigationStoreError> {
    initialize(tx)?;
    let body = record
        .bytes()
        .map_err(|_| InvestigationStoreError::Invalid)?;
    one(tx.execute(
        "INSERT INTO rejected_response_records VALUES (?1,?2,?3)",
        params![task, body, core_prov::content_hash(&body)],
    )?)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn load(
    connection: &Connection,
    task: &str,
) -> Result<Option<RejectedResponse>, InvestigationStoreError> {
    if !present(connection)? {
        return Ok(None);
    }
    let record: Option<RejectedResponse> =
        staged::read(connection, TABLE, task, None, MAX_REJECTED_RESPONSE_BYTES)?;
    if let Some(record) = &record {
        record
            .bytes()
            .map_err(|_| InvestigationStoreError::Invalid)?;
    }
    Ok(record)
}

/// Write the record when the terminal cause is a rejected response. Other
/// causes, and a record that cannot be built, leave nothing behind; the
/// terminal outcome never depends on this diagnostic.
pub(super) fn record_on_stop(
    tx: &Transaction<'_>,
    task: &StoredTask,
    reason: InvestigationStopReason,
    rejected: Option<&RejectedCompletion>,
) -> Result<(), InvestigationStoreError> {
    let Some(rejected) = rejected else {
        return Ok(());
    };
    if !matches!(
        reason,
        InvestigationStopReason::ActionDecode(_) | InvestigationStopReason::InvalidResponse
    ) {
        return Ok(());
    }
    let Ok(record) = RejectedResponse::capture(
        &rejected.step_id,
        &rejected.text,
        task.detail.terminal_diagnostic.as_ref(),
    ) else {
        return Ok(());
    };
    // A savepoint keeps a failed insert (an unexpected table or row) from
    // vetoing the stop: the record is dropped and the stop still commits.
    tx.execute_batch("SAVEPOINT rejected_response_record")?;
    if insert(tx, &task.detail.summary.investigation_id, &record).is_err() {
        tx.execute_batch("ROLLBACK TO rejected_response_record")?;
    }
    tx.execute_batch("RELEASE rejected_response_record")?;
    Ok(())
}

/// The completed response the worker last received, held until the task stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RejectedCompletion {
    pub step_id: String,
    pub text: String,
}

// Pilot inspection reads the table directly; the app has no reader yet.
#[cfg(test)]
impl JobStore {
    /// The saved rejected response of a failed task, if one was kept.
    pub(crate) fn investigation_rejected_response(
        &self,
        id: &str,
    ) -> Result<Option<RejectedResponse>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let record = load(&tx, id)?;
        tx.commit()?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RejectedResponse {
        RejectedResponse::capture("step-1", "{\"type\":\"unknown\"}", None).unwrap()
    }

    // AC-1403: the row is immutable and shares the caller's transaction.
    #[test]
    fn record_is_atomic_immutable_and_roundtrips() {
        let mut connection = Connection::open_in_memory().unwrap();
        assert!(load(&connection, "task").unwrap().is_none());
        {
            let tx = connection.transaction().unwrap();
            insert(&tx, "task", &record()).unwrap();
        }
        assert!(load(&connection, "task").unwrap().is_none());
        let tx = connection.transaction().unwrap();
        insert(&tx, "task", &record()).unwrap();
        assert!(insert(&tx, "task", &record()).is_err());
        tx.commit().unwrap();
        assert_eq!(load(&connection, "task").unwrap(), Some(record()));
        for statement in [
            "DELETE FROM rejected_response_records",
            "UPDATE rejected_response_records SET body=x'00'",
        ] {
            assert!(connection.execute(statement, []).is_err());
        }
    }

    #[test]
    fn record_rejects_schema_and_rehashed_invalid_payload() {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.commit().unwrap();
        let mut invalid = serde_json::to_value(record()).unwrap();
        invalid["response_hash"] = "not a hash".into();
        let body = serde_json::to_vec(&invalid).unwrap();
        connection
            .execute(
                "INSERT INTO rejected_response_records VALUES ('task',?1,?2)",
                params![body, core_prov::content_hash(&body)],
            )
            .unwrap();
        assert!(load(&connection, "task").is_err());
        connection
            .execute_batch("DROP TRIGGER rejected_response_records_no_delete")
            .unwrap();
        assert!(load(&connection, "task").is_err());
    }
}
