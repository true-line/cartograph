//! Projects: start a new one, switch between them, without reinstalling
//! (#1994, AC-1013, AC-1014).
//!
//! A project is everything Cartograph keeps about one system: the entries
//! of the app data directory in [`PROJECT_ENTRIES`] and every `state.db`
//! table but [`KEPT_TABLES`]. The loaded project lives at the root of the
//! app data directory, where every store expects it. Each other project is
//! parked whole under `projects/<id>/`, so switching back needs no re-ingest.
//! What belongs to the application ([`KEPT`], [`KEPT_TABLES`]) stays at the
//! root and is shared by every project.
//!
//! **Changes run at launch.** [`request`] only writes a marker naming the
//! change; the host then restarts. The next launch calls [`apply`] before
//! any store is open, so no store holds a connection, a cache or a lock on
//! what moves. The marker records each finished step, so a launch that
//! stops part way is finished by the next one and no step runs twice.
//!
//! **The steps.** Park: the loaded project's entries and its `state.db`
//! move into `projects/<id>/`, with its name and repositories. Then either
//! a fresh `state.db` is made (a new project) or the chosen project's
//! entries and `state.db` move back to the root (a switch). Either way the
//! application's tables are copied from the parked `state.db` into the
//! loaded one, so settings follow the application, not the project.
//!
//! Provider keys live in the OS keychain and are not touched. An entry in
//! neither list is left where it is.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file whose presence asks the next launch to change project.
pub(crate) const MARKER: &str = "project-change.pending";

/// The loaded project's identity and name.
pub(crate) const ACTIVE: &str = "project.json";

/// Where parked projects live.
pub(crate) const PARKED: &str = "projects";

/// The suffix of a parked project being deleted.
const DELETING: &str = ".deleting";

/// The entries of the app data directory that belong to the loaded project.
pub(crate) const PROJECT_ENTRIES: &[&str] = &[
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

/// The entries that belong to the application and stay at the root.
/// [`apply`] moves only what [`PROJECT_ENTRIES`] names; this list is what a
/// test checks every entry a launch makes against, so a new store is never
/// left out of both.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read by the test that checks every entry is listed"
    )
)]
pub(crate) const KEPT: &[&str] = &[
    "state.db",
    "state.db-wal",
    "state.db-shm",
    "logs",
    "adapters",
    "registries",
    "mcp-runtime",
    PARKED,
    ACTIVE,
    MARKER,
];

/// The `state.db` tables that belong to the application; every other table
/// belongs to the project.
pub(crate) const KEPT_TABLES: &[&str] = &[
    "tier_settings",
    "plugin_settings",
    "plugin_gates",
    "ingest_settings",
    "settings_investigation_choice",
    "run_limit_settings",
    "settings_local_model",
    "settings_local_context",
    "settings_local_endpoint",
    "egress_log",
    "investigation_mcp_clients",
];

/// A project's identity and what the picker shows for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectMeta {
    /// Opaque, made by the host: 16 lowercase hex digits.
    pub id: String,
    /// The name the user gave it, if any.
    #[serde(default)]
    pub name: Option<String>,
    /// The display names of its registered repositories when it was parked.
    #[serde(default)]
    pub repos: Vec<String>,
}

/// One project as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectEntry {
    #[serde(flatten)]
    pub meta: ProjectMeta,
    /// Whether it is the loaded project.
    pub active: bool,
}

