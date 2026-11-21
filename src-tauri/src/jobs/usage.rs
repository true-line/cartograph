//! Provider-reported usage of a job's model calls (#252, AC-1703).
//!
//! One row per job, summed over every bounded call the job made. Counts are
//! what each provider reported: a call that reported no token count is
//! counted in `unreported_calls`, and its tokens are never estimated. This
//! is not verified billing.
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;

/// What a job's model calls used, as the providers reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobUsage {
    pub job_id: i64,
    /// Bounded calls the job made that returned.
    pub calls: u64,
    /// Calls whose provider reported no input or no output token count.
    pub unreported_calls: u64,
    /// Summed over the calls that reported both counts.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Bytes of every request sent.
    pub request_bytes: u64,
    /// The model the latest call asked for.
    pub model: String,
}

/// One call's usage.
pub struct CallUsage<'a> {
    pub model: &'a str,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub request_bytes: u64,
}

pub(super) fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS job_usage (
             job_id           INTEGER PRIMARY KEY,
             calls            INTEGER NOT NULL CHECK (calls >= 0),
             unreported_calls INTEGER NOT NULL CHECK (unreported_calls >= 0),
             input_tokens     INTEGER NOT NULL CHECK (input_tokens >= 0),
             output_tokens    INTEGER NOT NULL CHECK (output_tokens >= 0),
             request_bytes    INTEGER NOT NULL CHECK (request_bytes >= 0),
             model            TEXT NOT NULL
         ) STRICT;",
    )
}

fn stored(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Add one call to the job's sums. Sums saturate rather than wrap.
pub(super) fn add(tx: &Transaction<'_>, job_id: i64, call: &CallUsage<'_>) -> rusqlite::Result<()> {
    let (input, output, unreported) = match (call.input_tokens, call.output_tokens) {
        (Some(input), Some(output)) => (input, output, 0),
        _ => (0, 0, 1),
    };
    let current: Option<(i64, i64, i64, i64, i64)> = tx
        .query_row(
            "SELECT calls, unreported_calls, input_tokens, output_tokens, request_bytes
             FROM job_usage WHERE job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (calls, unreported_calls, input_tokens, output_tokens, request_bytes) =
        current.unwrap_or_default();
    tx.execute(
        "INSERT INTO job_usage
             (job_id, calls, unreported_calls, input_tokens, output_tokens, request_bytes, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(job_id) DO UPDATE SET
             calls = excluded.calls, unreported_calls = excluded.unreported_calls,
             input_tokens = excluded.input_tokens, output_tokens = excluded.output_tokens,
             request_bytes = excluded.request_bytes, model = excluded.model",
        params![
            job_id,
            calls.saturating_add(1),
            unreported_calls.saturating_add(unreported),
            input_tokens.saturating_add(stored(input)),
            output_tokens.saturating_add(stored(output)),
            request_bytes.saturating_add(stored(call.request_bytes)),
            call.model,
        ],
    )?;
    Ok(())
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Usage of every job that still exists, newest job first.
pub(super) fn list(tx: &Transaction<'_>) -> rusqlite::Result<Vec<JobUsage>> {
    let mut stmt = tx.prepare(
        "SELECT u.job_id, u.calls, u.unreported_calls, u.input_tokens, u.output_tokens,
                u.request_bytes, u.model
         FROM job_usage u JOIN jobs j ON j.id = u.job_id
         ORDER BY u.job_id DESC",
    )?;
    stmt.query_map([], |row| {
        Ok(JobUsage {
            job_id: row.get(0)?,
            calls: count(row.get(1)?),
            unreported_calls: count(row.get(2)?),
            input_tokens: count(row.get(3)?),
            output_tokens: count(row.get(4)?),
            request_bytes: count(row.get(5)?),
            model: row.get(6)?,
        })
    })?
    .collect()
}

/// Drop the usage of jobs that no longer exist.
pub(super) fn prune(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM job_usage WHERE job_id NOT IN (SELECT id FROM jobs)",
        [],
    )?;
    Ok(())
}
