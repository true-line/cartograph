//! The persisted agent pool controls (#252, AC-1704): whether new agent
//! work is paused and the per-provider-mode concurrency limits. The app's
//! `agent_pool` module admits and holds jobs against them; this is only
//! their table, split out so the settings store can open it from app-store
//! (#1924).
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// Largest per-mode limit offered.
pub const MAX_LIMIT: u32 = 16;

/// The persisted controls. A limit of `0` means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPoolControls {
    pub paused: bool,
    pub local_limit: u32,
    pub cloud_limit: u32,
}

impl Default for AgentPoolControls {
    /// Nothing paused and no limit: the behavior before these controls.
    fn default() -> Self {
        Self {
            paused: false,
            local_limit: 0,
            cloud_limit: 0,
        }
    }
}

pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS agent_pool_controls (
             id          INTEGER PRIMARY KEY CHECK (id = 1),
             paused      INTEGER NOT NULL CHECK (paused IN (0, 1)),
             local_limit INTEGER NOT NULL CHECK (local_limit BETWEEN 0 AND {MAX_LIMIT}),
             cloud_limit INTEGER NOT NULL CHECK (cloud_limit BETWEEN 0 AND {MAX_LIMIT})
         ) STRICT;
         INSERT OR IGNORE INTO agent_pool_controls (id, paused, local_limit, cloud_limit)
         VALUES (1, 0, 0, 0);"
    ))
}

pub fn read(conn: &Connection) -> rusqlite::Result<AgentPoolControls> {
    conn.query_row(
        "SELECT paused, local_limit, cloud_limit FROM agent_pool_controls WHERE id = 1",
        [],
        |row| {
            Ok(AgentPoolControls {
                paused: row.get(0)?,
                local_limit: row.get(1)?,
                cloud_limit: row.get(2)?,
            })
        },
    )
}

pub fn write(conn: &Connection, controls: &AgentPoolControls) -> Result<(), String> {
    for (name, limit) in [
        ("local", controls.local_limit),
        ("cloud", controls.cloud_limit),
    ] {
        if limit > MAX_LIMIT {
            return Err(format!(
                "The {name} agent limit must be between 0 (no limit) and {MAX_LIMIT}, not {limit}."
            ));
        }
    }
    conn.execute(
        "UPDATE agent_pool_controls SET paused = ?1, local_limit = ?2, cloud_limit = ?3 WHERE id = 1",
        params![controls.paused, controls.local_limit, controls.cloud_limit],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}
