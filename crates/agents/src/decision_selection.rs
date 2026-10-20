//! Bounded historical assertion decisions and effective compiler rejection policy.
//! These projections establish neither freshness nor typed graph association.

use crate::{
    AssertionDecision, AssertionDecisionRecord, DecisionLog, parse_assertion_decision_row,
};
use rusqlite::{OptionalExtension, Row};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// Raw selection count, before duplicate normalization.
pub const MAX_SELECTED_ASSERTION_KEYS: usize = 64;
/// Combined UTF-8 bytes of the four stored text fields in one selected row.
pub const MAX_SELECTED_ASSERTION_ROW_BYTES: usize = 16 * 1024;
/// Cumulative selected stored text, and independently serialized selected output.
pub const MAX_SELECTED_ASSERTION_BYTES: usize = 128 * 1024;
/// Maximum decision rows examined by a complete compiler-policy observation.
pub const MAX_CURATION_ROWS: usize = 50_000;
/// Combined assertion JSON, decision, note and timestamp bytes in a full scan.
pub const MAX_CURATION_BYTES: usize = 32 * 1024 * 1024;

const ADMISSION_COLUMNS: &str = "typeof(assertion_json) = 'text'
    AND typeof(decision) = 'text' AND typeof(updated_at) = 'text'
    AND typeof(note) IN ('text', 'null'),
    length(CAST(assertion_json AS BLOB)), length(CAST(decision AS BLOB)),
    coalesce(length(CAST(note AS BLOB)), 0), length(CAST(updated_at AS BLOB))";
type RowAdmission = (bool, [Option<i64>; 4]);

/// One exact requested hash, including explicit absence from the decision log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectedAssertionDecision {
    /// Exact lowercase content hash requested by the host.
    pub content_hash: String,
    /// Historical decision, or explicit absence within the observed revision.
    pub record: Option<AssertionDecisionRecord>,
}

/// Owned compiler policy and selected annotations from one database revision.
/// Equality compares effective policy and complete selected records, rather than
/// relying on timestamps or treating unrelated accepted-note updates as changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurationObservation {
    /// Exact effective rejection set used by the existing specification compiler.
    pub rejected_hashes: BTreeSet<String>,
    /// Sorted unique requested decisions, with missing records retained.
    pub selected: Vec<SelectedAssertionDecision>,
}

/// Fixed categories never expose stored source, notes or SQL diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecisionSelectionError {
    /// At least one requested key is not a lowercase 64-digit hexadecimal hash.
    #[error("invalid assertion decision selection")]
    InvalidSelection,
    /// A key count, stored text, complete scan or selected output exceeds its bound.
    #[error("assertion decision observation exceeds the byte or item limit")]
    LimitExceeded,
    /// A row violates its stored shape, assertion contract or database key binding.
    #[error("observed assertion decision is invalid")]
    InvalidRecord,
    /// The local transaction or database query could not complete.
    #[error("assertion decision observation could not be read")]
    Storage,
}