/// What the next launch is asked to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "kebab-case")]
pub(crate) enum Change {
    /// Park the loaded project and start an empty one.
    New,
    /// Park the loaded project and load the parked project `to`.
    Switch { to: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    #[serde(flatten)]
    change: Change,
    /// The loaded project's id once it is parked.
    #[serde(default)]
    parked: Option<String>,
}

fn is_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let seed = format!("{nanos}:{}", std::process::id());
    core_prov::content_hash(seed.as_bytes())[..16].to_owned()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| format!("{} is not readable", file_name(path))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Write `value` to `path` whole or not at all.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parked_dir(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join(PARKED).join(id)
}

/// The loaded project's identity, made on first use: an installation from
/// before projects holds one project, which becomes this one.
pub(crate) fn active(data_dir: &Path) -> Result<ProjectMeta, String> {
    if let Some(meta) = read_json::<ProjectMeta>(&data_dir.join(ACTIVE))?
        && is_id(&meta.id)
    {
        return Ok(meta);
    }
    let meta = ProjectMeta {
        id: new_id(),
        name: None,
        repos: Vec::new(),
    };
    write_json(&data_dir.join(ACTIVE), &meta)?;
    Ok(meta)
}

/// Every project: the loaded one first, then the parked ones by name.
pub(crate) fn list(data_dir: &Path) -> Result<Vec<ProjectEntry>, String> {
    let mut parked = Vec::new();
    match std::fs::read_dir(data_dir.join(PARKED)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let id = entry.file_name().to_string_lossy().into_owned();
                if id.ends_with(DELETING) {
                    // A deletion that stopped part way is finished here.
                    let _ = remove(&entry.path());
                    continue;
                }
                if !is_id(&id) {
                    continue;
                }
                if let Some(meta) = read_json::<ProjectMeta>(&entry.path().join(ACTIVE))?
                    && meta.id == id
                {
                    parked.push(ProjectEntry {
                        meta,
                        active: false,
                    });
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    parked.sort_by(|left, right| {
        (display(&left.meta), &left.meta.id).cmp(&(display(&right.meta), &right.meta.id))
    });
    let mut all = vec![ProjectEntry {
        meta: active(data_dir)?,
        active: true,
    }];
    all.extend(parked);
    Ok(all)
}

fn display(meta: &ProjectMeta) -> String {
    meta.name.clone().unwrap_or_else(|| meta.repos.join(", "))
}

/// Name a project, the loaded one or a parked one. An empty name clears it.
pub(crate) fn rename(data_dir: &Path, id: &str, name: &str) -> Result<ProjectMeta, String> {
    let name = name.trim();
    if name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err("A project name is at most 120 characters, on one line.".into());
    }
    let path = if active(data_dir)?.id == id {
        data_dir.join(ACTIVE)
    } else if is_id(id) && parked_dir(data_dir, id).join(ACTIVE).exists() {
        parked_dir(data_dir, id).join(ACTIVE)
    } else {
        return Err("There is no such project.".into());
    };
    let mut meta: ProjectMeta = read_json(&path)?.ok_or("There is no such project.")?;
    meta.name = (!name.is_empty()).then(|| name.to_owned());
    write_json(&path, &meta)?;
    Ok(meta)
}

/// Delete a parked project. The loaded project cannot be deleted: start a
/// new one or switch first.
pub(crate) fn delete(data_dir: &Path, id: &str) -> Result<(), String> {
    if !is_id(id) || active(data_dir)?.id == id {
        return Err("Only a project that is not loaded can be deleted.".into());
    }
    let dir = parked_dir(data_dir, id);
    if !dir.join(ACTIVE).exists() {
        return Err("There is no such project.".into());
    }
    if pending_change(data_dir)?.is_some_and(|change| change == Change::Switch { to: id.into() }) {
        return Err("This project is about to be loaded.".into());
    }
    // One rename takes it out of the listing whole; a removal that stops
    // part way is finished by the next listing.
    let doomed = data_dir.join(PARKED).join(format!("{id}{DELETING}"));
    std::fs::rename(&dir, &doomed).map_err(|e| e.to_string())?;
    remove(&doomed).map_err(|e| e.to_string())
}

/// Ask the next launch to make `change`.
pub(crate) fn request(data_dir: &Path, change: Change) -> Result<(), String> {
    if let Change::Switch { to } = &change
        && (!is_id(to) || !parked_dir(data_dir, to).join(ACTIVE).exists())
    {
        return Err("There is no such project.".into());
    }
    write_json(
        &data_dir.join(MARKER),
        &Pending {
            change,
            parked: None,
        },
    )
}

/// The change waiting for the next launch, if any.
pub(crate) fn pending_change(data_dir: &Path) -> Result<Option<Change>, String> {
    Ok(read_json::<Pending>(&data_dir.join(MARKER))?.map(|pending| pending.change))
}

/// Make the change a [`request`] asked for, if any. `create_state` makes a
/// `state.db` with every store's schema. Returns whether a change was made.
/// Call it before any store is open.
pub(crate) fn apply(
    data_dir: &Path,
    create_state: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<bool, String> {
    let marker = data_dir.join(MARKER);
    let Some(mut pending) = read_json::<Pending>(&marker)
        .map_err(|e| format!("The project change could not be read: {e}"))?
    else {
        return Ok(false);
    };
    let failed = |what: &str, error: &dyn std::fmt::Display| {
        format!("Changing project could not {what}: {error}")
    };
    let parked = match pending.parked.clone() {
        Some(id) => id,
        None => {
            let id = park(data_dir).map_err(|e| failed("set the loaded project aside", &e))?;
            pending.parked = Some(id.clone());
            write_json(&marker, &pending).map_err(|e| failed("record its progress", &e))?;
            id
        }
    };
    let parked_state = parked_dir(data_dir, &parked).join("state.db");
    let state = data_dir.join("state.db");
    match &pending.change {
        Change::New => {
            // A launch that stopped here left a partial state; it is made again.
            for name in ["state.db", "state.db-wal", "state.db-shm"] {
                remove(&data_dir.join(name)).map_err(|e| failed(&format!("remove {name}"), &e))?;
            }
            create_state(&state).map_err(|e| failed("create the new state", &e))?;
            write_json(
                &data_dir.join(ACTIVE),
                &ProjectMeta {
                    id: new_id(),
                    name: None,
                    repos: Vec::new(),
                },
            )
            .map_err(|e| failed("name the new project", &e))?;
        }
        Change::Switch { to } => {
            let source = parked_dir(data_dir, to);
            let names = PROJECT_ENTRIES.iter().copied().chain(["state.db", ACTIVE]);
            for name in names {
                move_entry(&source.join(name), &data_dir.join(name))
                    .map_err(|e| failed(&format!("load {name}"), &e))?;
            }
            if !state.exists() {
                // The chosen project was never parked whole; it starts empty.
                create_state(&state).map_err(|e| failed("create the new state", &e))?;
            }
            remove(&source).map_err(|e| failed("tidy the loaded project", &e))?;
        }
    }
    if parked_state.exists() {
        carry_kept_tables(&state, &parked_state).map_err(|e| failed("keep the settings", &e))?;
    }
    std::fs::remove_file(&marker).map_err(|e| failed("finish", &e))?;
    Ok(true)
}

/// Move the loaded project's entries and its `state.db` into
/// `projects/<id>/` with its metadata, and return its id. Each entry moves
/// at most once, so a park that stopped part way resumes.
///
/// The root `project.json` stays until the change that follows replaces
/// it, so a resumed park finds the same project.
fn park(data_dir: &Path) -> Result<String, String> {
    let active = active(data_dir)?;
    let dir = parked_dir(data_dir, &active.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut meta = read_json::<ProjectMeta>(&dir.join(ACTIVE))?.unwrap_or_else(|| active.clone());
    meta.name.clone_from(&active.name);
    let state = data_dir.join("state.db");
    if state.exists() {
        // Fold the write-ahead log in, so the one file holds every row.
        let conn = Connection::open(&state).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(|e| e.to_string())?;
        meta.repos = repos(&conn);
    }
    write_json(&dir.join(ACTIVE), &meta)?;
    for name in PROJECT_ENTRIES.iter().copied().chain(["state.db"]) {
        move_entry(&data_dir.join(name), &dir.join(name)).map_err(|e| e.to_string())?;
    }
    for name in ["state.db-wal", "state.db-shm"] {
        remove(&data_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(meta.id)
}

/// The display names of the registered repositories in `conn`, sorted;
/// none when the table is missing or unreadable.
fn repos(conn: &Connection) -> Vec<String> {
    let read = || -> rusqlite::Result<Vec<String>> {
        let mut statement = conn.prepare(
            "SELECT display_name FROM registered_sources ORDER BY display_name, repo_key",
        )?;
        statement.query_map([], |row| row.get(0))?.collect()
    };
    read().unwrap_or_default()
}

/// Move `from` to `to`, replacing what is at `to`. A `from` that is gone
/// was moved by an earlier launch, so nothing is done.
fn move_entry(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(from).is_err() {
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_ok() {
        remove(to)?;
    }
    std::fs::rename(from, to)
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Copy each kept table's rows from `from` into `state`, by the columns both
/// have, in one transaction. A table either lacks is skipped. `from` is
/// only read from.
fn carry_kept_tables(state: &Path, from: &Path) -> rusqlite::Result<()> {
    let mut conn = Connection::open(state)?;
    conn.execute("ATTACH DATABASE ?1 AS old", [from.to_string_lossy()])?;
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
#[path = "projects_tests.rs"]
mod tests;
