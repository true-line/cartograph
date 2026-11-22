//! Bounded registry materialization; no implicit host policy or RSS claim.
use rusqlite::Connection;

/// Explicit caller ceilings for the complete registered-source inventory.
#[derive(Debug, Clone, Copy)]
pub struct SourceListLimits {
    pub max_rows: usize,
    /// Sum of raw UTF-8 bytes in all eight selected SQL text fields; NULL costs zero.
    pub max_bytes: usize,
}

/// Closed failures contain no registry locators, SQL or stored cell contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceListError {
    #[error("source registry row limit exceeded")]
    RowLimit,
    #[error("source registry byte limit exceeded")]
    ByteLimit,
    #[error("source registry state is invalid")]
    InvalidState,
    #[error("source registry storage is unavailable")]
    Storage,
}

pub(super) fn registry_error(error: String) -> SourceListError {
    if error == super::INVALID_STATE {
        SourceListError::InvalidState
    } else {
        SourceListError::Storage
    }
}

pub(super) fn preflight(
    conn: &Connection,
    limits: SourceListLimits,
) -> Result<(), SourceListError> {
    let count: i64 = conn
        .query_row("SELECT count(*) FROM registered_sources", [], |row| {
            row.get(0)
        })
        .map_err(|_| SourceListError::Storage)?;
    let count = usize::try_from(count).map_err(|_| SourceListError::InvalidState)?;
    if count > limits.max_rows {
        return Err(SourceListError::RowLimit);
    }
    // Only booleans and byte counts cross SQLite's boundary here. No text cell
    // is decoded or copied into Rust until every row has passed this scan.
    let columns = [
        "source_id",
        "repo_key",
        "root",
        "display_name",
        "kind",
        "origin_key",
        "clone_url",
        "github_repo",
    ];
    let projection = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let valid = if index < 5 {
                format!("typeof({column})='text'")
            } else {
                format!("typeof({column}) IN ('text','null')")
            };
            format!("{valid}, coalesce(length(CAST({column} AS BLOB)),0)")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT {projection}, typeof(ready)='integer' AND ready IN (0,1) FROM registered_sources"
    )).map_err(|_| SourceListError::Storage)?;
    let mut rows = statement.query([]).map_err(|_| SourceListError::Storage)?;
    let mut remaining = limits.max_bytes;
    while let Some(row) = rows.next().map_err(|_| SourceListError::Storage)? {
        for index in 0..columns.len() {
            if !row
                .get::<_, bool>(2 * index)
                .map_err(|_| SourceListError::Storage)?
            {
                return Err(SourceListError::InvalidState);
            }
            let length: i64 = row
                .get(2 * index + 1)
                .map_err(|_| SourceListError::Storage)?;
            let length = usize::try_from(length).map_err(|_| SourceListError::InvalidState)?;
            remaining = remaining
                .checked_sub(length)
                .ok_or(SourceListError::ByteLimit)?;
        }
        if !row
            .get::<_, bool>(2 * columns.len())
            .map_err(|_| SourceListError::Storage)?
        {
            return Err(SourceListError::InvalidState);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "bounded_tests.rs"]
mod tests;
