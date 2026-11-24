//! Host-owned logical source identities on the durable state spine (SPEC-05).
//! Registration binds a locator, not immutable bytes or a producer receipt.

use ingest::managed::{ManagedCheckout, ManagedOrigin, parse_managed_origin};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::{Component, Path, PathBuf};

#[path = "sources/bounded.rs"]
mod bounded;
#[path = "sources/retained_view.rs"]
mod retained_view;
pub use bounded::{SourceListError, SourceListLimits};
pub use retained_view::{RetainedSourceError, RetainedSourceView};

// Bound UTF-8 byte lengths before fetching retained strings. Paths allow the
// Windows long-path range; origin validation additionally applies ingest's cap.
pub const MAX_PATH_BYTES: usize = 128 * 1024;
const MAX_ORIGIN_BYTES: usize = 128 * 1024;
const MAX_DISPLAY_BYTES: usize = 4096;
const MAX_REPO_BYTES: usize = 512;
const ID_SUFFIX_BYTES: usize = 32;
const ID_RETRIES: usize = 8;
/// How long a writer waits for another registry connection's write
/// transaction. With WAL and `synchronous=FULL`, one commit's fsync can take
/// over a second on a loaded Windows disk (#839).
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const JOB_PREFIX: &str = "ingest-source-v1:";
const STORE_ERROR: &str = "Source registry storage is unavailable or invalid";
const INVALID_STATE: &str = "Source registry state is incompatible or corrupt";
const META_SCHEMA: &str = "CREATE TABLE source_registry_meta (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), schema_version INTEGER NOT NULL CHECK (schema_version = 1), registry_id TEXT NOT NULL, findings_retired INTEGER NOT NULL CHECK (findings_retired = 1)) STRICT";
const SOURCE_SCHEMA: &str = "CREATE TABLE registered_sources (source_id TEXT PRIMARY KEY NOT NULL, repo_key TEXT NOT NULL UNIQUE, root TEXT NOT NULL UNIQUE, display_name TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('local', 'managed')), origin_key TEXT UNIQUE, clone_url TEXT, github_repo TEXT, ready INTEGER NOT NULL CHECK (ready IN (0, 1)), CHECK ((kind = 'local' AND origin_key IS NULL AND clone_url IS NULL AND github_repo IS NULL AND ready = 1) OR (kind = 'managed' AND origin_key IS NOT NULL AND clone_url IS NOT NULL))) STRICT";
const SELECT_SOURCE: &str = "SELECT source_id, repo_key, root, display_name, kind, origin_key, clone_url, github_repo, ready FROM registered_sources";

/// The recovery error for an origin `parse_managed_origin` refused. It
/// echoes what was entered (trimmed, bounded) and names the accepted forms,
/// so a typo or a local path under the GitHub source is fixable in place.
/// Input that could carry a credential (userinfo, query, fragment, a
/// `file:` URL) gets a fixed message instead, never an echo (AC-0140).
fn managed_origin_error(input: &str) -> String {
    let trimmed = input.trim();
    let shown: String = trimmed
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let shown = if trimmed.chars().count() > 200 {
        format!("{shown}…")
    } else {
        shown
    };
    let bytes = trimmed.as_bytes();
    let local_path = trimmed.starts_with(['/', '\\', '~', '.'])
        || (bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes
                .get(2)
                .copied()
                .is_none_or(|c| matches!(c, b'\\' | b'/')));
    // Userinfo, a query or a fragment can carry a token; never echo those.
    let scp = trimmed
        .get(..15)
        .is_some_and(|s| s.eq_ignore_ascii_case("git@github.com:"));
    let may_hold_secret = trimmed[if scp { 15 } else { 0 }..].contains(['@', '?', '#']);
    if trimmed
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
    {
        "The local mirror was not found or is not a folder.".to_string()
    } else if may_hold_secret {
        "The GitHub address was refused: it carries a user name, token, query or \
         fragment. Enter https://github.com/owner/repo, git@github.com:owner/repo \
         or owner/repo."
            .to_string()
    } else if local_path {
        format!(
            "\"{shown}\" looks like a local folder, not a GitHub repository. \
             Choose \"Local folder\" as the source to recover it."
        )
    } else {
        format!(
            "\"{shown}\" is not a GitHub repository address. Enter \
             https://github.com/owner/repo, git@github.com:owner/repo or owner/repo."
        )
    }
}