impl DecisionLog {
    /// Read sorted unique exact hashes and their historical decisions within one
    /// SQLite read transaction. Missing rows remain explicit; unrelated rows are
    /// never loaded. Stored text lengths are admitted before text allocation.
    pub fn read_selected_assertions(
        &self,
        content_hashes: &[String],
    ) -> Result<Vec<SelectedAssertionDecision>, DecisionSelectionError> {
        use DecisionSelectionError::{InvalidRecord, Storage};
        let keys = selection_keys(content_hashes)?;
        let tx = self.conn.unchecked_transaction().map_err(|_| Storage)?;
        let mut present = BTreeSet::new();
        let mut total = 0usize;
        for key in &keys {
            let admission = tx
                .query_row(
                    &format!(
                        "SELECT {ADMISSION_COLUMNS} FROM assertion_decisions WHERE content_hash=?1"
                    ),
                    [key.as_str()],
                    |row| row_admission(row, 0),
                )
                .optional()
                .map_err(|_| Storage)?;
            let Some(admission) = admission else {
                continue;
            };
            charge(
                &mut total,
                admit_row(admission)?,
                MAX_SELECTED_ASSERTION_BYTES,
            )?;
            present.insert(*key);
        }
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            let record = if present.contains(key) {
                let mut statement = tx
                    .prepare(
                        "SELECT assertion_json, decision, note, updated_at
                         FROM assertion_decisions WHERE content_hash=?1",
                    )
                    .map_err(|_| Storage)?;
                let mut rows = statement.query([key.as_str()]).map_err(|_| Storage)?;
                let row = rows.next().map_err(|_| Storage)?.ok_or(InvalidRecord)?;
                Some(parse_bound_record(row, key)?)
            } else {
                None
            };
            result.push(SelectedAssertionDecision {
                content_hash: key.clone(),
                record,
            });
        }
        check_selected_output(&result)?;
        tx.commit().map_err(|_| Storage)?;
        Ok(result)
    }

    /// Observe the complete effective compiler rejection policy and requested
    /// historical records together. Unlike selected-only reads, every stored row
    /// is admitted and validated, including accepted and unselected decisions.
    /// The scan retains no whole history: only rejected hashes and at most 64
    /// requested records survive. Fixed 64-byte database keys are validated in
    /// addition to the four-field text budgets. Selected stored text and selected
    /// serialized output each have a 128 KiB bound, independently of the complete
    /// scan budget; the rejection set is bounded by row count.
    pub fn read_curation_observation(
        &self,
        content_hashes: &[String],
    ) -> Result<CurationObservation, DecisionSelectionError> {
        use DecisionSelectionError::{InvalidRecord, LimitExceeded, Storage};
        let keys = selection_keys(content_hashes)?;
        let tx = self.conn.unchecked_transaction().map_err(|_| Storage)?;
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM (SELECT 1 FROM assertion_decisions LIMIT ?1)",
                [(MAX_CURATION_ROWS + 1) as i64],
                |row| row.get(0),
            )
            .map_err(|_| Storage)?;
        if count < 0 || count as u64 > MAX_CURATION_ROWS as u64 {
            return Err(LimitExceeded);
        }
        // Admit all owned text before parsing any row. Both passes share the
        // read transaction, so another connection cannot grow a checked field.
        {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT typeof(content_hash)='text' AND length(CAST(content_hash AS BLOB))=64,
                 {ADMISSION_COLUMNS}, content_hash FROM assertion_decisions"
                ))
                .map_err(|_| Storage)?;
            let mut rows = statement.query([]).map_err(|_| Storage)?;
            let mut total = 0usize;
            let mut selected_total = 0usize;
            while let Some(row) = rows.next().map_err(|_| Storage)? {
                if !row.get::<_, bool>(0).map_err(|_| InvalidRecord)? {
                    return Err(InvalidRecord);
                }
                let bytes = admit_row(row_admission(row, 1).map_err(|_| InvalidRecord)?)?;
                charge(&mut total, bytes, MAX_CURATION_BYTES)?;
                // The key's type and exact byte length were checked above;
                // matching selected lengths cannot allocate an oversized key.
                let key: String = row.get(6).map_err(|_| InvalidRecord)?;
                if keys.contains(&key) {
                    charge(&mut selected_total, bytes, MAX_SELECTED_ASSERTION_BYTES)?;
                }
            }
        }
        let mut selected: Vec<_> = keys
            .into_iter()
            .map(|key| SelectedAssertionDecision {
                content_hash: key.clone(),
                record: None,
            })
            .collect();
        let indexes: BTreeMap<_, _> = selected
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.content_hash.clone(), index))
            .collect();
        let mut rejected_hashes = BTreeSet::new();
        {
            let mut statement = tx
                .prepare(
                    "SELECT assertion_json,decision,note,updated_at,content_hash
                 FROM assertion_decisions ORDER BY content_hash",
                )
                .map_err(|_| Storage)?;
            let mut rows = statement.query([]).map_err(|_| Storage)?;
            while let Some(row) = rows.next().map_err(|_| Storage)? {
                let key: String = row.get(4).map_err(|_| InvalidRecord)?;
                let record = parse_bound_record(row, &key)?;
                if record.decision == AssertionDecision::Rejected {
                    rejected_hashes.insert(key.clone());
                }
                if let Some(&index) = indexes.get(&key) {
                    selected[index].record = Some(record);
                    check_selected_output(&selected)?;
                }
            }
        }
        check_selected_output(&selected)?;
        tx.commit().map_err(|_| Storage)?;
        Ok(CurationObservation {
            rejected_hashes,
            selected,
        })
    }
}

fn selection_keys(hashes: &[String]) -> Result<BTreeSet<&String>, DecisionSelectionError> {
    if hashes.len() > MAX_SELECTED_ASSERTION_KEYS {
        return Err(DecisionSelectionError::LimitExceeded);
    }
    if hashes.iter().any(|hash| !crate::source_basis::hash(hash)) {
        return Err(DecisionSelectionError::InvalidSelection);
    }
    Ok(hashes.iter().collect())
}

fn row_admission(row: &Row<'_>, offset: usize) -> rusqlite::Result<RowAdmission> {
    Ok((
        row.get(offset)?,
        [
            row.get(offset + 1)?,
            row.get(offset + 2)?,
            row.get(offset + 3)?,
            row.get(offset + 4)?,
        ],
    ))
}

fn admit_row((valid_types, lengths): RowAdmission) -> Result<usize, DecisionSelectionError> {
    use DecisionSelectionError::{InvalidRecord, LimitExceeded};
    if !valid_types {
        return Err(InvalidRecord);
    }
    let mut bytes = 0usize;
    for length in lengths {
        let length = length
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(InvalidRecord)?;
        charge(&mut bytes, length, MAX_SELECTED_ASSERTION_ROW_BYTES).map_err(|_| LimitExceeded)?;
    }
    Ok(bytes)
}

fn charge(total: &mut usize, bytes: usize, limit: usize) -> Result<(), DecisionSelectionError> {
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= limit)
        .ok_or(DecisionSelectionError::LimitExceeded)?;
    Ok(())
}

fn parse_bound_record(
    row: &Row<'_>,
    key: &str,
) -> Result<AssertionDecisionRecord, DecisionSelectionError> {
    use DecisionSelectionError::InvalidRecord;
    if !crate::source_basis::hash(key) {
        return Err(InvalidRecord);
    }
    let stored = crate::read_decision_row(row).map_err(|_| InvalidRecord)?;
    let record = parse_assertion_decision_row(stored).map_err(|_| InvalidRecord)?;
    if record.assertion.provenance.content_hash != key {
        return Err(InvalidRecord);
    }
    Ok(record)
}

fn check_selected_output(
    result: &[SelectedAssertionDecision],
) -> Result<(), DecisionSelectionError> {
    // Count escaping without allocating an expanded JSON representation.
    serde_json::to_writer(&mut ResultBudget(0), result)
        .map_err(|_| DecisionSelectionError::LimitExceeded)
}

struct ResultBudget(usize);
impl Write for ResultBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SELECTED_ASSERTION_BYTES.saturating_sub(self.0) {
            return Err(std::io::Error::other("selected decision byte bound"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
