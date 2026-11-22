//! A failed task's stop records (ADR-0076, #1616): the rejected provider
//! response (AC-1403) or the action decoded before a host failure (AC-1405).
//! Each is one immutable row per task, written in the terminal transaction and
//! kept outside task and store occupancy, like the refusal diagnostics.
use super::*;
use agents::investigation::admitted_action::{AdmittedAction, MAX_ADMITTED_ACTION_RECORD_BYTES};
use agents::investigation::rejected_response::{MAX_REJECTED_RESPONSE_BYTES, RejectedResponse};
use rusqlite::Transaction;

const TABLE: &str = "rejected_response_records";
const ACTION_TABLE: &str = "admitted_action_records";

// The rejected-response schema is fixed as shipped (AC-1403); an existing
// table must keep matching it exactly.
fn limits(table: &str) -> (usize, &'static str) {
    if table == ACTION_TABLE {
        (
            MAX_ADMITTED_ACTION_RECORD_BYTES,
            "immutable admitted action",
        )
    } else {
        (MAX_REJECTED_RESPONSE_BYTES, "immutable rejected response")
    }
}

fn schema(table: &str) -> Vec<String> {
    let (limit, message) = limits(table);
    let mut statements = vec![format!(
        "CREATE TABLE {table} (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL CHECK(length(body)<={limit}), body_hash TEXT NOT NULL CHECK(length(body_hash)=64)) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!("CREATE TRIGGER {table}_no_{} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, '{message}'); END", verb.to_lowercase()));
    }
    statements
}

