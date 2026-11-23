//! #2475: the size of the scope a task prepared.
//!
//! Every scope shares the complete-graph identity (AC-1301), so the detail
//! pane needs the prepared scope's own size to show that a narrower scope is
//! narrower. One immutable row per task records the node and edge counts of
//! the held scope projection, bound to its scope snapshot id. A paged
//! whole-system scope holds no projection and has no row. The row is an
//! app-only read extension: it is not part of the hashed task, its ledger or
//! its result.
use super::*;
use rusqlite::OptionalExtension;

const TABLE: &str = "scope_size_records";
/// Above any retained graph, well inside SQLite's signed integers.
const MAX_FACTS: u64 = 1 << 40;
const MAX_ID_BYTES: usize = 128;

/// The node and edge counts of one prepared scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSize {
    /// The scope snapshot these counts describe.
    pub scope_snapshot_id: String,
    pub nodes: u64,
    pub edges: u64,
}

impl ScopeSize {
    fn validate(&self) -> Result<(), InvestigationStoreError> {
        if self.scope_snapshot_id.is_empty()
            || self.scope_snapshot_id.len() > MAX_ID_BYTES
            || !self.scope_snapshot_id.is_ascii()
            || self.nodes > MAX_FACTS
            || self.edges > MAX_FACTS
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

fn schema() -> Vec<String> {
    let mut statements = vec![format!(
        "CREATE TABLE {TABLE} (task_id TEXT PRIMARY KEY NOT NULL, scope_snapshot_id TEXT NOT NULL CHECK(length(scope_snapshot_id) BETWEEN 1 AND {MAX_ID_BYTES}), nodes INTEGER NOT NULL CHECK(nodes BETWEEN 0 AND {MAX_FACTS}), edges INTEGER NOT NULL CHECK(edges BETWEEN 0 AND {MAX_FACTS})) STRICT"
    )];
    for verb in ["UPDATE", "DELETE"] {
        statements.push(format!(
            "CREATE TRIGGER {TABLE}_no_{} BEFORE {verb} ON {TABLE} BEGIN SELECT RAISE(ABORT, 'immutable scope size'); END",
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

fn load(connection: &Connection, id: &str) -> Result<Option<ScopeSize>, InvestigationStoreError> {
    if !present(connection)? {
        return Ok(None);
    }
    let row = connection
        .query_row(
            &format!("SELECT scope_snapshot_id, nodes, edges FROM {TABLE} WHERE task_id=?1"),
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((scope_snapshot_id, nodes, edges)) = row else {
        return Ok(None);
    };
    let count = |value: i64| u64::try_from(value).map_err(|_| InvestigationStoreError::Invalid);
    let size = ScopeSize {
        scope_snapshot_id,
        nodes: count(nodes)?,
        edges: count(edges)?,
    };
    size.validate()?;
    Ok(Some(size))
}

impl JobStore {
    /// Records the size of the scope a task prepared, once its scope snapshot
    /// is saved. The first record stands: a repeat must match it, or the call
    /// fails and the saved row is unchanged.
    pub fn record_investigation_scope_size(
        &mut self,
        id: &str,
        size: &ScopeSize,
    ) -> Result<(), InvestigationStoreError> {
        size.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        if task.detail.scope_snapshot_id.as_deref() != Some(size.scope_snapshot_id.as_str()) {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(saved) = load(&tx, id)? {
            return if &saved == size {
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
            &format!("INSERT INTO {TABLE} VALUES (?1,?2,?3,?4)"),
            params![
                id,
                size.scope_snapshot_id,
                size.nodes as i64,
                size.edges as i64
            ],
        )?)?;
        tx.commit()?;
        Ok(())
    }

    /// The size of the scope a task prepared, when it held one and it still
    /// describes the task's saved scope snapshot.
    pub fn investigation_scope_size(
        &self,
        id: &str,
    ) -> Result<Option<ScopeSize>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        let size = load(&tx, id)?.filter(|size| {
            task.detail.scope_snapshot_id.as_deref() == Some(size.scope_snapshot_id.as_str())
        });
        tx.commit()?;
        Ok(size)
    }
}
