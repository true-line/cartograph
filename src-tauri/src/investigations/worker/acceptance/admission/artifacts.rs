//! Bounded immutable artifacts for @10 preparation and stage receipts.
//! A content reference proves byte identity, not semantic review or authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(super) const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactRef {
    pub sha256: String,
    pub byte_len: u64,
}

impl ArtifactRef {
    fn of(bytes: &[u8]) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            byte_len: bytes.len() as u64,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.byte_len > MAX_ARTIFACT_BYTES as u64
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid admission artifact reference.".into());
        }
        Ok(())
    }
}

/// Exclusive creation deliberately leaves the file in place on every error
/// after open. An uncertain write must not make a stage eligible for replay.
pub(super) fn write_new(path: &Path, value: &impl Serialize) -> Result<ArtifactRef, String> {
    let bytes = crate::investigations::context::bounded_json(value, MAX_ARTIFACT_BYTES)
        .map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    // Directory synchronization establishes publication durability on Unix.
    // Windows native durability remains an operator evidence requirement.
    #[cfg(unix)]
    File::open(path.parent().ok_or("Missing artifact parent.")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(ArtifactRef::of(&bytes))
}

/// Verify a regular file against its frozen reference before deserialization.
/// The controller separately validates protocol, family and semantic bindings.
pub(super) fn read_verified(path: &Path, expected: &ArtifactRef) -> Result<Vec<u8>, String> {
    expected.validate()?;
    if !std::fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Admission artifact must be a regular file.".into());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != expected.byte_len {
        return Err("Admission artifact length changed.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ARTIFACT_BYTES || ArtifactRef::of(&bytes) != *expected {
        return Err("Admission artifact content changed.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
