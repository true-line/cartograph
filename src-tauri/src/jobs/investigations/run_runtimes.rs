//! ADR-0085 (AC-1423): the tool-calling runtime a run executed on.
//!
//! The saved task already names the provider id and model. A run on the
//! tool-calling runtime also records its lane, the name and version the
//! runtime reported when it started and the harness package that drove the
//! loop with its installed version, in one immutable row
//! per task. A bounded-loop run has no row. The row is an app-only read
//! extension: it is not part of the hashed task, its ledger or its result.
use super::*;
use rusqlite::OptionalExtension;

const TABLE: &str = "run_runtime_records";
const MAX_FIELD_BYTES: usize = 64;

/// The harness and runtime identity one run executed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRuntime {
    /// `pi` for local and open models, `claude` for the Agent SDK.
    pub lane: String,
    /// The runtime's self-reported name.
    pub runtime: String,
    /// The runtime's self-reported version.
    pub version: String,
    /// The harness package that drove the loop and its installed version,
    /// such as `pi-agent-core@0.73.1`.
    pub runner: String,
}

impl RunRuntime {
    fn validate(&self) -> Result<(), InvestigationStoreError> {
        let field = |value: &str| {
            !value.is_empty()
                && value.len() <= MAX_FIELD_BYTES
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_+@".contains(&b))
        };
        if !matches!(self.lane.as_str(), "pi" | "claude")
            || !field(&self.runtime)
            || !field(&self.version)
            || !field(&self.runner)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

fn schema() -> Vec<String> {
    let mut statements = vec![format!(
        "CREATE TABLE {TABLE} (task_id TEXT PRIMARY KEY NOT NULL, lane TEXT NOT NULL CHECK(lane IN ('pi','claude')), runtime TEXT NOT NULL CHECK(length(runtime) BETWEEN 1 AND {MAX_FIELD_BYTES}), version TEXT NOT NULL CHECK(length(version) BETWEEN 1 AND {MAX_FIELD_BYTES}), runner TEXT NOT NULL CHECK(length(runner) BETWEEN 1 AND {MAX_FIELD_BYTES})) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!(
            "CREATE TRIGGER {TABLE}_no_{} BEFORE {verb} ON {TABLE} BEGIN SELECT RAISE(ABORT, 'immutable run runtime'); END",
            verb.to_lowercase()
        ));
    }
    statements
}

fn present(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    let mut expected = schema();
    let ceiling = expected.iter().map(String::len).max().unwrap_or(0);
    let pattern = format!("{TABLE}*");
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

fn load(connection: &Connection, id: &str) -> Result<Option<RunRuntime>, InvestigationStoreError> {
    if !present(connection)? {
        return Ok(None);
    }
    let row = connection
        .query_row(
            &format!("SELECT lane, runtime, version, runner FROM {TABLE} WHERE task_id=?1"),
            [id],
            |row| {
                Ok(RunRuntime {
                    lane: row.get(0)?,
                    runtime: row.get(1)?,
                    version: row.get(2)?,
                    runner: row.get(3)?,
                })
            },
        )
        .optional()?;
    if let Some(runtime) = &row {
        runtime.validate()?;
    }
    Ok(row)
}

impl JobStore {
    /// Records the runtime a run executed on. The worker records it after
    /// the run, whatever its outcome, so the row never gates a transition.
    /// The first record stands: a restarted runtime must report the same
    /// identity, or the call fails and the saved row is unchanged.
    pub fn record_investigation_runtime(
        &mut self,
        id: &str,
        runtime: &RunRuntime,
    ) -> Result<(), InvestigationStoreError> {
        runtime.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        load_execution_task(&tx, id)?;
        if let Some(saved) = load(&tx, id)? {
            return if &saved == runtime {
                Ok(())
            } else {
                Err(InvestigationStoreError::Invalid)
            };
        }
        if !present(&tx)? {
            for statement in schema() {
                tx.execute_batch(&statement)?;
            }
        }
        one(tx.execute(
            &format!("INSERT INTO {TABLE} VALUES (?1,?2,?3,?4,?5)"),
            params![
                id,
                runtime.lane,
                runtime.runtime,
                runtime.version,
                runtime.runner
            ],
        )?)?;
        tx.commit()?;
        Ok(())
    }

    /// The runtime a run executed on, if it used one.
    pub fn investigation_runtime(
        &self,
        id: &str,
    ) -> Result<Option<RunRuntime>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        load_execution_task(&tx, id)?;
        let runtime = load(&tx, id)?;
        tx.commit()?;
        Ok(runtime)
    }
}