fn storage_error(_: rusqlite::Error) -> String {
    // SQLite errors can include corrupt schema text. Do not expose that text.
    STORE_ERROR.into()
}

#[derive(Debug, Clone)]
pub struct RegisteredSource {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
    root: PathBuf,
    origin: Option<ManagedOrigin>,
    ready: bool,
    registry_id: String,
    app_data_dir: PathBuf,
}

/// Per-row host owner metadata copied in addition to admitted SQL text.
pub fn source_owner_metadata_bytes(rows: usize) -> Option<usize> {
    rows.checked_mul(
        MAX_PATH_BYTES
            .checked_add("reg_".len())?
            .checked_add(ID_SUFFIX_BYTES)?,
    )
}

/// One selected registry observation, including its validation temporaries.
/// lookup's coherent check_state SQL bounds every raw cell before decoding.
/// Logical payload/header roles only; allocator slack and SQLite are excluded.
pub fn selected_source_peak_bytes() -> Option<usize> {
    use context_hub::logical::{slots, sum};
    sum(&[
        "src_".len().checked_add(ID_SUFFIX_BYTES)?,
        MAX_REPO_BYTES,
        MAX_PATH_BYTES, // decoded root
        MAX_DISPLAY_BYTES,
        "managed".len(),
        MAX_ORIGIN_BYTES,  // origin key
        MAX_ORIGIN_BYTES,  // clone URL
        MAX_REPO_BYTES,    // optional GitHub repository
        MAX_DISPLAY_BYTES, // ManagedOrigin display clone
        source_owner_metadata_bytes(1)?,
        MAX_REPO_BYTES, // expected_repo validation clone
        MAX_PATH_BYTES
            .checked_add("/sources/src_/checkout".len())?
            .checked_add(ID_SUFFIX_BYTES)?, // ManagedCheckout root
        MAX_PATH_BYTES
            .checked_add("/sources/src_".len())?
            .checked_add(ID_SUFFIX_BYTES)?, // ManagedCheckout slot
        MAX_PATH_BYTES, // ManagedCheckout app_data_dir
        MAX_PATH_BYTES.checked_add("/sources".len())?, // intermediate join
        "reg_".len().checked_add(ID_SUFFIX_BYTES)?, // ManagedCheckout registry identity
        "src_".len().checked_add(ID_SUFFIX_BYTES)?, // ManagedCheckout source identity
        slots::<RegisteredSource>(1)?,
        std::mem::size_of::<ingest::managed::ManagedCheckout>(),
        "reg_".len().checked_add(ID_SUFFIX_BYTES)?, // check_state return
    ])
}

