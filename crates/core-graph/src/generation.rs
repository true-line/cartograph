//! Storage revision tokens. They order mutations, never recovery completion.
use crate::{Edge, GraphError, Node, SnapshotReadLimits, SqliteGraphStore};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TABLE: &str = "CREATE TABLE graph_generation (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 1), generation INTEGER NOT NULL CHECK (generation >= 0)) STRICT";
const OWNED: &str =
    "name GLOB 'graph_generation*' OR (tbl_name = 'graph_generation' AND name NOT GLOB 'sqlite_*')";
const EXHAUSTED: &str = "graph generation exhausted";
const OPERATIONS: [&str; 3] = ["INSERT", "UPDATE", "DELETE"];
const WATCHED: &[&str] = &[
    "nodes",
    "edges",
    "source_node_bindings",
    "source_edge_bindings",
    "coverage_inventories",
];

/// Failure of the private mutation-generation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GenerationError {
    /// Missing, partial, malformed or unsupported generation metadata.
    #[error("incompatible graph generation schema")]
    InvalidSchema,
    /// The next mutation would exceed the signed SQLite integer ceiling.
    #[error("graph generation exhausted")]
    Exhausted,
    /// The requested immutable acquisition basis is no longer current.
    #[error("graph revision changed")]
    Changed,
}

/// Opaque database-local mutation order, not a content hash or recovery status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreRevision {
    /// Durable database identity, independent of content and registration.
    pub graph_instance: String,
    /// Monotonic row-event token, never an operation or recovery count.
    pub generation: u64,
}

/// Complete bounded graph and mutation token from one SQLite read revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionedSnapshot {
    /// Revision observed in the same transaction as the graph rows.
    pub revision: StoreRevision,
    /// Complete graph admitted under the requested snapshot limits.
    pub graph: (Vec<Node>, Vec<Edge>),
}

/// Derive metadata admission from the same registry used to generate triggers.
pub(crate) fn trigger_count_for_tables(tables: &[&str]) -> usize {
    WATCHED
        .iter()
        .filter(|table| tables.contains(table))
        .count()
        * OPERATIONS.len()
}

fn schema() -> Vec<(String, String, String)> {
    let mut result = vec![("table".into(), "graph_generation".into(), TABLE.into())];
    for table in WATCHED {
        for operation in OPERATIONS {
            let name = format!(
                "graph_generation_{table}_{}",
                operation.to_ascii_lowercase()
            );
            let sql = format!(
                "CREATE TRIGGER {name} AFTER {operation} ON {table} BEGIN UPDATE graph_generation SET generation = generation + 1 WHERE singleton = 1 AND generation < 9223372036854775807; SELECT CASE WHEN changes() != 1 THEN RAISE(ABORT, '{EXHAUSTED}') END; END"
            );
            result.push(("trigger".into(), name, sql));
        }
    }
    result
}

/// Validate the complete optional schema, with bounded metadata allocation.
/// Absence is allowed only to support the additive migration.
pub(crate) fn validate_optional(connection: &Connection) -> Result<bool, GraphError> {
    let (count, valid): (i64, bool) = connection.query_row(
        &format!("SELECT count(*), coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=256 AND typeof(type)='text' AND length(CAST(type AS BLOB))<=16 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),1) FROM sqlite_schema WHERE {OWNED}"),
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count == 0 {
        return Ok(false);
    }
    let expected = schema();
    if count != expected.len() as i64 || !valid {
        return Err(GenerationError::InvalidSchema.into());
    }
    let mut statement = connection.prepare(&format!(
        "SELECT name,type,sql FROM sqlite_schema WHERE {OWNED}"
    ))?;
    let actual = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
            ))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if expected
        .iter()
        .any(|(kind, name, sql)| actual.get(name) != Some(&(kind.clone(), sql.clone())))
    {
        return Err(GenerationError::InvalidSchema.into());
    }
    let mut statement =
        connection.prepare("SELECT singleton,version,generation FROM graph_generation LIMIT 2")?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() != 1 || rows[0].0 != 1 || rows[0].1 != 1 || rows[0].2 < 0 {
        return Err(GenerationError::InvalidSchema.into());
    }
    Ok(true)
}

/// Proof that the complete optional generation schema was validated in the
/// caller's current read transaction, before any owner copies SQL bodies.
pub(crate) struct ValidatedGenerationSchema {
    present: bool,
}

