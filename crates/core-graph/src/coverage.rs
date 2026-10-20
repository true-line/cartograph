//! Recorded per-repo coverage inventories (AC-0239), separate from facts.
//!
//! An inventory is published in the same write transaction as its repo's
//! facts, so a crash leaves both old or both new. The body is opaque here: the
//! store keeps its exact bytes and their content hash, and grants no claim that
//! the inventory is complete or still matches the repo's registration.

use crate::{Edge, GraphError, Node, SqliteGraphStore};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_IDENTITY_BYTES: usize = 256;
/// Maximum stored inventory body, in UTF-8 bytes.
pub const MAX_INVENTORY_BYTES: usize = 1024 * 1024;

/// One repo's inventory and the basis it was recorded under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageRecord {
    /// Repo identity the facts are namespaced by.
    pub repo_key: String,
    /// Registered source the ingest read, when it read one.
    pub registered_source_id: Option<String>,
    /// The ingest that produced the inventory.
    pub ingest_id: String,
    /// Identity of the plugin basis the ingest applied. Layer hints and
    /// declared inputs are per-ingest options recorded in the inventory body.
    pub config_hash: String,
    /// Exact inventory JSON, as serialized by the producer.
    pub inventory_json: String,
}

/// A record read back with the content hash of its stored body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCoverage {
    /// The record as published.
    pub record: CoverageRecord,
    /// Content hash of `inventory_json`, verified on read.
    pub inventory_hash: String,
}

impl CoverageRecord {
    /// Content hash of the inventory body.
    pub fn inventory_hash(&self) -> String {
        core_prov::content_hash(self.inventory_json.as_bytes())
    }

    fn validate(&self) -> Result<(), GraphError> {
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.contains('\0')
        };
        if !identity(&self.repo_key)
            || !self.registered_source_id.as_deref().is_none_or(identity)
            || !identity(&self.ingest_id)
            || !identity(&self.config_hash)
            || self.inventory_json.len() > MAX_INVENTORY_BYTES
            || !serde_json::from_str::<serde_json::Value>(&self.inventory_json)
                .is_ok_and(|value| value.is_object())
        {
            return Err(GraphError::SourceBinding("invalid coverage record"));
        }
        Ok(())
    }
}

// Exact owned SQL is checked before use, as for source associations: a
// partial or future schema is refused, never CREATE-repaired.
const SCHEMA: &[(&str, &str, &str)] = &[
    (
        "table",
        "coverage_meta",
        "CREATE TABLE coverage_meta (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 1)) STRICT",
    ),
    (
        "table",
        "coverage_inventories",
        "CREATE TABLE coverage_inventories (repo_key TEXT PRIMARY KEY NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), registered_source_id TEXT CHECK (registered_source_id IS NULL OR length(CAST(registered_source_id AS BLOB)) BETWEEN 1 AND 256), ingest_id TEXT NOT NULL CHECK (length(CAST(ingest_id AS BLOB)) BETWEEN 1 AND 256), config_hash TEXT NOT NULL CHECK (length(CAST(config_hash AS BLOB)) BETWEEN 1 AND 256), inventory_hash TEXT NOT NULL CHECK (length(CAST(inventory_hash AS BLOB)) BETWEEN 1 AND 256), inventory_json TEXT NOT NULL CHECK (length(CAST(inventory_json AS BLOB)) <= 1048576)) STRICT",
    ),
];

const OWNED_SCHEMA_WHERE: &str = "name GLOB 'coverage_*' OR (tbl_name IN ('coverage_meta', 'coverage_inventories') AND name NOT GLOB 'sqlite_*')";