impl RegisteredSource {
    /// This source as the owner of receipts under another identity, such as
    /// its observation inputs' (ADR-0079): `id` is both its source id and
    /// its repo key.
    pub fn owning_as(&self, id: String) -> Self {
        Self {
            source_id: id.clone(),
            repo_key: id,
            ..self.clone()
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_managed(&self) -> bool {
        self.origin.is_some()
    }

    /// This is an availability check, not a freshness proof or operation guard.
    /// Callers acquire a managed read guard and re-read the registry before use.
    pub fn is_ready(&self) -> bool {
        self.ready
            && self.root.is_dir()
            && crate::paths::canonicalize(&self.root).is_ok_and(|root| root == self.root)
    }

    pub fn clone_url(&self) -> Option<&str> {
        self.origin.as_ref().map(|origin| origin.clone_url.as_str())
    }

    pub fn managed(&self) -> Result<Option<ManagedCheckout>, String> {
        self.origin
            .as_ref()
            .map(|_| {
                ManagedCheckout::new(&self.app_data_dir, &self.registry_id, &self.source_id)
                    .map_err(|_| INVALID_STATE.to_string())
            })
            .transpose()
    }

    pub fn ingest_job_kind(&self) -> String {
        format!("{JOB_PREFIX}{}", self.source_id)
    }
}

/// Resolve the binding before the caller mutates a retry job. Historical paths
/// are intentionally not guessed, canonicalized, or re-registered here.
pub fn source_id_from_ingest_job_kind(kind: &str) -> Result<&str, String> {
    if kind.starts_with("ingest:") {
        return Err("This historical job has no registered source; re-run ingestion".into());
    }
    let id = kind
        .strip_prefix(JOB_PREFIX)
        .filter(|id| valid_id(id, "src_"))
        .ok_or("This job does not have a supported registered ingestion binding")?;
    Ok(id)
}

pub struct SourceRegistry {
    #[cfg(any(test, feature = "test-support"))]
    checkout_accesses: std::sync::atomic::AtomicUsize,
    conn: Connection,
    registry_id: String,
    app_data_dir: PathBuf,
}

impl SourceRegistry {
    #[cfg(any(test, feature = "test-support"))]
    pub fn observe_checkout_access(&self) {
        self.checkout_accesses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn checkout_access_attempts(&self) -> usize {
        self.checkout_accesses
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn open(
        state_path: impl AsRef<Path>,
        app_data_dir: impl AsRef<Path>,
    ) -> Result<Self, String> {
        let app_data_dir = canonical_directory(app_data_dir.as_ref())?;
        let mut conn = Connection::open(state_path).map_err(storage_error)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage_error)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_error)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let existing: i64 = tx
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name IN ('source_registry_meta', 'registered_sources')",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if existing == 0 {
            tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(SOURCE_SCHEMA).map_err(storage_error)?;
            retire_legacy_findings(&tx)?;
            let registry_id = random_id(&tx, "reg_")?;
            require_one(
                tx.execute(
                    "INSERT INTO source_registry_meta(singleton, schema_version, registry_id, findings_retired) VALUES (1, 1, ?1, 1)",
                    [&registry_id],
                ).map_err(storage_error)?,
            )?;
        } else if existing != 2 {
            return Err(INVALID_STATE.into());
        }
        let registry_id = check_state(&tx, None)?;
        // Validate all durable associations at startup without requiring any
        // registered source or original managed mirror to exist on disk.
        read_sources(&tx, &app_data_dir, &registry_id, None)?;
        tx.commit().map_err(storage_error)?;
        Ok(Self {
            #[cfg(any(test, feature = "test-support"))]
            checkout_accesses: std::sync::atomic::AtomicUsize::new(0),
            conn,
            registry_id,
            app_data_dir,
        })
    }

    pub fn register_local(&mut self, path: &Path) -> Result<RegisteredSource, String> {
        let root = canonical_directory(path)?;
        let root_text = path_text(&root)?;
        let display_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Local source");
        bounded_text(display_name, MAX_DISPLAY_BYTES)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        check_state(&tx, Some(&self.registry_id))?;
        if let Some(source) = read_one(
            &tx,
            &self.app_data_dir,
            &self.registry_id,
            "root",
            root_text,
        )? {
            tx.commit().map_err(storage_error)?;
            return Ok(source);
        }
        let source_id = vacant_source_id(&tx)?;
        let repo_key = format!("local/{source_id}");
        require_one(
            tx.execute(
                "INSERT INTO registered_sources(source_id, repo_key, root, display_name, kind, ready) VALUES (?1, ?2, ?3, ?4, 'local', 1)",
                params![source_id, repo_key, root_text, display_name],
            ).map_err(storage_error)?,
        )?;
        let source = read_one(
            &tx,
            &self.app_data_dir,
            &self.registry_id,
            "source_id",
            &source_id,
        )?
        .ok_or(INVALID_STATE)?;
        tx.commit().map_err(storage_error)?;
        Ok(source)
    }

    pub fn reserve_managed(&mut self, url: &str) -> Result<RegisteredSource, String> {
        bounded_text(url, MAX_ORIGIN_BYTES)?;
        let origin = parse_managed_origin(url).map_err(|_| managed_origin_error(url))?;
        origin.validate().map_err(|_| INVALID_STATE.to_string())?;
        bounded_text(&origin.display_name, MAX_DISPLAY_BYTES)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        check_state(&tx, Some(&self.registry_id))?;
        if let Some(source) = read_one(
            &tx,
            &self.app_data_dir,
            &self.registry_id,
            "origin_key",
            &origin.key,
        )? {
            tx.commit().map_err(storage_error)?;
            return Ok(source);
        }
        let source_id = vacant_source_id(&tx)?;
        let checkout = ManagedCheckout::new(&self.app_data_dir, &self.registry_id, &source_id)
            .map_err(|_| INVALID_STATE.to_string())?;
        let root = path_text(checkout.root())?;
        let repo_key = origin
            .github_repo
            .clone()
            .unwrap_or_else(|| format!("local/{source_id}"));
        bounded_text(&repo_key, MAX_REPO_BYTES)?;
        require_one(
            tx.execute(
                "INSERT INTO registered_sources(source_id, repo_key, root, display_name, kind, origin_key, clone_url, github_repo, ready) VALUES (?1, ?2, ?3, ?4, 'managed', ?5, ?6, ?7, 0)",
                params![source_id, repo_key, root, origin.display_name, origin.key, origin.clone_url, origin.github_repo],
            ).map_err(storage_error)?,
        )?;
        let source = read_one(
            &tx,
            &self.app_data_dir,
            &self.registry_id,
            "source_id",
            &source_id,
        )?
        .ok_or(INVALID_STATE)?;
        tx.commit().map_err(storage_error)?;
        Ok(source)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<RegisteredSource>, String> {
        if !valid_id(id, "src_") {
            return Err("Invalid registered source identifier".into());
        }
        self.lookup("source_id", id)
    }

    pub fn get_by_repo(&self, repo: &str) -> Result<Option<RegisteredSource>, String> {
        bounded_text(repo, MAX_REPO_BYTES)?;
        self.lookup("repo_key", repo)
    }

    pub fn get_by_root(&self, canonical_root: &Path) -> Result<Option<RegisteredSource>, String> {
        self.lookup("root", path_text(canonical_root)?)
    }

    fn lookup(&self, field: &str, value: &str) -> Result<Option<RegisteredSource>, String> {
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        check_state(&tx, Some(&self.registry_id))?;
        let source = read_one(&tx, &self.app_data_dir, &self.registry_id, field, value)?;
        tx.commit().map_err(storage_error)?;
        Ok(source)
    }

    pub fn list(&self) -> Result<Vec<RegisteredSource>, String> {
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        check_state(&tx, Some(&self.registry_id))?;
        let sources = read_sources(&tx, &self.app_data_dir, &self.registry_id, None)?;
        tx.commit().map_err(storage_error)?;
        Ok(sources)
    }

    /// Materialize one coherent registry revision under explicit row and raw
    /// UTF-8 SQL text byte ceilings. These bound registry input materialization,
    /// not allocator overhead, derived copies, SQLite caches or process RSS.
    /// Zero ceilings admit an empty registry. The standalone read transaction
    /// must not be called inside another transaction on this registry connection.
    pub fn list_bounded(
        &self,
        limits: SourceListLimits,
    ) -> Result<Vec<RegisteredSource>, SourceListError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|_| SourceListError::Storage)?;
        bounded::preflight(&tx, limits)?;
        check_state(&tx, Some(&self.registry_id)).map_err(bounded::registry_error)?;
        let sources = read_sources(&tx, &self.app_data_dir, &self.registry_id, None)
            .map_err(bounded::registry_error)?;
        tx.commit().map_err(|_| SourceListError::Storage)?;
        Ok(sources)
    }

