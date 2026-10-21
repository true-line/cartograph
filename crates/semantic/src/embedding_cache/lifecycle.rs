//! Fixed host-owned disposable database lifecycle.
use super::*;

impl EmbeddingCache {
    /// Open a cache without authority to discard an existing foreign database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        Self::open_mode(path.as_ref(), false)
    }
    /// Host-only authority for the fixed disposable app-data cache. Never use a
    /// caller-selected database path. Linked files are not owned disposable state.
    pub fn open_disposable(app_data: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let directory = app_data.as_ref().canonicalize().map_err(error)?;
        let path = directory.join("embeddings.db");
        Self::check_owned_files(&path)?;
        Self::open_mode(&path, true)
    }
    fn check_owned_files(path: &Path) -> Result<(), ProviderError> {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            match std::fs::symlink_metadata(PathBuf::from(name)) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        return Err(error("cache or sidecar is not an owned regular file"));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if metadata.nlink() != 1 {
                            return Err(error("cache or sidecar has multiple hard links"));
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error(e)),
            }
        }
        Ok(())
    }
    fn open_mode(path: &Path, disposable: bool) -> Result<Self, ProviderError> {
        // NOFOLLOW also rejects symlinked ancestor paths on SQLite builds;
        // canonicalize only the parent, never follow the database itself.
        let path = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .map_err(error)?
            .join(
                path.file_name()
                    .ok_or_else(|| error("database filename required"))?,
            );
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(error)?;
        let mut result = Self {
            connection,
            disposable,
            path,
        };
        if let Err(failure) = result.initialize().and_then(|()| result.validate()) {
            if !disposable {
                return Err(failure);
            }
            result.recover()?;
        }
        Ok(result)
    }
    pub(super) fn initialize(&mut self) -> Result<(), ProviderError> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )
            .map_err(error)?;
        if count == 0 {
            let tx = self.connection.transaction().map_err(error)?;
            tx.execute_batch(SCHEMA).map_err(error)?;
            tx.execute(
                "INSERT INTO cache_meta VALUES(1,'',lower(hex(randomblob(16))),0,0)",
                [],
            )
            .map_err(error)?;
            tx.pragma_update(None, "user_version", 2).map_err(error)?;
            tx.commit().map_err(error)?;
        } else {
            // Reject foreign/newer schemas before journal-mode changes or DDL.
            self.validate()?;
        }
        self.connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(error)?;
        self.connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(error)?;
        Ok(())
    }
    pub(super) fn recover(&mut self) -> Result<(), ProviderError> {
        if !self.disposable {
            return Err(error("no disposable database authority"));
        }
        Self::check_owned_files(&self.path)?;
        // SQLite resets main without interpreting foreign schema/triggers or
        // unlinking paths. Genuine locking/read-only/I/O failures remain errors.
        use rusqlite::config::DbConfig;
        self.connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_RESET_DATABASE, true)
            .map_err(error)?;
        let reset = self.connection.execute_batch("VACUUM main");
        let restore = self
            .connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_RESET_DATABASE, false);
        reset.map_err(error)?;
        restore.map_err(error)?;
        self.initialize()?;
        self.validate()
    }
    pub(super) fn ensure(&mut self) -> Result<(), ProviderError> {
        if let Err(failure) = self.validate() {
            if !self.disposable {
                return Err(failure);
            }
            self.recover()?;
        }
        Ok(())
    }
}