fn owned_schema(connection: &Connection) -> Result<BTreeMap<String, (String, String)>, GraphError> {
    // Include every object on these tables, not just our names: a foreign
    // trigger or index could rewrite or suppress publication.
    let mut statement = connection.prepare(&format!(
        "SELECT name, type, sql FROM sqlite_schema WHERE {OWNED_SCHEMA_WHERE}"
    ))?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(crate) fn check_schema(connection: &Connection) -> Result<(), GraphError> {
    let actual = owned_schema(connection)?;
    if actual.len() != SCHEMA.len()
        || SCHEMA.iter().any(|(kind, name, sql)| {
            actual
                .get(*name)
                .map(|(actual_kind, actual_sql)| (actual_kind.as_str(), actual_sql.as_str()))
                != Some((*kind, *sql))
        })
    {
        return Err(GraphError::SourceBinding("incompatible coverage schema"));
    }
    let mut statement =
        connection.prepare("SELECT singleton, version FROM coverage_meta LIMIT 2")?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if rows != [(1, 1)] {
        return Err(GraphError::SourceBinding("unsupported coverage version"));
    }
    Ok(())
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), GraphError> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    if owned_schema(connection)?.is_empty() {
        for (_, _, sql) in SCHEMA {
            transaction.execute_batch(sql)?;
        }
        transaction.execute(
            "INSERT INTO coverage_meta(singleton, version) VALUES (1, 1)",
            [],
        )?;
    }
    check_schema(connection)?;
    transaction.commit()?;
    Ok(())
}

/// Replace `repo_key`'s inventory inside the caller's write transaction.
/// Without a record the old inventory is removed: it no longer describes the
/// facts being published, so the repo reads as unrecorded.
pub(crate) fn replace(
    connection: &Connection,
    repo_key: &str,
    record: Option<&CoverageRecord>,
) -> Result<(), GraphError> {
    check_schema(connection)?;
    connection.execute(
        "DELETE FROM coverage_inventories WHERE repo_key = ?1",
        [repo_key],
    )?;
    if let Some(record) = record {
        record.validate()?;
        if record.repo_key != repo_key {
            return Err(GraphError::SourceBinding("wrong-owner coverage record"));
        }
        connection.execute(
            "INSERT INTO coverage_inventories(repo_key, registered_source_id, ingest_id, config_hash, inventory_hash, inventory_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.repo_key,
                record.registered_source_id,
                record.ingest_id,
                record.config_hash,
                record.inventory_hash(),
                record.inventory_json
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn clear(connection: &Connection) -> Result<(), GraphError> {
    connection.execute("DELETE FROM coverage_inventories", [])?;
    Ok(())
}

fn read_all(connection: &Connection) -> Result<Vec<StoredCoverage>, GraphError> {
    check_schema(connection)?;
    let mut statement = connection.prepare(
        "SELECT repo_key, registered_source_id, ingest_id, config_hash, inventory_hash, inventory_json FROM coverage_inventories ORDER BY repo_key",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                CoverageRecord {
                    repo_key: row.get(0)?,
                    registered_source_id: row.get(1)?,
                    ingest_id: row.get(2)?,
                    config_hash: row.get(3)?,
                    inventory_json: row.get(5)?,
                },
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(record, inventory_hash)| {
            record.validate()?;
            // A body edited outside publication no longer matches its hash.
            if record.inventory_hash() != inventory_hash {
                return Err(GraphError::SourceBinding(
                    "coverage inventory hash mismatch",
                ));
            }
            Ok(StoredCoverage {
                record,
                inventory_hash,
            })
        })
        .collect()
}

/// A graph copy with the inventories recorded alongside it.
pub type CoveredSnapshot = ((Vec<Node>, Vec<Edge>), Vec<StoredCoverage>);

impl SqliteGraphStore {
    /// Copy the complete graph and every recorded inventory from one read
    /// transaction, so a qualification never describes a different revision
    /// than the facts it accompanies.
    pub fn read_snapshot_with_coverage(&self) -> Result<CoveredSnapshot, GraphError> {
        let transaction = self.conn.unchecked_transaction()?;
        let graph = self.read_snapshot_rows(None, None)?;
        let coverage = read_all(&self.conn)?;
        transaction.commit()?;
        Ok((graph, coverage))
    }

    /// Every recorded inventory, by repo.
    pub fn coverage_records(&self) -> Result<Vec<StoredCoverage>, GraphError> {
        let transaction = self.conn.unchecked_transaction()?;
        let coverage = read_all(&self.conn)?;
        transaction.commit()?;
        Ok(coverage)
    }
}

#[cfg(test)]
mod tests;
