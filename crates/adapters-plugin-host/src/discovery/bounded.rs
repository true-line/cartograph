//! Opt-in bounded discovery. Legacy best-effort discovery remains unchanged.
use super::{DiscoveredPlugin, PROJECT_ADAPTER_DIR, PluginScope};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Explicit aggregate admission across every supplied root and directory entry.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryLimits {
    /// All enumerated entries, including ignored extensions and shadowed copies.
    pub max_entries: usize,
    /// Total artifact bytes read to hash, including shadowed copies.
    pub max_file_bytes: u64,
    /// Native encoded path/name bytes plus one per charged field. Includes input
    /// roots, derived directories, visited paths and retained identity copies.
    /// Container/allocator overhead and process RSS are not measured by this cap.
    pub max_metadata_bytes: usize,
}

/// Fixed source-free discovery failures; no partial inventory is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// Too many entries were enumerated.
    #[error("plugin discovery entry limit exceeded")]
    EntryLimit,
    /// Aggregate artifact input exceeded its admission.
    #[error("plugin discovery file byte limit exceeded")]
    FileByteLimit,
    /// Paths or identity metadata exceeded their admission.
    #[error("plugin discovery metadata limit exceeded")]
    MetadataLimit,
    /// A wasm artifact is a symlink, nonregular file or has an invalid identity.
    #[error("plugin discovery artifact is invalid")]
    InvalidArtifact,
    /// Enumeration, metadata, open or streaming read failed.
    #[error("plugin discovery input is unavailable")]
    Io,
}

struct Budget {
    entries: usize,
    file_bytes: u64,
    metadata_bytes: usize,
}
impl Budget {
    fn metadata(&mut self, bytes: usize) -> Result<(), DiscoveryError> {
        self.metadata_bytes = bytes
            .checked_add(1)
            .and_then(|bytes| self.metadata_bytes.checked_sub(bytes))
            .ok_or(DiscoveryError::MetadataLimit)?;
        Ok(())
    }
    fn path(&mut self, path: &Path) -> Result<(), DiscoveryError> {
        self.metadata(path.as_os_str().as_encoded_bytes().len())
    }
}

/// Discover with legacy hashes, ordering and project precedence, under explicit
/// aggregate admission. Missing directories are empty; all other I/O errors
/// fail closed. Only regular, nonsymlink wasm entries are admitted. Pre-open
/// entry type and opened-handle metadata are checked, but filesystem races and
/// an immutable file revision are not excluded by these checks.
///
/// Hashing uses a fixed buffer, never materializes an artifact, and reads at most
/// the remaining admitted file bytes plus one sentinel byte to detect growth.
/// This bounds modeled input work and retained metadata, not exact process RSS.
pub fn discover_bounded(
    project_roots: &[PathBuf],
    user_dir: &Path,
    limits: DiscoveryLimits,
) -> Result<Vec<DiscoveredPlugin>, DiscoveryError> {
    let mut budget = Budget {
        entries: limits.max_entries,
        file_bytes: limits.max_file_bytes,
        metadata_bytes: limits.max_metadata_bytes,
    };
    budget.path(user_dir)?;
    // Admit every root before allocating the sorting vector. The per-field
    // charge also bounds the number of empty root paths supplied by the caller.
    for root in project_roots {
        budget.path(root)?;
        budget.metadata(
            root.as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add(PROJECT_ADAPTER_DIR.len() + 1)
                .ok_or(DiscoveryError::MetadataLimit)?,
        )?;
    }
    let mut roots: Vec<_> = project_roots.iter().collect();
    roots.sort();
    let mut by_id = BTreeMap::new();
    scan(user_dir, PluginScope::User, None, &mut budget, &mut by_id)?;
    for root in roots {
        scan(
            &root.join(PROJECT_ADAPTER_DIR),
            PluginScope::Project,
            Some(root),
            &mut budget,
            &mut by_id,
        )?;
    }
    Ok(by_id.into_values().collect())
}

fn scan(
    dir: &Path,
    scope: PluginScope,
    project_root: Option<&Path>,
    budget: &mut Budget,
    by_id: &mut BTreeMap<String, DiscoveredPlugin>,
) -> Result<(), DiscoveryError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(DiscoveryError::Io),
    };
    for entry in entries {
        budget.entries = budget
            .entries
            .checked_sub(1)
            .ok_or(DiscoveryError::EntryLimit)?;
        let entry = entry.map_err(|_| DiscoveryError::Io)?;
        let name = entry.file_name();
        budget.metadata(name.as_encoded_bytes().len())?;
        // Admit the joined path before entry.path() allocates it.
        budget.metadata(
            dir.as_os_str()
                .as_encoded_bytes()
                .len()
                .checked_add(name.as_encoded_bytes().len())
                .and_then(|n| n.checked_add(1))
                .ok_or(DiscoveryError::MetadataLimit)?,
        )?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or(DiscoveryError::InvalidArtifact)?;
        let kind = entry.file_type().map_err(|_| DiscoveryError::Io)?;
        if !kind.is_file() {
            return Err(DiscoveryError::InvalidArtifact);
        }
        // Reject known oversize inputs before opening/reading, then recheck the
        // actual handle; neither size observation replaces the streaming cap.
        if entry.metadata().map_err(|_| DiscoveryError::Io)?.len() > budget.file_bytes {
            return Err(DiscoveryError::FileByteLimit);
        }
        let mut file = File::open(&path).map_err(|_| DiscoveryError::Io)?;
        let metadata = file.metadata().map_err(|_| DiscoveryError::Io)?;
        if !metadata.is_file() {
            return Err(DiscoveryError::InvalidArtifact);
        }
        if metadata.len() > budget.file_bytes {
            return Err(DiscoveryError::FileByteLimit);
        }
        // Admit the fixed hex identity before allocating it, even for a shadowed copy.
        budget.metadata(64)?;
        let hash = hash_reader(&mut file, &mut budget.file_bytes)?;
        let shadowed = by_id
            .get(stem)
            .is_some_and(|existing| existing.scope == PluginScope::User);
        if scope == PluginScope::Project
            && by_id
                .get(stem)
                .is_some_and(|existing| existing.scope == PluginScope::Project)
        {
            continue;
        }
        // Each cloned identity/path field is charged before allocation.
        // Visited/shadowed metadata consumes admission permanently.
        budget.metadata(stem.len())?;
        budget.metadata(stem.len())?;
        budget.path(&path)?;
        if let Some(root) = project_root {
            budget.path(root)?;
        }
        by_id.insert(
            stem.to_owned(),
            DiscoveredPlugin {
                id: stem.to_owned(),
                path,
                content_hash: hash,
                scope,
                project_root: project_root.map(Path::to_path_buf),
                shadowed_user_copy: shadowed,
            },
        );
    }
    Ok(())
}

fn hash_reader(reader: &mut impl Read, remaining: &mut u64) -> Result<String, DiscoveryError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let take = buffer
            .len()
            .min(usize::try_from(remaining.saturating_add(1)).unwrap_or(usize::MAX));
        let count = reader
            .read(&mut buffer[..take])
            .map_err(|_| DiscoveryError::Io)?;
        if count == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        *remaining = remaining
            .checked_sub(count as u64)
            .ok_or(DiscoveryError::FileByteLimit)?;
        hasher.update(&buffer[..count]);
    }
}

#[cfg(test)]
#[path = "bounded_tests.rs"]
mod tests;
