//! Start a new project without reinstalling (#1994, AC-1012).
//!
//! Cartograph keeps one workspace under the app data directory, and every
//! ingest adds to it. **New project** removes everything tied to the system
//! loaded now and keeps what belongs to the application.
//!
//! **Two steps.** [`request`] only writes a marker; the host then restarts.
//! The next launch calls [`apply`] before any store is open, so no store
//! holds a connection, a cache or a lock on what is removed, and nothing a
//! running task wrote can come back. A launch that stops part way leaves
//! the marker and the next launch finishes the work.
//!
//! **What goes.** Each entry in [`REMOVED`]: the graph and its derived
//! caches, staged proposals and section drafts, retained source and its
//! receipts, managed checkouts, and the job and source locks. Every table
//! of `state.db` goes too (sources, findings, investigations and their
//! chats, reviews, decisions, work packets, jobs, metrics), except those in
//! [`KEPT_TABLES`].
//!
//! **What stays.** Settings, run limits, the local-model and investigation
//! choices, the egress log, MCP client identities ([`KEPT_TABLES`]), the
//! installed adapters and registries, the MCP runtime directory and the
//! debug log ([`KEPT`]). Provider keys live in the OS keychain and are not
//! touched. An entry in neither list is left as it is.

use rusqlite::Connection;
use std::path::Path;

/// The file whose presence asks the next launch to start a new project.
pub(crate) const MARKER: &str = "new-project.pending";

/// Where the old `state.db` waits while its kept tables are copied over.
const OLD_STATE: &str = "new-project-state.db";

/// The entries of the app data directory that belong to the loaded system.
pub(crate) const REMOVED: &[&str] = &[
    "graph.db",
    "graph.db-wal",
    "graph.db-shm",
    "embeddings.db",
    "embeddings.db-wal",
    "embeddings.db-shm",
    "knowledge-overlay.db",
    "knowledge-overlay.db-wal",
    "knowledge-overlay.db-shm",
    "proposals.sqlite",
    "proposals.sqlite-wal",
    "proposals.sqlite-shm",
    "section-drafts.sqlite",
    "section-drafts.sqlite-wal",
    "section-drafts.sqlite-shm",
    "retained-source",
    "sources",
    "source-locks",
    "job-executions",
];

/// The entries that belong to the application and stay.
pub(crate) const KEPT: &[&str] = &[
    "state.db",
    "state.db-wal",
    "state.db-shm",
    "logs",
    "adapters",
    "registries",
    "mcp-runtime",
    MARKER,
    OLD_STATE,
];

/// The `state.db` tables that belong to the application; every other table
/// starts empty.
pub(crate) const KEPT_TABLES: &[&str] = &[
    "tier_settings",
    "plugin_settings",
    "plugin_gates",
    "ingest_settings",
    "settings_investigation_choice",
    "run_limit_settings",
    "settings_local_model",
    "egress_log",
    "investigation_mcp_clients",
];

/// Ask the next launch to start a new project.
pub(crate) fn request(data_dir: &Path) -> std::io::Result<()> {
    std::fs::write(data_dir.join(MARKER), b"1\n")
}

/// Whether a new project is waiting for the next launch.
pub(crate) fn pending(data_dir: &Path) -> bool {
    data_dir.join(MARKER).exists()
}

/// Start the new project a [`request`] asked for, if any: remove what
/// belongs to the loaded system and carry the kept tables into a fresh
/// `state.db`, which `create_state` creates with every store's schema.
/// Returns whether a new project was started. Call it before any store is
/// open.
pub(crate) fn apply(
    data_dir: &Path,
    create_state: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<bool, String> {
    if !pending(data_dir) {
        // A launch that stopped after finishing but before tidying up left
        // the old state; the new one already holds what was kept.
        remove(&data_dir.join(OLD_STATE))
            .map_err(|e| format!("Starting a new project could not remove the old state: {e}"))?;
        return Ok(false);
    }
    let failed = |what: &str, error: &dyn std::fmt::Display| {
        format!("Starting a new project could not {what}: {error}")
    };
    for name in REMOVED {
        remove(&data_dir.join(name)).map_err(|e| failed(&format!("remove {name}"), &e))?;
    }
    let state = data_dir.join("state.db");
    let old = data_dir.join(OLD_STATE);
    if !old.exists() && state.exists() {
        // Fold the write-ahead log in, so the one file holds every row.
        Connection::open(&state)
            .and_then(|conn| conn.pragma_update(None, "wal_checkpoint", "TRUNCATE"))
            .map_err(|e| failed("read the current state", &e))?;
        std::fs::rename(&state, &old).map_err(|e| failed("set the current state aside", &e))?;
    }
    // A launch that stopped after setting the state aside left a partial
    // new one; it is made again.
    for name in ["state.db", "state.db-wal", "state.db-shm"] {
        remove(&data_dir.join(name)).map_err(|e| failed(&format!("remove {name}"), &e))?;
    }
    create_state(&state).map_err(|e| failed("create the new state", &e))?;
    if old.exists() {
        carry_kept_tables(&state, &old).map_err(|e| failed("keep the settings", &e))?;
    }
    // The new state is complete: removing the marker makes it the one the
    // next launch keeps. Only then is the old state removed, so a launch
    // that stops between the two still has every kept row.
    std::fs::remove_file(data_dir.join(MARKER)).map_err(|e| failed("finish", &e))?;
    remove(&old).map_err(|e| failed("remove the old state", &e))?;
    Ok(true)
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Copy each kept table's rows from `old` into `state`, by the columns both
/// have, in one transaction. A table either lacks is skipped.
fn carry_kept_tables(state: &Path, old: &Path) -> rusqlite::Result<()> {
    let mut conn = Connection::open(state)?;
    conn.execute("ATTACH DATABASE ?1 AS old", [old.to_string_lossy()])?;
    let tx = conn.transaction()?;
    for table in KEPT_TABLES {
        let columns = |schema: &str| -> rusqlite::Result<Vec<String>> {
            let mut statement =
                tx.prepare(&format!("SELECT name FROM {schema}.pragma_table_info(?1)"))?;
            statement.query_map([table], |row| row.get(0))?.collect()
        };
        let old_columns = columns("old")?;
        let shared: Vec<String> = columns("main")?
            .into_iter()
            .filter(|column| old_columns.contains(column))
            .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
            .collect();
        if shared.is_empty() {
            continue;
        }
        let list = shared.join(", ");
        tx.execute(&format!("DELETE FROM main.\"{table}\""), [])?;
        tx.execute(
            &format!("INSERT INTO main.\"{table}\" ({list}) SELECT {list} FROM old.\"{table}\""),
            [],
        )?;
    }
    tx.commit()?;
    conn.execute("DETACH DATABASE old", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "project_reset_tests.rs"]
mod tests;