pub(crate) fn validate_for_owner(
    connection: &Connection,
) -> Result<ValidatedGenerationSchema, GraphError> {
    Ok(ValidatedGenerationSchema {
        present: validate_optional(connection)?,
    })
}

impl ValidatedGenerationSchema {
    /// Subtract only exact known objects after the complete schema passed its
    /// allocation preflight and exact SQL checks. No arbitrary prefix is trusted.
    pub(crate) fn strip_objects(self, actual: &mut BTreeMap<String, (String, String)>) {
        if self.present {
            for (_, name, _) in schema() {
                actual.remove(&name);
            }
        }
    }
}

pub(crate) fn initialize(transaction: &Transaction<'_>) -> Result<(), GraphError> {
    if !validate_optional(transaction)? {
        for (_, _, sql) in schema() {
            transaction.execute_batch(&sql)?;
        }
        transaction.execute("INSERT INTO graph_generation VALUES(1,1,0)", [])?;
    }
    validate_optional(transaction)?;
    Ok(())
}

pub(crate) fn bump(transaction: &Transaction<'_>) -> Result<(), GraphError> {
    if transaction.execute("UPDATE graph_generation SET generation = generation + 1 WHERE singleton = 1 AND generation < 9223372036854775807",[])? != 1 {
        return Err(GenerationError::Exhausted.into());
    }
    Ok(())
}

/// Read within the caller's existing read transaction, never pair a standalone
/// generation lookup with a later graph query.
pub fn read_store_revision(transaction: &Transaction<'_>) -> Result<StoreRevision, GraphError> {
    if !validate_optional(transaction)? {
        return Err(GenerationError::InvalidSchema.into());
    }
    // Check instance shape in SQLite before copying any text into Rust. The
    // surrounding transaction pins this proof to the subsequent value read.
    let valid_instance: bool = transaction.query_row(
        "SELECT count(*)=1 AND coalesce(min(singleton=1 AND typeof(instance)='text' AND length(CAST(instance AS BLOB))=32 AND instance NOT GLOB '*[^0-9a-f]*'),0) FROM graph_instance",
        [], |row| row.get(0),
    )?;
    if !valid_instance {
        return Err(GenerationError::InvalidSchema.into());
    }
    let (graph_instance,generation): (String,i64) = transaction.query_row("SELECT instance,generation FROM graph_instance JOIN graph_generation ON graph_generation.singleton=graph_instance.singleton WHERE graph_generation.singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if graph_instance.len() != 32
        || !graph_instance
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(GenerationError::InvalidSchema.into());
    }
    Ok(StoreRevision {
        graph_instance,
        generation: generation as u64,
    })
}

pub(crate) fn storage_error(error: rusqlite::Error) -> GraphError {
    if matches!(&error,rusqlite::Error::SqliteFailure(_,Some(message)) if message==EXHAUSTED) {
        GenerationError::Exhausted.into()
    } else {
        GraphError::Storage(error)
    }
}

impl SqliteGraphStore {
    /// Observe the current revision. This does not make a later independent
    /// data read coherent with this token; use a revisioned read for that.
    pub fn read_revision(&self) -> Result<StoreRevision, GraphError> {
        let transaction = self.conn.unchecked_transaction()?;
        let revision = read_store_revision(&transaction)?;
        transaction.commit()?;
        Ok(revision)
    }

    /// Copy the complete bounded graph and its revision coherently.
    pub fn read_snapshot_with_revision_bounded(
        &self,
        limits: SnapshotReadLimits,
    ) -> Result<RevisionedSnapshot, GraphError> {
        self.read_snapshot_at_revision_bounded(None, limits)
    }
    /// Refuse a changed expected revision before reading complete graph rows.
    pub fn read_snapshot_at_revision_bounded(
        &self,
        expected: Option<&StoreRevision>,
        limits: SnapshotReadLimits,
    ) -> Result<RevisionedSnapshot, GraphError> {
        limits.validate()?;
        let transaction = self.conn.unchecked_transaction()?;
        let revision = read_store_revision(&transaction)?;
        if expected.is_some_and(|e| e != &revision) {
            return Err(GenerationError::Changed.into());
        }
        let graph = self.read_snapshot_rows_bounded(limits)?;
        transaction.commit()?;
        Ok(RevisionedSnapshot { revision, graph })
    }
}

#[cfg(test)]
mod tests;