    /// Managed readiness is durable operational state. Setting it cannot make
    /// a missing/substituted directory readable; readers still need their guard.
    pub fn set_ready(&mut self, source_id: &str, ready: bool) -> Result<(), String> {
        self.set_ready_batch(&[source_id.to_owned()], ready)
    }

    /// A system operation publishes every managed member's availability in one
    /// state transaction. A failure cannot expose only a prefix of the group.
    pub fn set_ready_batch(&mut self, source_ids: &[String], ready: bool) -> Result<(), String> {
        if source_ids.iter().any(|id| !valid_id(id, "src_")) {
            return Err("Invalid registered source identifier".into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        check_state(&tx, Some(&self.registry_id))?;
        for source_id in source_ids {
            let source = read_one(
                &tx,
                &self.app_data_dir,
                &self.registry_id,
                "source_id",
                source_id,
            )?
            .ok_or("Registered source is unavailable")?;
            if !source.is_managed() {
                return Err("Only managed sources have mutable readiness".into());
            }
            require_one(
                tx.execute(
                    "UPDATE registered_sources SET ready=?2 WHERE source_id=?1",
                    params![source_id, ready],
                )
                .map_err(storage_error)?,
            )?;
        }
        tx.commit().map_err(storage_error)
    }
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == ID_SUFFIX_BYTES
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn bounded_text(value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        Err("Source registration value is empty, too large, or invalid".into())
    } else {
        Ok(())
    }
}

fn path_text(path: &Path) -> Result<&str, String> {
    let text = path
        .to_str()
        .ok_or("Source registration requires a UTF-8 path")?;
    bounded_text(text, MAX_PATH_BYTES)?;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err("Source registration requires an absolute normalized path".into());
    }
    Ok(text)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    // Check the caller's representation before filesystem resolution as well as
    // the canonical result; neither boundary substitutes a lossy UTF-8 string.
    let input = path
        .to_str()
        .ok_or("Source registration requires a UTF-8 path")?;
    bounded_text(input, MAX_PATH_BYTES)?;
    let canonical = crate::paths::canonicalize(path)
        .map_err(|_| "Source directory is unavailable".to_string())?;
    path_text(&canonical)?;
    if !canonical.is_dir() {
        return Err("Source root must be a directory".into());
    }
    Ok(canonical)
}

fn require_one(count: usize) -> Result<(), String> {
    if count == 1 {
        Ok(())
    } else {
        Err(INVALID_STATE.into())
    }
}

fn random_id(conn: &Connection, prefix: &str) -> Result<String, String> {
    let suffix: String = conn
        .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
        .map_err(storage_error)?;
    let id = format!("{prefix}{suffix}");
    if !valid_id(&id, prefix) {
        return Err(INVALID_STATE.into());
    }
    Ok(id)
}

fn vacant_source_id(conn: &Connection) -> Result<String, String> {
    for _ in 0..ID_RETRIES {
        let id = random_id(conn, "src_")?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM registered_sources WHERE source_id=?1)",
                [&id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exists {
            return Ok(id);
        }
    }
    Err("Could not allocate a unique registered source identifier".into())
}

fn retire_legacy_findings(conn: &Connection) -> Result<(), String> {
    let table: Option<String> = conn
        .query_row(
            "SELECT type FROM sqlite_master WHERE name='findings'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    if let Some(kind) = table {
        let triggers: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='findings'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if kind != "table" || triggers != 0 {
            return Err(INVALID_STATE.into());
        }
        // All production findings currently originate from this detector. Keep
        // custom rows and every unrelated state table, including historical jobs.
        conn.execute(
            "DELETE FROM findings WHERE detector=?1",
            [ingest::preflight::DETECTOR_ID],
        )
        .map_err(storage_error)?;
    }
    Ok(())
}

fn check_state(conn: &Connection, expected_id: Option<&str>) -> Result<String, String> {
    // Accept exactly our private DDL (including constraints/conflict policies),
    // leaving all unrelated shared state tables untouched. Reject attached
    // triggers and user indexes; automatic UNIQUE indexes are SQLite-owned.
    for (name, schema) in [
        ("source_registry_meta", META_SCHEMA),
        ("registered_sources", SOURCE_SCHEMA),
    ] {
        let exact: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 AND type='table' AND sql=?2)",
            params![name, schema], |row| row.get(0),
        ).map_err(storage_error)?;
        let extras: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE tbl_name=?1 AND name!=?1 AND NOT (type='index' AND name GLOB 'sqlite_autoindex_*' AND sql IS NULL)",
            [name], |row| row.get(0),
        ).map_err(storage_error)?;
        if !exact || extras != 0 {
            return Err(INVALID_STATE.into());
        }
    }
    let invalid_meta: bool = conn.query_row(
        "SELECT count(*) != 1 OR coalesce(max(NOT (singleton=1 AND schema_version=1 AND findings_retired=1 AND typeof(registry_id)='text' AND length(CAST(registry_id AS BLOB))=36)), 1) FROM source_registry_meta",
        [], |row| row.get(0),
    ).map_err(storage_error)?;
    if invalid_meta {
        return Err(INVALID_STATE.into());
    }
    let registry_id: String = conn
        .query_row("SELECT registry_id FROM source_registry_meta", [], |row| {
            row.get(0)
        })
        .map_err(storage_error)?;
    if !valid_id(&registry_id, "reg_") || expected_id.is_some_and(|id| id != registry_id) {
        return Err(INVALID_STATE.into());
    }
    let invalid_rows: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM registered_sources WHERE
         typeof(source_id)!='text' OR length(CAST(source_id AS BLOB))!=36 OR
         typeof(repo_key)!='text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND ?1 OR
         typeof(root)!='text' OR length(CAST(root AS BLOB)) NOT BETWEEN 1 AND ?2 OR
         typeof(display_name)!='text' OR length(CAST(display_name AS BLOB)) NOT BETWEEN 1 AND ?3 OR
         typeof(kind)!='text' OR kind NOT IN ('local','managed') OR
         (origin_key IS NOT NULL AND (typeof(origin_key)!='text' OR length(CAST(origin_key AS BLOB)) NOT BETWEEN 1 AND ?4)) OR
         (clone_url IS NOT NULL AND (typeof(clone_url)!='text' OR length(CAST(clone_url AS BLOB)) NOT BETWEEN 1 AND ?4)) OR
         (github_repo IS NOT NULL AND (typeof(github_repo)!='text' OR length(CAST(github_repo AS BLOB)) NOT BETWEEN 1 AND ?1)) OR
         typeof(ready)!='integer' OR ready NOT IN (0,1))",
        params![MAX_REPO_BYTES as i64, MAX_PATH_BYTES as i64, MAX_DISPLAY_BYTES as i64, MAX_ORIGIN_BYTES as i64],
        |row| row.get(0),
    ).map_err(storage_error)?;
    if invalid_rows {
        return Err(INVALID_STATE.into());
    }
    Ok(registry_id)
}