fn present(connection: &Connection, table: &str) -> Result<bool, InvestigationStoreError> {
    let mut expected = schema(table);
    let ceiling = expected.iter().map(String::len).max().unwrap_or(0);
    let pattern = format!("{table}*");
    let mut actual: Vec<Option<String>> = connection.prepare(
        "SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE (name GLOB ?3 OR tbl_name GLOB ?3) AND name NOT GLOB 'sqlite_*' LIMIT ?2"
    )?.query_map(params![ceiling as i64, expected.len() as i64 + 1, pattern], |row| row.get(0))?.collect::<Result<_, _>>()?;
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

fn initialize(tx: &Transaction<'_>, table: &str) -> Result<(), InvestigationStoreError> {
    if !present(tx, table)? {
        for statement in schema(table) {
            tx.execute_batch(&statement)?;
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
fn insert(
    tx: &Transaction<'_>,
    task: &str,
    record: &RejectedResponse,
) -> Result<(), InvestigationStoreError> {
    let body = record
        .bytes()
        .map_err(|_| InvestigationStoreError::Invalid)?;
    insert_body(tx, TABLE, task, &body)
}

fn insert_body(
    tx: &Transaction<'_>,
    table: &str,
    task: &str,
    body: &[u8],
) -> Result<(), InvestigationStoreError> {
    initialize(tx, table)?;
    one(tx.execute(
        &format!("INSERT INTO {table} VALUES (?1,?2,?3)"),
        params![task, body, core_prov::content_hash(body)],
    )?)?;
    Ok(())
}

/// Insert inside a savepoint, so a failed insert (an unexpected table or row)
/// never vetoes the stop: the record is dropped and the stop still commits.
fn insert_never_decisive(
    tx: &Transaction<'_>,
    table: &str,
    task: &str,
    body: &[u8],
) -> Result<(), InvestigationStoreError> {
    tx.execute_batch("SAVEPOINT stop_record")?;
    if insert_body(tx, table, task, body).is_err() {
        tx.execute_batch("ROLLBACK TO stop_record")?;
    }
    tx.execute_batch("RELEASE stop_record")?;
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn load(
    connection: &Connection,
    task: &str,
) -> Result<Option<RejectedResponse>, InvestigationStoreError> {
    if !present(connection, TABLE)? {
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
    match reason {
        InvestigationStopReason::ActionDecode(_) | InvestigationStopReason::InvalidResponse => {
            record_rejected(tx, task, rejected)
        }
        InvestigationStopReason::PreparationAt(_) | InvestigationStopReason::HostLimitAt { .. } => {
            record_admitted(tx, task, rejected)
        }
        _ => Ok(()),
    }
}

/// The action the held response decoded to, as the exact canonical bytes the
/// worker hashed into the saved `action_hash` (AC-1405). The store never
/// re-serializes the response: a typed action and a JSON map can order fields
/// differently. A stop before the worker hashed an action keeps nothing.
fn record_admitted(
    tx: &Transaction<'_>,
    task: &StoredTask,
    held: &RejectedCompletion,
) -> Result<(), InvestigationStoreError> {
    let Some(canonical) = held.action.as_deref() else {
        return Ok(());
    };
    let Ok(record) = AdmittedAction::capture(&held.step_id, canonical) else {
        return Ok(());
    };
    let Ok(body) = record.bytes() else {
        return Ok(());
    };
    insert_never_decisive(
        tx,
        ACTION_TABLE,
        &task.detail.summary.investigation_id,
        &body,
    )
}

/// Write the record of a response the store itself rejected while admitting
/// it (a refused finish, AC-1404). The task's terminal diagnostic must already
/// be set, so a sensitive or replayed response keeps no text.
pub(super) fn record_rejected(
    tx: &Transaction<'_>,
    task: &StoredTask,
    rejected: &RejectedCompletion,
) -> Result<(), InvestigationStoreError> {
    let Ok(record) = RejectedResponse::capture(
        &rejected.step_id,
        &rejected.text,
        task.detail.terminal_diagnostic.as_ref(),
    ) else {
        return Ok(());
    };
    let Ok(body) = record.bytes() else {
        return Ok(());
    };
    insert_never_decisive(tx, TABLE, &task.detail.summary.investigation_id, &body)
}

/// The completed response the worker last received, held until the task stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedCompletion {
    pub step_id: String,
    pub text: String,
    /// The canonical action bytes behind the saved `action_hash`, once the
    /// worker has decoded and hashed the response (AC-1405).
    pub action: Option<Vec<u8>>,
}

// Pilot inspection reads the table directly; the app has no reader yet.
#[cfg(any(test, feature = "test-support"))]
impl JobStore {
    /// The saved rejected response of a failed task, if one was kept.
    pub fn investigation_rejected_response(
        &self,
        id: &str,
    ) -> Result<Option<RejectedResponse>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let record = load(&tx, id)?;
        tx.commit()?;
        Ok(record)
    }

    /// The saved action a task decoded before a host failure, if one was kept.
    pub fn investigation_admitted_action(
        &self,
        id: &str,
    ) -> Result<Option<AdmittedAction>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let record = if present(&tx, ACTION_TABLE)? {
            let record: Option<AdmittedAction> = staged::read(
                &tx,
                ACTION_TABLE,
                id,
                None,
                MAX_ADMITTED_ACTION_RECORD_BYTES,
            )?;
            if let Some(record) = &record {
                record
                    .bytes()
                    .map_err(|_| InvestigationStoreError::Invalid)?;
            }
            record
        } else {
            None
        };
        tx.commit()?;
        Ok(record)
    }
}

#[cfg(any(test, feature = "test-support"))]
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

    // A rejected-response table created by an earlier release still matches.
    #[test]
    fn shipped_rejected_response_schema_is_unchanged() {
        assert_eq!(
            schema(TABLE),
            [
                "CREATE TABLE rejected_response_records (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL CHECK(length(body)<=8192), body_hash TEXT NOT NULL CHECK(length(body_hash)=64)) STRICT",
                "CREATE TRIGGER rejected_response_records_no_update BEFORE UPDATE ON rejected_response_records BEGIN SELECT RAISE(ABORT, 'immutable rejected response'); END",
                "CREATE TRIGGER rejected_response_records_no_delete BEFORE DELETE ON rejected_response_records BEGIN SELECT RAISE(ABORT, 'immutable rejected response'); END",
            ]
        );
    }

    #[test]
    fn record_rejects_schema_and_rehashed_invalid_payload() {
        let mut connection = Connection::open_in_memory().unwrap();
        let tx = connection.transaction().unwrap();
        initialize(&tx, TABLE).unwrap();
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
