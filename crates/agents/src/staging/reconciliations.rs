//! Append-only reconciliation results beside staged proposals (ADR-0072
//! decision 2, slice 3).
//!
//! A result is metadata only: the exact comparisons the host made for one
//! staged record against one explicit recovery. It is written once, never
//! updated or deleted (job cleanup included), and never changes the staged
//! row it names.

use super::{ProposalStore, StagingError, get_on};
use crate::reconciliation::{MAX_RESULT_BYTES, ReconciliationResultV1};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

/// Most results one history read returns, newest first.
pub const MAX_RECONCILIATION_HISTORY: usize = 64;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS staged_reconciliation_results (
     sequence INTEGER PRIMARY KEY AUTOINCREMENT,
     result_id TEXT NOT NULL UNIQUE CHECK (length(CAST(result_id AS BLOB)) BETWEEN 1 AND 128),
     proposal_id TEXT NOT NULL CHECK (length(CAST(proposal_id AS BLOB)) BETWEEN 1 AND 256),
     result_json TEXT NOT NULL CHECK (length(CAST(result_json AS BLOB)) <= 65536),
     recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
 ) STRICT;
 CREATE INDEX IF NOT EXISTS staged_reconciliation_results_by_proposal
     ON staged_reconciliation_results (proposal_id, sequence);
 CREATE TRIGGER IF NOT EXISTS staged_reconciliation_results_immutable
 BEFORE UPDATE ON staged_reconciliation_results
 BEGIN SELECT RAISE(ABORT, 'reconciliation results are immutable'); END;
 CREATE TRIGGER IF NOT EXISTS staged_reconciliation_results_append_only
 BEFORE DELETE ON staged_reconciliation_results
 BEGIN SELECT RAISE(ABORT, 'reconciliation results are append-only'); END;";

/// One stored result and when the host recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedReconciliation {
    /// The strict, validated result.
    pub result: ReconciliationResultV1,
    /// SQLite UTC time it was first recorded; outside its identity.
    pub recorded_at: String,
}

const SELECT: &str =
    "SELECT result_id, proposal_id, result_json, recorded_at FROM staged_reconciliation_results";

type Row = (String, String, String, String);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

/// Strictly decode one stored row and bind it to its own staged record.
fn decode(
    (result_id, proposal_id, json, recorded_at): Row,
    staged: &crate::StagedProposal,
) -> Result<RecordedReconciliation, StagingError> {
    if json.len() > MAX_RESULT_BYTES || proposal_id != staged.proposal_id {
        return Err(StagingError::InvalidRecord);
    }
    let result =
        ReconciliationResultV1::decode(json.as_bytes()).map_err(|_| StagingError::InvalidRecord)?;
    if result.result_id != result_id || result.validate_for(staged).is_err() {
        return Err(StagingError::InvalidRecord);
    }
    Ok(RecordedReconciliation {
        result,
        recorded_at,
    })
}

impl ProposalStore {
    /// Record one result for the staged record it names. The result must be
    /// valid for that record's complete original content; the staged row is
    /// only read. Recording the same result again is idempotent and returns
    /// the first recording.
    pub fn record_reconciliation(
        &mut self,
        result: &ReconciliationResultV1,
    ) -> Result<RecordedReconciliation, StagingError> {
        let json = String::from_utf8(result.canonical_bytes()?)
            .map_err(|_| StagingError::InvalidRecord)?;
        ReconciliationResultV1::decode(json.as_bytes())?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let staged = get_on(&tx, &result.proposal_id)?
            .ok_or_else(|| StagingError::UnknownProposal(result.proposal_id.clone()))?;
        result.validate_for(&staged)?;
        tx.execute(
            "INSERT INTO staged_reconciliation_results (result_id, proposal_id, result_json)
             VALUES (?1, ?2, ?3) ON CONFLICT(result_id) DO NOTHING",
            params![result.result_id, result.proposal_id, json],
        )?;
        let stored = tx
            .query_row(
                &format!("{SELECT} WHERE result_id = ?1"),
                params![result.result_id],
                read_row,
            )
            .optional()?
            .ok_or(StagingError::InvalidRecord)?;
        let recorded = decode(stored, &staged)?;
        if recorded.result != *result {
            return Err(StagingError::InvalidRecord);
        }
        tx.commit()?;
        Ok(recorded)
    }

    /// The record's results, newest first, at most `limit`
    /// (1..=[`MAX_RECONCILIATION_HISTORY`]). Every row is strictly decoded and
    /// bound to the record; one that fails is an error, never skipped.
    pub fn reconciliations(
        &self,
        proposal_id: &str,
        limit: usize,
    ) -> Result<Vec<RecordedReconciliation>, StagingError> {
        if limit == 0 || limit > MAX_RECONCILIATION_HISTORY {
            return Err(StagingError::InvalidLimit);
        }
        let tx = self.conn.unchecked_transaction()?;
        let staged = get_on(&tx, proposal_id)?
            .ok_or_else(|| StagingError::UnknownProposal(proposal_id.into()))?;
        let rows = {
            let mut stmt = tx.prepare(&format!(
                "{SELECT} WHERE proposal_id = ?1 ORDER BY sequence DESC LIMIT ?2"
            ))?;
            stmt.query_map(params![proposal_id, limit as i64], read_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let recorded = rows
            .into_iter()
            .map(|row| decode(row, &staged))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit()?;
        Ok(recorded)
    }

    /// The record's latest result, if any.
    pub fn latest_reconciliation(
        &self,
        proposal_id: &str,
    ) -> Result<Option<RecordedReconciliation>, StagingError> {
        Ok(self.reconciliations(proposal_id, 1)?.into_iter().next())
    }
}

#[cfg(test)]
mod tests;