fn read_one(
    conn: &Connection,
    app_data_dir: &Path,
    registry_id: &str,
    field: &str,
    value: &str,
) -> Result<Option<RegisteredSource>, String> {
    let mut sources = read_sources(conn, app_data_dir, registry_id, Some((field, value)))?;
    if sources.len() > 1 {
        return Err(INVALID_STATE.into());
    }
    Ok(sources.pop())
}

fn read_sources(
    conn: &Connection,
    app_data_dir: &Path,
    registry_id: &str,
    filter: Option<(&str, &str)>,
) -> Result<Vec<RegisteredSource>, String> {
    let sql = match filter {
        Some((field @ ("source_id" | "repo_key" | "root" | "origin_key"), _)) => {
            format!("{SELECT_SOURCE} WHERE {field}=?1 ORDER BY source_id")
        }
        Some(_) => return Err(INVALID_STATE.into()),
        None => format!("{SELECT_SOURCE} ORDER BY source_id"),
    };
    let mut statement = conn.prepare(&sql).map_err(storage_error)?;
    let mut rows = match filter {
        Some((_, value)) => statement.query([value]),
        None => statement.query([]),
    }
    .map_err(storage_error)?;
    let mut sources = Vec::new();
    while let Some(row) = rows.next().map_err(storage_error)? {
        let source_id: String = row.get(0).map_err(storage_error)?;
        let repo_key: String = row.get(1).map_err(storage_error)?;
        let root = PathBuf::from(row.get::<_, String>(2).map_err(storage_error)?);
        let display_name: String = row.get(3).map_err(storage_error)?;
        let kind: String = row.get(4).map_err(storage_error)?;
        let origin_key: Option<String> = row.get(5).map_err(storage_error)?;
        let clone_url: Option<String> = row.get(6).map_err(storage_error)?;
        let github_repo: Option<String> = row.get(7).map_err(storage_error)?;
        let ready: bool = row.get(8).map_err(storage_error)?;
        if !valid_id(&source_id, "src_") {
            return Err(INVALID_STATE.into());
        }
        path_text(&root).map_err(|_| INVALID_STATE.to_string())?;
        bounded_text(&display_name, MAX_DISPLAY_BYTES).map_err(|_| INVALID_STATE.to_string())?;
        let origin = if kind == "managed" {
            let origin = ManagedOrigin {
                key: origin_key.ok_or(INVALID_STATE)?,
                clone_url: clone_url.ok_or(INVALID_STATE)?,
                display_name: display_name.clone(),
                github_repo,
            };
            origin.validate().map_err(|_| INVALID_STATE.to_string())?;
            let expected_root = ManagedCheckout::new(app_data_dir, registry_id, &source_id)
                .map_err(|_| INVALID_STATE.to_string())?;
            let expected_repo = origin
                .github_repo
                .clone()
                .unwrap_or_else(|| format!("local/{source_id}"));
            if root != expected_root.root() || repo_key != expected_repo {
                return Err(INVALID_STATE.into());
            }
            Some(origin)
        } else if kind == "local"
            && origin_key.is_none()
            && clone_url.is_none()
            && github_repo.is_none()
            && ready
            && repo_key == format!("local/{source_id}")
        {
            None
        } else {
            return Err(INVALID_STATE.into());
        };
        sources.push(RegisteredSource {
            source_id,
            repo_key,
            display_name,
            root,
            origin,
            ready,
            registry_id: registry_id.to_string(),
            app_data_dir: app_data_dir.to_path_buf(),
        });
    }
    Ok(sources)
}

#[cfg(test)]
#[path = "sources/tests.rs"]
mod tests;
